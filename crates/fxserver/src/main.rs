//! fxserver bin — ONLY the numbered boot sequence over the `fxserver` lib
//! modules. Everything durable lives behind them (see crate docs).
//!
//! Failure at step k => log + non-zero exit + STOP, never continue half-booted.
//! Exits from inside async boot fns are legal here: every one precedes
//! multi-task liveness, and a corrupt store is FATAL by design (downtime beats
//! silent data loss — a store wiped on OUR initiative would be silent loss).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::exit;

use fxcore::{Config, Orchestrator};
use fxserver::net;

const USAGE: &str = "\
fxserver — headless daemon owning all agents + state

USAGE: fxserver [FLAGS]

  --config <path>    alternate config TOML (default ~/.fxcode/config.toml)
  --bind <socket>    listen override, wins over config's bind_addr
  --data-dir <path>  state dir override (events.db + token live here)
  --rotate-token     mint a fresh pairing token, print it ONCE, exit
                     (store/orchestrator/listener never boot)
";

enum CliOutcome {
    /// Ready for steps 3–8.
    Boot(Cli),
    /// -h/--help courtesy: usage to stdout, exit 0.
    Help,
    /// Unknown flag / missing value / bad --bind socket: usage, exit 2.
    Reject(String),
}

struct Cli {
    config: Option<PathBuf>,
    bind: Option<SocketAddr>,
    data_dir: Option<PathBuf>,
    rotate_token: bool,
}

fn next_value(argv: &mut std::vec::IntoIter<String>, flag: &str) -> Option<String> {
    match argv.next() {
        Some(v) if !v.starts_with("--") => Some(v),
        _ => {
            eprintln!("flag {flag} requires a value");
            None
        }
    }
}

/// Hand-rolled argv walk for v0 (no clap yet). Mirrors spec exactly:
/// unknown/missing => stderr usage + 2; -h/--help => stdout usage + 0.
fn parse_cli(mut argv: std::vec::IntoIter<String>) -> CliOutcome {
    let mut cli = Cli {
        config: None,
        bind: None,
        data_dir: None,
        rotate_token: false,
    };
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-h" | "--help" => return CliOutcome::Help,
            "--rotate-token" => cli.rotate_token = true,
            "--config" => match next_value(&mut argv, "--config") {
                Some(v) => cli.config = Some(PathBuf::from(v)),
                None => return CliOutcome::Reject("--config".into()),
            },
            "--bind" => match next_value(&mut argv, "--bind") {
                Some(v) => match v.parse::<SocketAddr>() {
                    Ok(addr) => cli.bind = Some(addr),
                    Err(_) => {
                        return CliOutcome::Reject(format!("--bind is not a socket address: {v}"));
                    }
                },
                None => return CliOutcome::Reject("--bind".into()),
            },
            "--data-dir" => match next_value(&mut argv, "--data-dir") {
                Some(v) => cli.data_dir = Some(PathBuf::from(v)),
                None => return CliOutcome::Reject("--data-dir".into()),
            },
            other => return CliOutcome::Reject(format!("unknown flag {other}")),
        }
    }
    CliOutcome::Boot(cli)
}

/// Steps 3–8. Each fallible step logs its OWN error and exits with its class:
///   2 — configuration domain (unusable config / bad flags)
///   1 — boot-time IO / orchestration / listener
///   0 — graceful completion (serve returned after full teardown)
async fn boot(cli: Cli) {
    // ── 3. Config: defaults <- config.toml <- CLI overrides ──
    // --config given: load_from wires data_dir override precedence correctly
    // (explicit parameter beats BOTH env default AND any file statement).
    let loaded = match &cli.config {
        Some(path) => Config::load_from(path, cli.data_dir.clone()),
        None => Config::load().map(|mut cfg| {
            if let Some(dir) = &cli.data_dir {
                cfg.data_dir = dir.clone();
            }
            cfg
        }),
    };
    let mut cfg = match loaded {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::error!(%err, "config unusable");
            exit(2);
        }
    };
    if let Some(addr) = cli.bind {
        cfg.bind_override = Some(addr); // CLI wins over config.toml, verbatim w/ port
    }

    // ── 4. --rotate-token short-circuit: no listener ever binds ──
    if cli.rotate_token {
        match fxserver::pair::rotate_token(&cfg.data_dir) {
            Ok(_) => return, // new token printed by pair.rs exactly once
            Err(err) => {
                tracing::error!(%err, "token rotation failed");
                exit(1);
            }
        }
    }

    // ── 5. Orchestrator: SQLite(WAL) open + whole-log projection fold ──
    let orchestrator = match Orchestrator::new(cfg.clone()).await {
        Ok(orch) => orch,
        Err(err) => {
            tracing::error!(%err, "orchestrator boot failed");
            exit(1);
        }
    };

    // ── 6. Pairing token (prints itself to stderr ONCE, first creation only) ──
    if let Err(err) = fxserver::pair::ensure_token(&cfg.data_dir) {
        tracing::error!(%err, "pairing token unavailable");
        exit(1);
    }

    // ── 7. Listen address: chain bottoms out at loopback; pick() logs loudly ──
    let (addr, method) = fxserver::ifaddr::pick(cfg.bind_override).await;
    tracing::info!(%addr, ?method, "binding");

    // ── 8. Serve until SIGTERM/Ctrl-C; child kill ladder runs inside fxcore ──
    if let Err(err) = net::serve(std::sync::Arc::new(orchestrator), addr, &cfg.data_dir).await {
        tracing::error!(%err, "listener failed");
        exit(1);
    }
}

fn main() {
    // fxserver owns tokio outright (fxapp embeds its own quarantine runtime;
    // here multi-thread is simply correct for N concurrent agent connections).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio multithread runtime");

    // ── 1. Tracing: EnvFilter (RUST_LOG), default directive info when unset ──
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // ── 2. CLI argv walk ──
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse_cli(argv.into_iter()) {
        CliOutcome::Boot(cli) => runtime.block_on(boot(cli)),
        CliOutcome::Help => print!("{USAGE}"),
        CliOutcome::Reject(reason) => {
            eprintln!("{reason}");
            eprintln!("{USAGE}");
            exit(2);
        }
    }
}
