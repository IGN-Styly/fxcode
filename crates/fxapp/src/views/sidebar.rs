//! Sidebar: agent list w/ status dots + session list + "New session" affordance.

// TODO:
//
// pub struct SidebarView { /* selected session id etc. */ }
//
// Render from AppState.agents + AppState.threads keys:
// - per agent: name (driver label), status dot (Starting/Ready/Busy/Crashed colors),
//   sessions grouped underneath; click → select session (sets active thread)
// - "New session" button → pick agent + cwd input → Command::StartAgent/NewSession flow
//   (cwd picker can be a plain text Input for v0; native dialog later)
// - Crashed agents get a retry affordance → Command::StartAgent again
