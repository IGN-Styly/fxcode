//! Replies — exactly one per Command. Errors are data, not transport failures.

// Imports to restore as you define the types:
// use std::path::PathBuf;              // if replies carry cwd (they don't today)
// use crate::driver::DriverId;
// use crate::ids::{AgentId, Seq, TurnId};   // ← TurnId needed by PromptAccepted
// use crate::content::ContentBlock; // if DetectedDriver needs it

// TODO: define:
//
// pub struct FxError { pub code: FxErrorCode, pub message: String }
// pub enum FxErrorCode {
//     UnknownCommand, ProtocolVersion, AuthFailed,
//     AgentNotFound, AgentStartFailed, SessionNotFound, TurnNotActive,
//     StoreFailure, CursorTooOld /* triggers SnapshotRequired */, Internal,
// }
//
// pub enum Reply {
//     DetectedAgents(Vec<DetectedDriver>),
//     Started { agent: AgentId },
//     SessionCreated { session: SessionId },
//     PromptAccepted { turn: TurnId },      // actual results arrive as FxEvents
//     Cancelled,
//     PermissionRecorded,
//     Subscribed { head_seq: Seq },
//     Error(FxError),
// }
//
// pub struct DetectedDriver {
//     pub driver: DriverId,
//     pub found: bool,
//     pub version: Option<String>,          // parsed from --version output
//     pub spec_used: DriverSpec,            // what would be spawned
// }
//     CANONICAL wire type lives HERE (it crosses the protocol via Reply::DetectedAgents).
//     fxcore/driver/detect.rs imports this — it must NOT redefine its own.
//
// TODO: thiserror on a wrapper if useful; but FxError itself must stay Serialize-able
// (it crosses the wire), so don't implement std::error::Error on it directly — wrap instead.
