//! View layer. Rules:
//! - render(state) only — views never mutate projections directly
//! - user intents call ConnectionManager::send(Command)
//! - ElementIds derive from SessionId/ToolCallId/etc., NEVER list indexes

pub mod connect;
pub mod message;
pub mod perms;
pub mod setup;
pub mod sidebar;
pub mod thread;
pub mod tool_call;

use gpui::*;
use gpui_component::*;

// TODO:
//
// /// Window root. Routes between connect.rs (no server yet / fatal) and the dock.
// pub struct WorkspaceView {
//     active_session: Option<SessionId>,     // sidebar writes it, thread.rs reads it
// }
// impl Render for WorkspaceView {
//     // match conn_status (AppState.conn_status):
//     //   Ready → Dock layout: Sidebar | ThreadView | StatusBar
//     //           + permission modal overlay orchestration (below).
//     //   Connecting { attempt } → dock STILL renders (stale projections beat a blank
//     //           screen; they are only ever cleared by SnapshotRequired) with an amber
//     //           banner "reconnecting (attempt N)" (id "reconn-banner").
//     //   Disconnected { fatal: None } → ConnectScreen full-window.
//     //   Disconnected { fatal: Some(_) } → ConnectScreen WITH the mapped fatal error
//     //           line (connect.rs owns the string mapping); park here until human acts.
// }
//
// STATUS BAR (24px row inside the dock, id "statusbar"; M0 exit requires this to exist):
//   left  : conn chip — dot+label from ConnStatus (Ready green / attempt N amber / fatal
//           red); clicking opens ConnectScreen even when connected.
//   center: active_session's cwd file_name from AppState.threads, else "no session".
//   right : pending-permission count badge (>0 amber; click ⇒ open the perms modal queue
//           head manually) · ws RTT read straight off WsHandle.rtt_ms (M0 latency badge).
//
// PERMISSION MODAL ORCHESTRATION (one trigger site lives HERE):
//   observe AppState.perms; on pending empty→non-empty (or dialog just closed ∧ still
//   non-empty) with no open dialog ⇒ open_dialog(perms::PermissionDialog { request_id:
//   first key }) — oldest first. Full contract in views/perms.rs; nothing else in the
//   tree may call open_dialog for permissions.
//
// ElementId NAMESPACE REGISTRY (single place views must not collide; extend per file):
//   ("agent-row", AgentId) · ("session-row", SessionId) · "new-session"
//   ("msg", usize flow-stable message index) · ("tool-call", ToolCallId)
//   ("perm-option", OptionId) · "composer" · "stop-turn" · "jump-latest" · "plan-header"
//   "connect-url" / "connect-token" / "connect-submit" / "connect-error"
//   "setup-rescan" · ("setup-driver", DriverId) · "statusbar" · "reconn-banner".
//
// Dock notes (gpui-component): serializable panel layout; start with fixed 2-pane split
// (sidebar resizable), add dock persistence in M3.
