//! End-to-end: script a raw WebSocket CLIENT against a REAL running fxserver
//! (impl.md Phase 5.4: "auth fail, auth ok, Subscribe replay ... small rust test").
//!
//! Hermetic by construction:
//! - REAL `Orchestrator` over a tempdir store (empty event log ⇒ Welcome head_seq
//!   is deterministic; replay arms are covered exhaustively at the FUNCTION level
//!   in net/handshake tests because emitting genuine events needs agent processes).
//! - Client side uses async-tungstenite (DEV-DEPENDENCY ONLY — documented in
//!   Cargo.toml; fxapp already shares this workspace pin).
//! - The server harness mounts fxserver::router on an EPHEMERAL port and shuts it
//!   down through its own CancellationToken, never touching process-wide signals.
//!
//! Known coverage limit (documented deviation): no e2e test drives live EVENT
//! frames end-to-end — fxcore exposes no injectable bus once `Orchestrator` is
//! built, and every Command that mutates state requires an agent process. The
//! transport-level ordering guarantees ARE pinned by the loop-level tests over a
//! real EventBus (net/client tests: replay→pending→live ascending).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use async_tungstenite::tungstenite::Message as Ws;
use fxcore::{Config, Orchestrator};
use fxproto::command::Command;
use fxproto::content::ContentBlock;
use fxproto::envelope::Message;
use fxproto::ids::{Seq, SessionId};
use fxproto::reply::{FxErrorCode, Reply};
use fxserver::{net, pair};

const TIMEOUT: Duration = Duration::from_secs(5);

// ── Harness ──────────────────────────────────────────────────────────────────

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("fxserver-e2e-{tag}-{}-{nanos}", std::process::id()));
        Self(dir)
    }
    fn path(&self) -> &PathBuf {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Server = (
    String,
    Arc<Orchestrator>,
    Scratch,
    CancellationToken,
    tokio::task::JoinHandle<()>,
);

async fn boot_server(tag: &'static str) -> Server {
    let scratch = Scratch::new(tag);

    // Config with INJECTED data_dir (fxcore::Config::load_from contract); the
    // TOML file deliberately does not exist => pure defaults + mkdir.
    let missing_cfg = scratch.path().join("nope.toml");
    let cfg = Config::load_from(&missing_cfg, Some(scratch.path().clone())).expect("config");
    assert_eq!(&cfg.data_dir, scratch.path());

    // Token exists BEFORE the listener: exercise pair::ensure_token lifecycle.
    let _token = pair::ensure_token(&cfg.data_dir).expect("ensure_token");

    let orch = Arc::new(
        Orchestrator::new(cfg.clone())
            .await
            .expect("orchestrator boots"),
    );

    // Ephemeral port: OS-assigned, then re-bound (race window is theoretical).
    let addr = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        addr
    };
    let listener = tokio::net::TcpListener::bind(addr).await.expect("rebind");

    let cancel = CancellationToken::new();
    let app = net::router(Arc::clone(&orch), cancel.clone(), cfg.data_dir.clone());
    // Move a PRIVATE clone into the shutdown future ('static bound of axum):
    // cancelling the harness token then tears both down together.
    let shutdown_token = cancel.clone();
    let server_task = tokio::spawn(async move {
        // ConnectInfo needed EXACTLY like prod serve(): the /ws handler's
        // peer-address audit extractor is wired via the make-service layer.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move { shutdown_token.cancelled().await })
        .await
        .expect("serve loop");
    });

    (ws_url(addr), orch, scratch, cancel, server_task)
}

fn ws_url(addr: SocketAddr) -> String {
    format!("ws://{addr}/ws")
}

async fn teardown(server: Server) {
    let (_url, orch, _scratch, cancel, task) = server;
    cancel.cancel();
    task.await.expect("serve loop ends");
    match Arc::try_unwrap(orch) {
        Ok(owned) => owned.shutdown().await,
        Err(_) => panic!("orchestrator handle leaked"),
    }
}

// ── Tiny typed client ────────────────────────────────────────────────────────

/// connect_async() output stream type (tokio-runtime feature).
type Session = async_tungstenite::WebSocketStream<async_tungstenite::tokio::ConnectStream>;

async fn connect(url: &str) -> Session {
    let (session, _response) = tokio::time::timeout(
        TIMEOUT,
        async_tungstenite::tokio::connect_async(url.to_owned()),
    )
    .await
    .expect("connect within budget")
    .expect("upgrade succeeds for ANY path (auth lives above)");
    session
}

async fn send(session: &mut Session, msg: &Message) {
    let json = serde_json::to_string(msg).expect("envelope serializes");
    tokio::time::timeout(TIMEOUT, session.send(Ws::Text(json)))
        .await
        .expect("send within budget")
        .expect("socket healthy");
}

async fn send_raw(session: &mut Session, text: &str) {
    tokio::time::timeout(TIMEOUT, session.send(Ws::Text(text.into())))
        .await
        .expect("raw send within budget")
        .expect("socket healthy");
}

/// Read ONE meaningful frame; Text payloads decode to envelope Messages,
/// Close yields Err(reason). Server-side Pongs/forwarded Pings are latency
/// plumbing (client.rs PING_INTERVAL) and pass through silently.
async fn next_frame(session: &mut Session) -> Result<Message, String> {
    loop {
        let ws = tokio::time::timeout(TIMEOUT, session.next())
            .await
            .expect("frame within budget")
            .expect("stream not ended");
        match ws.expect("ws ok") {
            Ws::Text(t) => {
                return Ok(serde_json::from_str(t.as_str()).expect("server speaks our envelope"));
            }
            Ws::Close(frame) => {
                return Err(frame.map(|f| f.reason.to_string()).unwrap_or_default());
            }
            Ws::Ping(_) | Ws::Pong(_) => continue, // transparent keepalive
            other => panic!("unexpected transport frame: {other:?}"),
        }
    }
}

fn hello(token: &str) -> Message {
    Message::Hello {
        proto_version: fxproto::envelope::PROTO_VERSION,
        token: token.to_owned(),
    }
}

async fn read_stored_token(data_dir: &std::path::Path) -> String {
    pair::load_token(data_dir).expect("token readable in harness")
}

// ── Cases (impl.md 5.4 checklist order) ─────────────────────────────────────

#[tokio::test]
async fn auth_fail_observes_pinned_close_reason() {
    let server = boot_server("auth-fail").await;
    let (url, _orch, scratch, ..) = &server;
    let good = read_stored_token(scratch.path()).await;
    drop(good); // prove the check really compares tokens

    let mut ws = connect(url).await;
    send(&mut ws, &hello("definitely-wrong")).await;
    let reason = next_frame(&mut ws).await.unwrap_err();
    assert_eq!(
        reason, "auth_failed",
        "pinned string per fxproto/envelope.rs"
    );
    teardown(server).await;
}

#[tokio::test]
async fn version_mismatch_observes_protocol_version_close() {
    let server = boot_server("version").await;
    let (url, ..) = &server;
    let mut ws = connect(url).await;
    send(
        &mut ws,
        &Message::Hello {
            proto_version: 9999,
            token: "x".into(),
        },
    )
    .await;
    assert_eq!(next_frame(&mut ws).await.unwrap_err(), "protocol_version");
    teardown(server).await;
}

#[tokio::test]
async fn first_frame_not_hello_is_rejected() {
    let server = boot_server("first-frame").await;
    let (url, ..) = &server;
    let mut ws = connect(url).await;
    send(
        &mut ws,
        &Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;
    assert_eq!(next_frame(&mut ws).await.unwrap_err(), "protocol_version");
    teardown(server).await;
}

#[tokio::test]
async fn auth_ok_welcome_then_double_subscribe_violation() {
    let server = boot_server("double-sub").await;
    let (url, _orch, scratch, ..) = &server;
    let token = read_stored_token(scratch.path()).await;

    let mut ws = connect(url).await;
    send(&mut ws, &hello(&token)).await;

    let Message::Welcome { head_seq, .. } = next_frame(&mut ws).await.expect("welcome") else {
        panic!("expected Welcome");
    };
    assert_eq!(head_seq.as_u64(), 0, "fresh tempdir store has an empty log");

    // FIRST subscribe is legal — replay of nothing arrives silently (live attach).
    send(
        &mut ws,
        &Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;

    // SECOND subscribe post-handshake = FAIL-V2 over the REAL socket.
    send(
        &mut ws,
        &Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;
    assert_eq!(next_frame(&mut ws).await.unwrap_err(), "protocol_version");
    teardown(server).await;
}

#[tokio::test]
async fn steady_state_request_gets_correlated_reply() {
    let server = boot_server("correlation").await;
    let (url, _orch, scratch, ..) = &server;
    let token = read_stored_token(scratch.path()).await;

    let mut ws = connect(url).await;
    send(&mut ws, &hello(&token)).await;
    let Message::Welcome { .. } = next_frame(&mut ws).await.unwrap() else {
        panic!("welcome first");
    };
    send(
        &mut ws,
        &Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;

    // Harmless command WITHOUT agent machinery: NewSession against a ghost
    // agent exercises reader→orchestrator.execute→writer with id preserved.
    let cmd = Command::Prompt {
        session: SessionId::from_raw("ghost-session".to_owned()),
        blocks: vec![ContentBlock::Text { text: "hi".into() }],
    };
    send(
        &mut ws,
        &Message::Request {
            id: 42,
            command: cmd,
        },
    )
    .await;

    let reply = next_frame(&mut ws).await.expect("reply arrives over wire");
    let Message::Response { id, reply } = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(id, 42, "correlation echo verbatim");
    let Reply::Error(err) = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(
        err.code,
        FxErrorCode::SessionNotFound,
        "ghost-session unknown: command ran through the REAL orchestrator actor"
    );
    teardown(server).await;
}

#[tokio::test]
async fn malformed_garbage_closes_socket_with_protocol_version() {
    let server = boot_server("garbage").await;
    let (url, ..) = &server;
    let mut ws = connect(url).await;
    send_raw(&mut ws, "{{not-an-envelope").await;
    assert_eq!(next_frame(&mut ws).await.unwrap_err(), "protocol_version");
    teardown(server).await;
}

#[tokio::test]
async fn second_connection_after_rotate_token_fails_auth() {
    // Lifecycle integration: rotate swaps the secret on disk; NEW connections
    // must present the NEW token (rotation takes effect without restart —
    // conn_entrypoint loads per attempt).
    let server = boot_server("rotate").await;
    let (url, _orch, scratch, ..) = &server;
    let old = pair::load_token(scratch.path()).unwrap();

    let rotated = pair::rotate_token(scratch.path()).unwrap();
    assert_ne!(old, rotated);

    let mut ws = connect(url).await;
    send(&mut ws, &hello(&old)).await;
    assert_eq!(next_frame(&mut ws).await.unwrap_err(), "auth_failed");

    let mut ws2 = connect(url).await;
    send(&mut ws2, &hello(&rotated)).await;
    assert!(matches!(
        next_frame(&mut ws2).await,
        Ok(Message::Welcome { .. })
    ));

    // Let go of both sessions BEFORE teardown: lingering client sockets keep
    // their conn_entrypoint tasks (and thus AppState) alive, which would trip
    // teardown's sole-holder check.
    let _ = ws.close(None).await;
    let _ = ws2.close(None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(ws);
    drop(ws2);
    tokio::time::sleep(Duration::from_millis(100)).await;
    teardown(server).await;
}

#[tokio::test]
async fn healthz_answers_without_auth() {
    let server = boot_server("healthz").await;
    let (url, ..) = &server;
    // Hand-rolled minimal HTTP client (no extra deps):
    let http_addr = url
        .trim_start_matches("ws://")
        .trim_end_matches("/ws")
        .to_owned();
    let mut stream = tokio::net::TcpStream::connect(&http_addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    stream.flush().await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut buf))
        .await
        .expect("healthz body within budget")
        .unwrap();
    let body = String::from_utf8_lossy(&buf);
    assert!(body.contains("200"), "status line: {body}");
    assert!(body.contains(r#""ok":true"#), "{body}");
    assert!(body.contains(env!("CARGO_PKG_VERSION")), "{body}");

    // Contrasted with /ws-ish junk route: keep surface honest (route count FINAL).
    teardown(server).await;
}
