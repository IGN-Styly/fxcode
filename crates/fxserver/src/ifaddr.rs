//! Choose the listen address.
//!
//! Priority (docs/crates.md) — first match wins, every outcome logged with its method:
//!   1. cfg.bind_override (explicit --bind / config.toml)
//!   2. tailscale IP via `tailscale ip -4` (CLI probe)
//!   3. tailscale IP via interface scan (any addr in 100.64.0.0/10 — CGNAT range)
//!   4. loopback 127.0.0.1
//!
//! REMINDER: we never JOIN anything — host's tailscaled owns membership; we bind to
//! what it provides.

// Imports to restore as you implement:
// use std::net::{IpAddr, SocketAddr};
// use std::time::Duration;
//
// use tokio::process::Command;      // tokio Timer used for the CLI timeout
// use if_addrs::get_if_addrs;       // NEW DEP for fxserver (see FLAG at bottom)

// TODO:
//
// /// Single source of truth for the port when only an IP is known (steps 2–4).
// /// 8949 is arbitrary but MUST stay stable: clients type/persist full URLs against it.
// pub const DEFAULT_PORT: u16 = 8949;
//
// /// Which step of the priority chain produced an answer — returned by pick() so
// /// main.rs can log it loudly, and unit-testable without touching the network.
// #[derive(Clone, Copy, Debug, PartialEq, Eq)]
// pub enum Method { BindOverride, TailscaleCli, InterfaceScan, Loopback }
//
// /// NEVER fails: the chain bottoms out at loopback. Tailscale absence is a logged
// /// degradation, not an error (a laptop without tailnet must still serve localhost).
// pub fn pick(cfg_bind_override: Option<SocketAddr>) -> (SocketAddr, Method);
//   match cfg_bind_override {
//       Some(a) => (a, Method::BindOverride),          // taken verbatim, port included
//       None    => via_cli()                            // step 2
//           .or_else(via_interfaces)                    // step 3
//           .map_or_else(                                // step 4 + warn! "no tailnet,
//               || (SocketAddr::from(([127,0,0,1], DEFAULT_PORT)), Method::Loopback),
//                                                        //  remote clients unavailable"
//               |ip| (SocketAddr::new(ip, DEFAULT_PORT), method_for(ip)),
//   }
//   (method_for: re-derive which helper hit, or thread it through; keep classify pure.)
//
// helpers:
//   fn via_cli() -> Option<IpAddr>
//     Spawn `tailscale ip -4`; await with 1_000 ms timeout; parse FIRST stdout line as
//     IPv4 (trim). Tailscale ABSENT (ENOENT on spawn), non-zero exit, timeout, or
//     unparseable output => None each, tracing::debug! which one (absence is normal,
//     not a warning). One attempt, no retry.
//
//   fn via_interfaces() -> Option<IpAddr>
//     if_addrs::get_if_addrs(); return first IP in 100.64.0.0/10. Range check WITHOUT
//     a cidr crate: CGNAT = octets [100, 64..=127, *, *] i.e.
//     o[0] == 100 && (64..=127).contains(&o[1]). First match in crate enumeration
//     order wins (deterministic enough; log which interface name).
//
// TODO test: pure fn classify(override: Option<SocketAddr>, cli: Option<IpAddr>,
// scan: Option<IpAddr>) -> Method pinning the whole priority chain as a table:
//   Some(_) => BindOverride   (even when cli/scan would answer — explicit wins)
//   None + cli => TailscaleCli
//   None + no-cli + scan => InterfaceScan
//   all None => Loopback
// IO fns above stay thin wrappers around these results.
//
// FLAG (not fixable here): needs `if-addrs` added to crates/fxserver/Cargo.toml +
// workspace deps — file outside this scaffold's edit scope; add at implementation time.
