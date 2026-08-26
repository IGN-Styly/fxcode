//! Permission modal: renders AppState.perms.pending entries.

// TODO:
//
// pub struct PermissionDialog { request_id: RequestId }
//
// Triggered when pending becomes non-empty (observe AppState / subscribe in WorkspaceView):
// window.open_dialog(...) per docs — overlays go through WindowExt, not element trees.
//
// Render: session id, tool summary, option buttons grouped allow vs reject;
// click ⇒ ConnectionManager::send(Command::PermissionResponse { .. }) then close.
//
// Edge cases:
// - multiple pendings queued ⇒ show sequentially (first-in)
// - PermissionResolved arrives externally (turn cancelled server-side) ⇒ auto-dismiss;
//   folds moving it out of `pending` is the signal — no special messaging needed.
