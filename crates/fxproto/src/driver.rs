//! Driver ids and spawn specifications.
//!
//! A "driver" = a supported coding agent (claude-code-acp, gemini --acp, codex-acp).
//! All v0 drivers speak ACP; the registry in fxcore maps DriverId → how to spawn it.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
//
// use serde::{Deserialize, Serialize};

// TODO: define:
//
// /// Ord is REQUIRED: fxcore keys BTreeMaps by DriverId (config overrides, detection
// /// cache) — see fxcore/src/config.rs and fxcore/src/driver/mod.rs.
// #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum DriverId {
//     ClaudeCode,
//     GeminiCli,
//     CodexCli,
// }
//     impl: label() -> &'static str for UI display, EXACT strings (sidebar agent rows):
//       ClaudeCode => "Claude Code"
//       GeminiCli  => "Gemini CLI"
//       CodexCli   => "Codex CLI"
//
// /// HOW to spawn the agent binary. `program` is resolved via PATH unless it contains
// /// a path separator. Spec resolution precedence (fxcore/src/config.rs + detect.rs):
// ///   config override  >  autodetect result (PATH/known-locations probe)  >  the
// ///   per-driver Default below. The Default is therefore a LAST RESORT that must be
// ///   spawnable on a clean machine — not what detect.rs scans PATH for.
// #[derive(Clone, Debug, Serialize, Deserialize)]
// pub struct DriverSpec {
//     pub program: String,
//     #[serde(default)]
//     pub args: Vec<String>,
//     #[serde(default)]
//     pub env: BTreeMap<String, String>,
// }
//
// TODO: Default impls per DriverId (env always empty; these are the fallback specs):
//   ClaudeCode → ("npx", ["-y", "@agentclientprotocol/claude-agent-acp"], {})
//       npm-package form so it works with zero local install. detect.rs separately
//       probes PATH for a native `claude-code-acp` binary first (args [] when found);
//       the npx default only fires when neither config nor autodetect produced one.
//   GeminiCli  → ("gemini", ["--acp"], {})        matches detect.rs scan target "gemini"
//   CodexCli   → ("codex-acp", [], {})            matches detect.rs scan target "codex-acp"
// (Verify exact invocations against each agent's docs before the M1 exit check.)
