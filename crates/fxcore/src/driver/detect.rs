//! Find installed agent binaries. Linux + macOS only by design.
//!
//! Strategy per driver:
//!   1. Config override (already merged into registry) wins outright.
//!   2. PATH scan for the well-known program name.
//!   3. Known install locations fallbacks (nvm/volta node bins for npx-based drivers).
//!   4. If found, run `<prog> --version` (or equivalent) with a short timeout and
//!      parse the version string for display purposes only.

// Imports to restore as you implement:
// use std::path::PathBuf;
// use fxproto::driver::{DriverId, DriverSpec};
// use fxproto::reply::DetectedDriver;   // CANONICAL type — defined in fxproto, not here
//
// pub fn detect(id: DriverId, override_spec: Option<&DriverSpec>) -> DetectedDriver;
//     (resolved_program from detection feeds DriverRegistry's SpawnPlan internally)
//
// Per-driver well-known names:
//   ClaudeCode → "claude-code-acp"; also accept bare `claude` if it supports ACP flags?
//               (verify against @agentclientprotocol/claude-agent-acp docs when implementing)
//   GeminiCli  → "gemini" (needs --acp flag at spawn time)
//   CodexCli   → "codex-acp" (Zed's adapter)
//
// TODO: version probing helper: run program with args, capture stdout first line,
// timeout ~2s, kill on hang. Never fail hard — absence is a normal result.
//
// NOTE: keep all spawn/env handling POSIX (tokio::process::Command); no cfg(windows)
// branches anywhere, ever (docs/architecture.md platform scope).
