//! Find installed agent binaries. Linux + macOS only by design.
//!
//! Strategy per driver (precedence order — first hit wins):
//!   1. Config override: already merged into DriverRegistry's overrides map by
//!      config.rs; when present, detection SKIPS scanning entirely and only
//!      version-probes the overridden program (probe failure is tolerated and
//!      reported as found=true / version=None — config is an explicit human
//!      statement, not a guess).
//!   2. PATH scan for the well-known program name(s) in SCAN_TARGETS below,
//!      honoring POSIX `PATH` semantics: candidate dirs split on ':', empty
//!      entries mean current directory ONLY if PATH literally contains ""
//!      (libc behavior); a name containing '/' bypasses scanning.
//!   3. Known install locations fallbacks (nvm/volta/homebrew node bins) for
//!      npx-based drivers — see KNOWN_LOCATIONS.
//!   4. If any candidate resolves: run `<prog> --version` with a short timeout
//!      and parse the first stdout line for display purposes only (the value
//!      feeds DetectedDriver.version; it never gates spawnability).
//!   5. Nothing found: fall back to the per-driver Default spec from
//!      fxproto/src/driver.rs (spawnable last resort, e.g. the `npx -y ...`
//!      form for ClaudeCode). detected_version = None means "unverified".

// DECISION (kept): do NOT accept bare `claude` as a ClaudeCode autodetect
// target in v0. docs/research/acp.md records exactly one ACP path for Claude
// Code: the npm adapter package `@agentclientprotocol/claude-agent-acp` (no
// first-party ACP flag exists for the `claude` binary). Accepting `claude`
// speculatively creates false positives that only fail at initialize time.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fxproto::driver::{DriverId, DriverSpec};
use fxproto::reply::DetectedDriver;

/// What detect() hands back to DriverRegistry: the wire-facing report PLUS the
/// resolved program (which DetectedDriver deliberately omits — reply.rs pins
/// spec_used as NOT PATH-resolved; the resolved program stays internal so
/// SpawnPlan can actually spawn it).
#[derive(Debug, Clone)]
pub struct Detection {
    pub report: DetectedDriver,
    /// Executable chosen by override > scan > known-locations. None = nothing
    /// resolved (SpawnPlan will use the spec's raw program name as-is, letting
    /// PATH resolution happen at spawn time as the last resort).
    pub resolved_program: Option<PathBuf>,
}

/// Async because step 4 spawns processes via tokio::process::Command with a
/// timeout; do NOT downgrade to sync without dropping the version probe.
pub async fn detect(id: DriverId, override_spec: Option<&DriverSpec>) -> Detection {
    match override_spec {
        Some(spec) => {
            // Step 1: config is an explicit human statement — found=true no
            // matter what the probe says; probe failure ⇒ version=None.
            let version = probe_version(&spec.program, &spec.args).await;
            let resolved = resolve_on_path(&spec.program).or_else(|| {
                spec.program
                    .contains('/')
                    .then(|| PathBuf::from(&spec.program))
            });
            Detection {
                report: DetectedDriver {
                    driver: id,
                    found: true,
                    version,
                    spec_used: spec.clone(),
                },
                resolved_program: resolved,
            }
        }
        None => scan_and_probe(id).await,
    }
}

async fn scan_and_probe(id: DriverId) -> Detection {
    // Steps 2–3: per target name, PATH hits first, then known locations.
    // FIRST executable candidate found (in that order) wins.
    let locations = known_locations(id);
    for name in scan_targets(id) {
        // A name containing '/' bypasses scanning entirely (libc semantics).
        let direct = name.contains('/').then(|| PathBuf::from(name));
        let mut all = Vec::new();
        match direct {
            Some(p) => all.push(p),
            None => {
                all.extend(resolve_on_path(name));
                all.extend(locations.iter().cloned());
            }
        }
        for candidate in &all {
            if candidate.is_file() && is_executable(candidate) {
                tracing::debug!(target: "detect", path = %candidate.display(), "detect hit");
                return probe_found(id, candidate.clone(), name).await;
            }
        }
    }

    // Step 5: exhausted — fall back to the per-driver default spec.
    tracing::debug!(target: "detect", id = ?id, "no candidate found; using default spec");
    Detection {
        report: DetectedDriver {
            driver: id,
            found: false,
            version: None,
            spec_used: id.default_spec(),
        },
        resolved_program: None,
    }
}

/// Build the report for a concrete executable: synthesized spec + version probe.
async fn probe_found(id: DriverId, resolved: PathBuf, display_name: &str) -> Detection {
    let args = spawn_args(id)
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let version = probe_version(display_name, &args).await;
    Detection {
        report: DetectedDriver {
            driver: id,
            found: true,
            version,
            spec_used: DriverSpec {
                program: display_name.to_owned(),
                args,
                env: Default::default(),
            },
        },
        resolved_program: Some(resolved),
    }
}

/// Well-known binary names probed IN ORDER. ClaudeCode deliberately lists only
/// the adapter binary — see the DECISION block above.
fn scan_targets(id: DriverId) -> &'static [&'static str] {
    match id {
        DriverId::ClaudeCode => &["claude-code-acp"],
        DriverId::GeminiCli => &["gemini"], // needs --acp arg at spawn time
        DriverId::CodexCli => &["codex-acp"],
    }
}

/// Args used both for probing and for the synthesized spec of a scanned hit.
fn spawn_args(id: DriverId) -> &'static [&'static str] {
    match id {
        DriverId::ClaudeCode => &[],
        DriverId::GeminiCli => &["--acp"],
        DriverId::CodexCli => &[],
    }
}

fn dirs_in_env_path() -> Vec<PathBuf> {
    let Ok(path) = std::env::var("PATH") else {
        return Vec::new();
    };
    // libc semantics: an empty entry ("" or "::") means the CURRENT directory.
    path.split(':').map(PathBuf::from).collect()
}

/// Every directory in $PATH where `program` resolves to an executable.
fn resolve_all_path_matches(program: &str) -> Vec<PathBuf> {
    if program.contains('/') {
        // A name containing '/' bypasses scanning entirely.
        let p = PathBuf::from(program);
        return if p.is_file() && is_executable(&p) {
            vec![p]
        } else {
            vec![]
        };
    }
    let mut hits = Vec::new();
    for dir in dirs_in_env_path() {
        let candidate = dir.join(program);
        if candidate.is_file() && is_executable(&candidate) {
            hits.push(candidate);
        }
    }
    hits
}

/// Manual `which`: split $PATH on ':', return first dir where
/// dir.join(program) passes the executable test. No libc dependency.
fn resolve_on_path(program: &str) -> Option<PathBuf> {
    resolve_all_path_matches(program).into_iter().next()
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// nvm/volta/homebrew node bins — checked only if PATH scan misses, because
/// GUI-spawned servers often have minimal PATH (no nvm sourced). Dirs that
/// don't exist are skipped silently (debug!-logged).
fn known_locations(id: DriverId) -> Vec<PathBuf> {
    let _ = id; // same list serves every driver today; param keeps call sites honest
    let mut out = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        let home = home.to_owned();
        if let Ok(nvm_dir) = std::env::var("NVM_DIR") {
            out.push(PathBuf::from(&nvm_dir).join("versions/node"));
        } else {
            out.push(PathBuf::from(&home).join(".nvm/versions/node"));
        }
        for base in std::mem::take(&mut out) {
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    out.push(entry.path().join("bin"));
                }
            } else {
                tracing::debug!(target: "detect", dir = %base.display(), "skipping absent dir");
            }
        }
        out.push(PathBuf::from(&home).join(".volta/bin"));
    }

    out.push(PathBuf::from("/opt/homebrew/bin"));
    out.push(PathBuf::from("/usr/local/bin"));
    out
}

/// `$PROGRAM --version`, capture first line of stdout, VERSION_PROBE_TIMEOUT
/// then kill. Any failure (spawn err, timeout, empty output) => None. Never
/// fails hard — absence/unreachability is a normal result here.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

async fn probe_version(program: &str, args: &[String]) -> Option<String> {
    use tokio::io::AsyncReadExt;

    let mut child = tokio::process::Command::new(program)
        .arg("--version")
        .args(args.iter().filter(|a| !a.is_empty()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let read_line = async move {
        let mut buf = [0u8; 256];
        let n = stdout.read(&mut buf).await.unwrap_or(0);
        String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(64)
            .collect::<String>()
    };

    tokio::pin!(read_line);
    let result = match tokio::time::timeout(VERSION_PROBE_TIMEOUT, &mut read_line).await {
        Ok(line) => line,
        Err(_) => return None,
    };
    drop(child); // kill_on_drop cleans up if still alive
    (!result.is_empty()).then_some(result)
}
