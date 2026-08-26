//! fxserver — headless daemon owning all agents + state. Target: thin (~500 lines
//! of production logic; tests excluded from that budget). If logic wants to live
//! here, it belongs in fxcore instead.
//!
//! Crate layout note: modules live in this LIB target and `main.rs` is a thin
//! boot shell over it — this is what makes `tests/e2e.rs` (and the unit tests in
//! each module) able to exercise the daemon end-to-end without spawning a child
//! process. All production behavior is unchanged by the lib/bin split.

pub mod ifaddr;
pub mod net;
pub mod pair;
