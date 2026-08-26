//! fxserver — headless daemon owning all agents + state. Target: thin (~500 lines).
//! If logic wants to live here, it belongs in fxcore instead.

mod ifaddr;
mod net;
mod pair;

// Imports to restore as you implement:
// use std::sync::Arc;
//
// use tokio::signal::unix::{signal, SignalKind};
//
// use fxcore::{Config, Orchestrator};

// TODO: fn main() — tokio runtime built HERE (fxserver owns tokio outright, unlike
// fxapp). Numbered boot sequence; failure at step k => log + exit non-zero + STOP,
// never continue half-booted:
//
//   1. tracing_subscriber init: EnvFilter, default directive "info" when RUST_LOG is
//      unset. No failure path (bad filter strings fall back to default) — proceed.
//   2. CLI args, hand-rolled over std::env for v0 (no clap yet):
//         --config <path>      alternate config toml (default ~/.fxcode/config.toml)
//         --bind <socket>      listen override, wins over config's bind_override
//         --data-dir <path>    state dir override
//         --rotate-token       short-circuit mode, see step 4
//      Unknown flag / missing value => print usage to stderr, exit 2.
//   3. Config::load(): defaults <- config.toml <- CLI overrides. Parse error =>
//      log + exit 2. File absent => pure defaults, log info (first-run is normal).
//   4. --rotate-token short-circuit: pair::rotate_token(&cfg.data_dir), print new
//      token to stderr, exit 0. Store/orchestrator/listener never boot.
//   5. Orchestrator::new(cfg.clone()).await — opens SQLite (WAL), folds the whole
//      log into projections. Failure => log + exit 1. Corrupt store is FATAL by
//      design: never auto-wipe or auto-rebuild (downtime beats silent data loss).
//   6. pair::ensure_token(&cfg.data_dir) — loads existing token or generates +
//      chmod 600 + prints it to stderr ONCE (first boot only; pair.rs owns rules).
//      Failure (dir unwritable / token unreadable / token corrupt) => exit 1;
//      fail-closed, see pair.rs for why corruption never auto-regenerates.
//   7. ifaddr::pick(cfg.bind_override) -> addr. Never fails (loopback floor);
//      log chosen address AND method at info, loudly (which of: bind_override /
//      tailscale-cli / interface-scan / loopback).
//   8. net::serve(Arc::new(orchestrator), addr).await — binds and runs until the
//      shutdown signal (below). Bind failure (port taken, EACCES) => log + exit 1.
//
// Shutdown handling — net::serve takes the orchestrator handle and wires this future
// into axum's graceful-shutdown slot; main just awaits serve():
//   - select! over BOTH tokio::signal::ctrl_c() AND
//     tokio::signal::unix::signal(SignalKind::terminate()) — SIGTERM is what
//     systemd/docker send; Ctrl-C covers interactive dev. Either triggers shutdown.
//   - First signal:
//       a. Stop accepting new WS connections. Existing conns are closed by
//          client.rs teardown with Close(1001 going_away); clients reconnect and
//          replay from their cursors — no per-connection drain ceremony here.
//       b. orchestrator.shutdown().await: stop accepting commands, SIGTERM every
//          agent child, wait GRACE = 5_000 ms, SIGKILL survivors, final WAL
//          checkpoint on the store. (5s: agents acknowledge ACP cancel/exit fast;
//          anything still alive after 5s is wedged and killing it is correct.)
//       c. flush + exit 0.
//   - Second signal while shutting down: abort immediately, exit 130. Implement by
//     re-awaiting either signal stream; skip remaining cleanup.
//
// TODO tests: none in this file — pure orchestration. Covered via pair.rs/ifaddr.rs
// units and net/ end-to-end (crates.md test table).
fn main() {
    // scaffold: fill in per TODO above
}
