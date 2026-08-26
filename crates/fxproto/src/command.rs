//! Commands — everything a client can ask the server to do.
//!
//! One enum, tagged serde (`#[serde(tag = "type", rename_all = "snake_case")]`).
//! Every command produces exactly one Reply (see reply.rs) — request/response pairing
//! is by JSON-RPC-style correlation id added at the envelope layer, not here.

// Imports to restore as you define the types:
// use std::path::PathBuf;
//
// use crate::content::{ContentBlock, McpServerSpec};
// use crate::driver::DriverId;
// use crate::ids::{AgentId, OptionId, RequestId, SessionId};

// TODO: define:
//
// #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// pub enum Command {
//     /// Ask which agents are installed/detectable on the server machine.
//     DetectAgents,
//
//     /// Spawn an agent process. Reply carries the AgentId. No cwd here: the process
//     /// inherits the server's working directory; filesystem scope is anchored per
//     /// session by NewSession.cwd (that is what ACP session/new takes). NOTE
//     /// docs/architecture.md sketches `StartAgent { driver, cwd }` — stale; this file
//     /// and fxcore cmd/session.rs both carry cwd on NewSession only.
//     StartAgent { driver: DriverId },
//
//     /// Open a session (ACP session/new) on a running agent. cwd anchors fs scope.
//     NewSession { agent: AgentId, cwd: PathBuf, mcp_servers: Vec<McpServerSpec> },
//
//     /// Send user content to a session; starts a turn.
//     Prompt { session: SessionId, blocks: Vec<ContentBlock> },
//
//     /// Cancel the running turn (ACP session/cancel). Also sweeps pending permissions.
//     Cancel { session: SessionId },
//
//     /// Answer a PermissionRequested. Server completes the parked ACP request.
//     PermissionResponse { request_id: RequestId, option_id: OptionId },
// }
//
// NO Subscribe variant — deliberately. Subscribing is a transport/handshake concern:
// it happens once per connection BEFORE any command (envelope::Message::Subscribe,
// handled by fxserver net/handshake.rs replay-then-live attach), and its resync twin
// SnapshotRequired only makes sense at that layer too. A Command::Subscribe would be a
// second way to do the same thing; fxserver net/client.rs already rejects any such
// frame post-handshake.
//
// EXACT command → reply pairing (one Reply each; errors are Reply::Error(FxError)):
//   DetectAgents        → DetectedAgents  (never errors: not-found drivers are
//                         DetectedDriver { found: false } rows — data, not failures)
//   StartAgent          → Started         | Error(AgentStartFailed)
//   NewSession          → SessionCreated  | Error(AgentNotFound)
//   Prompt              → PromptAccepted  | Error(SessionNotFound | TurnNotActive)
//   Cancel              → Cancelled       | Error(SessionNotFound | TurnNotActive)
//   PermissionResponse  → PermissionRecorded | Error(PermissionNotFound)
