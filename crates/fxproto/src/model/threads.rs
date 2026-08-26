//! Thread projection: the transcript of one session. This is what the UI renders.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
//
// use crate::content::{PlanEntry, Role};
// use crate::event::FxEvent;
// use crate::ids::{SessionId, ToolCallId, TurnId};

// TODO: define:
//
// #[derive(Default)]
// pub struct ThreadsState {
//     pub threads: BTreeMap<SessionId, ThreadState>,
// }
//
// pub struct ThreadState {
//     /// Transcript in arrival order. User chunks and agent chunks interleave here;
//     /// consecutive same-role text chunks MERGE (append) so streaming doesn't
//     /// explode list length.
//     pub messages: Vec<Message>,
//     /// Keyed upsert target — ToolCallUpsert replaces by id, never appends dupes.
//     /// Rendered inline relative to `messages` via insertion index (see below).
//     pub tool_calls: BTreeMap<ToolCallId, ToolCall>,
//     /// Where each tool call sits in the transcript flow (Vec of discriminants).
//     pub flow: Vec<FlowItem>,                 // Text(msg_idx) | Tool(ToolCallId)
//     pub plan: Vec<PlanEntry>,
//     pub active_turn: Option<TurnId>,
// }
//
// pub struct Message { pub role: Role, pub text: String }
// pub struct ToolCall { title, kind, status, output: Option<String>, _meta: Option<Value> }
//
// pub enum FlowItem { Message(usize), Tool(ToolCallId) }
//     ← lets thread.rs render one flat VirtualList over messages + cards in true order.
//
// /// Fold rules:
// /// - Chunk: merge into last FlowItem::Message if same role, else push new message+flow item.
// /// - ToolCallUpsert: insert-or-update map entry; append flow item only when first seen.
// /// - PlanUpdated / TurnStarted / TurnFinished: obvious field updates; TurnFinished clears active_turn.
// /// - PermissionRequested/Resolved: ignore here (perms.rs owns those) but Resolved may
// ///   want to annotate the matching tool card later — leave a comment hook.
// pub fn apply_thread(state: &mut ThreadsState, ev: &FxEvent);
