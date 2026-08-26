//! Driver ids and spawn specifications.
//!
//! A "driver" = a supported coding agent (claude-code-acp, gemini --acp, codex-acp).
//! All v0 drivers speak ACP; the registry in fxcore maps DriverId → how to spawn it.

// use serde::{Deserialize, Serialize};  ← restore when defining DriverId/DriverSpec
//
// #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum DriverId {
//     ClaudeCode,
//     GeminiCli,
//     CodexCli,
// }
//     impl: label() -> &str for UI display.
//
// /// HOW to spawn the agent binary. Defaults per driver come from fxcore's detect.rs;
// /// users override via ~/.fxcode/config.toml. `program` is resolved via PATH unless
// /// it contains a path separator.
// #[derive(Clone, Serialize, Deserialize)]
// pub struct DriverSpec {
//     pub program: String,
//     pub args: Vec<String>,
//     #[serde(default)]
//     pub env: BTreeMap<String, String>,
// }
//
// TODO: Default impls per DriverId:
//   ClaudeCode → ("npx", ["-y", "@agentclientprotocol/claude-agent-acp"], {})
//   GeminiCli  → ("gemini", ["--acp"], {})
//   CodexCli   → ("codex-acp", [], {})
// (Verify exact invocations against each agent's docs when implementing.)
