//! Configuration: defaults merged with ~/.fxcode/config.toml (path injectable
//! for tests via [`Config::load_from`]).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use fxproto::driver::{DriverId, DriverSpec};

/// Runtime configuration. Constructed ONLY via [`Config::load`] /
/// [`Config::load_from`]; callers treat `Err` as process-exit-nonzero.
#[derive(Debug, Clone)]
pub struct Config {
    /// State dir: SQLite db (data_dir/events.db) + pairing token
    /// (data_dir/token) live here. Default: $XDG_DATA_HOME/fxcode when that
    /// env var is set and non-empty, else $HOME/.fxcode. NEVER relocated by
    /// anything but config.toml itself (see [`Config::load_from`]).
    pub data_dir: PathBuf,

    /// Listen address override. None ⇒ auto-detect (fxserver/src/ifaddr.rs:
    /// tailscale ip -4 > 100.64.0.0/10 scan > 127.0.0.1). Parsed strictly via
    /// std::net::SocketAddr::from_str; a bad value is a FATAL ConfigError,
    /// never a silent fallback to autodetect (silent binds are a security bug).
    pub bind_override: Option<SocketAddr>,

    /// Per-driver spawn overrides. MISSING KEYS mean "let detect.rs autodetect
    /// decide"; present keys REPLACE the whole spec for that driver — table-wise
    /// replace, never field-wise merge with detected/default values (a partial
    /// `[drivers.gemini_cli]` keeping only `program` LOSES default args; users
    /// write complete tables). Detect precedence chain overall:
    ///   this map  >  autodetect probe (PATH + known locations)  >  per-DriverId Default.
    pub drivers: BTreeMap<DriverId, DriverSpec>,
}

impl Config {
    /// Fixed config file path: $HOME/.fxcode/config.toml.
    fn default_config_path() -> Result<PathBuf, ConfigError> {
        let home = home_dir().ok_or(ConfigError::HomeDir)?;
        Ok(home.join(".fxcode").join("config.toml"))
    }

    /// Production entrypoint: default data_dir computed from the environment,
    /// fixed config path, no explicit overrides. Merge chain — every field:
    /// hardcoded default < config.toml < (CLI flags: fxserver main.rs MAY layer
    /// its own override AFTER load(); nothing inside fxcore reads env vars or
    /// argv directly).
    ///
    /// 1. Compute default data_dir from $XDG_DATA_HOME/$HOME ($HOME unset ⇒
    ///    Err(ConfigError::HomeDir)).
    /// 2. defaults: bind_override = None; drivers = empty BTreeMap.
    /// 3. Parse the TOML file if present; NOT-found is NOT an error (first
    ///    boot). Any other IO error is fatal: ConfigError::Io.
    /// 4. Map onto Config (see load_from).
    /// 5. mkdir_all(data_dir). Failure ⇒ ConfigError::Io — FATAL at boot.
    pub fn load() -> Result<Self, ConfigError> {
        // HomeDir must fire even when no config file exists anywhere.
        let data_dir = default_data_dir()?;
        match Config::default_config_path() {
            Ok(path) => Self::load_from(&path, Some(data_dir)),
            Err(e) => Err(e),
        }
    }

    /// Test/injectable twin of [`Config::load`]: parse `config_path` (missing
    /// file allowed, same as load) against an EXPLICIT `data_dir`. When
    /// `data_dir` is Some it wins over BOTH the environment default AND any
    /// `data_dir` key inside the file (the caller stating a dir explicitly is
    /// stronger than a file statement); None behaves exactly like load().
    pub fn load_from(config_path: &Path, data_dir: Option<PathBuf>) -> Result<Self, ConfigError> {
        // Defaults.
        let mut this = Self {
            data_dir: match &data_dir {
                Some(dir) => dir.clone(),
                None => default_data_dir()?,
            },
            bind_override: None,
            drivers: BTreeMap::new(),
        };

        let raw = match std::fs::read_to_string(config_path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // First boot: fine, keep defaults.
                make_data_dir(&this.data_dir)?;
                return Ok(this);
            }
            Err(err) => return Err(ConfigError::Io(err)),
        };

        let table: toml::Table = toml::from_str(&raw).map_err(|err| parse_err(err.to_string()))?;

        for key in table.keys() {
            if !matches!(key.as_str(), "data_dir" | "bind_addr" | "drivers") {
                tracing::warn!(
                    key = %key,
                    "unknown key in {} — ignoring",
                    config_path.display()
                );
            }
        }

        if let Some(v) = table.get("data_dir") {
            if let Some(dir) = v.as_str() {
                if data_dir.is_none() {
                    this.data_dir = PathBuf::from(dir);
                } else {
                    tracing::debug!("explicit data_dir parameter overrode config.toml's value");
                }
            } else {
                return Err(not_a_string("data_dir"));
            }
        }

        if let Some(v) = table.get("bind_addr") {
            let s = v.as_str().ok_or_else(|| not_a_string("bind_addr"))?;
            let addr = SocketAddr::from_str(s)
                .map_err(|err| not_parsable(format!("bind_addr {s:?}: {err}")))?;
            this.bind_override = Some(addr);
        }

        if let Some(drivers) = table.get("drivers").and_then(|v| v.as_table()) {
            for (name, row) in drivers {
                let Some(id) = parse_driver_config_name(name) else {
                    tracing::warn!(
                        driver = %name,
                        "unknown [drivers.*] table in {} — skipping row",
                        config_path.display()
                    );
                    continue;
                };
                let spec = RawDriverSpec::from_row(row)?;
                this.drivers.insert(id, spec.into_driver_spec());
            }
        }

        make_data_dir(&this.data_dir)?;
        Ok(this)
    }
}

/// Internal serde shape mirroring one `[drivers.<name>]` row BEFORE conversion
/// into the canonical `fxproto::driver::DriverSpec`. Never escapes config.rs.
struct RawDriverSpec {
    program: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

impl RawDriverSpec {
    fn from_row(value: &toml::Value) -> Result<Self, ConfigError> {
        let fail = |msg: String| not_parsable(msg);
        let table = value
            .as_table()
            .ok_or_else(|| fail(format!("driver spec must be a [table], got {value}")))?;
        let program = table
            .get("program")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| fail("driver spec requires string field `program`".into()))?
            .to_owned();
        let mut args = Vec::new();
        if let Some(a) = table.get("args") {
            let list = a
                .as_array()
                .ok_or_else(|| fail("`args` must be an array of strings".into()))?;
            for item in list {
                let s = item
                    .as_str()
                    .ok_or_else(|| fail("`args` items must be strings".into()))?;
                args.push(s.to_owned());
            }
        }
        let mut env = BTreeMap::new();
        if let Some(e) = table.get("env") {
            let map = e
                .as_table()
                .ok_or_else(|| fail("`env` must be an inline table".into()))?;
            for (k, v) in map {
                let val = v
                    .as_str()
                    .ok_or_else(|| fail("`env` values must be strings".into()))?;
                env.insert(k.clone(), val.to_owned());
            }
        }
        Ok(Self { program, args, env })
    }

    fn into_driver_spec(self) -> DriverSpec {
        DriverSpec {
            program: self.program,
            args: self.args,
            env: self.env,
        }
    }
}

/// snake_case names used as config.toml / serde keys for [`DriverId`]
/// (serde derive on DriverId uses `rename_all = "snake_case"`; a free fn since
/// foreign-type impl blocks are illegal).
fn parse_driver_config_name(name: &str) -> Option<DriverId> {
    match name {
        "claude_code" => Some(DriverId::ClaudeCode),
        "gemini_cli" => Some(DriverId::GeminiCli),
        "codex_cli" => Some(DriverId::CodexCli),
        _ => None,
    }
}

fn not_a_string(key: &str) -> ConfigError {
    parse_err(format!("{key} must be a string"))
}

fn not_parsable(msg: impl Into<String>) -> ConfigError {
    parse_err(msg)
}

/// Build a `ConfigError::Parse` carrying a rendered message.
///
/// NOTE (DEVIATION from the stub's variant shape): `serde::de::Error::custom`
/// would be the canonical constructor for a raw `toml::de::Error`, but the
/// serde crate is not a direct fxcore dependency (fxcore names only
/// serde_json) and Cargo.toml edits are off-limits — so `Parse` carries a
/// String instead and toml syntax failures convert with `.to_string()`.
fn parse_err(msg: impl Into<String>) -> ConfigError {
    ConfigError::Parse(msg.into())
}

fn default_data_dir() -> Result<PathBuf, ConfigError> {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("fxcode"));
    }
    Ok(home_dir().ok_or(ConfigError::HomeDir)?.join(".fxcode"))
}

fn home_dir() -> Option<PathBuf> {
    // POSIX-only by design (docs/architecture.md platform scope).
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn make_data_dir(dir: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir).map_err(ConfigError::Io)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// $HOME unset / empty. Fatal: nowhere deterministic to put state.
    #[error("cannot determine home directory ($HOME unset)")]
    HomeDir,
    /// File/dir access failed (not plain absence — see load_from).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// TOML syntax error or value that cannot become its target type
    /// (bad SocketAddr string, bad DriverSpec row). Carries a rendered message
    /// instead of the raw `toml::de::Error` (see parse_err note above).
    #[error("config parse: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "fxcore-config-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("scratch");
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    impl std::ops::Deref for Scratch {
        type Target = PathBuf;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    #[test]
    fn missing_file_is_first_boot_defaults() {
        let scratch = Scratch::new("missing");
        let cfg_path = scratch.join("config.toml"); // deliberately absent
        let data_dir = scratch.join("data");
        let cfg = Config::load_from(&cfg_path, Some(data_dir.clone())).unwrap();
        assert_eq!(cfg.data_dir, data_dir);
        assert_eq!(cfg.bind_override, None);
        assert!(cfg.drivers.is_empty());
        assert!(data_dir.is_dir(), "mkdir_all ran at load time");
    }

    #[test]
    fn full_schema_merges_over_defaults() {
        let scratch = Scratch::new("merge");
        let cfg_path = scratch.join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
bind_addr = "127.0.0.1:7421"

[drivers.gemini_cli]
program = "/opt/bin/gemini"
args = ["--acp", "--verbose"]
env = { KEY = "value" }
"#,
        )
        .unwrap();
        let cfg = Config::load_from(&cfg_path, Some(scratch.join("d"))).unwrap();
        assert_eq!(
            cfg.bind_override,
            Some(SocketAddr::from_str("127.0.0.1:7421").unwrap())
        );
        let gemini = cfg.drivers.get(&DriverId::GeminiCli).unwrap();
        assert_eq!(gemini.program, "/opt/bin/gemini");
        assert_eq!(
            gemini.args,
            vec!["--acp".to_owned(), "--verbose".to_owned()]
        );
        assert_eq!(gemini.env.get("KEY").map(String::as_str), Some("value"));
        // Table-wise replace: absent drivers stay absent (no default prefill).
        assert!(!cfg.drivers.contains_key(&DriverId::ClaudeCode));
    }

    #[test]
    fn unknown_keys_and_driver_rows_warn_and_are_skipped() {
        let scratch = Scratch::new("unknown");
        let cfg_path = scratch.join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
bind_addr = "0.0.0.0:1"
future_thing = true

[drivers.cursor]
program = "cursor-acp"

[drivers.codex_cli]
program = "codex-acp-x"
"#,
        )
        .unwrap();
        let cfg = Config::load_from(&cfg_path, Some(scratch.join("d"))).unwrap();
        // Unknown top-level key tolerated, unknown driver row skipped —
        // exactly one (valid) row survived.
        assert_eq!(cfg.drivers.len(), 1);
        assert!(cfg.drivers.contains_key(&DriverId::CodexCli));
        assert!(cfg.bind_override.is_some());
    }

    #[test]
    fn bad_bind_addr_is_fatal_never_silent() {
        let scratch = Scratch::new("badaddr");
        let cfg_path = scratch.join("config.toml");
        std::fs::write(&cfg_path, "bind_addr = \"not-an-addr\"").unwrap();
        let err = Config::load_from(&cfg_path, Some(scratch.join("d"))).unwrap_err();
        assert!(err.to_string().contains("config parse"), "{err}");
    }

    #[test]
    fn bad_driver_row_is_fatal() {
        let scratch = Scratch::new("badrow");
        let cfg_path = scratch.join("config.toml");
        std::fs::write(&cfg_path, "[drivers.codex_cli]\nargs = [\"x\"]").unwrap();
        assert!(Config::load_from(&cfg_path, Some(scratch.join("d"))).is_err());
    }

    #[test]
    fn explicit_data_dir_beats_file_value() {
        let scratch = Scratch::new("beatdir");
        let cfg_path = scratch.join("config.toml");
        std::fs::write(&cfg_path, "data_dir = \"/tmp/from-file\"").unwrap();
        let forced = scratch.join("forced");
        let cfg = Config::load_from(&cfg_path, Some(forced.clone())).unwrap();
        assert_eq!(cfg.data_dir, forced);
        assert!(forced.is_dir());
        // None + file value still applies for the injectable ctor too.
        let cfg2 = Config::load_from(&cfg_path, None).unwrap();
        assert_eq!(cfg2.data_dir, PathBuf::from("/tmp/from-file"));
        assert!(cfg2.data_dir.is_dir(), "file-provided dir was created");
    }
}
