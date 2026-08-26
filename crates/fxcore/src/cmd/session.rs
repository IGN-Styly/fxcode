//! Session/agent/turn lifecycle handlers.

// Imports to restore as you define the types:
// use std::path::PathBuf;
//
// use tracing::warn;
//
// use super::Ctx;
// use fxproto::command::Command;
// use fxproto::content::{ContentBlock, StopReason};
// use fxproto::driver::DriverId;
// use fxproto::event::{AgentStatus, FxEvent};
// use fxproto::ids::{AgentId, SessionId};
// use fxproto::reply::Reply;

// Handler-wide conventions (apply to every fn below):
// - Event emissions happen ONLY via ctx.sink.emit (cmd/mod.rs pipeline G1–G3).
// - "no event" on a validation failure is LOAD-BEARING: rejections that change
//   no state must not dirty the log (Reply::Error already tells the client why).
// - AgentStatus::Crashed is emitted in exactly two places system-wide:
//     (a) start_agent when spawn/initialize fails before Ready (below), and
//     (b) the AcpConnection actor itself on post-Ready process death
//         (driver/acp/mod.rs — sole owner, so no duplicates).
//   Every other failure path returns a plain Reply::Error. Nothing else may
//   emit Crashed.

// TODO: one fn per command branch:

// StartAgent → Reply::Started | Error(AgentStartFailed)
//
// pub async fn start_agent(ctx: &mut Ctx, driver: DriverId) -> Result<Reply>;
//   1. plan = ctx.registry.plan(driver).await
//        Err => return Error(AgentStartFailed, detail). NO EVENT (agent did not
//               exist yet; detection/planning touched no state worth logging).
//   2. agent_id = ctx.idgen.agent()                       // mint BEFORE spawn so
//                                                         // events can carry it
//      emit AgentStatus { agent: agent_id, driver, status: Starting }.
//   3. conn = AcpConnection::start(&agent_id, &plan, ctx.events_tx.clone()).await
//        Ok  =>
//          4a. emit AgentStatus { status: Ready }
//          5a. ctx.conns.insert(agent_id, Arc::new(conn));
//              return Reply::Started { agent: agent_id }.
//        Err(e) after retries exhausted =>
//          4b. emit AgentStatus { agent: agent_id, driver,
//                                 status: Crashed { exit_code: None } }   // (a)
//          5b. return Error(AgentStartFailed, format!("{e}")). The starting
//              attempt IS on the record even though nothing else ever was —
//              replays show the failed spawn, which audit matters more than
//              tidiness.
//   Retry ladder during step 3 lives inside AcpConnection::start
//   (START_ATTEMPTS / START_BACKOFF_MS); this handler stays linear.

// NewSession → Reply::SessionCreated | Error(AgentNotFound or Internal)
//
// pub async fn new_session(ctx: &mut Ctx, agent: AgentId, cwd: PathBuf,
//                          mcp: Vec<McpServerSpec>) -> Result<Reply>;
//   1. conn = ctx.conns.get(&agent) missing OR not ConnState::Ready
//        => return Error(AgentNotFound, "not running/not ready"). NO EVENT.
//   2. acp_session = conn.new_session(&cwd, &mcp).await
//        Err(e) => return Error(Internal, "ACP session/new failed: {e}").
//                 NO EVENT. NOTE vs command.rs pairing table (which lists only
//                 AgentNotFound): a LIVE agent failing session/new cannot be
//                 honestly reported as AgentNotFound; Internal carries the
//                 detail instead. Flagged to fxproto owners in scaffold report.
//   3. session_id = SessionId::from_raw(acp_session.clone())   // ADOPTED verbatim
//      conn.register_session(session_id, acp_session).await    // actor map entry
//   4. emit SessionCreated { session: session_id, agent, cwd, mcp_servers: mcp }
//        THE durable record (one event one fact; cwd/mcp live ONLY here).
//   5. ctx.session_agent.insert(session_id, agent);
//   6. return Reply::SessionCreated { session: session_id }.
//   Ordering note: because sink.emit completed at step 4, any client observing
//   the Reply observes >= its seq (pipeline G2) — replay can never miss the
//   session a successful reply promised.

// Prompt → Reply::PromptAccepted | Error(SessionNotFound | TurnNotActive)
//
// pub async fn prompt(ctx: &mut Ctx, s: SessionId, blocks: Vec<ContentBlock>)
//     -> Result<Reply>;
//   1. !ctx.projections.session_exists(&s) => Error(SessionNotFound). NO EVENT.
//   2. ctx.projections.turn_active(&s)     => Error(TurnNotActive,
//            "turn already running"). NO EVENT. (Same check doubles as the fold's
//            active_turn invariant guard; server side enforces it HERE first.)
//   3. agent = ctx.session_agent[&s] (guaranteed by construction of step 1;
//            absent => Internal bug-guard return, warn! logged).
//      turn  = ctx.idgen.turn();
//   4. emit TurnStarted  { session: s, turn };
//   5. echo user content: text = flatten(blocks) per fxproto event.rs rule
//      (Texts joined; non-text dropped + debug! — same normalize R1 semantics)
//      if !text.is_empty(): emit Chunk { session: s, turn, role: User, text }.
//      Empty-after-flatten prompt (images only) skips the echo silently — v0
//      transcript must not contain empty messages.
//   6. emit AgentStatus { agent, driver: <from AgentsState>, status: Busy }
//            (status dots track agents with an open turn; cleared at 8a/8c).
//   7. handle = tokio::spawn(turn_task(...)); ctx.turn_tasks.insert(s, handle);
//            task captures: sink clone, conn Arc, agent, s, turn, weak ref to
//            perms registry + events (see perms.rs sweep on conn death).
//   8. return Reply::PromptAccepted { turn } IMMEDIATELY (does NOT await the
//            turn). Accepted nondeterminism: reply delivery races the turn's
//            first events across DIFFERENT transports; total order is defined
//            by seq alone (bus/WS clients), never by reply arrival.
//   TURN TASK body (runs outside Ctx; touches only captured clones):
//   8a. Ok(stop_reason) = conn.prompt(acp_session, blocks).await =>
//            emit TurnFinished { session: s, turn, stop_reason };
//            emit AgentStatus { agent, status: Ready };
//       ---- both under one sink call sequence; chunk/tool events streamed by
//       ---- the agent DURING prompt() precede these via the global pump.
//   8b. Err(_conn_died_or_protocol_dead) =>
//            emit TurnFinished { session: s, turn, stop_reason: Cancelled };
//            do NOT emit Crashed here — the ACTOR owns Crashed exclusively
//            (prevents double-emission when child death caused this Err);
//            then trigger sweep_cancelled_for_conn_death for s via perms.rs
//            (answers parked responders Outcome::Cancelled + emits
//            PermissionResolved { chosen: None } each — see cmd/perms.rs).
//   8c. either arm finally removes ctx entry by... it CAN'T touch Ctx (single-
//            threaded actor owns it) => turn completion ALSO sends a tiny
//            Job::TurnDone { session } through the orchestrator cmd channel;
//            actor loop pops turn_tasks[s] there. No lock choreography needed.

// Cancel → Reply::Cancelled | Error(SessionNotFound | TurnNotActive)
//
// pub async fn cancel(ctx: &mut Ctx, s: SessionId) -> Result<Reply>;
//   1. !session_exists(s) => Error(SessionNotFound). NO EVENT.
//   2. None = ctx.turn_tasks.remove(&s) => Error(TurnNotActive,
//            "no running turn"). NO EVENT.
//   3. claim = finish_claim once-signal shared between watchdog and turn task:
//            whoever claims FIRST emits the force-finish TurnFinished; the other
//            observes Claimed and becomes a no-op (fold W7 would absorb double
//            finishes anyway, but we avoid the warn! noise).
//   4. perms::sweep_cancelled(ctx, s): answer ALL pending permission requests
//            for s with outcome Cancelled + PermissionResolved{chosen: None}
//            events (BEFORE the cancel notification reaches the agent — fixed
//            order makes tests deterministic; ACP REQUIRES answering pendings).
//   5. conn.cancel(acp_session)   // fire-and-forget notification
//   6. spawn WATCHDOG: after CANCEL_WATCHDOG force-empties:
//            if claim not taken { emit TurnFinished { stop_reason: Cancelled } +
//                                  emit AgentStatus Ready } and drop `handle`
//            (aborts the stuck prompt future; agent-side zombie turns die at
//            next session/prompt contract anyway).
//   7. return Reply::Cancelled NOW (ack only; TurnFinished still streams from
//            6/the normal turn-task path 8a when the agent responds promptly).
//
// Watchdog tunable: default 10s; env override FX_CANCEL_WATCHDOG_MS read ONCE
// at orchestrator boot (NOT per-cancel) so integration tests shorten it
// without cfg churn. Documented for tests/orchestrator.rs watchdog scenario.
// pub const CANCEL_WATCHDOG: Duration = Duration::from_secs(10);

// Cross-cutting source-of-truth recap (error-path matrix):
//   validation misses                    → plain Error, zero events
//   spawn/initialize exhausted           → Crashed(a) + Error(AgentStartFailed)
//   post-Ready child death               → Crashed(b) by actor (+ turn task 8b)
//   mid-turn ACP protocol error          → turn 8b only (TurnFinished Cancelled)
//   store append failure (any emitter)   → Err propagates; NO partial pipeline
