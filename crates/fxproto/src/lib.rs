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

// TODO: re-export the everyday items so downstream crates can `use fxproto::prelude::*`:
//
//   pub mod prelude {
//       pub use crate::command::*;
//       pub use crate::envelope::*;
//       pub use crate::event::*;
//       pub use crate::ids::*;
//   }
