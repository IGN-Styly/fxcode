//! WebSocket serving layer. One route, per-client task pairs.

pub mod client;
pub mod handshake;

// Imports to restore as you define serve():
// use std::net::SocketAddr;
// use std::sync::Arc;
// use fxcore::Orchestrator;

// TODO:
//
// pub async fn serve(orch: std::sync::Arc<Orchestrator>, addr: SocketAddr) -> anyhow::Result<()> {
//     axum router:
//       GET /ws      → ws upgrade → client::run(orch.clone(), ws)
//       GET /healthz → 200 "ok" (NO auth — for tailscale/service monitors)
//
//   axum::serve with graceful shutdown wired to the same shutdown signal as main.
// }
