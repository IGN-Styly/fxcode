//! Envelope — every frame that crosses the WebSocket is one Message.
//!
//! Handshake flow (all enforced server-side, see fxserver/src/net/handshake.rs):
//!   client → Hello { proto_version, token }
//!   server → Welcome { server_version, head_seq }   |   WS close, reason below
//!   client → Subscribe { last_seq }                  (exactly once, right after Welcome)
//!   server → replay of Event frames, then live attach
//!            (or SnapshotRequired if cursor too far behind head)
//!
//! Failure transport: auth/version/lag failures do NOT get Message variants. The server
//! sends a WebSocket Close frame with one of these exact reason strings, then drops the
//! socket; the client matches on the string for UX:
//!   "auth_failed"       token mismatch
//!   "protocol_version"  proto_version != PROTO_VERSION (also used for any later
//!                       protocol violation, e.g. a second Subscribe mid-session)
//!   "resubscribe"       server dropped a lagging client (fxserver/src/net/client.rs);
//!                       client reconnects and re-Subscribes from its stored cursor
//!
//! Latency checks (M0 badge) use transport-level WS ping/pong frames — no Message
//! variant for that, ever.
//!
//! Correlation: Request carries `id`, a per-connection u64 minted by the CLIENT
//! (ConnectionManager counter starting at 1, monotonically increasing). Response echoes
//! it verbatim. Subscribe/SnapshotRequired/Event never carry an id — they are not
//! request/response traffic.
//!
//! Seq vs correlation id: seq is the GLOBAL log position stamped by the server's event
//! store (typed as ids::Seq); `id` is connection-local bookkeeping (plain u64). They are
//! unrelated numbers; don't conflate them.

// Imports to restore as you define the types:
// use serde::{Deserialize, Serialize};
//
// use crate::command::Command;
// use crate::event::Sequenced;
// use crate::ids::Seq;
// use crate::model::agents::AgentsState;
// use crate::model::perms::PermsState;
// use crate::model::threads::ThreadsState;
// use crate::reply::Reply;

// TODO: define:
//
// pub const PROTO_VERSION: u32 = 1;
//
// #[derive(Serialize, Deserialize, Clone, Debug)]
// #[serde(tag = "type", rename_all = "snake_case")]
// pub enum Message {
//     // handshake (client → server, then server → client once)
//     Hello   { proto_version: u32, token: String },
//     Welcome { server_version: String, head_seq: Seq },
//
//     // steady state
//     Request  { id: u64, command: Command },
//     Response { id: u64, reply: Reply },
//     Event    { event: Sequenced<FxEvent> },
//
//     // resync (handshake only — see flow above; cmd dispatch rejects Command::Subscribe
//     // defensively because subscription is envelope-level, not a Command)
//     Subscribe        { last_seq: Seq },
//     SnapshotRequired { snapshot: Snapshot },
// }
//
// /// Full projection dump for clients too far behind to replay cheaply. Concrete shape —
// /// the three model states, serialized whole:
// #[derive(Serialize, Deserialize, Clone, Debug)]
// pub struct Snapshot {
//     /// Seq of the LAST event already reflected in this snapshot. The client replaces
//     /// all stores with the three fields, sets last_seq = baseline_seq, and folds every
//     /// subsequent Event frame on top. Guarantee: the next Event after this frame has
//     /// seq == baseline_seq + 1 (no gap, no overlap) — replay drained before live,
//     /// same ordering rule as the non-snapshot path (fxserver/src/net/handshake.rs).
//     pub baseline_seq: Seq,
//     pub agents: AgentsState,      // fxproto::model::agents::AgentsState
//     pub threads: ThreadsState,    // fxproto::model::threads::ThreadsState
//     pub perms: PermsState,        // fxproto::model::perms::PermsState
// }
//     ⇒ model states MUST derive Serialize + Deserialize (agents.rs already does; the
//     threads.rs / perms.rs TODOs still need it added, along with their transitive
//     types in content.rs — flagged to those files' owners).
//
// TODO: helper `Message::request(cmd)` auto-assigning ids? No — id assignment belongs to
// the caller (ConnectionManager owns the counter). Keep this crate dumb.
//
// NOTE (doc drift, deliberate): docs/crates.md sketches this enum as tuple variants
// Command(..)/Reply(..)/Event(Sequenced<FxEvent>) without correlation ids. That shape
// cannot satisfy the correlation contract that command.rs/reply.rs require ("pairing is
// by JSON-RPC-style correlation id added at the envelope layer"), so the named-field
// Request/Response forms here supersede it. Update docs/crates.md when convenient.
