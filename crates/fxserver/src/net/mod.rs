//! WebSocket serving layer. One route set, per-client task pairs.
//!
//! Surface map:
//!   GET /healthz — NO AUTH (tokenscale/service monitors); {"ok":true,"version":…}
//!   GET /ws      — upgrade → handshake::run (auth + replay/snapshot branch)
//!                  → client::run (post-auth task pair)
//!
//! Route count is FINAL for v0: no metrics, no admin endpoints. Anything that
//! wants a route belongs in fxcore behind a command first.
//!
//! Shutdown plumbing lives HERE (main.rs stays a thin boot shell): first
//! SIGTERM/SIGINT fires the shared CancellationToken so every live client task
//! sends Close(1001 going_away), then orchestrator.shutdown() SIGTERMs children,
//! waits fxcore's 5 s kill-ladder grace, SIGKILLs survivors, checkpoints WAL.
//! A SECOND signal aborts the process immediately with exit 130, deliberately
//! skipping remaining cleanup — the operator asked twice.
//!
//! Halves note: axum WebSocket cannot be reassembled after split(), so the
//! split happens ONCE here and BOTH handshake::run (`&mut` borrows) and
//! client::run (ownership) work over the halves directly.
//!
//! serve-with vs serve: tests drive [`router`] against their own ephemeral
//! TcpListener so they never touch process-global signal handlers.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{ConnectInfo, State, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use futures::StreamExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use fxcore::Orchestrator;

use crate::pair;

use client::Commander;
pub mod client;
pub mod handshake;

/// Small shim so `Arc<Orchestrator>` -> `Arc<dyn Commander>` coercion reads
/// explicitly (unsized coercions don't fire on Arc without a typed target).
trait CoerceDyn<T: ?Sized> {
    fn coerce_dyn(self) -> Arc<T>;
}
impl<X: Commander + 'static> CoerceDyn<dyn Commander> for Arc<X> {
    fn coerce_dyn(self) -> Arc<dyn Commander> {
        self
    }
}

#[derive(Clone)]
struct AppState {
    orch: Arc<Orchestrator>,
    cancel: CancellationToken,
    /// Token FILE basis — loaded PER CONNECTION ATTEMPT so `--rotate-token`
    /// takes effect without restart; load failure fails CLOSED ("auth_failed").
    data_dir: PathBuf,
}

/// Bind + run until SIGTERM/Ctrl-C has fully unwound everything, in THIS order:
///   1. graceful accept-stop + connection drain — every live WRITER emits
///      Close(1001 going_away) via the CancellationToken; readers/pumps quit,
///   2. orchestrator.shutdown(): intake closes, queued replies land, agent
///      children SIGTERMed -> fxcore's 5 s grace -> SIGKILL, WAL checkpointed.
///
/// Ownership note (fxcore contract): `shutdown` consumes `self` BY VALUE, so
/// step 2 runs AFTER the router/Arc clones died above — verified via
/// `Arc::try_unwrap`, which doubles as a leak tripwire. Bind failure (port
/// taken / EACCES) => Err up to main.
///
/// Signature note vs stub sketch (`anyhow::Result<()>`): anyhow is NOT on this
/// crate's final dep list; io::Error carries identical information here.
pub async fn serve(
    orch: Arc<Orchestrator>,
    addr: SocketAddr,
    data_dir: &Path,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "fxserver listening");

    {
        let cancel = CancellationToken::new();
        let app = router(Arc::clone(&orch), cancel.clone(), data_dir.to_path_buf())
            .into_make_service_with_connect_info::<SocketAddr>();
        // Fast-resolving shutdown future: stop accepting + fire the token ONCE.
        // The heavy teardown runs below, after sockets drained (see doc order).
        let graceful = async move {
            wait_first_signal().await;
            warn!("shutdown signal received");
            // "Second signal => abort immediately, exit 130", skipping cleanup:
            let _second_signal_killer = arm_double_signal_abort();
            cancel.cancel();
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(graceful)
            .await?;
    }
    // Everything socket-side is gone now; the router/AppState clones included.

    match Arc::try_unwrap(orch) {
        Ok(owned) => {
            owned.shutdown().await;
            info!("orchestrator drained; farewell");
            Ok(())
        }
        Err(_still_shared) => {
            error!(
                holders = Arc::strong_count(&_still_shared),
                "orchestrator handle leaked outside serve; orderly child kill SKIPPED"
            );
            Err(std::io::Error::other("orchestrator handle leaked"))
        }
    }
}

/// Router WITHOUT a bound server — public for tests/e2e.rs to mount on its own
/// listener; production always goes through [`serve`].
pub fn router(orch: Arc<Orchestrator>, cancel: CancellationToken, data_dir: PathBuf) -> Router {
    let state = AppState {
        orch,
        cancel,
        data_dir,
    };
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
}

async fn healthz() -> Json<serde_json::Value> {
    // Deliberately unauthenticated: monitors ping this constantly.
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn ws_upgrade(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| async move { conn_entrypoint(state, peer, socket).await })
}

/// One connection lifetime: token load → handshake (borrows halves) →
/// client::run (consumes halves). Token-load failure NEVER regenerates or
/// leaks internals: close("auth_failed") + loud server-side audit line.
async fn conn_entrypoint(state: AppState, peer: SocketAddr, socket: axum::extract::ws::WebSocket) {
    debug!(%peer, "connection upgraded");

    let stored_token = match pair::load_token(&state.data_dir) {
        Ok(token) => Some(token),
        Err(err) => {
            error!(%err, %peer, "server token unreadable; rejecting as auth_failed");
            None
        }
    };

    let (mut tx, mut rx) = socket.split();
    let outcome = handshake::run(
        Arc::clone(&state.orch),
        stored_token.as_deref(),
        env!("CARGO_PKG_VERSION"),
        handshake::HANDSHAKE_TIMEOUT,
        handshake::REPLAY_GAP_LIMIT,
        &mut tx,
        &mut rx,
    )
    .await;

    let authed = match outcome {
        Ok(authed) => {
            info!(%peer, "client authenticated");
            authed
        }
        Err(closed) => {
            // THE audit trail for brute-force attempts (which stage, which reason).
            warn!(%peer, stage = ?closed.stage, reason = closed.reason, "handshake ended");
            return;
        }
    };

    // Arc<Orchestrator> coerces to the Commander seam at this typed binding.
    let commander: Arc<dyn client::Commander> = Arc::clone(&state.orch).coerce_dyn();
    client::run(tx, rx, commander, authed, state.cancel).await;
}

/// Waits for the FIRST arrival among SIGINT (Ctrl-C) and SIGTERM (what systemd/
/// docker send). Signal-stream setup errors degrade gracefully to Ctrl-C-only.
async fn wait_first_signal() {
    let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            match sigterm {
                Ok(mut term) => term.recv().await,
                Err(err) => {
                    warn!(%err, "SIGTERM handler unavailable; Ctrl-C only");
                    std::future::pending::<Option<()>>().await
                }
            }
        } => {}
    }
}

/// Second-signal abort: arm AFTER teardown began per main.rs spec ("abort
/// immediately, exit 130"). Spawned from serve's graceful body via a helper so
/// tests never install process-global handlers.
pub(crate) fn arm_double_signal_abort() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        wait_first_signal().await;
        std::process::exit(130);
    })
}
