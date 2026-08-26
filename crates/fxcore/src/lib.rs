//! fxcore — the server brain. Owns agent processes, sessions, and the event log.
//!
//! Knows NOTHING about sockets or UI. fxserver is a thin shell around `Orchestrator`.
//!
//! Concurrency model (see docs/architecture.md): commands funnel through ONE mpsc
//! consumed by a single actor task → totally ordered handling without lock
//! choreography. Long-running turns run as spawned tasks that emit events through the
//! same persist→broadcast path.
//!
//! Module map:
//! - `config`        Config load/merge (~/.fxcode/config.toml)
//! - `orchestrator`  THE entrypoint: execute(cmd) + subscribe()
//! - `bus`           broadcast wrapper; lag = drop+flag, never block
//! - `proj`          boot-time projection rebuild via fxproto::model folds
//! - `store`         EventStore trait + SQLite impl (single writer)
//! - `driver`        registry, detection, ACP connection actor + normalization
//! - `cmd`           command handlers — the ONLY mutators of orchestrator state

pub mod bus;
pub mod cmd;
pub mod config;
pub mod driver;
pub mod orchestrator;
pub mod proj;
pub mod store;

// TODO: re-export the facade:
//
//   pub use config::Config;
//   pub use orchestrator::Orchestrator;
//   pub use store::EventStore;
//
// TODO: top-level Error enum (thiserror) covering Store, DriverSpawn, Acp, Config.
