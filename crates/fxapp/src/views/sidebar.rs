//! Sidebar: agent list w/ status dots + session list + "New session" affordance.

use fxproto::ids::{AgentId, SessionId};

// TODO:
//
// pub struct SidebarView {
//     selected_session: Option<SessionId>,          // mirrors WorkspaceView's active thread
//     new_session: Option<NewSessionDraft>,         // pending "new session" dialog state
// }
// struct NewSessionDraft { agent: AgentId, cwd_input: Entity<InputState>,
//                           awaiting_ready: bool }  // see two-phase flow below
//
// DATA SOURCE (exact paths):
//   cx.global::<AppState>().agents.agents           — BTreeMap, iteration == spawn order
//                                                     (uuid v7 lexicographic), do not sort.
//   Sessions grouped UNDER an agent come from AgentState.sessions (fold rule S2 order) —
//   NEVER from AppState.threads keys (that map says "which transcripts exist", agents.rs
//   owns "which sessions belong to which agent"; threads may contain a session whose
//   agent row is missing (S3 garbled log) — those render NOWHERE in the sidebar).
//   Session label = threads.threads[&sid].cwd file_name(); missing key ⇒ "(loading)"
//   disabled row; empty cwd ⇒ full path string.
//
// RENDER:
//   per agent row ("agent-row", AgentId): driver.label() + status dot color map:
//     Starting → amber · Ready → green · Busy → blue · Crashed { .. } → red ·
//     Stopped → grey. Crashed rows keep the tooltip with exit_code when present.
//   session rows nested under their agent ("session-row", SessionId);
//   selected ⇒ accent background. Clicking sets SidebarView.selected_session AND
//   WorkspaceView's active session (plain state write — NOT a protocol command).
//
// INTENTS → COMMANDS:
//   "New session" button on a READY agent ⇒ open NewSessionDraft (cwd as plain text Input
//     for v0; native dialog later); confirm ⇒ send(Command::NewSession {
//       agent, cwd: parsed_input, mcp_servers: vec![] }) (v0 sends none — MCP UI is M3+).
//   Two-phase case: agent Stopped/Crashed/absent ⇒ flow sends StartAgent { driver } first,
//     marks awaiting_ready=true, and on observing AgentStatus::Ready for that AgentId via
//     a cx.observe on AppState automatically proceeds to the NewSession step above.
//   Crashed/Stopped agent rows also expose an inline retry arrow → StartAgent { driver }.
//     (Both paths spawn a NEW AgentId per ids.rs minting rules — never reuse stale ids.)
//
// STATES ENUMERATED:
//   no-agents  (agents.agents empty): centered "No agents yet" + "Open Setup" button
//              → SetupView.
//   ready      : full list as above.
//   reconnect  : inherited from WorkspaceView banner; sidebar itself stays rendered from
//              last-known projections (they never clear except on SnapshotRequired).
