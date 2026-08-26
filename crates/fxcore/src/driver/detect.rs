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

// DECISION (was an open question — resolved, M1-verifiable):
//
// Do NOT accept bare `claude` as a ClaudeCode autodetect target in v0.
//
// Rationale:
// - docs/research/acp.md records exactly one ACP path for Claude Code: the npm
//   adapter package `@agentclientprotocol/claude-agent-acp` (renamed from the
//   deprecated @zed-industries/claude-code-acp). Unlike Gemini CLI, which has a
//   first-party `--acp` flag, no source documents a native ACP mode for the
//   `claude` binary itself.
// - Accepting `claude` speculatively creates false positives: a plain `claude`
//   install would be reported as "found" then fail at initialize handshake —
//   worse UX than found=false plus the correct install hint.
// - The conservative choice also matches fxproto/src/driver.rs: the Default
//   ClaudeCode spec IS the npx adapter form, so v0 never needs a native binary.
//
// M1 verification hook: impl.md Phase 3.4/4.x runs detection against the real
// machine. If upstream ships native ACP (`claude --acp` or similar), add
// "claude" to SCAN_TARGETS AFTER "claude-code-acp" AND require an initialize
// handshake probe before reporting found — until that probe exists, `claude`
// stays out of the target list entirely.

// Imports to restore as you implement:
// use std::path::{Path, PathBuf};
//
// use fxproto::driver::{DriverId, DriverSpec};
// use fxproto::reply::DetectedDriver;   // CANONICAL type — defined in fxproto, not here

// TODO:
//
// /// What detect() hands back to DriverRegistry: the wire-facing report PLUS the
// /// resolved program (which DetectedDriver deliberately omits — reply.rs pins
// /// spec_used as NOT PATH-resolved; the resolved program stays internal so
// /// SpawnPlan can actually spawn it).
// pub struct Detection {
//     pub report: DetectedDriver,
//     /// Executable chosen by override > scan > known-locations. None = nothing
//     /// resolved (SpawnPlan will use the spec's raw program name as-is, letting
//     /// PATH resolution happen at spawn time as the last resort).
//     pub resolved_program: Option<PathBuf>,
// }
//
// /// Async because step 4 spawns processes via tokio::process::Command with a
// /// timeout; do NOT downgrade to sync without dropping the version probe.
// pub async fn detect(id: DriverId, override_spec: Option<&DriverSpec>) -> Detection;
//     1. Some(spec) = override_spec
//            => probe_version(spec.program, &spec.args) (fire-and-tolerate)
//            => Detection { report: { found: true, version, spec_used: spec.clone() },
//                           resolved_program: resolve_on_path(&spec.program)
//                                              .or_else(|| spec.program contains '/'
//                                                         then Some(program.into()) else None) }
//     2..3. for name in scan_targets(id): check resolve_on_path(name), then each
//           entry of known_locations(id); FIRST executable candidate wins
//           (executable = exists + owner/group/other x-bit set — POSIX mode test,
//           std::os::unix::fs::PermissionsExt; never cfg(windows)).
//           Found => spec_used = DriverSpec { program: name, args: SPAWN_ARGS[id],
//                                             env: {} }; version-probe it.
//     5. Exhausted => spec_used = DriverSpec::default-for(id) (fxproto);
//        found=false, version=None, resolved_program=None.
//
// /// Well-known binary names probed IN ORDER. ClaudeCode deliberately lists only
// /// the adapter binary — see the DECISION block above.
// fn scan_targets(id: DriverId) -> &'static [&'static str];
//   ClaudeCode => ["claude-code-acp"]
//   GeminiCli  => ["gemini"]                 // needs --acp arg at spawn time
//   CodexCli   => ["codex-acp"]
//
// /// Args used both for probing and for the synthesized spec of a scanned hit.
// fn spawn_args(id: DriverId) -> &'static [&'static str];
//   ClaudeCode => [] ; GeminiCli => ["--acp"] ; CodexCli => []
//
// /// nvm/volta/homebrew node bins — checked only if PATH scan misses, because
// /// GUI-spawned servers often have minimal PATH (no nvm sourced).
// fn known_locations(id: DriverId) -> Vec<PathBuf>;
//   ClaudeCode+CodexCli: $NVM_DIR/versions/node/*/bin, ~/.volta/bin,
//                        /opt/homebrew/bin, /usr/local/bin (each + program name;
//                        skip dirs that don't exist — silent, debug!-logged)
//   GeminiCli: same list but for "gemini".
//
// /// `$PROGRAM --version`, capture first line of stdout, VERSION_PROBE_TIMEOUT
// /// then kill. Any failure (spawn err, timeout, empty output) => None. Never
// /// fails hard — absence/unreachability is a normal result here.
// const VERSION_PROBE_TIMEOUT: std::time::Duration = Duration::from_secs(2);
// async fn probe_version(program: &str, args: &[String]) -> Option<String>;
//     implementation note: tokio::process::Command + tokio::time::timeout +
//     child.kill_on_drop(true); trim trailing newline; cap length at 64 chars.
//
// /// Manual `which`: split $PATH on ':', return first dir where
// /// dir.join(program) passes the executable test above. No libc dependency.
// fn resolve_on_path(program: &str) -> Option<PathBuf>;
//
// NOTE: keep all spawn/env handling POSIX (tokio::process::Command); no
// cfg(windows) branches anywhere, ever (docs/architecture.md platform scope).
