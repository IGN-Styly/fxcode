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
// /// Window root. Routes between connect.rs (no server yet) and the dock workspace.
// pub struct WorkspaceView { /* Entity handles: AppState via cx.global, ConnManager */ }
// impl Render for WorkspaceView {
//     // match conn status:
//     //   Disconnected/Connecting → ConnectScreen
//     //   Ready → Dock layout: Sidebar | ThreadView | StatusBar (+ permission modal overlay)
// }
//
// Dock notes (gpui-component): serializable panel layout; start with fixed 2-pane split
// (sidebar resizable), add dock persistence in M3.
