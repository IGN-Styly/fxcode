//! Permission modal: renders AppState.perms.pending entries.

use fxproto::ids::RequestId;

// TODO:
//
// pub struct PermissionDialog { request_id: RequestId }
//
// ORCHESTRATION (WorkspaceView owns the trigger; this file owns one dialog):
//   Observe AppState.perms: pending transitions empty → non-empty AND no dialog currently
//   open ⇒ window.open_dialog(PermissionDialog { request_id: FIRST key }) — first key is
//   the OLDEST ask (BTreeMap + uuid v7 ⇒ chronological). Overlays go through WindowExt
//   (open_dialog), not element trees.
//
// RENDER (data source — exact path, read LIVE every paint):
//   cx.global::<AppState>().perms.pending.get(&self.request_id):
//     session id, summary.title (ToolCallSummary), option buttons grouped:
//     AllowOnce|AllowAlways under "Allow" · RejectOnce|RejectAlways under "Deny".
//   Button click ⇒ send(Command::PermissionResponse { request_id, option_id }) THEN close
//     the dialog. The fold chain clears `pending` when PermissionResolved lands; do NOT
//     optimistically mutate PermsState from the view (single-writer rule: folds only).
//
// EDGE CASES (all fall out of the fold, zero imperative code):
// - multiple pendings queued ⇒ sequential showings: closing/failing one lets the next
//   empty→non-empty or still-non-empty transition open the next oldest.
// - PermissionResolved arriving EXTERNALLY (turn cancelled server-side, watchdog) removes
//   the map entry behind our back ⇒ get() returns None ⇒ render a 1-frame empty body and
//   close — auto-dismiss IS the "cancelled" UX. The card badge/audit row come from the
//   folds (threads.rs W6 / perms.rs R3), not from messaging here.
//
// ElementIds: ("perm-option", OptionId) per button · ("perm-session") · ("perm-summary").
//   Note OptionId inside an ElementId requires Display-able rendering — ids are strings
//   under #[serde(transparent)], fine as-is.
//
// STATES ENUMERATED:
//   entry-present : full modal as above.
//   entry-vanished: handled = close immediately (see above); never render an error state —
//                   disappearing permissions are protocol-normal, not failures.
