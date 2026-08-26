# Crate Blueprints

Companion to [architecture.md](architecture.md) — the internal shape of each crate: file tree,
key types, and the rules that keep dependencies flowing one way. Constraint update: **Linux +
macOS only**, no Windows support by design (simplifies process/spawn/PTY code throughout).

Dependency graph (arrows point at what may be imported):

```
        fxproto  ◀──────────────────────────┐
           ▲                                │
         fxcore ◀──── fxserver              │
                            ▲               │
                          fxapp ────────────┘   (fxapp imports ONLY fxproto)
```

## Workspace migration (M0 step zero)

Root `Cargo.toml` becomes a virtual manifest; today's `src/main.rs` moves into `crates/fxapp`:

```toml
[workspace]
resolver = "2"
members = ["crates/fxproto", "crates/fxcore", "crates/fxserver", "crates/fxapp"]

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
# …shared versions pinned once here; crates reference via workspace = true
```

---

## crates/fxproto — the contract

Pure types + the canonical state model. **No async runtime, no I/O, no GPUI.** Both binaries
import it; because the event-fold lives here, server validation and client UI can never drift
apart. Serde round-trip stability is this crate's public API.

```
src/
  lib.rs        re-exports; docs pointing at architecture.md
  ids.rs        AgentId, SessionId, TurnId, ToolCallId, RequestId, Seq(u64), DriverId
                — Copy newtypes, #[serde(transparent)], Display impls
  content.rs    normalized shapes: ContentBlock, McpServerSpec, PlanEntry,
                Role, StopReason, ToolCallStatus/Kind
  command.rs    Command enum (client → server intent)
  reply.rs      Reply enum + FxError { code, message } — every command gets exactly one
  event.rs      FxEvent enum (normalized; see architecture.md) + Sequenced<T> { seq, inner }
  envelope.rs   Message enum — everything that crosses the wire:
                  Hello { proto_version, token } / Welcome { server_version, head_seq }
                  Command(..) / Reply(..) / Event(Sequenced<FxEvent>)
                  Subscribe { last_seq } / SnapshotRequired { baseline, snapshot }
  driver.rs     DriverId enum { ClaudeCode, GeminiCli, CodexCli }, DriverSpec { program, args, env }
  model/        canonical projections + folds (the shared brain)
    mod.rs      pub use
    agents.rs   AgentState { id, driver, status, sessions }  + apply_agent(&mut, &FxEvent)
    threads.rs  ThreadState { messages, tool_calls: BTreeMap<ToolCallId, _>, plan } 
                + apply_thread(&mut, &FxEvent)     ← tool_call_id keyed upserts live here
    perms.rs    PendingPermission { request_id, options, tool_call } + registry fold
```

Rules:

- `FxEvent` payloads never embed raw ACP JSON; vendor extras ride in `_meta: Option<JsonValue>`.
- Fold functions are total: applying any event to any state is defined (unknown session ⇒
  create-or-ignore, logged via `tracing`, never panic).
- Golden serde tests: fixtures captured from real agents (claude/gemini/codex transcripts) must
  round-trip byte-stably; failures block release.

Deps: `serde`, `serde_json`, `thiserror`, `tracing`. Nothing else.

---

## crates/fxcore — the server brain

Owns processes, sessions, the log. Knows nothing about sockets or UI.

```
src/
  lib.rs            pub use Orchestrator, Config, EventStore; module docs
  config.rs         Config { data_dir, bind_override, drivers: HashMap<DriverId, DriverSpec> }
                    load() merges defaults + ~/.fxcode/config.toml
  orchestrator.rs   THE entrypoint (see below)
  bus.rs            thin wrapper over tokio broadcast; lag policy: drop + flag, never block
  proj.rs           boot-time projection rebuild: EventStore::replay(0) → fold into model states
  store/
    mod.rs          trait EventStore { append, replay, head_seq }
    sqlite.rs       rusqlite (WAL): CREATE TABLE IF NOT EXISTS events(
                      seq INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER, kind TEXT, json TEXT)
                    single writer task; appends funnel through mpsc to keep ordering trivial
  driver/
    mod.rs          DriverRegistry: resolve(DriverId) -> SpawnPlan { spec, detected_version }
    detect.rs       PATH scan + known install locations (linux/mac):
                      ClaudeCode → claude-code-acp / npx @agentclientprotocol/claude-agent-acp
                      GeminiCli  → gemini --acp        CodexCli → codex-acp
                    config overrides win over autodetect
    acp/
      mod.rs        AcpConnection actor: owns child process (tokio Command, piped stdio),
                    runs the official agent-client-protocol crate client-side;
                    one connection : many ACP sessions; restart/backoff policy
      normalize.rs  pure fn: ACP session/update notification → Vec<FxEvent>
                    (+ request_permission → PermissionRequested + parked reply slot)
  cmd/
    mod.rs          dispatch(Command) → handler; the ONLY mutator of orchestrator state
    session.rs      StartAgent/NewSession/Prompt/Cancel flows incl. turn lifecycle bookkeeping
    perms.rs        pending-permission map: RequestId → oneshot sender; cancel-turn sweeps
                    unanswered requests with outcome "cancelled" (ACP requires it)

tests/
  fake_agent.rs     in-process ACP *agent* built on the same official SDK's server side —
                    scripted responses/chunks/tool-calls/permission prompts
  orchestrator.rs   full flows against FakeAgent: prompt→chunks→stop, permission round-trip,
                    kill -9 mid-turn → restart → replay integrity
```

Orchestrator shape:

```rust
pub struct Orchestrator { /* registry, store, connections, projections, bus */ }

impl Orchestrator {
    pub async fn new(cfg: Config) -> Result<Self>;          // opens store, rebuilds projections
    pub async fn execute(&self, cmd: Command) -> Result<Reply>;
    pub fn subscribe(&self) -> BroadcastReceiver;            // post-persist fanout
}

// Concurrency model: execute() pushes onto ONE mpsc consumed by a single actor task
// → totally ordered command handling like T3, no lock choreography. Long-running turns
// run as spawned tasks that emit events through the same persist→broadcast path.
```

Deps: `tokio`, `agent-client-protocol`, `rusqlite` (bundled), `futures`, `tracing`,
`toml`, `fxproto`.

---

## crates/fxserver — the daemon

A thin shell around fxcore. Target: ~500 lines. If it grows past that, logic belongs in fxcore.

```
src/
  main.rs        init tracing → Config::load → Orchestrator::new → print pairing token on
                 first boot → serve. Graceful shutdown on SIGTERM/Ctrl-C (kills child agents).
  pair.rs        token lifecycle: generate (rand 32B, hex), chmod 600 file at
                 ~/.fxcode/token, print to stderr once; rotate subcommand
  ifaddr.rs      pick listen addr: cfg.bind_override > `tailscale ip -4` / tailscaled
                 LocalAPI socket > scan interfaces for 100.64.0.0/10 > 127.0.0.1.
                 NOTE: fxserver never joins the tailnet itself — the host's tailscaled
                 owns membership; we only bind to the interface it provides.
  net/
    mod.rs       axum app: single WS route /ws, health GET /healthz (no auth)
    handshake.rs first frame must be Hello: check proto_version, constant-time token compare;
                 then expect Subscribe → ReplayFrom(store, cursor) → attach bus live stream.
                 Gap too large (> N events) ⇒ SnapshotRequired instead.
    client.rs    per-conn task pair: read loop (frames → orchestrator.execute → Reply back),
                 write loop (replay buffer then broadcast, bounded chan; on lag disconnect
                 client with Resubscribe notice — cursor makes this cheap)
```

Deps: `axum` (ws feature), `tokio`, `rand`, `subtle` (ct compare), `tracing`,
`fxcore`, `fxproto`.

Auth stance: Tailscale = transport identity; token = seatbelt vs other tailnet devices. No TLS,
no accounts, no sessions — deliberately.

---

## crates/fxapp — the GPUI client

Thin by construction: views render projections; the only mutation path is
`event → fxproto::model fold → cx.notify()`.

```
src/
  main.rs        bootstrap: gpui_platform::application, gpui_component::init, open window
  theme.rs       theme selection + tokens (gpui-component ThemeRegistry)
  conn/
    mod.rs       ConnectionManager (Entity): status (Connecting/Ready/Lost), send_command()
                 with correlation, exposes event subscription to stores. Reconnect loop w/
                 exponential backoff; on Ready re-subscribes from stored last_seq.
    ws.rs        THE ONLY FILE THAT KNOWS TOKIO EXISTS: owns a small embedded tokio Runtime,
                 runs async-tungstenite there, bridges frames in/out via channels compatible
                 with GPUI's executor. Contained blast radius if swapped later.
    cursor.rs    last_seq persistence (~/.fxcode/client-state.json) + resubscribe/snapshot
                 handling (SnapshotRequired → clear local projections, refold from snapshot)
  store/
    mod.rs       AppState (GPUI Global): holds Entity<Agents>, Entity<Threads>,
                 Entity<Perms>; ConnectionManager events dispatch into these via
                 fxproto::model folds, then notify() observers
  views/
    mod.rs       WorkspaceView: Dock layout — sidebar | thread | status bar
    sidebar.rs   gpui-component Sidebar: agent list w/ status dots, session list, "New session"
    thread.rs    thread view: VirtualList of messages + tool-call cards (keyed by ToolCallId);
                 composer Input at bottom → Prompt command; stop button → Cancel
    message.rs   role-styled bubbles; agent text via TextView (markdown)
    tool_call.rs card per ToolCallUpsert: title, kind icon, Spinner→Badge status transition
    perms.rs     permission modal (window.open_dialog): options rendered from
                 PendingPermission; click → PermissionResponse; auto-dismiss on turn cancel
    connect.rs   connect screen: server addr + pairing token entry (stored after first OK)
    setup.rs     (M3) DetectAgents results; guided enable/disable per driver
```

Patterns carried over from React (per research notes):

- `Entity<T>` ≈ Zustand store; views subscribe via `cx.observe`; **always `cx.notify()`** after
  fold mutations.
- ElementIds derive from domain ids (`SessionId`, `ToolCallId`) — never list indexes.
- Overlays go through `WindowExt` (`open_dialog`, `push_notification`), not element trees.

Deps: `gpui`, `gpui_platform`, `gpui-component`, `async-tungstenite`+`tokio` (inside conn/ws.rs
only), `serde_json` (cursor file), `fxproto`. **Never `fxcore`** — the client cannot spawn
agents even by accident.

---

## Test strategy summary

| Crate | Approach |
|---|---|
| fxproto | serde goldens from captured agent traffic; fold property tests (apply any event order ⇒ valid state) |
| fxcore | normalize.rs unit tests; SQLite tempdir tests; FakeAgent integration drives real flows incl. crash/replay |
| fxserver | handshake/auth unit tests; spin daemon + ws client end-to-end in CI |
| fxapp | logic kept in stores/folds (already covered upstream); manual smoke checklist per milestone |
