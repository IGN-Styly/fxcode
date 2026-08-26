//! Replies — exactly one per Command. Errors are data, not transport failures.

use serde::{Deserialize, Serialize};

use crate::driver::{DriverId, DriverSpec};
use crate::ids::{AgentId, SessionId, TurnId};

/// Wire-level error. Data, not a Rust error: it crosses as Reply::Error and must
/// round-trip. Derives: Debug, Clone, PartialEq, Serialize, Deserialize. Manual
/// Display impl ("{code}: {message}") for tracing. Do NOT implement
/// std::error::Error here — fxcore keeps its own internal thiserror type and
/// converts to FxError at the Orchestrator::execute boundary (one conversion site).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxError {
    pub code: FxErrorCode,
    pub message: String,
}

impl std::fmt::Display for FxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FxErrorCode {
    UnknownCommand,     // envelope carried a Command we don't dispatch (future-proofing)
    ProtocolVersion,    // handshake version mismatch (also sent as close reason)
    AuthFailed,         // bad pairing token (handshake)
    AgentNotFound,      // NewSession targeted an unknown/not-running agent id
    AgentStartFailed,   // StartAgent spawn/initialize failed
    SessionNotFound,    // Prompt/Cancel for unknown session (projection says so)
    TurnNotActive,      // Cancel with no running turn; Prompt while turn in flight
    PermissionNotFound, // PermissionResponse for unknown/expired request_id
    // (cmd/perms.rs respond() demands exactly this)
    StoreFailure, // event store append/replay failed
    Internal,     // catch-all; message carries detail, never panic across wire
}
//     NOTE no CursorTooOld code: cursor staleness never round-trips as a Reply — the
//     gap policy lives at handshake time and answers with envelope::Message::
//     SnapshotRequired instead of an error.

impl FxErrorCode {
    /// Stable tracing/UX string == wire string (snake_case), single source via serde.
    pub fn as_str(self) -> &'static str {
        match self {
            FxErrorCode::UnknownCommand => "unknown_command",
            FxErrorCode::ProtocolVersion => "protocol_version",
            FxErrorCode::AuthFailed => "auth_failed",
            FxErrorCode::AgentNotFound => "agent_not_found",
            FxErrorCode::AgentStartFailed => "agent_start_failed",
            FxErrorCode::SessionNotFound => "session_not_found",
            FxErrorCode::TurnNotActive => "turn_not_active",
            FxErrorCode::PermissionNotFound => "permission_not_found",
            FxErrorCode::StoreFailure => "store_failure",
            FxErrorCode::Internal => "internal",
        }
    }
}

/// One per Command variant — pairing table lives in command.rs; keep them in sync.
///
/// SERDE DEVIATION (documented): the crates.md/reply.rs sketch had tuple variants
/// `DetectedAgents(Vec<DetectedDriver>)`. Internally-tagged enums CANNOT serialize
/// newtype variants holding sequences (serde inserts the tag into a map/struct only),
/// so both are struct variants here:
///   DetectedAgents { drivers } → {"type":"detected_agents","drivers":[…]}
///   Error(err)                 → {"type":"error", …FxError fields flattened}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// DetectAgents result. found:false rows are data (drives install-hint UI),
    /// not errors.
    DetectedAgents {
        drivers: Vec<DetectedDriver>,
    },
    Started {
        agent: AgentId,
    }, // ← StartAgent
    SessionCreated {
        session: SessionId,
    }, // ← NewSession; cwd/mcp ride FxEvent::SessionCreated
    PromptAccepted {
        turn: TurnId,
    }, // ← Prompt; actual results arrive as FxEvents
    Cancelled,          // ← Cancel (ack; TurnFinished arrives separately)
    PermissionRecorded, // ← PermissionResponse
    Error(FxError),     // any command may fail this way
}
//     NO Subscribed variant: subscribing happens at the envelope/handshake layer
//     (envelope::Message::Subscribe → replay → live attach); head_seq already reaches
//     the client via Welcome { server_version, head_seq } before any command exists.

/// One row of the DetectAgents answer: state of ONE supported driver on the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectedDriver {
    pub driver: DriverId,
    pub found: bool,
    /// parsed from --version output; None if !found or probe failed/timed out
    pub version: Option<String>,
    /// config override or built-in default — what WOULD be spawned. NOT PATH-resolved:
    /// detection's resolved program stays internal to fxcore's SpawnPlan; clients only
    /// need launch shape.
    pub spec_used: DriverSpec,
}
//     CANONICAL wire type lives HERE (it crosses the protocol via Reply::DetectedAgents;
//     fxapp views/setup.rs renders it). fxcore/driver/detect.rs imports this — it must
//     NOT redefine its own.

// Serde note: Reply uses internally-tagged {"type": "started", ...}; unit variants are
// bare tags ("cancelled", "permission_recorded"); Error nests {"code","message"}.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_flattens_fields_and_display_matches_wire() {
        let e = FxError {
            code: FxErrorCode::SessionNotFound,
            message: "no such session".into(),
        };
        assert_eq!(e.to_string(), "session_not_found: no such session");
        let reply = Reply::Error(e.clone());
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(
            json,
            r#"{"type":"error","code":"session_not_found","message":"no such session"}"#
        );
        assert_eq!(serde_json::from_str::<Reply>(&json).unwrap(), reply);
    }

    #[test]
    fn detected_agents_carries_rows_as_field() {
        let rows = vec![
            DetectedDriver {
                driver: DriverId::GeminiCli,
                found: true,
                version: Some("0.9".into()),
                spec_used: DriverId::GeminiCli.default_spec(),
            },
            DetectedDriver {
                driver: DriverId::CodexCli,
                found: false,
                version: None,
                spec_used: DriverId::CodexCli.default_spec(),
            },
        ];
        let json = serde_json::to_value(Reply::DetectedAgents { drivers: rows }).unwrap();
        assert_eq!(json["type"], "detected_agents");
        assert_eq!(json["drivers"].as_array().unwrap().len(), 2);
        assert_eq!(json["drivers"][0]["spec_used"]["program"], "gemini");
    }

    #[test]
    fn unit_replies_are_bare_tags() {
        assert_eq!(
            serde_json::to_string(&Reply::Cancelled).unwrap(),
            r#"{"type":"cancelled"}"#
        );
        assert_eq!(
            serde_json::to_string(&Reply::PermissionRecorded).unwrap(),
            r#"{"type":"permission_recorded"}"#
        );
    }
}
