//! Normalized content shapes — what a message is made of after translation from ACP.
//!
//! Rule (from architecture.md): payloads never embed raw ACP JSON. Vendor extras ride
//! opaquely in `_meta: Option<serde_json::Value>` fields where needed.

// TODO: define:
//
// /// One block of user/agent content. Mirrors ACP content blocks, minus the parts
// /// we normalize away. Tagged serde: #[serde(tag = "type", rename_all = "snake_case")]
// pub enum ContentBlock {
//     Text { text: String },
//     Image { media_type: String, data: String /* base64 */ },
//     // EmbeddedResource later (ACP supports it) — add when first needed.
// }
//
// pub enum Role { User, Agent }   // thought-chunks get their own event kind, not a role
//
// /// Why a turn stopped — mirrors ACP stopReason 1:1 so nothing is lost in translation.
// pub enum StopReason { EndTurn, MaxTokens, MaxTurnRequests, Refusal, Cancelled }
//
// pub struct McpServerSpec { name: String, command: String, args: Vec<String>, env: BTreeMap<String,String> }
//     → sent in Command::NewSession; ordering-stable via BTreeMap.
//
// pub struct PlanEntry { content: String, status: PlanEntryStatus, priority: Option<u8> }
// pub enum PlanEntryStatus { Pending, InProgress, Completed }
//
// pub enum ToolCallKind { Read, Edit, Delete, Move, Search, Execute, Think, Fetch, Other }
// pub enum ToolCallStatus { Pending, InProgress, Completed, Failed }
//     → these mirror ACP kinds/statuses; keep variants aligned with ACP v1 schema.
//
// TODO: unit-test that serde representations match the shapes sketched in docs/architecture.md.
