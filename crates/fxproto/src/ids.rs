//! Strongly-typed identifiers.
//!
//! Conventions:
//! - Copy newtypes wrapping a single String (or u64 for Seq).
//! - Serialize transparently: bare string on the wire, no `{ "AgentId": ... }`.
//! - Derives to aim for: Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize.
//! - Generated server-side only; clients treat ids as opaque.
//! - Display impls forward to inner value (nice for tracing/logs).

// TODO: define:
//
// pub struct AgentId(String);        // one agent process instance
// pub struct SessionId(String);      // one chat thread (mirrors ACP sessionId)
// pub struct TurnId(String);         // one prompt→stopReason cycle within a session
// pub struct ToolCallId(String);     // mirrors ACP toolCallId; key for UI upserts
// pub struct RequestId(String);      // our id for a pending permission request
// pub struct OptionId(String);       // permission option chosen (mirrors ACP optionId)
//
// pub struct DriverId / enum DriverId — see `driver.rs`; keep the id type there.
//
// pub struct Seq(pub u64);           // global monotonic event sequence, stamped by the store
//
// TODO: decide + implement id generation (uuid v7 suggested: sortable, no deps beyond uuid).
// Server assigns; constructors take Strings and do NOT validate format (trust the server).
