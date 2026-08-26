//! Strongly-typed identifiers.
//!
//! Conventions:
//! - Clone newtypes wrapping a single String; Seq wraps u64 and is also Copy + Ord.
//! - Serialize transparently: bare string/u64 on the wire, no `{ "AgentId": ... }`.
//! - Derives: Clone, PartialEq, Eq, Hash, Serialize, Deserialize (+ Debug);
//!   Seq additionally derives Copy, PartialOrd, Ord.
//! - Display impls forward to inner value (nice for tracing/logs).
macro_rules! string_id {
    ($name:ident) => {
        // PartialOrd + Ord: BTreeMap keys in model projections (threads/agents
        // keyed by uuid v7 ids, lexicographic == creation order).
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn from_raw(inner: String) -> Self {
                Self(inner)
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
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

use std::fmt::Display;

use serde::{Deserialize, Serialize, Serializer};

string_id!(AgentId);
string_id!(SessionId);
string_id!(TurnId);
string_id!(ToolCallId);
string_id!(RequestId);
string_id!(OptionId);
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(u64);
impl Seq {
    /// Constructor for the two legitimate builders ONLY: fxcore IdGen/EventStore
    /// stamping fresh seqs and replay/rehydration reading them back from SQLite.
    /// Everything else treats Seq as opaque and reads it via as_u64().
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }
    // cursor persistence stores plain u64
    pub fn as_u64(self) -> u64 {
        self.0
    }
}
impl Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.serialize_u64(self.0)
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    #[test]
    fn seq_display_and_wire() {
        let s = Seq(42);
        assert_eq!(format!("{s}"), "42");
        assert_eq!(serde_json::to_string(&s).unwrap(), "42"); // bare u64, no wrapper
        let back: Seq = serde_json::from_str("42").unwrap();
        assert_eq!(back.as_u64(), 42);
    }
    #[test]
    fn agent_id_display_and_wire() {
        let id = AgentId::from_raw("abc".into());
        assert_eq!(format!("{id}"), "abc");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"abc\""); // bare string
        let back: AgentId = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(back.as_str(), "abc");
    }
}
