//! Per-connection task pair after successful handshake.

// Imports to restore as you implement:
// use std::sync::Arc;
// use axum::extract::ws::{Message as WsFrame, WebSocket};
// use fxcore::Orchestrator;

// TODO:
//
// pub async fn run(orch: Arc<Orchestrator>, ws: WebSocket, auth: AuthedClient) {
//     split ws into sink/stream; two tasks + a select:
//
//     READ loop:
//       Message::Request { id, command } →
//         Subscribe is rejected here (already handled at handshake)
//         orch.execute(command).await → Message::Response { id, reply }
//         (execute serializes through the orchestrator actor — no per-conn state races)
//
//     WRITE loop:
//       initial: replay buffer from handshake, then forward from orch.subscribe()
//       on broadcast::RecvError::Lagged(n) ⇒ we skipped n events for THIS client:
//         send a close w/ Resubscribe reason and drop the connection
//         (cursor makes reconnect cheap — never try to backfill inline)
//
//     teardown: on either side ending, cancel the other; nothing to clean up
//     server-side because all real state lives in the orchestrator.
// }
//
// Backpressure note: tokio mpsc/broadcast between loops must be bounded; if write side
// can't keep up, lag handling above is the escape hatch.
//
// TODO test: two concurrent clients both receive every event exactly once, in order.
