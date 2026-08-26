# fxcode Architecture

Plan for the fxcode agent manager: a headless **server** that owns coding agents, and a thin
GPUI **client** that projects their state. Decisions locked in 2026-08-26:

- **Two binaries from day 1** — remote access via Tailscale is a first-class goal, so the
  client/server seam is real, not aspirational.
- **Event log from day 1** — every state change is a persisted, sequenced event; UI state is a
  fold over the log (T3-style decider/projector, minus the ceremony).
- **ACP-only drivers in v0**, using the official Rust SDK — no hand-written JSON-RPC.

## Big picture

```
┌───────────────┐  commands (WS)   ┌───────────────────────────────────────┐
│    fxapp      │ ────────────────▶│               fxserver                │
│  (GPUI GUI)   │                  │                                       │
│               │◀──────────────── │  orchestrator ──▶ driver registry     │
│  projections  │  events (WS)     │       │                 │             │
│  (in-memory)  │                  │  event store      ACP connections     │
└───────────────┘                  │   (SQLite)           │ stdio         │
       ▲                           └───────────────────────┼───────────────┘
       │ tailnet                                           ▼
  future web/mobile                        claude-code-acp · gemini --acp ·
  (same protocol)                          codex-acp … (subprocesses)
```

The server owns everything that has side effects: agent subprocesses, sessions, the event log.
Clients are stateless except for a resume cursor (`last_seq`) and their projections. Kill the
client, kill the network, restart the machine — reconnect, replay, continue.

## Workspace layout

```
crates/
  fxproto/    wire types: Command, FxEvent, envelopes, cursor. serde only — no logic.
  fxcore/     server brain: orchestrator, driver registry, ACP adapter, event store,
              projections. No networking, no UI. Testable headless.
  fxserver/   thin binary: config, token auth, WebSocket listener, wires fxcore to the wire.
  fxapp/      thin binary: GPUI UI — connection manager, stores, views.
```

Dependency rule: `fxproto ← fxcore ← fxserver`; `fxproto ← fxapp`. `fxcore` never imports
network or GPUI crates; `fxapp` never spawns processes. This keeps both sides swappable
(e.g., an eventual web client reuses only `fxproto`; `fxcore` gets fuzzed/integration-tested
without sockets).

## Protocol (fxproto)

One WebSocket endpoint. Two logical streams multiplexed by message type:

```rust
// client → server
enum Command {
    DetectAgents,
    StartAgent { driver: DriverId },      // cwd lives on NewSession (anchors ACP session scope)
    NewSession { agent: AgentId, cwd: PathBuf, mcp_servers: Vec<McpServerSpec> },
    Prompt { session: SessionId, blocks: Vec<ContentBlock> },
    Cancel { session: SessionId },
    PermissionResponse { request_id: RequestId, option_id: OptionId },
}

// server → client
enum Reply { /* per-command acks/errors */ }

#[serde(tag = "type")]
enum FxEvent {
    AgentStatus   { agent: AgentId, status: AgentStatus },
    SessionCreated{ session: SessionId, agent: AgentId, cwd: PathBuf, mcp_servers }, // durable session record
    TurnStarted   { session: SessionId, turn: TurnId },
    Chunk         { session: SessionId, turn: TurnId, role, text },       // normalized message chunk
    ToolCallUpsert{ session: SessionId, tool_call: ToolCallId, title, kind, status, output },
    PlanUpdated   { session: SessionId, entries },
    PermissionRequested { request_id, session, tool_call, options },
    PermissionResolved  { request_id, chosen: Option<OptionId> },          // None = cancelled
    TurnFinished  { session: SessionId, turn: TurnId, stop_reason },
}
```

Rules:

- Subscription is **envelope-level, not a command**: after the `Hello`/`Welcome` handshake the
  client sends one envelope `Message::Subscribe { last_seq }`; the server replays everything
  after the cursor and attaches live. Too far behind (> N events) ⇒ `SnapshotRequired`
  carrying a full projection `Snapshot { baseline_seq, agents, threads, perms }`.
- Every `FxEvent` gets a **global monotonic `seq`** (`Seq(u64)` newtype) stamped by the event
  store at append time.
- Normalized events, not ACP pass-through: the driver layer translates `session/update`
  notifications into the shapes above so the client never learns vendor quirks. ACP `_meta`
  extensions get preserved opaquely for drivers that need them. Non-text content blocks ride
  `_meta`; only text is flattened into `Chunk`.
- Version field in the handshake; mismatch = explicit close (`"protocol_version"`), not silent
  drift. Close reasons are a fixed vocabulary: `"auth_failed"`, `"protocol_version"`,
  `"resubscribe"`.

## fxcore (the deep module)

Public surface stays tiny; guts are submodules:

```rust
pub struct Orchestrator { /* … */ }
impl Orchestrator {
    pub async fn new(cfg: Config) -> Result<Self>;
    pub async fn execute(&self, cmd: Command) -> Result<Reply>;
    pub fn subscribe(&self) -> broadcast-style receiver;          // post-persist fanout
    pub async fn replay_from(&self, cursor: Seq);                 // handshake replay leg
    pub fn projection_snapshot(&self) -> Snapshot;                // snapshot leg
    pub async fn shutdown(&self);
}

pub trait EventStore: Send + Sync {
    pub async fn append(&self, ev: FxEvent) -> Result<Sequenced<FxEvent>>;
    pub async fn replay_batch(&self, after: Seq, limit: usize)
        -> Result<Vec<Sequenced<FxEvent>>>;                       // paginated: bounded memory
}
```

Ids: minted ONLY by fxcore's `IdGen` (uuid v7; typed ctors for agent/turn/request).
`SessionId` and `ToolCallId` are adopted verbatim from the agent — never generated.

Internals:

- **Driver registry.** `DriverId → DriverSpec { binary, args, env }`. All v0 drivers speak ACP,
  so one `AcpDriver` covers claude (via `@agentclientprotocol/claude-agent-acp`),
  gemini (`--acp`), codex (`codex-acp`). Detection scans PATH + known locations. Non-ACP agents
  later become new drivers implementing the same normalization contract — orchestrator code
  doesn't change.
- **ACP connections.** Official `agent-client-protocol` Rust crate (1.0, schema-derived).
  One connection per agent process; many sessions per connection, mirroring ACP semantics.
- **Permission pump.** `session/request_permission` arrives as a server→client *request* over
  ACP; orchestrator turns it into `PermissionRequested`, parks the pending reply keyed by
  `request_id`, completes it when `PermissionResponse` lands (or auto-cancels on turn cancel —
  ACP requires clients answer pending requests with `"cancelled"`).
- **Event store.** SQLite (rusqlite, WAL mode): one `events(seq INTEGER PRIMARY KEY, json TEXT,
  ts)` table. Append-only; projections are folds. No ORM, no migrations framework yet.
- **In-memory projections for command validation** (e.g., reject `Prompt` for unknown session),
  rebuilt by folding the log at startup. These are the same projections the client builds —
  same fold functions live in `fxproto`/shared module where practical.

## fxserver

- Loads config (~/.fxcode/config.toml), opens the store, boots the orchestrator.
- Binds WebSocket on the **Tailscale interface only** (or loopback when no tailnet). The host's
  `tailscaled` owns tailnet membership — fxcode just binds to the interface it provides, so
  Tailscale provides transport encryption + device identity while we add one application gate:
  a **pairing token** generated on first boot, printed to stderr, stored in
  ~/.fxcode/token. Client presents it once during handshake; server keeps an allowlist.
  Cheap insurance against other devices on the tailnet. (Fallback alternative: run the listener
  on loopback only and expose via `tailscale serve` — adds a CLI dependency, so not default.)
- Backpressure: bounded broadcast channel; slow clients get disconnected and told to resubscribe
  from their cursor rather than blocking the orchestrator.

## fxapp (GPUI client)

- **ConnectionManager** (entity): owns the WS connection, handshake, cursor tracking,
  reconnect-with-replay loop. Emits connection-status events.
- **Stores** (GPUI entities / globals): `SessionsStore`, `ThreadsStore`, `PermissionsStore` —
  each applies `FxEvent`s via the shared fold functions. Views never touch raw events.
- **Views** (gpui-component): sidebar (agents/sessions), thread view (streaming markdown,
  tool-call cards keyed by `tool_call_id` upserts), permission dialog modal, agent setup screen.
- Async note: fxcore/tokio lives server-side only. The client uses GPUI's own executor +
  an async WebSocket client; no tokio dependency needed in `fxapp`.

## Technology choices

| Concern   | Choice                                    | Rationale                                        |
|-----------|-------------------------------------------|--------------------------------------------------|
| ACP       | `agent-client-protocol` Rust crate (v1)   | Schema-derived, maintained upstream; pin ACP v1  |
| Async     | tokio (fxcore/fxserver only)              | Process mgmt + SDK ecosystem                     |
| Transport | WebSocket, JSON frames                    | Same mental model as ACP; debuggable; browser-ready later |
| Storage   | SQLite, WAL                               | Single file, cursor-friendly, zero ops           |
| Auth      | Tailnet + static pairing token            | Identity delegated to Tailscale; token = seatbelt|
| UI        | gpui-component                            | Already researched; matches GPUI idioms          |

## Milestones (tracer bullets, each ends runnable)

- **M0 — Skeleton pulse.** Workspace + 4 crates; `fxserver` answers `Ping` over WS;
  `fxapp` window shows connection status + round-trip latency.
- **M1 — One agent end-to-end.** Spawn claude-code-acp (or gemini `--acp`);
  initialize → session/new → prompt → chunks stream into a dumb scrolling view. Events in memory.
- **M2 — Durability + permissions.** SQLite log with `seq` cursors; reconnect/replay works;
  permission round-trip; multiple concurrent sessions across ≥2 agents.
- **M3 — Product surface.** Markdown rendering, tool-call cards, plan display, session/thread
  management, agent detection + setup screen, pairing UX.
- **M4 — Checkpoints.** Per-turn checkpoints via hidden git refs; diff/revert between turns
  (stolen from T3 Code).
- **M5 — Hardening + second client.** Crash/recovery tests, slow-client handling, and a
  read-only mobile/web client speaking the same protocol to prove the seam.

## Open questions / risks

- GPUI client async: if GPUI's executor fights the WS library, fall back to spawning a tokio
  runtime inside `fxapp` behind ConnectionManager — contained blast radius.
- ACP v2 draft is breaking; stay pinned to v1 until clients we care about move.
- Terminal/PTY support (ACP `terminal` capability) is unscoped; likely M6+, needs its own design.
- Platform scope: Linux + macOS only, by design. No Windows support planned; agent-spawn and
  PTY code can assume POSIX.

## References

- docs/impl.md — implementation order (phase-by-phase with exit checks)
- docs/crates.md — per-crate blueprints (file trees, key types, rules)
- docs/research/acp.md — protocol notes (v1)
- docs/research/acp-in-t3code.md — T3 Code internals; source of the event-sourcing and
  thin-client patterns adapted here
