//! Replies — exactly one per Command. Errors are data, not transport failures.

// Imports to restore as you define the types:
// use crate::driver::{DriverId, DriverSpec};
// use crate::ids::{AgentId, SessionId, TurnId};

// TODO: define:
//
// /// Wire-level error. Data, not a Rust error: it crosses as Reply::Error and must
// /// round-trip. Derives: Debug, Clone, PartialEq, Serialize, Deserialize. Manual
// /// Display impl ("{code}: {message}") for tracing. Do NOT implement
// /// std::error::Error here — fxcore keeps its own internal thiserror type and
// /// converts to FxError at the Orchestrator::execute boundary (one conversion site).
// pub struct FxError { pub code: FxErrorCode, pub message: String }
//
// #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// #[serde(rename_all = "snake_case")]
// pub enum FxErrorCode {
//     UnknownCommand,        // envelope carried a Command we don't dispatch (future-proofing)
//     ProtocolVersion,       // handshake version mismatch (also sent as close reason)
//     AuthFailed,            // bad pairing token (handshake)
//     AgentNotFound,         // NewSession targeted an unknown/not-running agent id
//     AgentStartFailed,      // StartAgent spawn/initialize failed
//     SessionNotFound,       // Prompt/Cancel for unknown session (projection says so)
//     TurnNotActive,         // Cancel with no running turn; Prompt while turn in flight
//     PermissionNotFound,    // PermissionResponse for unknown/expired request_id
//                            // (cmd/perms.rs respond() demands exactly this)
//     StoreFailure,          // event store append/replay failed
//     Internal,              // catch-all; message carries detail, never panic across wire
// }
//     NOTE no CursorTooOld code: cursor staleness never round-trips as a Reply — the
//     gap policy lives at handshake time and answers with envelope::Message::
//     SnapshotRequired instead of an error.
//
// /// One per Command variant — pairing table lives in command.rs; keep them in sync.
// #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// #[serde(tag = "type", rename_all = "snake_case")]
// pub enum Reply {
//     /// DetectAgents result. found:false rows are data (drives install-hint UI),
//     /// not errors.
//     DetectedAgents(Vec<DetectedDriver>),
//     Started { agent: AgentId },           // ← StartAgent
//     SessionCreated { session: SessionId },// ← NewSession; cwd/mcp ride FxEvent::SessionCreated
//     PromptAccepted { turn: TurnId },      // ← Prompt; actual results arrive as FxEvents
//     Cancelled,                            // ← Cancel (ack; TurnFinished arrives separately)
//     PermissionRecorded,                   // ← PermissionResponse
//     Error(FxError),                       // any command may fail this way
// }
//     NO Subscribed variant: subscribing happens at the envelope/handshake layer
//     (envelope::Message::Subscribe → replay → live attach); head_seq already reaches
//     the client via Welcome { server_version, head_seq } before any command exists.
//
// /// One row of the DetectAgents answer: state of ONE supported driver on the server.
// #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// pub struct DetectedDriver {
//     pub driver: DriverId,
//     pub found: bool,
//     pub version: Option<String>,          // parsed from --version output; None if !found
//                                           // or probe failed/timed out
//     pub spec_used: DriverSpec,            // config override or built-in default — what
//                                           // WOULD be spawned. NOT PATH-resolved: detection's
//                                           // resolved program stays internal to fxcore's
//                                           // SpawnPlan; clients only need launch shape.
// }
//     CANONICAL wire type lives HERE (it crosses the protocol via Reply::DetectedAgents;
//     fxapp views/setup.rs renders it). fxcore/driver/detect.rs imports this — it must
//     NOT redefine its own.
//
// Serde note: Reply uses internally-tagged {"type": "started", ...}; unit variants are
// bare tags ("cancelled", "permission_recorded"); Error nests {"code","message"}.
