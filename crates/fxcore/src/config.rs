//! Configuration: defaults merged with ~/.fxcode/config.toml.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
// use std::path::{Path, PathBuf};
//
// use fxproto::driver::{DriverId, DriverSpec};   // CANONICAL home: fxproto/src/driver.rs.
//                                                // Do NOT re-define or alias here — same
//                                                // ban as reply.rs applies to DriverSpec.
// TODO:
//
// #[derive(Debug, Clone)]
// pub struct Config {
//     /// State dir: SQLite db (data_dir/events.db) + pairing token
//     /// (data_dir/token) live here. Default: $XDG_DATA_HOME/fxcode when that
//     /// env var is set and non-empty, else $HOME/.fxcode. NEVER relocated by
//     /// anything but config.toml itself (see load()).
//     pub data_dir: PathBuf,
//
//     /// Listen address override. None ⇒ auto-detect (fxserver/src/ifaddr.rs:
//     /// tailscale ip -4 > 100.64.0.0/10 scan > 127.0.0.1). Parsed strictly via
//     /// std::net::SocketAddr::from_str; a bad value is a FATAL ConfigError,
//     /// never a silent fallback to autodetect (silent binds are a security bug).
//     pub bind_override: Option<std::net::SocketAddr>,
//
//     /// Per-driver spawn overrides. MISSING KEYS mean "let detect.rs autodetect
//     /// decide"; present keys REPLACE the whole spec for that driver — table-wise
//     /// replace, never field-wise merge with detected/default values (a partial
//     /// `[drivers.gemini_cli]` keeping only `program` LOSES default args; users
//     /// write complete tables). Detect precedence chain overall:
//     ///   this map  >  autodetect probe (PATH + known locations)  >  per-DriverId Default.
//     pub drivers: BTreeMap<DriverId, DriverSpec>,
// }
//
// //////////////////////////////////////////////////////////////////////////////
// FULL config.toml SCHEMA — every key that exists in v0. Anything else in the
// file produces a tracing::warn!("unknown key ...") and is ignored.
//
// // ~/.fxcode/config.toml — path is FIXED (see load()); data_dir below does NOT
// // relocate it.
// data_dir = "/var/lib/fxcode"            # optional string; omitted ⇒ default above
// bind_addr = "100.101.102.103:7421"      # optional "ip:port"; omitted ⇒ autodetect
//
// [drivers.claude_code]                   # keys = DriverId snake_case serde names:
// program = "claude-code-acp"             #   claude_code | gemini_cli | codex_cli
// args = ["--flag"]                       # required string; optional, default []
// env = { KEY = "value", OTHER = "x" }    # optional, default {}
//
// (An unknown driver key like [drivers.cursor] fails TOML→DriverId parsing;
// policy below converts that one row into a warning + skip, not a fatal error.)
// //////////////////////////////////////////////////////////////////////////////
//
// impl Config {
//     /// Merge chain, in order — every field: hardcoded default < config.toml <
//     /// (CLI flags: fxserver main.rs MAY layer its own override AFTER load();
//     ///  nothing inside fxcore reads env vars or argv directly).
//     ///
//     /// 1. Compute default data_dir from $XDG_DATA_HOME/$HOME as documented on
//     ///    the field ($HOME unset ⇒ Err(ConfigError::HomeDir)).
//     /// 2. defaults: bind_override = None; drivers = empty BTreeMap.
//     /// 3. Parse $HOME/.fxcode/config.toml if present (toml crate); NOT-found
//     ///    file is NOT an error (first boot). Any other IO error (permissions)
//     ///    IS fatal: ConfigError::Io.
//     /// 4. Map onto Config: `data_dir` replaces; `bind_addr` → SocketAddr parse
//     ///    (failure ⇒ ConfigError::Parse); `[drivers.*]` deserialized into a
//     ///    BTreeMap<String, RawDriverTable> FIRST so unknown driver names warn +
//     ///    skip instead of failing the whole file; each surviving row is
//     ///    converted whole into DriverSpec (mandatory `program`, defaulted
//     ///    args/env) and inserted into Config.drivers.
//     /// 5. mkdir_all(data_dir). Failure ⇒ ConfigError::Io — FATAL at boot
//     ///    (store/token cannot be created later).
//     /// Returns the final Config; callers treat Err as process-exit-nonzero
//     /// (maps through crate::Error::Config before any client exists).
//     pub fn load() -> Result<Self, ConfigError>;
// }
//
// /// Internal serde shape mirroring the raw TOML (Option-heavy so every absent
// /// key keeps its default). Never escapes config.rs; convert to Config in step 4.
// struct TomlConfig { data_dir: Option<PathBuf>, bind_addr: Option<String>,
//                     drivers: Option<BTreeMap<String, DriverSpec>> }
//     (step 4's unknown-name tolerance means step 3 actually parses drivers as
//      BTreeMap<String, DriverSpec> only after filtering names via
//      DriverId parse-attempt per row — implement as the two-phase map above.)
//
// #[derive(Debug, thiserror::Error)]
// pub enum ConfigError {
//     /// $HOME unset / empty. Fatal: nowhere deterministic to put state.
//     #[error("cannot determine home directory ($HOME unset)")]
//     HomeDir,
//     /// File/dir access failed (not plain absence — see load() step 3).
//     #[error("io: {0}")]
//     Io(#[from] std::io::Error),
//     /// TOML syntax error or value that cannot become its target type
//     /// (bad SocketAddr string, bad DriverSpec row). Message includes path+span
//     /// context from the toml crate where available.
//     #[error("config parse: {0}")]
//     Parse(#[from] toml::de::Error),
// }
