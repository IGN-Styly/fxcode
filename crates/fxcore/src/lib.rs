//! fxcore — the server brain. Owns agent processes, sessions, and the event log.
//!
//! Knows NOTHING about sockets or UI. fxserver is a thin shell around `Orchestrator`.
//!
//! Concurrency model (full contract in orchestrator.rs): commands funnel through ONE
//! mpsc consumed by a single actor task → totally ordered handling without lock
//! choreography. Long-running turns run as spawned tasks that emit events through the
//! SAME persist→project→broadcast pipeline (`cmd::EventSink`); the sink serializes
//! emitters with an internal mutex so broadcast order == seq order globally. A second
//! long-lived task (`cmd::ConnPump`) drains per-agent connection channels into the
//! same sink — two state owners total: the actor and the sink mutex.
//!
//! Module map:
//! - `ids`           IdGen — the ONLY id minting site (agent/turn/request only)
//! - `config`        Config load/merge (~/.fxcode/config.toml); full TOML schema there
//! - `orchestrator`  THE entrypoint: execute(cmd) + subscribe() + replay_from()
//!   + projection_snapshot() + shutdown()
//! - `bus`           broadcast wrapper; lag = drop+flag, never block; BUS_CAPACITY
//! - `proj`          boot-time projection rebuild + command-validation reads
//! - `store`         EventStore trait + SQLite impl (single writer, Option A)
//! - `driver`        registry, detection, ACP connection actor + normalization
//! - `cmd`           command handlers — the ONLY mutators of orchestrator state

pub mod bus;
pub mod cmd;
pub mod config;
pub mod driver;
pub mod ids;
pub mod orchestrator;
pub mod proj;
pub mod store;

// Facade re-exports — the COMPLETE list of what fxserver/tests may name
// without a module path. Anything not here is intentionally internal; adding to
// this list is the only way a type becomes public API.

pub use bus::{BUS_CAPACITY, BusError, BusReceiver, EventBus};
pub use config::{Config, ConfigError};
pub use orchestrator::Orchestrator;
pub use store::EventStore;
pub use store::sqlite::SqliteStore; // integration tests open real tempdir stores
//
// Crate-root error type (defined below) is implicitly part of the facade:
//
// Reachable-but-not-re-exported (intentional middle ground): everything under
// `pub mod`s that internal seams need by path but consumers shouldn't bare-name:
// - `driver::*` / `cmd::*`: fxserver drives everything through Orchestrator; the
//   wire layer must not reach past it. (Orchestrator's own methods — execute /
//   subscribe / replay_from / projection_snapshot / shutdown — are the ONLY wire-
//   facing surface; docs/crates.md's "ReplayFrom(store, …)" handshake sketch is
//   superseded by replay_from(), see flagged-conflicts note there.)
// - `proj::Projections`: state leaves fxcore only via
//   `Orchestrator::projection_snapshot()` (envelope::Snapshot building).
// - `ids::IdGen`: exactly one instance lives inside Orchestrator. Still PUBLIC
//   by path (`fxcore::ids::IdGen`) because tests inject deterministic gens via
//   `Orchestrator::new_with_ids`.
//
// Top-level error type. thiserror enum at the crate root. Handlers return
// Result<Reply, Self>; Orchestrator::execute converts Err → Reply::Error(FxError)
// at exactly ONE site (fxproto/reply.rs contract). Wire outcomes that are DATA
// (AgentNotFound, SessionNotFound, TurnNotActive, PermissionNotFound,
// AgentStartFailed-as-reply) are constructed by handlers as Reply::Error DIRECTLY
// and never appear as variants here — this enum is infrastructure failures only.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Config missing/unparsable. Fatal at boot, before any client exists —
    /// never crosses the wire. (fxserver logs it and exits nonzero.)
    #[error("config: {0}")]
    Config(#[from] config::ConfigError),
    /// Spawning/initializing an agent process failed. Detail string carries the
    /// program + OS error. Converts to FxErrorCode::AgentStartFailed.
    #[error("agent spawn failed: {0}")]
    AgentStart(String),
    /// ACP connection broke mid-operation (child died, protocol violation).
    /// During StartAgent → AgentStartFailed; mid-turn it becomes
    /// TurnFinished{stop_reason: cancelled} + AgentStatus::Crashed events
    /// emitted by the turn task, not an Err.
    #[error("acp connection: {0}")]
    Acp(String),
    /// Event store append/replay/open failure. → FxErrorCode::StoreFailure.
    #[error(transparent)]
    Store(#[from] store::StoreError),
    /// execute() raced shutdown (actor gone or stopping). → FxErrorCode::Internal.
    #[error("orchestrator is shutting down")]
    ShuttingDown,
}
