//! Strongly-typed identifiers.
//!
//! Conventions:
//! - Copy newtypes wrapping a single String (or u64 for Seq).
//! - Serialize transparently: bare string on the wire, no `{ "AgentId": ... }`.
//! - Derives: Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize (+ Debug).
//! - Display impls forward to inner value (nice for tracing/logs).

// Imports to restore as you define the types:
// use serde::{Deserialize, Serialize};   // derive macros for every newtype below

// WHO MINTS WHAT — read before implementing anything that creates ids:
//
//   AgentId    MINTED BY US (fxcore IdGen) when StartAgent succeeds.
//   SessionId  ADOPTED from the agent: ACP session/new RETURNS a sessionId and we
//              reuse it verbatim as ours. Never generate one. This keeps future
//              session/load-resume a straight passthrough.
//   TurnId     MINTED BY US per Prompt (ACP has no turn concept; turns are implicit
//              prompt→stopReason cycles — we make them addressable).
//   ToolCallId ADOPTED from the agent (arrives in tool_call / tool_call_update).
//              Never generate; upsert semantics depend on agent-side stability.
//   RequestId  MINTED BY US when normalize.rs converts an inbound request_permission.
//   OptionId   ADOPTED from the agent: arrives inside request_permission's options list;
//              echoed verbatim in Command::PermissionResponse. Never generate.
//   Seq        NOT an id we mint at all: stamped exclusively by EventStore::append
//              (single SQLite AUTOINCREMENT source of truth). See RULES below.
//
// ALGORITHM (minted ids only): uuid v7 (druid crate) — time-ordered so
// log/BTreeMap iteration reads chronologically, and stateless (no generator struct needed
// in production code paths). The druid dep lives in fxcore, NOT here (see Cargo.toml rule).
//
// RULES:
// - Generation NEVER happens in fxproto. Constructors take Strings, validate nothing,
//   trust the caller (server assigns; clients treat ids as opaque).
// - The druid crate must NOT be added to fxproto's Cargo.toml: this crate is pure serde
//   types; minting is fxcore's job (crates/fxcore/src/ids.rs IdGen).

// TODO: define the six id newtypes (all EXACTLY this shape):
//
// #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
// #[serde(transparent)] // bare string on the wire, no {"AgentId": ...}
// pub struct AgentId(String);
//     ...same pattern for SessionId, TurnId, ToolCallId, RequestId, OptionId.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(String);
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(String);
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(String);
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OptionId(String);

// Uniform API per id type (private inner field keeps clients from fabricating ids
// by accident while still allowing reads for ACP calls and logging):
//
//     impl AgentId {
//         /// ONLY constructor. Call sites: fxcore IdGen (minted ids) and ACP
//         /// adoption sites (SessionId, ToolCallId, OptionId). Nothing else.
//         pub fn from_raw(inner: String) -> Self;
//         /// For passing to the agent SDK / tracing without cloning.
//         pub fn as_str(&self) -> &str;
//     }
//     impl std::fmt::Display for AgentId { /* forwards to inner */ }
//
// Seq differs only in payload type and one extra accessor; it also derives ordering
// because cursors compare seqs everywhere (replay gap checks, lag detection):
//
// #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
//          Serialize, Deserialize)]
// #[serde(transparent)]
// pub struct Seq(u64);
//     impl Seq { pub fn as_u64(self) -> u64; }    // cursor persistence stores plain u64
//     impl Display as above.
//
// Wire note: #[serde(transparent)] on ALL of them — golden fixtures must contain bare
// strings/u64, never newtype wrappers.
