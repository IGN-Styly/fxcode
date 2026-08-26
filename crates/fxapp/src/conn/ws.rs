//! THE ONLY FILE THAT KNOWS TOKIO EXISTS (docs/crates.md rule).
//!
//! Owns a small embedded tokio Runtime running async-tungstenite; bridges frames to
//! GPUI's executor via channels. If GPUI-native async ever suffices, only this file dies.

use futures::{SinkExt, StreamExt};

// TODO:
//
// pub enum Frame { Out(fxproto::envelope::Message), In(...) }  // or just Message both ways
//
// pub struct WsHandle {
//     out_tx: channel::Sender<Message>,        // main → runtime task
//     in_rx: channel::Receiver<Message>,       // runtime task → main
//     close: ...                               // graceful shutdown signal
// }
//
// impl WsHandle {
//     /// Connect + return handle. DNS/TCP/WS/handshake errors surface as Err here;
//     /// protocol-level auth failures arrive as an In frame (Close reason) instead.
//     pub fn connect(url: &str) -> Result<Self>;
// }
//
// Runtime internals (inside tokio::runtime::Builder::new_multi_thread().enable_all()):
//   - async_tungstenite::connect_async(url)
//   - split sink/stream; pump loops both directions with bounded channels
//   - ping/pong keepalive + idle timeout → treat as disconnect, report to conn/mod.rs
//
// TODO decide channel flavor for the bridge: std mpsc polled from GPUI timers vs
// smol/flume vs futures channel w/ GPUI background tasks. Constraint: whatever the
// GPUI side awaits must run on GPUI's executor, NOT inside tokio.
