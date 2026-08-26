//! Choose the listen address AND the address advertised to humans/clients.
//!
//! DECISION SPLIT (user-facing fix 2026-08: wildcard default bind):
//! - BIND: cfg.bind_override verbatim, else **0.0.0.0:{DEFAULT_PORT}**. We always
//!   listen on every interface unless explicitly told otherwise — the pairing
//!   token handshake is the security boundary (constant-time compare, closed
//!   sockets on failure), so wildcard binding costs nothing and removes the
//!   whole class of "server unreachable from my phone" surprises.
//! - ADVERTISE: best guess at a reachable IP for the startup log line /
//!   printed QR-style hint, purely informational:
//!   tailscale CLI `ip -4` > any 100.64.0.0/10 interface > loopback.
//!
//! REMINDER: we never JOIN anything — host's tailscaled owns membership; we only
//! surface what it provides for display.
//!
//! Structure note: [`classify`] is the PURE advertisement decision core
//! (unit-testable matrix below); [`via_cli`] / [`via_interfaces`] are thin IO
//! wrappers whose results are threaded back through classify; [`bind_addr`] is
//! the pure BIND decision. The three compose in [`plan_listen`].

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use if_addrs::get_if_addrs;
use tokio::process::Command;
use tracing::{debug, info};

/// Single source of truth for the port when only an IP is known (steps 2–4).
/// 8949 is arbitrary but MUST stay stable: clients type/persist full URLs against it.
pub const DEFAULT_PORT: u16 = 8949;

/// Best-effort budget for the `tailscale` CLI probe. One attempt, no retry:
/// absence/degradation falls through to the interface scan, never errors.
/// (~500ms: a cold tailscaled CLI is usually <50ms; anything slower is degraded.)
const CLI_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Which step of the ADVERTISE chain produced an answer — logged so operators
/// know where the printed hint came from; unit-testable without touching IO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    BindOverride,
    TailscaleCli,
    InterfaceScan,
    Loopback,
}

/// What to actually PASS TO THE KERNEL as listen socket address. Wildcard by
/// default (see module docs); overrides win verbatim including port.
pub fn bind_addr(bind_override: Option<SocketAddr>) -> SocketAddr {
    bind_override.unwrap_or(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        DEFAULT_PORT,
    ))
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

/// PURE advertisement-chain decision over already-fetched candidates. The
/// table this pins (v4/v6 normalization = identity comparison on parsed
/// [`IpAddr`]s):
///   cli Some cgnat        => TailscaleCli
///   no-cli + scan Some cgnat => InterfaceScan (only real CGNAT hits count;
///                              v6 or other scan results normalize away)
///   everything else       => Loopback
pub fn classify(cli: Option<IpAddr>, scan: Option<IpAddr>) -> Method {
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

/// Outcome of the listen-planning phase: where the kernel listens vs what we
/// TELL humans to point at. These differ on purpose (wildcard bind default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenPlan {
    pub bind: SocketAddr,
    /// Informational: the most plausible reachable IP for clients (tailscale >
    /// CGNAT-range iface > loopback). NOT used for binding.
    pub advertise_ip: IpAddr,
    pub advertise_method: Method,
}

/// NEVER fails: advertise bottoms out at loopback, bind at 0.0.0.0. IO runs
/// once per source; classify + bind_addr own ALL branching on the results.
/// Logs loudly; main.rs prints the advertise line too when it differs from a
/// friendly form of `bind`.
pub async fn plan_listen(bind_override: Option<SocketAddr>) -> ListenPlan {
    let cli = via_cli().await;
    let scan = via_interfaces();
    let advertise_method = if cli.is_some() || scan.is_some() {
        classify(cli, scan) // advertisement decision only
    } else {
        Method::Loopback
    };

    let advertise_ip = match advertise_method {
        Method::TailscaleCli => {
            let ip = cli.filter(is_cgnat).expect("method implies cli cgnat hit");
            debug!(%ip, "tailscale CLI answered");
            ip
        }
        Method::InterfaceScan => scan.filter(is_cgnat).expect("method implies scan hit"),
        // Loopback floor — including when an explicit bind override exists
        // (advertising the override's literal address is main.rs's job, since
        // only IT knows whether that addr is reachable or a kernel-internal one).
        _ => IpAddr::V4(Ipv4Addr::LOCALHOST),
    };
    // If an override exists, IT is also the best advertisement we know (the
    // operator pointed somewhere deliberately); surface its IP verbatim.
    let advertise_ip = match bind_override {
        Some(over) => over.ip(),
        None => advertise_ip,
    };

    let bind = bind_addr(bind_override);
    info!(%bind, %advertise_ip, ?advertise_method, "listen planned");
    ListenPlan {
        bind,
        advertise_ip,
        advertise_method,
    }
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
    fn advertisement_chain_matrix() {
        let ts_ip = v4([100, 101, 102, 103]);
        // CLI beats interface scan.
        assert_eq!(classify(Some(ts_ip), Some(ts_ip)), Method::TailscaleCli);
        assert_eq!(classify(None, Some(ts_ip)), Method::InterfaceScan);
        assert_eq!(classify(None, None), Method::Loopback);
        // Normalization: junk answers degrade one step, never up.
        assert_eq!(
            classify(None, Some(IpAddr::V6("fd00::1".parse().unwrap()))),
            Method::Loopback
        );
        // A non-CGNAT CLI answer (plain LAN tailscale exit configs) degrades to scan.
        assert_eq!(
            classify(Some(v4([192, 168, 1, 1])), Some(ts_ip)),
            Method::InterfaceScan
        );
        assert_eq!(classify(Some(v4([192, 168, 1, 1])), None), Method::Loopback);
        assert_eq!(
            classify(Some(IpAddr::V6(Ipv6Addr::LOCALHOST)), None),
            Method::Loopback
        );
    }

    #[test]
    fn bind_defaults_wildcard_override_wins_verbatim() {
        let over = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 1234));
        assert_eq!(bind_addr(Some(over)), over);
        let wild = bind_addr(None);
        assert!(wild.ip().is_unspecified());
        assert_eq!(wild.port(), DEFAULT_PORT);
    }
}
