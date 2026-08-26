//! ConnectionManager entity: owns the server connection lifecycle.
//!
//! Responsibilities:
//! - handshake (Hello/Welcome), token from connect screen or client-state
//! - send Command with correlation ids; route Reply to the awaiting caller
//! - receive Events → hand to AppState folds → bump last_seq cursor → notify stores
//! - reconnect loop w/ exponential backoff; on Ready re-Subscribe from stored last_seq
//!   (SnapshotRequired ⇒ clear projections, refold from snapshot)

pub mod cursor;
pub mod ws;

use fxproto::command::Command;
use fxproto::reply::Reply;

// TODO:
//
// #[derive(Clone, PartialEq)]
// pub enum ConnStatus { Disconnected, Connecting { attempt: u32 }, Ready }
//
// pub struct ConnectionManager {
//     status: ConnStatus,
//     cmd_tx: Option<ws::CmdSender>,       // live while Ready
//     next_id: u64,                        // correlation counter (client-side only)
//     pending: HashMap<u64, oneshot::Sender<Reply>>,   // awaiting responses
//     url: String, token: String,
// }
//
// impl ConnectionManager {
//     pub fn spawn(cx: &mut App, url: String, token: String) -> Entity<Self>;
//     // spawn = create entity + start background task (GPUI cx.spawn / background executor)
//     // that pumps ws.rs frames into this entity via cx.update.
//
//     pub async fn send(&mut self, cmd: Command) -> Result<Reply>;  // correlates id
//
//     // internal: on_event(seq'd ev) → AppState::apply(ev); persist cursor; cx.notify()
//     // internal: on_status(s) → update + notify observers (views show banner/badge)
// }
