//! fxserver — headless daemon owning all agents + state. Target: thin (~500 lines).
//! If logic wants to live here, it belongs in fxcore instead.

mod ifaddr;
mod net;
mod pair;

// TODO: fn main() boot sequence —
//   1. tracing_subscriber init (env filter RUST_LOG=info default)
//   2. Config::load()
//   3. Orchestrator::new(cfg).await  (opens SQLite, rebuilds projections)
//   4. pair::ensure_token(&cfg.data_dir) → prints to stderr on FIRST boot only
//   5. let addr = ifaddr::pick(&cfg)  → log loudly which addr/interface was chosen
//   6. net::serve(orchestrator, addr).await
//   7. graceful shutdown: SIGTERM / Ctrl-C → orchestrator.shutdown() (SIGTERM children,
//      grace window, SIGKILL), flush store, exit 0.
//
// TODO: CLI args (clap? or keep hand-rolled for v0): --config path, --bind addr,
//       --rotate-token, --data-dir override.
fn main() {
    // scaffold: fill in per TODO above
}
