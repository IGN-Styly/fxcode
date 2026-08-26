# Implementation Order

Step-by-step build order. Each step lists the files (blueprints in [crates.md](crates.md))
and its **exit check** — don't move on until it passes. The rule throughout:

> `cargo check --workspace` stays green. Every step ends runnable or testable.

Roughly maps to milestones M0–M5 in [architecture.md](architecture.md); steps within a
milestone are dependency-ordered so nothing is stubbed twice.

---

## Phase 0 — Types (fxproto)

*Everything else consumes these; they're pure and fully unit-testable.*

- [x] **0.1** `fxproto/src/ids.rs` — id newtypes + Seq. *Check: compiles; Display impls.*
- [x] **0.2** `fxproto/src/content.rs` — ContentBlock, Role, StopReason, Plan*, ToolCall*, McpServerSpec.
- [x] **0.3** `fxproto/src/driver.rs` — DriverId (+labels) and DriverSpec (+defaults).
- [x] **0.4** `fxproto/src/event.rs` — Sequenced<T>, FxEvent (incl. SessionCreated), AgentStatus, permission types.
- [x] **0.5** `fxproto/src/command.rs` + `reply.rs` — Command/Reply/FxError.
- [x] **0.6** `fxproto/src/envelope.rs` — Message enum, PROTO_VERSION, Snapshot (concrete shape).
- [x] **0.7** Serde golden tests: round-trip fixtures of every variant. *Check: `cargo test -p fxproto`.*

## Phase 1 — Shared brain (fxproto::model)

- [x] **1.1** `model/agents.rs`, `threads.rs`, `perms.rs` — states + folds per their trigger maps.
  Derive Serialize/Deserialize (Snapshot needs them).
- [x] **1.2** Fold property tests: any event into fresh state ⇒ valid state. Delivery is
  exactly-once (see model/mod.rs contract), so idempotence claims are scoped to
  keyed/upsert events (ToolCallUpsert, PermissionResolved); append-shaped events (Chunk,
  SessionCreated) must NOT be re-applied. Chunk merging collapses consecutive same-role
  text and breaks at TurnStarted. *Check: `cargo test -p fxproto`.*

## Phase 2 — Persistence (fxcore/store, fxcore/bus)

- [ ] **2.1** `store/mod.rs` trait finalized, `store/sqlite.rs` impl (WAL, append/replay/head_seq).
- [ ] **2.2** SQLite tests (tempdir): ordering, suffix replay, reopen-persistence. *Check: green.*
- [ ] **2.3** `bus.rs` EventBus + lag policy. Test: subscriber sees strictly increasing seq.

## Phase 3 — Server core wiring (fxcore)

- [x] **3.1** `config.rs` — Config::load + defaults + data_dir creation.
- [x] **3.2** `cmd/mod.rs` EventSink (append→project→broadcast pump) + dispatch skeleton;
  `proj.rs` Projections::rebuild from empty/small logs.
- [x] **3.3** `orchestrator.rs` — new(), execute() via single mpsc actor, subscribe(),
  shutdown(). *Check: orchestrator boots on tempdir store; unknown command errors cleanly.*
- [x] **3.4** `driver/detect.rs` + `driver/mod.rs` registry — DetectAgents works against your
  real machine (whatever agents you have installed).

## Phase 4 — ACP (fxcore/driver/acp) ← riskiest, start early

- [x] **4.1** Read the `agent-client-protocol` crate docs/source; note real type names for
  PendingAcpRequest.responder etc. directly into `acp/mod.rs` comments.
- [x] **4.2** `acp/mod.rs` AcpConnection: spawn + initialize handshake only.
  *Check: integration test starts FakeAgent-less real binary? No — use FakeAgent.*
- [x] **4.3** `tests/fake_agent.rs` — Script engine + duplex harness; initialize handshake passes.
- [x] **4.4** `acp/normalize.rs` — session_update → Vec<FxEvent>, request_permission split,
  stop_reason mapping. Unit tests per mapping row.
- [x] **4.5** Full prompt flow through orchestrator vs FakeAgent (`tests/orchestrator.rs`
  happy_turn). **= M1 server-side exit.**

## Phase 5 — Daemon (fxserver)

- [x] **5.1** `pair.rs` token lifecycle + `ifaddr.rs` addr picking (+ classify() unit test).
- [x] **5.2** `main.rs` boot chain + graceful shutdown (children killed).
- [x] **5.3** `net/handshake.rs` (version, constant-time token, replay/snapshot branch) +
  `net/client.rs` task pair.
- [x] **5.4** End-to-end: script a ws client against running fxserver — auth fail, auth ok,
  Subscribe replay. *Check: manual curl/websocat or a small rust test.*

## Phase 6 — Client spine (fxapp)

- [ ] **6.1** `conn/ws.rs` — tokio runtime quarantine, connect/pump loops. *Check: echo test
  against fxserver healthz/ws.*
- [ ] **6.2** `conn/cursor.rs` + `conn/mod.rs` ConnectionManager: handshake, correlation,
  reconnect/backoff, event dispatch into AppState.
- [ ] **6.3** `store/mod.rs` AppState global applying folds + notify.
- [ ] **6.4** `main.rs` boot order + `views/connect.rs` screen. *Exit: window shows
  connection status + latency badge (replaces HelloWorld).*

## Phase 7 — One agent end-to-end (**M1 exit**)

- [ ] **7.1** `views/thread.rs` minimal: flat message list from ThreadState, composer → Prompt.
- [ ] **7.2** Drive claude-code-acp (or gemini --acp) from the UI: prompt → streamed chunks →
  stopReason. Kill/restart fxapp mid-turn → reconnect replays transcript.
  *This is the first demoable moment — commit a tag.*

## Phase 8 — Durability + permissions (**M2 exit**)

- [x] **8.1** `cmd/perms.rs` respond + sweep_cancelled; watchdog timeout in cancel flow.
- [x] **8.2** `views/perms.rs` modal wired to PermsState.
- [x] **8.3** SnapshotRequired path: force it (tiny N in dev builds), verify refold.
- [x] **8.4** Multiple concurrent sessions across ≥2 agent types simultaneously.
- [x] **8.5** Crash tests green in `tests/orchestrator.rs` (crash_and_replay, cursor_replay,
  ordering_guarantee).

## Phase 9 — Product surface (**M3 exit**)

- [ ] **9.1** `views/sidebar.rs`, `message.rs` (TextView markdown — measure streaming perf),
  `tool_call.rs` cards keyed by ToolCallId upserts.
- [ ] **9.2** `theme.rs`; Dock layout persistence; status bar.
- [ ] **9.3** `views/setup.rs` DetectAgents UX + pairing UX polish (rotate-token subcommand).

## Phase 10 — Checkpoints (**M4**) & Hardening (**M5**)

- [ ] **10.1** Design pass first (new doc): hidden git refs per turn, diff/revert commands —
      protocol additions go through fxproto goldens like everything else.
- [ ] **10.2** Slow-client disconnect/resubscribe soak test; SIGTERM under load.
- [ ] **10.3** Second client proof: read-only CLI or web client speaking raw fxproto —
      validates the seam without touching fxcore/fxapp.

---

## Standing rules

- New protocol field? fxproto first, goldens second, everything else follows.
- Any logic tempting you inside `fxserver` or view files goes down a layer instead.
- Windows code paths: never. POSIX assumptions are load-bearing.
- If a step's exit check needs a real agent binary, gate it behind a `--ignored` test so CI
  stays hermetic (FakeAgent covers CI; real agents cover local smoke).
