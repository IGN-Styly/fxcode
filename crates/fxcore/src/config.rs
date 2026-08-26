//! Configuration: defaults merged with ~/.fxcode/config.toml.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
// use std::path::PathBuf;
//
// use crate::driver::DriverSpec;          // once defined in driver/mod.rs
// use fxproto::driver::DriverId;

// TODO:
//
// #[derive(Debug, Clone)]
// pub struct Config {
//     /// State dir: SQLite db + token live here. Default: ~/.fxcode
//     pub data_dir: PathBuf,
//
//     /// Listen address override. None ⇒ auto-detect (see fxserver/src/ifaddr.rs).
//     pub bind_override: Option<std::net::SocketAddr>,
//
//     /// Per-driver spawn overrides. Missing keys fall back to detect.rs autodetect,
//     /// which itself falls back to DriverId default specs.
//     pub drivers: BTreeMap<DriverId, DriverSpec>,
// }
//
// TODO: Config::load() —
//   1. defaults (data_dir = $HOME/.fxcode or $XDG_DATA_HOME/fxcode)
//   2. parse config.toml if present (toml crate); unknown keys ignored w/ warning
//   3. mkdir_all data_dir
//
// TODO: serde struct mirroring Config for toml deserialization (Option-heavy), then convert.
