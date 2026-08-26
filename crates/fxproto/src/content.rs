//! Normalized content shapes — what a message is made of after translation from ACP.
//!
//! Rule (from architecture.md): payloads never embed raw ACP JSON. Vendor extras ride
//! opaquely in `_meta: Option<serde_json::Value>` fields where needed.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
//
// use serde::{Deserialize, Serialize};

// TODO: define:
//
// /// One block of user/agent content. Mirrors ACP content blocks, minus the parts
// /// we normalize away. Internally tagged so JSON reads {"type": "text", "text": ...}.
// #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(tag = "type", rename_all = "snake_case")]
// pub enum ContentBlock {
//     Text { text: String },
//     Image { media_type: String, data: String /* base64 */ },
//     // EmbeddedResource later (ACP supports it) — add when first needed.
// }
//     media_type stays a plain String (mime strings are open-ended; an enum would churn
//     against agent vendors). Eq/PartialEq: golden tests assert structural equality.
//
// /// Speaker of a Chunk. Thought-chunks are deliberately NOT a Role yet: their shape
// /// is an open decision parked in fxcore normalize.rs ("own variant vs Chunk w/ role?
// /// lean: defer") and in event.rs's trailing TODO. Keep Role two-variant until real
// /// ACP traffic settles it; revisit at M1.
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]        // "user" / "agent"
// pub enum Role { User, Agent }
//
// /// Why a turn stopped — mirrors ACP v1 stopReason 1:1 so nothing is lost in
// /// translation. normalize.rs owns the exhaustive acp::StopReason → StopReason match.
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum StopReason { EndTurn, MaxTokens, MaxTurnRequests, Refusal, Cancelled }
//
// /// An MCP server the CLIENT wants attached to a session. Sent in Command::NewSession;
// /// echoed verbatim by FxEvent::SessionCreated so replays rebuild what was attached.
// #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// pub struct McpServerSpec {
//     pub name: String,
//     pub command: String,
//     pub args: Vec<String>,
//     pub env: BTreeMap<String, String>,   // BTreeMap: byte-stable wire ordering for goldens
// }
//     NOTE canonical name: McpServerSpec. docs/architecture.md sketches `McpServer` —
//     stale shorthand; crates.md + this file + command.rs all say McpServerSpec.
//
// /// One row of an agent's plan. priority mirrors ACP's plan-entry priority enum;
// /// Option because agents may omit it.
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum PlanPriority { High, Medium, Low }
//
// #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// pub struct PlanEntry { pub content: String, pub status: PlanEntryStatus, pub priority: Option<PlanPriority> }
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum PlanEntryStatus { Pending, InProgress, Completed }
//
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum ToolCallKind { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, Other }
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum ToolCallStatus { Pending, InProgress, Completed, Failed }
//     → these mirror ACP kinds/statuses 1:1; keep variants aligned with ACP v1 schema
//       (normalize.rs's exhaustive matches break compile if ACP grows a variant we lack).
//
// Serde summary for goldens: ContentBlock = internally tagged "type"; every other enum =
// externally tagged unit variants rendered as bare snake_case strings ("end_turn",
// "in_progress", ...). Structs = plain field maps.
//
// TODO: unit-test that serde representations match the shapes sketched in docs/architecture.md.
