//! Canonical projections + fold functions — the shared brain.
//!
//! BOTH sides run these:
//! - fxserver (fxcore/src/proj.rs): rebuilds state at boot by folding the event log,
//!   and uses it to validate commands (e.g. reject Prompt for unknown session).
//! - fxapp (src/store/mod.rs): applies live events to UI stores.
//!
//! Contract rules:
//! - Folds are TOTAL: any event applied to any state is defined. Unknown session ⇒
//!   create-or-ignore (tracing::debug!), never panic.
//! - Idempotent per event; ordering comes from Seq, folds assume events arrive in order
//!   (both sides guarantee it).
//! - No I/O, no clocks — pure `fn apply(&mut State, &Sequenced<FxEvent>)`.

pub mod agents;
pub mod perms;
pub mod threads;

// TODO: re-export states + apply fns here.
