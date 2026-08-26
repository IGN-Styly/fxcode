//! The single v0 transport driver: one ACP connection per agent process,
//! many sessions per connection (mirrors ACP semantics).
//!
//! Naming convention used throughout: `acp::X` = types from the pinned
//! `agent-client-protocol` crate (workspace dep, currently 1.3.x / schema
//! 1.4.x); bare type names = ours (FxEvent etc.). Verified upstream names as
//! of this scaffold: `Stdio`, `Responder<T>`, `SessionNotification`,
//! `SessionUpdate`, `ContentChunk`, `ToolCall`, `ToolCallUpdate`,
//! `ToolCallUpdateFields`, `ToolKind`, `ToolCallStatus`, `Plan`, `PlanEntry`,
//! `RequestPermissionRequest/Response`, `RequestPermissionOutcome`,
//! `SelectedPermissionOutcome`, `InitializeRequest/Response`,
//! `ProtocolVersion::{V1, LATEST}`, `NewSessionRequest/Response`,
//! `PromptRequest/PromptResponse`, `StopReason`. NOTE those enums are
//! `#[non_exhaustive]` upstream — see normalize.rs for how this file deals
//! with that.

pub mod normalize;

// Imports to restore as you implement:
// use std::collections::BTreeMap;
// use std::sync::Arc;
//
// use agent_client_protocol as acp;
// use fxproto::content::{ContentBlock, McpServerSpec, StopReason};
// use fxproto::driver::DriverSpec;
// use fxproto::event::FxEvent;
// use fxproto::ids::{AgentId, OptionId, RequestId, SessionId};
// use tokio::process::Command;
// use tokio::sync::{mpsc, watch};
//
// use crate::driver::SpawnPlan;

// TODO:
//
// /// How drivers hand events upward WITHOUT depending on cmd/orchestrator layers:
// /// a plain mpsc of raw FxEvents into ONE global unbounded channel owned by the
// /// Orchestrator (pump-task ownership DECIDED in cmd/mod.rs — one pump drains
// /// everything). Contract for emitters (this module):
// ///   - NEVER assign seq (store does), never touch projections/bus/store;
// ///   - events arrive here already-normalized (normalize.rs output);
// ///   - send() is effectively infallible-and-blocking-free (unbounded); a send
// ///     error means the Orchestrator dropped the receiver (shutting down) —
// ///     treat as "stop producing", log once, exit the actor quietly.
// pub type EventTx = tokio::sync::mpsc::UnboundedSender<FxEvent>;
//
// /// Tunables (single source of truth — tests import these):
// pub const INIT_TIMEOUT: Duration = Duration::from_secs(5);
// /// Start-phase retry ladder (pre-Ready crashes ONLY): attempt delays
// /// [500ms, 2000ms] before giving up => max START_ATTEMPTS=3 tries total,
// /// worst-case ~2.5s of sleeping + 3 x INIT_TIMEOUT.
// pub const START_ATTEMPTS: u32 = 3;
// pub const START_BACKOFF_MS: &[u64] = &[500, 2000];
// /// Grace given to the child after SIGTERM during shutdown(); then SIGKILL.
// pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
//
// /// The parked half of an inbound ACP `session/request_permission`.
// ///
// /// `responder` is the REAL SDK type (`acp::jsonrpc::Responder<T>` re-exported
// /// as `acp::Responder<T>`): consume-once — call `.respond(RequestPermissionResponse)`
// /// / `.respond_with_result(...)` exactly once; dropping it without responding
// /// surfaces as a JSON-RPC error frame to the agent. That is why every removal
// /// path below MUST go through one of the two explicit completions.
// pub struct PendingAcpRequest {
//     pub our_id: RequestId,          // minted in normalize.rs via IdGen.request()
//     /// RAW ACP session id (SDK `acp::SessionId` stringified) — NOT our
//     /// fxproto SessionId. Translation to our side happens where this value was
//     /// created (cmd layer knows the mapping; see PermsEntry in cmd/perms.rs).
//     pub acp_session: String,
//     pub responder: acp::Responder<acp::RequestPermissionResponse>,
// }
//
// impl PendingAcpRequest {
//     /// User picked an option: respond Selected { option_id }. Consumes self.
//     pub fn respond_selected(self, option_id: OptionId);
//     /// Cancel/watchdog/sweep/crash: respond Outcome::Cancelled. Consumes self.
//     /// ACP REQUIRES clients answer pending permission requests this way
//     /// (docs/research/acp.md Permissions model).
//     pub fn respond_cancelled(self);
// }
//
// /// Owns the child process + JSON-RPC plumbing via the official
// /// `agent-client-protocol` crate (client side). One actor task per AgentId.
// /// Many SessionsId mappings live INSIDE one actor (ACP allows that).
// pub struct AcpConnection {
//     // child handle (for shutdown), command_tx (outbound requests below),
//     // join handle, watch::Sender<ConnState>
// }
//
// /// What the actor publishes about itself (drives AgentStatus emissions):
// pub enum ConnState { Initializing, Ready, Crashed { exit_code: Option<i32> }, Stopped }
//
// /// Commands INTO the actor (mpsc inside AcpConnection). Every variant maps to
// /// exactly one public method below — methods serialize through this channel,
// /// which is what makes `&self` receiver signatures sound.
// enum ConnCmd {
//     NewSession { our_session: SessionId, cwd: PathBuf, mcp: Vec<McpServerSpec>,
//                  reply: oneshot::Sender<Result<String /*acp session*/, String>> },
//     Prompt    { our_session: SessionId, blocks: Vec<ContentBlock>,
//                  reply: oneshot::Sender<Result<StopReason, String>> },
//     Cancel    { our_session: SessionId },
//     RespondPermission { request: PendingAcpRequest, option_id: OptionId },
//     Shutdown,
// }
//
// impl AcpConnection {
//     /// Spawn + connect + initialize, with bounded retries. Steps IN ORDER:
//     ///   1. tokio Command(plan.resolved_program or spec.program), args+env from
//     ///      spec; stdin/stdout piped, stderr PIPED AND FORWARDED to tracing
//     ///      (agents log there; stdout is ACP-only traffic); kill_on_drop(true).
//     ///   2. Build the SDK client-side connection over `acp::Stdio`, registering
//     ///      handlers BEFORE any traffic flies: inbound `SessionNotification`
//     ///      handler + server->client request handler (routes
//     ///      `RequestPermissionRequest` only; fs/terminal requests are answered
//     ///      protocol-error since v0 advertises neither capability). Both handlers
//     ///      forward onto an internal mpsc consumed by THIS actor's select loop.
//     ///   3. Send `acp::InitializeRequest { protocol_version: V1,
//     ///      client_capabilities: <none: no fs.read/writeTextFile, no terminal> }`
//     ///      under INIT_TIMEOUT. Mismatch: negotiated < V1 => Err(AgentStart).
//     ///      Empty `auth_methods` expected; NON-empty => Err whose message lists
//     ///      each AuthMethod id (v0 has no auth UX — surface it honestly instead
//     ///      of half-authenticating).
//     ///   4. Publish Ready (see publish_lifecycle below). On ANY failure of steps
//     ///      2–3 while attempts remain: kill child, sleep START_BACKOFF_MS[i],
//     ///      retry from step 1. Attempts exhausted => Crashed + Err(AgentStart).
//     /// NOTE `events` is the GLOBAL pump sender (cmd/mod.rs). On success the
//     /// caller (cmd/session.rs start_agent) inserts Arc<Self> into Ctx.conns.
//     pub async fn start(agent: &AgentId, plan: &SpawnPlan, events: EventTx)
//         -> Result<Self, crate::Error>;
//
//     /// Register the acp<->our session mapping (called by cmd/session.rs right
//     /// after new_session succeeds). Inbound traffic for UNKNOWN acp session ids
//     /// => warn! + drop (we cannot fabricate an fxproto SessionId; ids.rs says
//     /// adoption happens exactly once, here).
//     pub async fn register_session(&self, our: SessionId, acp_id: String) -> Result<(), String>;
//     /// ACP session/new; returns the AGENT's sessionId string. Caller adopts it.
//     /// Failure to find `our` registered-but-unconfirmed slots is impossible by
//     /// construction (registration happens after adoption); unknown our-session
//     /// => Err("not registered").
//     pub async fn new_session(&self, cwd: &Path, mcp: &[McpServerSpec]) -> Result<String>;
//
//     /// session/prompt. Resolves ONLY at PromptResponse.stop_reason (whole turn
//     /// done), per ACP. Streaming arrives via events as normalized FxEvents, so
//     /// the caller MUST treat Ok(_) as "turn finished", not "turn started".
//     /// Correlation: the SDK assigns JSON-RPC ids internally; our TurnId <-> this
//     /// future binding lives in cmd/session.rs (Ctx.turn_tasks), NOT here.
//     pub async fn prompt(&self, acp_session: &str, blocks: Vec<ContentBlock>)
//         -> Result<StopReason>;
//
//     /// session/cancel notification (fire-and-forget in ACP terms).
//     pub async fn cancel(&self, acp_session: &str);
//
//     /// Answer an inbound session/request_permission. The REQUEST side arrived as
//     /// a normalized PermissionRequested through events; orchestrator parked the
//     /// PendingAcpRequest under its RequestId — this call supplies the chosen
//     /// option (respond_selected). Completion failure (conn already dead) =>
//     /// debug!-logged; ordering authority remains cmd/perms.rs's map removal.
//     pub async fn respond_permission(&self, request: PendingAcpRequest, option_id: OptionId);
//
//     /// Kill the process tree. Ordering (exact):
//     ///   1. drop the SDK connection => child stdin closes (agent sees EOF),
//     ///   2. SIGTERM the child,
//     ///   3. await exit <= SHUTDOWN_GRACE; still alive => SIGKILL,
//     ///   4. reap via wait(), set ConnState::Stopped, publish AgentStatus::Stopped.
//     /// Idempotent (state machine guards double-kill); safe after Crashed too.
//     /// Does NOT answer parked permissions — that stays cmd/perms.rs's job.
//     pub async fn shutdown(&self);
// }
//
// /// Lifecycle publication rule (ONE writer: this actor):
// ///   Initializing => emitted by cmd/session.rs as AgentStatus::Starting BEFORE
// ///                   start() is called (so clients see something immediately).
// ///   Ready        => on step 3 success: AgentStatus::Ready.
// ///   post-Ready child exit => AgentStatus::Crashed { exit_code }, EXACTLY ONCE,
// ///                   emitted HERE (never by turn tasks — prevents duplicates).
// ///                   NO AUTO-RESPAWN AFTER READY: sessions die with the process
// ///                   (unless loadSession, M2+ scope), and fxproto model/agents.rs
// ///                   pins recovery-by-fresh-AgentId ("nothing leaves Crashed
// ///                   except via a NEW AgentId"). Recovery = user runs StartAgent
// ///                   again. The START-phase ladder above is exempt because no
// ///                   Ready was ever published — state stayed Starting throughout.
// ///   Stopped      => only via shutdown().
//
// /// Actor select-loop (runs until Shutdown or child exit):
// ///   tokio::select! over:
// ///     - internal mpsc of SDK-callback messages:
// ///         * SessionNotification(note) =>
// ///             route by our-registry[acp sid]; unknown => warn+drop;
// ///             else normalize::session_update(&note, tool_call_state_entry)
// ///             -> Vec<FxEvent>, drain-send all onto events.
// ///             `tool_call_state_entry`: per-(acp sid, ToolCallId) composed view
// ///             OWNED BY THE ACTOR (BTreeMap member) — see normalize.rs rows for
// ///             tool_call/tool_call_update merge rules; actor passes &mut entry.
// ///         * ServerRequest(RequestPermissionRequest(req, responder)) =>
// ///             normalize::request_permission(req, gen, responder.into()) — mints
// ///             RequestId, emits PermissionRequested onto events, and the
// ///             PendingAcpRequest rides out to cmd layer via a dedicated
// ///             oneshot (park_tx) — NOT via events (FxEvent payloads must stay
// ///             serde-clean; Responder is neither Clone nor serializable).
// ///         * other server->client requests => respond protocol-error (cap off).
// ///     - child.wait() completion => Crashed publication (+ if any prompt futures
// ///       are outstanding they error out promptly; their handling lives in the
// ///       turn task, cmd/session.rs).
// ///     - watch/broadcast of ConnCmds (see enum above).
// ///
// /// Cleanup-of-parked-request obligations (who answers them, audit trail):
// ///   - normal answer            => cmd/perms.rs respond()
// ///   - user cancel / watchdog   => cmd/perms.rs sweep_cancelled()
// ///   - process DEATH mid-perm   => turn task notices prompt() Err and invokes
// ///                                 cmd/perms.rs::sweep_cancelled_for_conn_death()
// ///   all three respond Outcome::Cancelled / emit PermissionResolved{chosen:None};
// ///   the RESPONDER is answered BEFORE the FxEvent is persisted (why: recorded
// ///   audit rows should reflect reality already-told-to-agent; if persist fails
// ///   afterwards the stale pending row is cleaned by the next sweep — logged).
