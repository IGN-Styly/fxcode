//! Permission projection: pending asks + their resolutions.

// Imports to restore as you define the types:
// use std::collections::{BTreeMap, VecDeque};
//
// use crate::event::{FxEvent, PermissionOption, ToolCallSummary};
// use crate::ids::{RequestId, OptionId, SessionId};

// TODO: define:
//
// #[derive(Default)]
// pub struct PermsState {
//     /// Pending = needs a modal on screen. Keyed by RequestId.
//     pub pending: BTreeMap<RequestId, PendingPermission>,
//     /// Recent resolutions (bounded ring, say 50) for audit display.
//     pub recent: VecDeque<ResolvedPermission>,
// }
//
// pub struct PendingPermission {
//     pub session: SessionId,
//     pub summary: ToolCallSummary,
//     pub options: Vec<PermissionOption>,
// }
//
// pub struct ResolvedPermission { request_id: RequestId, chosen: Option<OptionId> } // None = cancelled
//
// /// Fold: PermissionRequested → insert pending; PermissionResolved → move to recent.
// /// Server-side this state also gates Command::PermissionResponse validation
// /// (unknown/expired request_id ⇒ error reply).
// pub fn apply_perms(state: &mut PermsState, ev: &FxEvent);
