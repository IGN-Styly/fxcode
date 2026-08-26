//! Canonical events — the only way state changes anywhere in the system.
//!
//! These are NORMALIZED: fxcore's driver layer translates raw ACP `session/update`
//! notifications into these shapes (fxcore/src/driver/acp/normalize.rs). The client
//! never sees vendor quirks. Persisted to SQLite and broadcast to clients with a Seq.

// Imports to restore as you define the types:
// use std::path::PathBuf;
//
// use serde::{Deserialize, Serialize};
// use serde_json::Value;
//
// use crate::content::{
//     ContentBlock, McpServerSpec, PlanEntry, Role, StopReason, ToolCallKind, ToolCallStatus,
// };
// use crate::driver::DriverId;
// use crate::ids::{AgentId, OptionId, RequestId, SessionId, Seq, ToolCallId, TurnId};

// TODO: define Sequenced<T> first:
//
// /// Event + its global order stamp. Semantics pinned here (store + bus rely on them):
// /// - `seq` is assigned by EventStore::append at persist time (fxcore/src/store), never
// ///   by drivers or clients. First event gets 1; strictly increasing, no gaps, no reuse.
// /// - 0 is reserved as "nothing yet": head_seq of an empty log and a fresh client cursor.
// /// - replay(after) returns events strictly AFTER `after`, ascending; bus fanout happens
// ///   post-persist, so every subscriber sees strictly increasing seq per subscription
// ///   (fxcore/src/bus.rs). Clients track the max seq seen as their resume cursor
// ///   (~/.fxcode/client-state.json) and send it back via Subscribe { last_seq }.
// #[derive(Serialize, Deserialize, Clone, Debug)]
// pub struct Sequenced<T> { pub seq: Seq, pub inner: T }
//
// TODO: define the event enum (tagged: #[serde(tag = "type", rename_all = "snake_case")]):
//
// #[derive(Serialize, Deserialize, Clone, Debug)]
// pub enum FxEvent {
//     /// Agent process lifecycle. Carries `driver` so the agents fold can construct a
//     /// brand-new AgentState entry (first sight of this agent) without guessing.
//     AgentStatus { agent: AgentId, driver: DriverId, status: AgentStatus },
//
//     TurnStarted  { session: SessionId, turn: TurnId },
//     /// Streaming text for the transcript. role=User chunks echo what was sent.
//     /// See "chunk vs blocks" decision at the bottom of this file for how ContentBlocks
//     /// flatten into `text`.
//     Chunk        { session: SessionId, turn: TurnId, role: Role, text: String },
//     /// Upsert keyed by tool_call — UI replaces in place, never appends duplicates.
//     ToolCallUpsert {
//         session: SessionId, tool_call: ToolCallId,
//         title: String, kind: ToolCallKind, status: ToolCallStatus,
//         output: Option<String>,
//         _meta: Option<Value>,   // vendor extras preserved opaquely
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
//     /// one fact; do NOT reintroduce a separate MCP event).
//     SessionCreated {
//         session: SessionId, agent: AgentId, cwd: PathBuf, mcp_servers: Vec<McpServerSpec>,
//     },
// }
//
// /// Process status of one agent. Reused VERBATIM by model::agents::AgentState (status
// /// field) and rendered directly by the sidebar status dots — no duplicate view type.
// /// Transition rules (asserted in fold tests, see model/agents.rs):
// ///   Starting → Ready | Crashed ; Ready ⇄ Busy ; any → Stopped (server shutdown).
// #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
// pub enum AgentStatus { Starting, Ready, Busy, Crashed { exit_code: Option<i32> }, Stopped }
//
// /// Minimal tool identity attached to a permission ask — enough for the modal header,
// /// not the whole card. The full record lives in the thread's tool_calls map.
// #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
// pub struct ToolCallSummary { pub tool_call: ToolCallId, pub title: String }
//
// #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
// pub struct PermissionOption { pub option_id: OptionId, pub name: String, pub kind: PermissionOptionKind }
// #[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
// pub enum PermissionOptionKind { AllowOnce, AllowAlways, RejectOnce, RejectAlways }
//     → mirrors ACP option kinds; keep aligned.
//
// TODO: chunk-vs-blocks decision (settled — do not reopen without M1 traffic evidence):
// ACP delivers message chunks as ContentBlock arrays; Command::Prompt accepts arbitrary
// blocks. Encoding rule: consecutive Text blocks flatten into Chunk.text (joined as the
// agent delivered them). Image/non-text blocks ARE forwarded to the agent in Prompt, but
// v0 does NOT echo them into the transcript — normalize drops them from the echo with a
// tracing::debug! line (the composer only sends [Text] today, so nothing real is lost).
// If M1 traffic proves otherwise, extend Chunk with `_meta` — never add a Role variant
// (thought-chunks are likewise deferred entirely: logged, not modeled).
