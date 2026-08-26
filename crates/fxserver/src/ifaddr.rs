//! Choose the listen address.
//!
//! Priority (docs/crates.md):
//!   1. cfg.bind_override (explicit --bind / config.toml)
//!   2. tailscale IP via `tailscale ip -4` (CLI)
//!   3. tailscale IP via interface scan (any addr in 100.64.0.0/10 — CGNAT range)
//!   4. loopback 127.0.0.1
//!
//! REMINDER: we never JOIN anything — host's tailscaled owns membership; we bind to
//! what it provides. Log the chosen address + method loudly either way.

// TODO:
//
// pub fn pick(cfg_bind_override: Option<SocketAddr>) -> SocketAddr;
//
// helpers:
//   fn via_cli() -> Option<IpAddr>      // run `tailscale ip -4`, timeout ~1s, parse first line
//   fn via_interfaces() -> Option<IpAddr>  // enumerate ifs (nix/if-addrs crate or std?
//                                          // pick a small crate: `if-addrs` is fine),
//                                          // match 100.64.0.0/10
//
// TODO test: pure fn classify(addr) -> Method for the priority chain (unit-testable
// without network); IO fns stay thin wrappers.
