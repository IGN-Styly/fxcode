//! Canonical events — the only way state changes anywhere in the system.
//!
//! These are NORMALIZED: fxcore's driver layer translates raw ACP `session/update`
//! notifications into these shapes (fxcore/src/driver/acp/normalize.rs). The client
//! never sees vendor quirks. Persisted to SQLite and broadcast to clients with a Seq.

// Imports to restore as you define the types:
// use crate::content::{
//     ContentBlock, McpServerSpec, PlanEntry, Role, StopReason, ToolCallKind, ToolCallStatus,
// };
// use crate::driver::DriverId;
// use crate::ids::{AgentId, OptionId, RequestId, SessionId, ToolCallId, TurnId};

// TODO: define Sequenced<T> first:
//
// /// Event + its global order stamp. The store assigns Seq at append time; clients
// /// track the max they've seen as their resume cursor.
// #[derive(Serialize, Deserialize)]
// pub struct Sequenced<T> { pub seq: u64, pub inner: T }
//
// TODO: define the event enum (tagged: #[serde(tag = "type", rename_all = "snake_case")]):
//
// pub enum FxEvent {
//     /// Agent process lifecycle.
//     AgentStatus { agent: AgentId, driver: DriverId, status: AgentStatus },
//
//     TurnStarted  { session: SessionId, turn: TurnId },
//     /// Streaming text for the transcript. role=User chunks echo what was sent.
//     Chunk        { session: SessionId, turn: TurnId, role: Role, text: String },
//     /// Upsert keyed by tool_call — UI replaces in place, never appends duplicates.
//     ToolCallUpsert {
//         session: SessionId, tool_call: ToolCallId,
//         title: String, kind: ToolCallKind, status: ToolCallStatus,
//         output: Option<String>,
//         _meta: Option<serde_json::Value>,   // vendor extras preserved opaquely
//     },
//     PlanUpdated  { session: SessionId, entries: Vec<PlanEntry> },
//
//     /// Agent asked permission. Server parks the ACP request under `request_id`.
//     PermissionRequested {
//         request_id: RequestId, session: SessionId,
//         tool_call: ToolCallSummary, options: Vec<PermissionOption>,
//     },
//     /// Recorded so late-joining clients see the resolution too.
//     PermissionResolved { request_id: RequestId, chosen: Option<OptionId> }, // None = cancelled
//
//     TurnFinished { session: SessionId, turn: TurnId, stop_reason: StopReason },
//     /// Emitted when NewSession command succeeds. THE record that a session exists —
//     /// without it, replays can't rebuild the agent→sessions list. Carries everything
//     /// session/new established (replaces an earlier "McpAttached" idea — one event,
//     /// one fact).
//     SessionCreated {
//         session: SessionId, agent: AgentId, cwd: PathBuf, mcp_servers: Vec<McpServerSpec>,
//     },
// }
//
// pub enum AgentStatus { Starting, Ready, Busy, Crashed { exit_code: Option<i32> }, Stopped }
//
// pub struct ToolCallSummary { tool_call: ToolCallId, title: String }   // enough to render a prompt
// pub struct PermissionOption { option_id: OptionId, name: String, kind: PermissionOptionKind }
// pub enum PermissionOptionKind { AllowOnce, AllowAlways, RejectOnce, RejectAlways }
//     → mirrors ACP option kinds; keep aligned.
//
// TODO: content blocks vs Chunk.text: ACP delivers message chunks as ContentBlock arrays.
// Decision to encode here: Chunk flattens text-only chunks; non-text blocks become their own
// variant or ride _meta. Revisit at M1 when real traffic shows what agents actually send.
