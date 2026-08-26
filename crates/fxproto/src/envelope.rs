//! Envelope — every frame that crosses the WebSocket is one Message.
//!
//! Handshake flow (all enforced server-side, see fxserver/src/net/handshake.rs):
//!   client → Hello { proto_version, token }
//!   server → Welcome { server_version, head_seq }   |   Close(AuthFailed|ProtocolVersion)
//!   client → Subscribe { last_seq }
//!   server → replay of Event frames, then live attach
//!            (or SnapshotRequired if cursor too far behind head)
//!
//! After handshake: client sends Command / server sends Reply + Event(Sequenced<FxEvent>).
//! Correlation: each client-originated frame carries a monotonically increasing `id`;
//! Replies echo it. Events never carry one.

// Imports to restore as you define the types:
// use crate::command::Command;
// use crate::event::Sequenced;
// use crate::reply::Reply;

// TODO: define:
//
// pub const PROTO_VERSION: u32 = 1;
//
// #[serde(tag = "type", rename_all = "snake_case")]
// pub enum Message {
//     // handshake (client → server, then server → client once)
//     Hello   { proto_version: u32, token: String },
//     Welcome { server_version: String, head_seq: u64 },
//
//     // steady state
//     Id(u64)? — no: prefer wrapping:
//     Request  { id: u64, command: Command },
//     Response { id: u64, reply: Reply },
//     Event    { event: Sequenced<FxEvent> },
//
//     // resync
//     Subscribe        { last_seq: u64 },
//     SnapshotRequired { baseline_seq: u64, snapshot: Snapshot },
// }
//
// /// Full projection dump for clients too far behind to replay cheaply.
// /// Concrete shape — the three model states, serialized whole:
// pub struct Snapshot {
//     pub baseline_seq: u64,        // client resets last_seq to this
//     pub agents: AgentsState,      // fxproto::model::agents::AgentsState
//     pub threads: ThreadsState,    // fxproto::model::threads::ThreadsState
//     pub perms: PermsState,        // fxproto::model::perms::PermsState
// }
//     ⇒ model states MUST derive Serialize + Deserialize (add to their TODO derives).
//     Client on receipt: replace all stores with snapshot fields, set last_seq =
//     baseline_seq, then live events continue the fold.
//
// TODO: helper `Message::request(cmd)` auto-assigning ids? No — id assignment belongs to
// the caller (ConnectionManager owns the counter). Keep this crate dumb.
