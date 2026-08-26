//! WebSocket serving layer. One route, per-client task pairs.

pub mod client;
pub mod handshake;

// Imports to restore as you define serve():
// use std::net::SocketAddr;
// use std::sync::Arc;
//
// use axum::routing::get;
// use fxcore::Orchestrator;

// TODO:
//
// pub async fn serve(orch: Arc<Orchestrator>, addr: SocketAddr) -> anyhow::Result<()> {
//     axum router:
//       GET /ws      → ws upgrade → handshake::run(orch.clone(), ws) → client::run(...)
//                      (handshake owns auth + replay/snapshot; client.rs only sees
//                       post-auth traffic — see handshake.rs for the split)
//       GET /healthz → 200 "ok" (NO auth — for tailscale/service monitors)
//
//     axum::serve(...).with_graceful_shutdown(shutdown_signal(orch.clone())):
//       shutdown_signal = select! { ctrl_c() | SIGTERM } (exact recipe lives in
//       main.rs steps "Shutdown handling"); on fire:
//         1. stop accepting new conns (axum does this),
//         2. orchestrator.shutdown().await — SIGTERM children, 5_000 ms grace,
//            SIGKILL survivors, WAL checkpoint,
//       In-flight /ws connections: each client task watches a shutdown
//       CancellationToken (tokio_util) shared from here; on cancel the writer sends
//       Close(1001 going_away) and both loops exit (client.rs teardown matrix).
// }
//
// Route count is FINAL for v0: no metrics, no admin endpoints. Anything that wants a
// route belongs in fxcore behind a command first.
