//! Commands — everything a client can ask the server to do.
//!
//! One enum, tagged serde (`#[serde(tag = "type", rename_all = "snake_case")]`).
//! Every command produces exactly one Reply (see reply.rs) — request/response pairing
//! is by JSON-RPC-style correlation id added at the envelope layer, not here.

// Imports to restore as you define the types:
// use crate::content::ContentBlock;
// use crate::driver::DriverId;
// use crate::ids::{AgentId, OptionId, RequestId, Seq, SessionId};

// TODO: define:
//
// pub enum Command {
//     /// Ask which agents are installed/detectable on the server machine.
//     DetectAgents,
//
//     /// Spawn an agent process. Reply carries the AgentId.
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
//
//     /// Replay events after `last_seq`, then attach live. See envelope.rs Subscribe flow.
//     Subscribe { last_seq: Seq },
// }
