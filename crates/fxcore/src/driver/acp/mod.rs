//! The single v0 transport driver: one ACP connection per agent process,
//! many sessions per connection (mirrors ACP semantics).

pub mod normalize;

// TODO:
//
// /// How drivers hand events upward WITHOUT depending on cmd/orchestrator layers:
// /// a plain mpsc of raw FxEvents. The orchestrator owns a pump task that drains ALL
// /// connection channels and runs the one true pipeline: store.append(seq) →
// /// projections.apply → bus.send. Drivers never assign seq, never touch the store.
// pub type EventTx = tokio::sync::mpsc::Sender<FxEvent>;
//
// /// The parked half of an inbound ACP `session/request_permission`. The SDK's
// /// request carries a responder/completion the CLIENT side must eventually call
// /// (exact type = whatever agent-client-protocol exposes for replying to a
// /// server→client request — check its docs). We wrap it with our id so cmd/perms.rs
// /// can complete it later without knowing SDK internals.
// pub struct PendingAcpRequest {
//     pub our_id: RequestId,          // fxproto id we generated in normalize.rs
//     pub acp_session: String,
//     pub responder: acp::Responder,  // ← real name TBD from the crate; completes the JSON-RPC reply
// }
//
// /// Owns the child process + JSON-RPC plumbing via the official `agent-client-protocol`
// /// crate (client side). One actor task per AgentId.
// pub struct AcpConnection { /* child handle, outbound request tx, join handle */ }
//
// impl AcpConnection {
//     /// Spawn per SpawnPlan; perform ACP initialize handshake; negotiate capabilities.
//     /// On success: Ready. Wire protocol: ndjson over stdio (handled by the SDK crate).
//     pub async fn start(plan: &SpawnPlan, events: EventTx) -> Result<Self>;
//
//     /// ACP session/new → returns ACP sessionId; we map it to our SessionId upstream.
//     pub async fn new_session(&self, cwd: &Path, mcp: &[McpServerSpec]) -> Result<String>;
//
//     /// session/prompt. Resolves ONLY at stopReason (whole turn done), per ACP.
//     /// Streaming arrives via event_sink as normalized FxEvents.
//     pub async fn prompt(&self, acp_session: &str, blocks: Vec<ContentBlock>) -> Result<StopReason>;
//
//     /// session/cancel notification (fire-and-forget in ACP terms).
//     pub async fn cancel(&self, acp_session: &str);
//
//     /// Answer an inbound session/request_permission. The REQUEST side arrived as a
//     /// normalized PermissionRequested through event_sink; orchestrator parked a oneshot
//     /// under RequestId — this call supplies the outcome.
//     pub async fn respond_permission(&self, request: PendingAcpRequest, option_id: OptionId);
//
//     /// Kill the process tree (SIGTERM, grace, SIGKILL). Idempotent.
//     pub async fn shutdown(&self);
// }
//
// Restart/backoff policy: crash detection (wait on child exit) → AgentStatus::Crashed event
// → exponential backoff respawn attempts (max N, then give up). Sessions die with the
// process unless the agent advertises loadSession — resume is M2+ scope; log loudly.
//
// Inbound routing inside this actor:
//   - notifications → normalize::session_update() → event_sink
//   - server→client REQUESTS (request_permission) → emit PermissionRequested +
//     register PendingAcpRequest keyed by our RequestId so respond_permission can complete it.
//     Turn cancel MUST sweep unanswered requests with outcome "cancelled" (ACP requirement).
