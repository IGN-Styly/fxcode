//! fxproto — the contract between `fxserver` and `fxapp`.
//!
//! Pure types + the canonical state model. NO async runtime, NO I/O, NO GPUI.
//!
//! Design rule: the event-fold lives here (see `model`), so server-side command
//! validation and client UI projections can never drift apart.
//!
//! Stability rule: serde round-trip stability of everything in this crate is its
//! public API — golden tests guard it.
//!
//! Layout:
//! - `ids`       strong id newtypes
//! - `content`   normalized content shapes (no ACP leakage)
//! - `driver`    driver ids + spawn specs
//! - `command`   client → server intents
//! - `reply`     server → client acks/errors
//! - `event`     canonical events + Sequenced envelope
//! - `envelope`  the Message enum that actually crosses the WebSocket
//! - `model`     projection states + fold functions shared by both sides

pub mod command;
pub mod content;
pub mod driver;
pub mod envelope;
pub mod event;
pub mod ids;
pub mod model;
pub mod reply;

// Re-export the everyday items so downstream crates can `use fxproto::prelude::*`.
// Explicit list (NOT globs): globs over overlapping modules invite silent collisions
// when two modules grow same-named helpers; this list is collision-free today.
pub mod prelude {
    pub use crate::command::Command;
    pub use crate::content::{
        ContentBlock, McpServerSpec, PlanEntry, PlanEntryStatus, PlanPriority, Role, StopReason,
        ToolCallKind, ToolCallStatus,
    };
    pub use crate::driver::{DriverId, DriverSpec};
    pub use crate::envelope::{Message, PROTO_VERSION, Snapshot};
    pub use crate::event::{
        AgentStatus, FxEvent, PermissionOption, PermissionOptionKind, Sequenced, ToolCallSummary,
    };
    pub use crate::ids::{AgentId, OptionId, RequestId, Seq, SessionId, ToolCallId, TurnId};
    pub use crate::reply::{DetectedDriver, FxError, FxErrorCode, Reply};
}
//
// Deliberately NOT in the prelude: `model::*` states + folds. They are imported by
// exactly one site per binary (fxcore/src/proj.rs, fxapp/src/store/mod.rs); putting
// AgentsState/ThreadsState/PermsState in every consumer's namespace buys nothing.
// Import them via explicit paths (`fxproto::model::agents::AgentsState`).
