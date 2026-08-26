//! In-process fake ACP agent for integration tests — NO real CLIs in CI.
//!
//! Built on the SAME official `agent-client-protocol` crate, but implementing the
//! AGENT side, run over in-memory duplex streams instead of stdio. This exercises our
//! real client code end-to-end while staying hermetic and scriptable.

// TODO:
//
// pub struct FakeAgent { /* scripted behaviors */ }
//
// Scriptable knobs:
// - reply to initialize with chosen capabilities/auth methods
// - on session/prompt: emit an arbitrary script of updates:
//     chunks (split across many notifications), tool_call + tool_call_update sequences,
//     plan updates, then a stopReason
// - optionally send session/request_permission and WAIT for the outcome before proceeding
// - optionally "crash": drop the connection mid-turn
// - optionally stall forever (watchdog tests)
//
// pub struct Script(Vec<Step>);
// pub enum Step { Chunk(String), ToolCall(..), Plan(..), AskPermission(..), Crash, Stall, Stop(StopReason) }
//
// Harness shape: tokio mpsc duplex or duplex stream pair fed into the SDK's server-side
// connection type; expose handles so tests can assert on what the CLIENT sent us
// (prompts, cancels, permission outcomes).
