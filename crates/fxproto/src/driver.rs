//! Driver ids and spawn specifications.
//!
//! A "driver" = a supported coding agent (claude-code-acp, gemini --acp, codex-acp).
//! All v0 drivers speak ACP; the registry in fxcore maps DriverId → how to spawn it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Ord is REQUIRED: fxcore keys BTreeMaps by DriverId (config overrides, detection
/// cache) — see fxcore/src/config.rs and fxcore/src/driver/mod.rs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverId {
    ClaudeCode,
    GeminiCli,
    CodexCli,
}

impl DriverId {
    /// UI display string. EXACT strings (sidebar agent rows).
    pub fn label(self) -> &'static str {
        match self {
            DriverId::ClaudeCode => "Claude Code",
            DriverId::GeminiCli => "Gemini CLI",
            DriverId::CodexCli => "Codex CLI",
        }
    }
}

/// HOW to spawn the agent binary. `program` is resolved via PATH unless it contains
/// a path separator. Spec resolution precedence (fxcore/src/config.rs + detect.rs):
///   config override  >  autodetect result (PATH/known-locations probe)  >  the
///   per-driver [`DriverId::default_spec`] below. That is therefore a LAST RESORT
///   that must be spawnable on a clean machine — not what detect.rs scans PATH for.
///
/// PartialEq/Eq are not in the crates.md sketch but are required transitively:
/// reply.rs `DetectedDriver` (which embeds a DriverSpec) derives PartialEq for
/// golden tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverSpec {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl DriverId {
    /// Per-driver LAST-RESORT default specs (env always empty):
    /// - ClaudeCode → npx package form so it works with zero local install. detect.rs
    ///   separately probes PATH for a native `claude-code-acp` binary first (args []
    ///   when found); the npx default only fires when neither config nor autodetect
    ///   produced one.
    /// - GeminiCli → matches detect.rs scan target "gemini".
    /// - CodexCli → matches detect.rs scan target "codex-acp".
    // TODO(pre-M1): verify exact invocations against each agent's docs before the M1 exit check.
    pub fn default_spec(self) -> DriverSpec {
        let (program, args) = match self {
            DriverId::ClaudeCode => (
                "npx",
                vec![
                    "-y".to_string(),
                    "@agentclientprotocol/claude-agent-acp".to_string(),
                ],
            ),
            DriverId::GeminiCli => ("gemini", vec!["--acp".to_string()]),
            DriverId::CodexCli => ("codex-acp", Vec::new()),
        };
        DriverSpec {
            program: program.into(),
            args,
            env: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_exact() {
        assert_eq!(DriverId::ClaudeCode.label(), "Claude Code");
        assert_eq!(DriverId::GeminiCli.label(), "Gemini CLI");
        assert_eq!(DriverId::CodexCli.label(), "Codex CLI");
    }

    #[test]
    fn wire_is_bare_snake_case_and_defaults_are_per_id() {
        assert_eq!(
            serde_json::to_string(&DriverId::GeminiCli).unwrap(),
            "\"gemini_cli\""
        );
        let claude = DriverId::ClaudeCode.default_spec();
        assert_eq!(claude.program, "npx");
        assert_eq!(
            claude.args,
            vec!["-y", "@agentclientprotocol/claude-agent-acp"]
        );
        assert!(claude.env.is_empty());
        assert_eq!(DriverId::GeminiCli.default_spec().program, "gemini");
        assert_eq!(DriverId::CodexCli.default_spec().program, "codex-acp");
    }

    #[test]
    fn spec_round_trips_with_optional_fields_defaulted() {
        let json = r#"{"program":"claude-code-acp"}"#;
        let spec: DriverSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.args, Vec::<String>::new());
        assert!(spec.env.is_empty());
        // Compact re-serialization fills in all fields (args/env are plain defaults,
        // no skip_serializing_if), so compare VALUES across the trip.
        assert_eq!(
            serde_json::from_str::<DriverSpec>(&serde_json::to_string(&spec).unwrap()).unwrap(),
            spec
        );
    }
}
