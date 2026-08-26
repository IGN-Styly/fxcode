//! Orchestrator integration tests against FakeAgent — the M1/M2 acceptance suite.

// COMMON HARNESS (shared preamble; every scenario uses it verbatim):
//
//   async fn fresh_orchestrator(tag: &str) -> (Orchestrator, TempDir) {
//     1. tempdir store dir ("fxcore-test-{tag}-{uuid}");
//     2. std::env::set_var("FX_CANCEL_WATCHDOG_MS", "200")  // shorten watchdog
//        (env is process-global: tests in this file MUST run with
//         `--test-threads=1`, asserted via a static OnceGuard that panics
//         otherwise — documented here so the runner never "fixes" it away);
//     3. Config { data_dir: tmp.path(), ..default }, Orchestrator::new(cfg);
//     4. test seam injection (FLAGGED — needs one #[cfg(test)]-visible hook in
//        src/orchestrator.rs, e.g.
//            pub fn inject_connection_factory_for_tests(
//                f: Arc<dyn Fn(&SpawnPlan) -> BoxFuture<'static, Result<AcpConnection>>>,
//            )
//        ): the factory ignores the SpawnPlan and returns an AcpConnection bound
//        to start_harness()'s duplex client half; store it under a reserved
//        DriverId slot via registry.set_plan_for_tests.
//   }
//   helpers: subscribe_orch(o), collect_until(rx, pred, timeout 5s),
//            fold_all(events) -> ThreadsState (fxproto::model::apply_thread).

// TODO: scenarios (each = spawn Orchestrator w/ tempdir store + FakeAgent):
//
// happy_turn:
//   SETUP  orchestrator + FakeAgent(Script[Chunk(Agent,"Hel"), Chunk(Agent,"lo"),
//          Chunk(User,"echo-back"), Stop(EndTurn)]).
//   ACT    StartAgent{ClaudeCode} -> NewSession{agent,"/tmp/w",[]} ->
//          Prompt{"hi"}.await PromptAccepted{turn}.
//   ASSERT sequence in subscription:
//     seq strict-increasing AND gapless from head+1;
//     order == [AgentStatus(Starting..Ready), SessionCreated, TurnStarted,
//               Chunk(User,"hi") /*the echo*/, Chunk(Agent,"Hello"),
//               Chunk(User,"echo-back"), TurnFinished{EndTurn}];
//     fold_all().threads[s].messages merged per W2 => exactly TWO messages:
//       (User "hi"), (Agent "Hello"+" echo...wait role flip") =>
//       precisely: messages = [(User,"hi"), (Agent,"Hello"),
//                              (User,"echo-back")] — merging stops at role flips;
//     active_turn cleared to None after TurnFinished;
//     turn ids minted by deterministic IdGen appear as "t-000001".
//
// tool_call_lifecycle:
//   SETUP  Script[ToolCall{id:"call_1",title:"Read x"}, ToolCallUpdate{
//          id:"call_1", status:InProgress, output:None}, ToolCallUpdate{
//          id:"call_1", status:Completed, output:Some("done")}, Stop(EndTurn)].
//   ACT    full prompt flow.
//   ASSERT after ALL events: tool_calls.len()==1 (keyed upsert, no dupes);
//     final entry = {kind Other, status Completed, output Some("done")};
//     flow has exactly ONE FlowItem::Tool positioned at FIRST appearance index;
//     mid-stream checkpoint (collect until first Completed upsert arrives)
//     proves intermediate InProgress was delivered before final overwrite.
//
// permission_roundtrip:
//   SETUP  options fixture [("opt-allow","Allow",allow_once),
//                           ("opt-deny","Deny",reject_once)];
//          Script[AskPermission(opts), Chunk(Agent,"granted"), Stop(EndTurn)].
//   ACT    prompt; wait PermissionRequested event (assert options list matches +
//          request_id well-formed); send PermissionResponse{request_id,opt-allow}.
//   ASSERT PermissionRecorded reply; THEN Chunk/Stop arrive (turn blocked until
//     answer — ordering pinned by collect-until with 5s cap); agent side sees
//     ObservedRequest::Outcome{selected opt-allow}; PermissionResolved{Some} in
//     stream AFTER PermissionRequested; threads card badge stamped Chosen (W6);
//     perms.recent has exactly one ResolvedPermission{chosen:Some}.
//
// cancel_sweeps_pending_permissions:
//   SETUP  same options fixture; Script[AskPermission(opts), Stall].
//   ACT    prompt; await PermissionRequested; Cancel{session}.
//   ASSERT Cancelled ack promptly (<1s); EVERY pending permission emitted ONE
//     PermissionResolved{chosen:None}; agent received Outcome{cancelled} for
//     EACH BEFORE any further step ran; TurnFinished{Cancelled} arrives within
//     FX_CANCEL_WATCHDOG_MS slack (watchdog path since Stall never answers);
//     following Prompt succeeds (TurnNotActive guard released).
//
// watchdog_force_finishes_without_agent_ack:
//   SETUP  Script[Stall] ONLY (no cancel acknowledgement at all: fake agent's
//          cancel handler suppressed via InitBehavior flag).
//   ACT    prompt then cancel.
//   ASSERT TurnFinished{Cancelled} published at ~200ms (env override), NOT
//     waiting on SDK timeout; no Crashed emitted (agent alive — matrix in
//     cmd/session.rs); session reusable after.
//
// crash_and_replay:
//   SETUP  Script[Chunk(Agent,"partial"), Crash]; pre-crash orchestrator A.
//   ACT    full flow until AgentStatus{Crashed} observed; clone-fold all three
//          states from A's subscription log.
//   ACT'   DROP A entirely; REOPEN same tempdir dir via Orchestrator::new.
//   ASSERT reopened.head_seq == pre-crash max seq (no loss/dup on disk);
//     rebuilt AgentsState/ThreadsState/PermsState EQUAL the cloned pre-crash
//     folds (projections rebuild determinism, golden compare via PartialEq);
//     restarted StartAgent mints a FRESH agent id (agents.rs resurrection rule);
//     model forbids nothing else being reused.
//
// cursor_replay:
//   SETUP  store with >=6 events from two mini-turns (Chunk/Stop twice).
//   ACT    open SqliteStore directly on same dir for replay(k), k in {0,3,N};
//          in parallel Orchestrator::subscribe() attached BEFORE appending the
//          FINAL live turn.
//   ASSERT replay(k).len() == N-k exactly, ascending seqs, first > k;
//     live subscriber receives the final turn AFTER every replayed suffix item
//     when stitching replay ++ live (handshake contract simulation);
//     union of (replay(0) ++ live-collected-minus-replayed-overlap) covers every
//     persisted seq EXACTLY once.
//
// ordering_guarantee:
//   SETUP  FOUR independent sessions across ONE FakeAgent (or two agents to
//          also exercise registry fan-out).
//   ACT    join_all of 4x10 prompts concurrently across distinct sessions
//          (same-session concurrency is ILLEGAL by design and covered by the
//           TurnNotActive error test below).
//   ASSERT single global subscriber: every observed seq strictly increasing
//     overall; every seq unique across BOTH subscription and direct
//     EventStore::replay(0) cross-check; per session, TurnStarted_n always
//     preceded by TurnFinished_{n-1} (handler serialization held even under
//     concurrent dispatch of DIFFERENT sessions).
//
// detect_agents_and_turn_not_active_guards:
//   SETUP  default harness, NO scripted traffic.
//   ACT    DetectAgents command; Prompt on unknown session; second Prompt
//          racing an in-flight turn (fake script Stall); Cancel on idle session.
//   ASSERT DetectedAgents rows len==3 in DriverId declaration order, injected
//     driver found:true version Some; all guard cases return exactly their
//     Reply::Error code (SessionNotFound / TurnNotActive) with ZERO events
//     appended (head_seq unchanged — validation must never dirty the log).
