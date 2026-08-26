//! Session/agent/turn lifecycle handlers.
//!
//! Handler-wide conventions (apply to every fn):
//! - Event emissions happen ONLY via ctx.sink.emit (cmd/mod.rs pipeline G1–G3).
//! - "no event" on a validation failure is LOAD-BEARING: rejections that change
//!   no state must not dirty the log (Reply::Error already tells the client why).
//! - AgentStatus::Crashed is emitted in exactly two places system-wide:
//! - start_agent when spawn/initialize fails before Ready, and
//! - the AcpConnection runner itself on post-Ready process death (driver/acp
//!   owns it exclusively, so no duplicates).
//!
//! Every other failure path returns a plain Reply::Error. Nothing else may
//! emit Crashed.
//!
//! Accepted nondeterminism (prompt step 8): reply delivery races the turn's
//! first events across DIFFERENT transports; total order is defined by seq
//! alone (bus/WS clients), never by reply arrival.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fxproto::content::{ContentBlock, McpServerSpec, Role as FxRole, StopReason};
use fxproto::driver::DriverId;
use fxproto::event::{AgentStatus, FxEvent};
use fxproto::ids::{AgentId, SessionId, TurnId};
use fxproto::reply::{FxError, FxErrorCode, Reply};
use tracing::{debug, warn};

use super::{Ctx, EventSink, FinishClaim, InternalCmd, PermShared, TurnHandle, perms};

/// Watchdog tunable: default 10s; env override FX_CANCEL_WATCHDOG_MS read ONCE
/// at orchestrator boot (NOT per-cancel) so integration tests shorten it
/// without cfg churn (documented for tests/orchestrator.rs scenarios).
pub const CANCEL_WATCHDOG: Duration = Duration::from_secs(10);

pub fn watchdog_from_env() -> Duration {
    match std::env::var("FX_CANCEL_WATCHDOG_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        Some(ms) => Duration::from_millis(ms),
        None => CANCEL_WATCHDOG,
    }
}

fn reply_err(code: FxErrorCode, message: impl Into<String>) -> Reply {
    Reply::Error(FxError {
        code,
        message: message.into(),
    })
}

/// Consecutive Text blocks flatten per event.rs's chunk-vs-blocks rule; other
/// blocks drop with a debug line (identical to normalize R1 semantics: the
/// composer sends [Text] today, nothing real is lost).
fn flatten_blocks(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => out.push_str(text),
            other => {
                debug!(target: "session", block = ?other, "non-text prompt block dropped from echo");
            }
        }
    }
    out
}

// StartAgent → Reply::Started | Error(AgentStartFailed)
pub async fn start_agent(ctx: &mut Ctx<'_>, driver: DriverId) -> Result<Reply, crate::Error> {
    // 1. Planning failure: nothing existed yet, nothing is logged.
    let plan = match ctx.registry.plan(driver).await {
        Ok(p) => p,
        Err(e) => return Ok(reply_err(FxErrorCode::AgentStartFailed, e.to_string())),
    };

    // 2. Mint BEFORE spawn so events can carry it; publish Starting immediately
    //    so clients see something while the INIT_TIMEOUT ladder runs.
    let agent_id = ctx.idgen.agent();
    ctx.sink
        .emit(FxEvent::AgentStatus {
            agent: agent_id.clone(),
            driver,
            status: AgentStatus::Starting,
        })
        .await?;

    // 3./4a./4b. Retry ladder + factory indirection live inside the acp module.
    match crate::driver::acp::spawn_agent_connection(
        &agent_id,
        &plan,
        ctx.events_tx.clone(),
        ctx.idgen.clone(),
        ctx.permreg_tx.clone(),
    )
    .await
    {
        Ok(conn) => {
            ctx.sink
                .emit(FxEvent::AgentStatus {
                    agent: agent_id.clone(),
                    driver,
                    status: AgentStatus::Ready,
                })
                .await?;
            ctx.conns.insert(agent_id.clone(), Arc::new(conn));
            Ok(Reply::Started { agent: agent_id })
        }
        Err(e) => {
            // The starting attempt IS on the record even though nothing else
            // ever was — replays show the failed spawn (audit > tidiness).
            // Crashed{exit_code: None} — site (a) above.
            ctx.sink
                .emit(FxEvent::AgentStatus {
                    agent: agent_id.clone(),
                    driver,
                    status: AgentStatus::Crashed { exit_code: None },
                })
                .await?;
            Ok(reply_err(FxErrorCode::AgentStartFailed, e.to_string()))
        }
    }
}

// NewSession → Reply::SessionCreated | Error(AgentNotFound or Internal)
pub async fn new_session(
    ctx: &mut Ctx<'_>,
    agent: AgentId,
    cwd: PathBuf,
    mcp_servers: Vec<McpServerSpec>,
) -> Result<Reply, crate::Error> {
    // 1. Known AND sessionable (Ready/Busy only)?
    let running = ctx.sink.with_projections(|p| p.agent_running(&agent)).await;
    if !running {
        return Ok(reply_err(
            FxErrorCode::AgentNotFound,
            "agent not running/not ready",
        ));
    }
    let Some(conn) = ctx.conns.get(&agent).cloned() else {
        return Ok(reply_err(FxErrorCode::AgentNotFound, "agent not connected"));
    };

    // 2. ACP session/new. NOTE vs command.rs pairing table (which lists only
    //    AgentNotFound): a LIVE agent failing session/new cannot be honestly
    //    reported as AgentNotFound; Internal carries the detail instead
    //    (flagged to fxproto owners in scaffold report).
    let acp_session = match conn.new_session(&cwd, &mcp_servers).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(reply_err(
                FxErrorCode::Internal,
                format!("ACP session/new failed: {e}"),
            ));
        }
    };

    // 3. Adopt VERBATIM + register in the connection registry (adoption
    //    happens exactly once, here).
    let session_id = SessionId::from_raw(acp_session);
    if let Err(e) = conn.register_session(session_id.clone(), session_id.as_str().to_owned()) {
        return Err(crate::Error::Acp(format!("session registry: {e}")));
    }

    // 4. THE durable record (one event one fact; cwd/mcp live ONLY here).
    ctx.sink
        .emit(FxEvent::SessionCreated {
            session: session_id.clone(),
            agent: agent.clone(),
            cwd,
            mcp_servers,
        })
        .await?;

    // Ordering note: sink.emit completed above ⇒ any client observing the Reply
    // observes >= its seq (pipeline G2) — replay can never miss a session a
    // successful reply promised.

    // 5./6.
    ctx.session_agent.insert(session_id.clone(), agent);
    Ok(Reply::SessionCreated {
        session: session_id,
    })
}

// Prompt → Reply::PromptAccepted | Error(SessionNotFound | TurnNotActive)
pub async fn prompt(
    ctx: &mut Ctx<'_>,
    s: SessionId,
    blocks: Vec<ContentBlock>,
) -> Result<Reply, crate::Error> {
    // 1./2. Existence then active-turn polarity ("same check doubles as the
    // fold's active_turn invariant guard; server side enforces HERE first").
    let (exists, active, owner) = ctx
        .sink
        .with_projections(|p| (p.session_exists(&s), p.turn_active(&s), p.session_owner(&s)))
        .await;
    if !exists {
        return Ok(reply_err(FxErrorCode::SessionNotFound, "unknown session"));
    }
    if active {
        return Ok(reply_err(
            FxErrorCode::TurnNotActive,
            "turn already running",
        ));
    }

    // 3. Owner exists by construction of step 1 (bug-guard otherwise).
    let Some(agent) = owner.or_else(|| ctx.session_agent.get(&s).cloned()) else {
        warn!(target: "session", session = %s, "no owner for existing session; internal state bug");
        return Ok(reply_err(
            FxErrorCode::Internal,
            "session has no owning agent",
        ));
    };
    let driver = ctx
        .sink
        .with_projections(|p| {
            p.agents
                .agents
                .get(&agent)
                .map(|a| a.driver)
                .unwrap_or(DriverId::ClaudeCode)
        })
        .await;
    let turn = ctx.idgen.turn();

    // 4./5./6. Started → user echo (silent skip when empty-after-flatten) → Busy.
    ctx.sink
        .emit(FxEvent::TurnStarted {
            session: s.clone(),
            turn: turn.clone(),
        })
        .await?;
    let echo = flatten_blocks(&blocks);
    if !echo.is_empty() {
        ctx.sink
            .emit(FxEvent::Chunk {
                session: s.clone(),
                turn: turn.clone(),
                role: FxRole::User,
                text: echo,
            })
            .await?;
    }
    ctx.sink
        .emit(FxEvent::AgentStatus {
            agent: agent.clone(),
            driver,
            status: AgentStatus::Busy,
        })
        .await?;

    // 7. Spawn the turn task capturing everything BY VALUE; it never touches
    //    actor maps again — completion reports through job_tx (step 8c).
    let claim = Arc::new(FinishClaim::default());
    let Some(conn) = ctx.conns.get(&agent).cloned() else {
        return Ok(reply_err(
            FxErrorCode::Internal,
            "connection vanished during prompt",
        ));
    };
    let task_ctx = TurnTaskCtx {
        sink: ctx.sink.clone(),
        conn,
        perms: Arc::clone(ctx.pending_perms),
        job_tx: ctx.job_tx.clone(),
        agent: agent.clone(),
        driver,
        session: s.clone(),
        turn: turn.clone(),
        // Raw acp string == our adopted string (verbatim adoption ⇒ equal).
        acp_session: s.as_str().to_owned(),
        claim: Arc::clone(&claim),
    };
    let handle = tokio::spawn(turn_task(task_ctx));

    ctx.turn_tasks.insert(
        s.clone(),
        TurnHandle {
            turn: turn.clone(),
            abort: handle.abort_handle(),
            claim,
        },
    );

    // 8. Reply immediately; results arrive as events.
    Ok(Reply::PromptAccepted { turn })
}

// Cancel → Reply::Cancelled | Error(SessionNotFound | TurnNotActive)
pub async fn cancel(ctx: &mut Ctx<'_>, s: SessionId) -> Result<Reply, crate::Error> {
    // 1.
    let exists = ctx.sink.with_projections(|p| p.session_exists(&s)).await;
    if !exists {
        return Ok(reply_err(FxErrorCode::SessionNotFound, "unknown session"));
    }
    // 2. Nothing to cancel is NOT an event-worthy fact either.
    let Some(slot) = ctx.turn_tasks.get(&s) else {
        return Ok(reply_err(FxErrorCode::TurnNotActive, "no running turn"));
    };
    let abort = slot.abort.clone();
    let claim = Arc::clone(&slot.claim);
    let turn = slot.turn.clone();
    let agent_for_watchdog = ctx.session_agent.get(&s).cloned();

    // 4. Sweep pendings BEFORE the cancel notification reaches the agent —
    //    fixed order makes tests deterministic; ACP REQUIRES answering pendings.
    perms::sweep_cancelled(ctx, &s).await;

    // 5. Fire-and-forget cancel notification to the OWNING connection.
    if let Some(agent) = agent_for_watchdog.clone()
        && let Some(conn) = ctx.conns.get(&agent)
    {
        conn.cancel(s.as_str()).await;
    }

    // 6. Watchdog force-finishes when the agent never acknowledges: whoever
    //    claims first emits the finish pair; aborted tasks simply emit nothing
    //    (next boot's fold absorbs the gap per threads.rs W7 semantics anyway).
    let wd_sink = ctx.sink.clone();
    let wd_job_tx = ctx.job_tx.clone();
    let wd_claim = Arc::clone(&claim);
    tokio::spawn(async move {
        tokio::time::sleep(watchdog_from_env()).await;
        if wd_claim.try_claim() {
            if let Err(err) = wd_sink
                .emit(FxEvent::TurnFinished {
                    session: s.clone(),
                    turn: turn.clone(),
                    stop_reason: StopReason::Cancelled,
                })
                .await
            {
                tracing::error!(target: "session", error=?err, "watchdog TurnFinished emit failed");
            }
            if let (Some(a), Some(d)) = (
                agent_for_watchdog.clone(),
                watchdog_driver(&wd_sink, &agent_for_watchdog).await,
            ) {
                let _ = wd_sink
                    .emit(FxEvent::AgentStatus {
                        agent: a,
                        driver: d,
                        status: AgentStatus::Ready,
                    })
                    .await;
            }
            abort.abort(); // kill the stuck prompt future
            let _ = wd_job_tx.send(InternalCmd::TurnDone { session: s });
        }
    });

    // 7. Ack NOW.
    Ok(Reply::Cancelled)
}

async fn watchdog_driver(sink: &EventSink, agent: &Option<AgentId>) -> Option<DriverId> {
    sink.with_projections(|p| {
        agent
            .as_ref()
            .and_then(|a| p.agents.agents.get(a))
            .map(|a| a.driver)
    })
    .await
}
#[allow(dead_code)]
fn _unused(_: Option<Option<DriverId>>) {}

// ── TURN TASK ────────────────────────────────────────────────────────────────

struct TurnTaskCtx {
    sink: EventSink,
    conn: Arc<crate::driver::acp::AcpConnection>,
    /// Shared perms registry — conn-death sweeps run WITHOUT Ctx.
    perms: PermShared,
    /// Backchannel into the actor loop for completion bookkeeping (8c).
    job_tx: tokio::sync::mpsc::UnboundedSender<InternalCmd>,
    agent: AgentId,
    driver: DriverId,
    session: SessionId,
    turn: TurnId,
    /// Raw acp string == our adopted string (verbatim adoption ⇒ equality).
    acp_session: String,
    claim: Arc<FinishClaim>,
}

async fn turn_task(t: TurnTaskCtx) {
    let TurnTaskCtx {
        sink,
        conn,
        perms,
        job_tx,
        agent,
        driver,
        session,
        turn,
        acp_session,
        claim,
    } = t;

    let _ = acp_session; // carried for future load-session resume support

    // Whole-turn RPC: streaming chunks arrived earlier via the global pump.
    let result = conn.prompt(&session, turn.clone(), Vec::new()).await;

    // Winner-takes-finish: normal completion, watchdog force-finish and any
    // late cancellation all converge here; double emission impossible.
    if !claim.try_claim() {
        // Watchdog claimed → it owns abort/cleanup notifications too.
        let _ = job_tx.send(InternalCmd::TurnDone {
            session: session.clone(),
        });
        return;
    }

    match result {
        // 8a. Prompt resolves ONLY at stop_reason (whole turn done).
        Ok(stop_reason) => {
            if let Err(err) = sink
                .emit(FxEvent::TurnFinished {
                    session: session.clone(),
                    turn: turn.clone(),
                    stop_reason,
                })
                .await
            {
                tracing::error!(target: "session", error=?err, "TurnFinished append failed");
            }
            let _ = sink
                .emit(FxEvent::AgentStatus {
                    agent: agent.clone(),
                    driver,
                    status: AgentStatus::Ready,
                })
                .await;
        }
        Err(err) => {
            // 8b. Conn died / protocol dead mid-turn. NO Crashed here — the
            // ACTOR owns Crashed exclusively (prevents duplicates when child
            // death caused this Err). Sweep parked permissions afterwards.
            debug!(target: "session", session = %session, error = %err,
                   "prompt failed; closing turn as cancelled");
            let _ = sink
                .emit(FxEvent::TurnFinished {
                    session: session.clone(),
                    turn: turn.clone(),
                    stop_reason: StopReason::Cancelled,
                })
                .await;
            let mut guard = perms.lock().await;
            perms::sweep_cancelled_for_conn_death(&mut guard, &sink, &session).await;
        }
    }

    // 8c. Completion pops the actor-owned entry.
    let _ = job_tx.send(InternalCmd::TurnDone { session });
}
