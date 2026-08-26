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
//!
//! Structure note: [`classify`] is the PURE decision core (unit-testable matrix
//! below); [`via_cli`] / [`via_interfaces`] are thin IO wrappers whose results are
//! threaded back through classify so prod and tests share one chain.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use if_addrs::get_if_addrs;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Single source of truth for the port when only an IP is known (steps 2–4).
/// 8949 is arbitrary but MUST stay stable: clients type/persist full URLs against it.
pub const DEFAULT_PORT: u16 = 8949;

/// Best-effort budget for the `tailscale` CLI probe. One attempt, no retry:
/// absence/degradation falls through to the interface scan, never errors.
/// (~500ms: a cold tailscaled CLI is usually <50ms; anything slower is degraded.)
const CLI_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Which step of the priority chain produced an answer — returned by pick() so
/// main.rs can log it loudly, and unit-testable without touching the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    BindOverride,
    TailscaleCli,
    InterfaceScan,
    Loopback,
}

/// CGNAT range check WITHOUT a cidr crate: Tailscale hands out 100.64.0.0/10,
/// i.e. octets [100, 64..=127, *, *]. IPv6 never qualifies (the loopback floor
/// and explicit overrides are the only non-v4 answers).
pub fn is_cgnat(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (64..=127).contains(&o[1])
        }
        IpAddr::V6(_) => false,
    }
}

/// PURE priority-chain decision over already-fetched candidates. The table this
/// pins (v4/v6 normalization = identity comparison on parsed [`IpAddr`]s):
///   Some(override)         => BindOverride     (even when cli/scan would answer)
///   None + cli Some        => TailscaleCli
///   None + no-cli + scan S => InterfaceScan    (only if S is a real CGNAT hit;
///                                              v6 or any other scan result
///                                              normalizes to "no candidate")
///   all None               => Loopback
pub fn classify(
    bind_override: Option<SocketAddr>,
    cli: Option<IpAddr>,
    scan: Option<IpAddr>,
) -> Method {
    if bind_override.is_some() {
        return Method::BindOverride;
    }
    let cgnat_cli = cli.filter(is_cgnat);
    let cgnat_scan = scan.filter(is_cgnat);
    if cgnat_cli.is_some() {
        Method::TailscaleCli
    } else if cgnat_scan.is_some() {
        Method::InterfaceScan
    } else {
        Method::Loopback
    }
}

/// NEVER fails: the chain bottoms out at loopback. Tailscale absence is a logged
/// degradation, not an error (a laptop without tailnet must still serve localhost).
/// Logs the chosen address AND method at info, loudly.
pub async fn pick(cfg_bind_override: Option<SocketAddr>) -> (SocketAddr, Method) {
    // IO once per helper; classify owns ALL branching on the results.
    let cli = via_cli().await;
    let scan = via_interfaces();
    let method = classify(cfg_bind_override, cli, scan);

    let addr = match method {
        // Taken verbatim, port included — validation happened in Config parse.
        Method::BindOverride => cfg_bind_override.expect("classify matched override"),
        Method::TailscaleCli => {
            SocketAddr::new(cli.filter(is_cgnat).expect("cli hit"), DEFAULT_PORT)
        }
        Method::InterfaceScan => {
            SocketAddr::new(scan.filter(is_cgnat).expect("scan hit"), DEFAULT_PORT)
        }
        Method::Loopback => {
            warn!(
                "no tailnet found (CLI absent + no 100.64.0.0/10 interface); \
                 remote clients unavailable"
            );
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)
        }
    };
    info!(%addr, ?method, "listen address selected");
    (addr, method)
}

/// Step 2 probe: spawn `tailscale ip -4`, take the FIRST stdout line as IPv4.
/// Spawn-ENOENT (tailscale absent), non-zero exit, timeout, unparseable output
/// => None each; absence is NORMAL, so all four degrade at debug level.
async fn via_cli() -> Option<IpAddr> {
    let output = match tokio::time::timeout(
        CLI_PROBE_TIMEOUT,
        Command::new("tailscale").args(["ip", "-4"]).output(),
    )
    .await
    {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(Ok(out)) => {
            debug!(status = %out.status, "`tailscale ip -4` exited nonzero");
            return None;
        }
        Ok(Err(err)) => {
            debug!(error = %err, "`tailscale` not spawnable (absent is normal)");
            return None;
        }
        Err(_elapsed) => {
            debug!("`tailscale ip -4` timed out");
            return None;
        }
    };
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_owned();
    match line.parse::<Ipv4Addr>() {
        Ok(ip) => {
            debug!(%ip, "tailscale CLI answered");
            Some(IpAddr::V4(ip))
        }
        Err(_) => {
            debug!(raw = %line, "unparseable tailscale output");
            None
        }
    }
}

/// Step 3 fallback: first interface address inside 100.64.0.0/10, in crate
/// enumeration order (deterministic enough; interface name logged).
fn via_interfaces() -> Option<IpAddr> {
    for iface in get_if_addrs().ok()? {
        let ip = iface.ip();
        if is_cgnat(&ip) {
            debug!(interface = %iface.name, %ip, "found tailnet-range interface");
            return Some(ip);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{Ipv6Addr, SocketAddrV4};

    fn v4(o: [u8; 4]) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(o))
    }

    #[test]
    fn cgnat_range_boundaries_exact() {
        assert!(!is_cgnat(&v4([99, 255, 255, 255])));
        assert!(
            !is_cgnat(&v4([100, 63, 255, 255])),
            "100.64.0.0/10 starts AT 64"
        );
        assert!(is_cgnat(&v4([100, 64, 0, 0])));
        assert!(is_cgnat(&v4([100, 127, 255, 255])));
        assert!(!is_cgnat(&v4([100, 128, 0, 0])), "/10 ends AT 127");
        assert!(!is_cgnat(&v4([101, 64, 0, 0])));
        assert!(
            !is_cgnat(&IpAddr::V6(Ipv6Addr::LOCALHOST)),
            "v6 never cgnat"
        );
    }

    #[test]
    fn priority_chain_matrix() {
        let over = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1234));
        let ts_ip = v4([100, 101, 102, 103]);
        // Explicit override ALWAYS wins — even with every other source live.
        assert_eq!(
            classify(Some(over), Some(ts_ip), Some(ts_ip)),
            Method::BindOverride
        );
        // CLI beats interface scan.
        assert_eq!(
            classify(None, Some(ts_ip), Some(ts_ip)),
            Method::TailscaleCli
        );
        assert_eq!(classify(None, None, Some(ts_ip)), Method::InterfaceScan);
        assert_eq!(classify(None, None, None), Method::Loopback);
        // Normalization: junk answers degrade one step, never up.
        assert_eq!(
            classify(None, None, Some(IpAddr::V6("fd00::1".parse().unwrap()))),
            Method::Loopback
        );
        assert_eq!(
            classify(None, Some(v4([192, 168, 1, 1])), Some(ts_ip)),
            Method::InterfaceScan
        );
        assert_eq!(
            classify(None, Some(IpAddr::V6(Ipv6Addr::LOCALHOST)), None),
            Method::Loopback
        );
    }
}
