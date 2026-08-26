//! In-process fake ACP agent for integration tests — NO real CLIs in CI.
//!
//! Built on the SAME official `agent-client-protocol` crate, implementing the
//! AGENT side, run over in-memory duplex streams instead of stdio. This
//! exercises our real client code end-to-end while staying hermetic and
//! scriptable.

// Imports to restore as you implement:
// use std::sync::Arc;
//
// use agent_client_protocol as acp;
// use tokio::io::DuplexStream;
// use tokio::sync::{mpsc, oneshot};
//
// use fxproto::content::Role;

// TRANSPORT (DECIDED): `tokio::io::duplex(64 * 1024)` — one 64 KiB pair. The
// CLIENT half is handed to our AcpConnection's SDK builder in place of a child
// process' stdio (start() gains a test-only variant or SpawnPlan transport
// injection; see flagged note in tests/orchestrator.rs). The AGENT half drives
// the SDK agent-side connection so wire framing, JSON-RPC ids and ndjson line
// discipline are REAL — only the OS pipe is fake.
//
// BRIDGING FACTS (verified against SDK v1.3.0 source; see also acp/mod.rs SDK
// INTEGRATION FACTS): transports are `acp::ByteStreams<OB, IB>` with FUTURES-IO
// bounds, so each tokio DuplexStream half bridges via tokio_util::compat:
//   client side: ByteStreams::new(client_io.compat_write(), agent-half…) — no.
//   Concretely, for pair (a, b):
//     CLIENT builder transport = ByteStreams::new(a.compat_write(), b.compat())
//       outgoing=a (client writes), incoming=b (client reads)
//     AGENT  builder transport = ByteStreams::new(b.compat_write(), a.compat())
//   (.compat() adapts the tokio AsyncRead into futures::AsyncRead via
//   FuturesAsyncReadCompatExt; .compat_write() the reverse. Import from
//   tokio_util::compat::{TokioAsyncReadCompatExt, FuturesAsyncReadCompatExt}.)
//
// AGENT-SIDE MECHANICS: same Builder lifecycle as the client — register one
// `.on_receive_request::<Req,_>(async |req, responder, cx| …)` handler PER
// inbound method we must serve (InitializeRequest, NewSessionRequest,
// PromptRequest) + CancelNotification as a notification handler
// (`.on_receive_notification::<CancelNotification,_>`), then run
// `connect_with(transport, main_fn)` where main_fn parks until the duplex
// closes / Crash fires. Outbound from inside main_fn on the provided
// ConnectionTo<Agent>-counterpart: notifications via
// `cx.send_notification(SessionNotification { session_id, update, .. })`
// (`send_request_to`/`send_notification_to` variants target peers explicitly);
// session/request_permission is a normal request from agent→client whose
// REQUEST callback hands us its Responder — answer it exactly once with
// RequestPermissionResponse { outcome } when Step::AskPermission observes the
// client's choice (or with Outcome::Cancelled on timeout/crash).

// TODO:
//
// /// Deterministic behavior for one fake agent instance. Steps fire IN ORDER per
// /// session/prompt received (each prompt restarts the script unless `repeat`;
// /// see FakeAgent::prompt_script).
// pub struct Script(pub Vec<Step>);
//
// /// EXACT variants (no catch-alls: extend deliberately when a test needs more).
// pub enum Step {
//     /// One streamed text chunk notification. Role selects WHICH chunk kind the
//     /// agent emits: User => user_message_chunk (echo abuse), Agent =>
//     /// agent_message_chunk. Longer multi-chunk flows = several variants.
//     Chunk(Role, String),
//     /// tool_call(pending by default; kind/status overridable).
//     ToolCall { id: String, title: String },
//     /// tool_call_update with same-id upsert semantics; Some(output) rides
//     /// raw_output so normalize row O is exercised.
//     ToolCallUpdate { id: String, status: acp::ToolCallStatus,
//                      output: Option<String> },
//     /// Full plan snapshot replace.
//     Plan(Vec<acp::PlanEntry>),
//     /// Agent sends session/request_permission and STOPS the script until it
//     /// observes the client's outcome (timeout 30s => script fails the test via
//     /// panic in agent task). Options are verbatim PermissionOptions.
//     AskPermission(Vec<acp::PermissionOption>),
//     /// Drop the duplex mid-turn (EOF to client) WITHOUT answering anything
//     /// pending — exercises crash detection + turn task 8b path.
//     Crash,
//     /// Never respond to this prompt; used ONLY against CANCEL_WATCHDOG env
//     /// override in orchestrator tests (otherwise hangs 10s).
//     Stall,
//     /// End the turn with this stop reason (the ONLY terminal step).
//     Stop(acp::StopReason),
// }
//
// pub struct FakeAgent {
//     script: Script,
//     /// Fixed capabilities/auth surface for initialize:
//     /// default: agent_capabilities = { load_session: false }, auth_methods = []
//     /// (empty => AcpConnection::start proceeds; non-empty fixture variant lets
//     /// tests assert the auth-refusal error path).
//     init: InitBehavior,
// }
// struct InitBehavior { protocol_version: acp::ProtocolVersion, auth_methods: Vec<...> }
//
// impl FakeAgent {
//     pub fn new(script: Script) -> Arc<Self>;
//     /// Bind onto ONE half of the duplex and serve until the half closes or
//     /// Crash fires. Returns a join handle; tests should await it for panics.
//     pub fn serve(self: Arc<Self>, io: DuplexStream) -> tokio::task::JoinHandle<()>;
// }
//
// /// What tests get to OBSERVE the client through:
// #[derive(Debug)]
// pub enum ObservedRequest {
//     NewSession { cwd: std::path::PathBuf,
//                  mcp_servers: Vec<acp::McpServer> /* sdk shape */ },
//     Prompt    { session_id: String, blocks: Vec<acp::ContentBlock> },
//     Cancelled { session_id: String },                       // session/cancel
//     Outcome   { session_id: String, outcome: acp::RequestPermissionOutcome },
// }
//
// /// Handle bundle returned by TestHarness::start (see below).
// pub struct Harness {
//     pub observed_rx: mpsc::Receiver<ObservedRequest>,
//     /// Mirrors sessions the agent minted: "sess-000001", "sess-000002", ...
//     /// deterministic counter — assertions can pin exact ids.
//     pub sessions_tx: mpsc::Receiver<String>,
// }
//
// /// One-call wiring (every orchestrator scenario starts like this):
// ///   1. let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
// ///   2. let h = FakeAgent::new(script).serve(agent_io);       // spawn + stash txs
// ///   3. return Harness { observed_rx, sessions_tx } + client_io for injection;
// ///   observe helpers: `next_observed(timeout)` with tokio::time::timeout(5s)
// ///   so hangs FAIL tests with the pending request name in the panic.
// pub async fn start_harness(script: Script) -> (Harness, DuplexStream);
//
// Behavioral contract pinned (asserted by the harness's own sanity test):
// - initialize                => responds V1, chosen caps as configured.
// - session/new               => mints "sess-{n:06}" sequentially, records it.
// - session/prompt            => runs Script steps in order; AskPermission
//                                BLOCKS steps until outcome arrives; Stop ends.
// - session/cancel            => aborts current step loop and replies stopReason
//                                cancelled EVEN MID-Stall (ACP requires agents
//                                answer cancel promptly with that stopReason).
