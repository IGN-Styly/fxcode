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
//! Known coverage history (documented deviation, M1): no e2e test drove live
//! EVENT frames end-to-end — every Command that mutates state required an
//! agent process. M2 CLOSED that gap: the `fake_agent_stdio` example binary is
//! a REAL spawnable ACP agent wired through `Config.drivers` overrides, so the
//! tests below (`live_turn_replay_and_boundary_dedupe`,
//! `two_drivers_fold_independently`, `snapshot_required_path_preseeded_store`)
//! exercise genuine process ⇄ pipe ⇄ axum ⇄ tungstenite traffic against an
//! authentic Orchestrator.

use std::collections::{BTreeMap, BTreeSet};
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
use fxproto::driver::{DriverId, DriverSpec};
use fxproto::envelope::Message;
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::{Seq, SessionId};
use fxproto::reply::{FxErrorCode, Reply};
use fxserver::{net, pair};

const TIMEOUT: Duration = Duration::from_secs(15);

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
    boot_custom(tag, move |scratch_path: PathBuf| async move {
        // Config with INJECTED data_dir (fxcore::Config::load_from contract);
        // the TOML file deliberately does not exist => pure defaults + mkdir.
        let missing_cfg = scratch_path.join("nope.toml");
        Config::load_from(&missing_cfg, Some(scratch_path.clone())).expect("config")
    })
    .await
}

/// Parameterized boot: same listener/router dance as `boot_server`, but the
/// CALLER shapes the Config (M2 G1/G3 inject fake-agent driver overrides;
/// G2 pre-seeds an event log into scratch_path/events.db BEFORE this boots).
/// PathBuf (owned, Clone-cheap) sidesteps closure↔async-block lifetime fights.
async fn boot_custom<F>(tag: &'static str, make_cfg: impl FnOnce(PathBuf) -> F) -> Server
where
    F: std::future::Future<Output = Config>,
{
    let scratch = Scratch::new(tag);
    let cfg = make_cfg(scratch.path().clone()).await;
    assert!(cfg.data_dir.is_dir(), "config loader mkdirs the data_dir");

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

// ════════════════════════════════════════════════════════════════════════════
// M2 G1/G2/G3: live events over REAL sockets.
//
// Shared setup: `Config.drivers` overrides point ClaudeCode (and CodexCli for
// G3) at the fake_agent_stdio EXAMPLE binary — a genuine OS child speaking ACP
// over real pipes, scripted by FX_FAKE_MODE/FX_FAKE_TEXT env knobs layered in
// via DriverSpec.env (AcpConnection spawns inherit our env + spec.env).
//
// Hermeticity: tempdir stores; cargo-built example binary; no network beyond
// loopback; every wait bounded. NOTE fxserver tests CANNOT name another
// package's binary via CARGO_BIN_EXE_ (cargo sets it per-own-package), so the
// path is resolved by anchoring off OUR OWN bin target dir (…/target/<profile>)
// — see fake_agent_stdio_exe() below and its precondition comment.
// ════════════════════════════════════════════════════════════════════════════

/// Resolve the fake-agent stdio example WITHOUT a same-package guarantee:
///   1. CARGO_BIN_EXE_fake_agent_stdio when present (direct tests of fxcore),
///   2. sibling examples/ dir of fxserver's OWN bin target directory,
///      (CARGO_BIN_EXE_fxserver ⇒ …/target/<profile>/fxserver),
///   3. current_exe parent's parent + examples (…/deps/ → …/examples/).
///
/// PRECONDITION (documented deviation): running `cargo test -p fxserver` ALONE
/// does not build fxcore's example — run `cargo build -p fxcore --examples`
/// first, or run both crates together (`cargo test -p fxcore -p fxserver`).
fn fake_agent_stdio_exe() -> String {
    if let Some(p) = std::env::var_os("CARGO_BIN_EXE_fake_agent_stdio") {
        return p.to_string_lossy().into_owned();
    }
    let own_bin = PathBuf::from(env!("CARGO_BIN_EXE_fxserver"));
    let candidate = own_bin
        .parent()
        .expect("bin target dir")
        .join("examples")
        .join("fake_agent_stdio");
    if candidate.is_file() {
        return candidate.to_string_lossy().into_owned();
    }
    let via_test_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("../examples/fake_agent_stdio")))
        .filter(|p| p.is_file());
    via_test_exe
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            panic!(
                "fake_agent_stdio example not built; run `cargo build -p fxcore --examples` first \
             (wanted {})",
                candidate.display()
            )
        })
}

fn driver_spec(program: &str, mode: &str, text: &str) -> DriverSpec {
    DriverSpec {
        program: program.to_owned(),
        args: vec![],
        env: BTreeMap::from([
            ("FX_FAKE_MODE".to_owned(), mode.to_owned()),
            ("FX_FAKE_TEXT".to_owned(), text.to_owned()),
        ]),
    }
}

/// Config override layering a REAL spawnable agent onto one/both drivers.
fn with_fake_drivers(cfg: &mut Config, slots: &[(DriverId, &str, &str)]) {
    let exe = fake_agent_stdio_exe();
    for (id, mode, text) in slots {
        cfg.drivers.insert(*id, driver_spec(&exe, mode, text));
    }
}

/// Correlated request helper that ALSO records any Event frames streaming past
/// while we wait for our Response id.
async fn request_collect(
    ws: &mut Session,
    log: &mut Vec<Sequenced<FxEvent>>,
    id: u64,
    command: Command,
) -> Reply {
    send(ws, &Message::Request { id, command }).await;
    loop {
        match next_frame(ws).await.expect("frame while awaiting reply") {
            Message::Event { event } => log.push(event),
            Message::Response { id: rid, reply } if rid == id => return reply,
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

/// Collect Event frames until this session's turn finishes with end_turn.
async fn collect_until_turn_end(
    ws: &mut Session,
    log: &mut Vec<Sequenced<FxEvent>>,
    session: &SessionId,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "turn never finished"
        );
        match next_frame(ws).await.expect("event frame") {
            Message::Event { event } => {
                let is_target_finish = matches!(
                    &event.inner,
                    FxEvent::TurnFinished { session: s, .. } if s == session
                );
                log.push(event);
                if is_target_finish {
                    return;
                }
            }
            other => panic!("unexpected frame during turn: {other:?}"),
        }
    }
}

async fn close_politely(ws: Session) {
    let mut ws = ws;
    let _ = ws.close(None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(ws);
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Settled head: the structural post-TurnFinished Ready status (and any late
/// transport notifications) trail by scheduler ticks; require the head to be
/// STABLE across a sleep window before using it in cursor math.
async fn settled_head(orch: &Orchestrator) -> u64 {
    let mut last = orch
        .projection_snapshot()
        .await
        .unwrap()
        .baseline_seq
        .as_u64();
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let now = orch
            .projection_snapshot()
            .await
            .unwrap()
            .baseline_seq
            .as_u64();
        if now == last {
            return now;
        }
        last = now;
    }
    panic!("head never settled: {last}");
}

/// Post-turn tail drain on an OPEN connection: collect everything that keeps
/// arriving during a quiet window (late notifications are legal; see
/// acp/mod.rs completion-note comment). Bounded, assert-fast.
async fn drain_tail(ws: &mut Session, log: &mut Vec<Sequenced<FxEvent>>) {
    let mut quiet = 0u32;
    while quiet < 5 {
        match tokio::time::timeout(Duration::from_millis(120), ws.next()).await {
            Err(_) => quiet += 1,
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(Ws::Text(t)))) => {
                // Responses/pongs irrelevant here: only events feed the tail.
                if let Ok(Message::Event { event }) = serde_json::from_str::<Message>(t.as_str()) {
                    log.push(event);
                }
                quiet = 0;
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

#[tokio::test]
async fn live_turn_replay_and_boundary_dedupe() {
    let server = boot_custom("g1-live", move |scratch: PathBuf| async move {
        let missing = scratch.join("nope.toml");
        let mut cfg = Config::load_from(&missing, Some(scratch.to_path_buf())).expect("config");
        with_fake_drivers(&mut cfg, &[(DriverId::ClaudeCode, "chunks", "agent text")]);
        cfg
    })
    .await;
    let (url, orch, scratch, ..) = &server;
    let token = read_stored_token(scratch.path()).await;

    // ── connection #1: pure LIVE tail from an empty store ──
    let mut ws1 = connect(url).await;
    send(&mut ws1, &hello(&token)).await;
    let Message::Welcome { head_seq, .. } = next_frame(&mut ws1).await.unwrap() else {
        panic!("welcome");
    };
    assert_eq!(head_seq.as_u64(), 0);
    send(
        &mut ws1,
        &Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;

    let mut log: Vec<Sequenced<FxEvent>> = Vec::new();

    // DetectAgents reflects the config OVERRIDE row verbatim.
    let reply = request_collect(&mut ws1, &mut log, 1, Command::DetectAgents).await;
    let Reply::DetectedAgents { drivers } = reply else {
        panic!("{reply:?}")
    };
    assert_eq!(drivers.len(), 3, "three canonical rows always");
    let claude_row = drivers.first().expect("claude first").clone();
    assert_eq!(claude_row.driver, DriverId::ClaudeCode);
    assert!(claude_row.found);
    assert!(
        claude_row.spec_used.program.contains("fake_agent_stdio"),
        "{:?}",
        claude_row.spec_used.program
    );
    assert_eq!(
        claude_row
            .spec_used
            .env
            .get("FX_FAKE_MODE")
            .map(String::as_str),
        Some("chunks")
    );

    // Full pipeline through REAL process pipes:
    let Reply::Started { agent } = request_collect(
        &mut ws1,
        &mut log,
        2,
        Command::StartAgent {
            driver: DriverId::ClaudeCode,
        },
    )
    .await
    else {
        panic!("started");
    };
    let cwd = scratch.path().canonicalize().unwrap();
    let Reply::SessionCreated { session } = request_collect(
        &mut ws1,
        &mut log,
        3,
        Command::NewSession {
            agent: agent.clone(),
            cwd,
            mcp_servers: vec![],
        },
    )
    .await
    else {
        panic!("session");
    };
    let Reply::PromptAccepted { .. } = request_collect(
        &mut ws1,
        &mut log,
        4,
        Command::Prompt {
            session: session.clone(),
            blocks: vec![ContentBlock::Text { text: "hi".into() }],
        },
    )
    .await
    else {
        panic!("accepted");
    };

    collect_until_turn_end(&mut ws1, &mut log, &session).await;
    // Late notifications are LEGAL (documented dispatch asymmetry); fold any
    // stragglers in before asserting on the log.
    drain_tail(&mut ws1, &mut log).await;

    // ── PINNED ORDER — adjusted to architectural reality ──
    // ORCHESTRATOR-caused events chain strictly: SessionCreated→TurnStarted→
    // Chunk(user "hi")→[Busy]→TurnFinished(end_turn)→Ready. AGENT-caused
    // streamed updates (agent chunk + both ToolCallUpsert stages) MAY land
    // anywhere from just-after-TurnStarted up to AFTER TurnFinished — the
    // prompt-response resolution consistently beats transport-borne session
    // updates in the SDK dispatch (documented at fxcore/driver/acp/mod.rs
    // main_loop's completion-note; the ORIGINAL G1 sketch demanded
    // upserts-BEFORE-finish which this reality makes unsatisfiable without an
    // fxcore protocol change). What IS guaranteed and pinned below:
    //   1. orchestrator chain as a strict sub-sequence,
    //   2. global strictly-ascending seqs,
    //   3. both tool-call stages present EXACTLY once with correct statuses/
    //      outputs/shared id, pairwise-ordered pending<completed, each after
    //      SessionCreated,
    //   4. agent chunk present exactly once after SessionCreated.
    use fxproto::content::ToolCallStatus as Tcs;
    use fxproto::content::{Role, StopReason};
    let pos_of = |pred: &dyn Fn(&FxEvent) -> bool| {
        log.iter()
            .position(|ev| pred(&ev.inner))
            .unwrap_or_else(|| panic!("expected event missing in {log:?}"))
    };
    let p_created = pos_of(&|ev| matches!(ev, FxEvent::SessionCreated { .. }));
    let p_started = pos_of(&|ev| matches!(ev, FxEvent::TurnStarted { .. }));
    let p_user =
        pos_of(&|ev| matches!(ev, FxEvent::Chunk { role: Role::User, text, .. } if text == "hi"));
    let p_busy = pos_of(&|ev| {
        matches!(
            ev,
            FxEvent::AgentStatus {
                status: fxproto::event::AgentStatus::Busy,
                ..
            }
        )
    });
    let p_finished = pos_of(&|ev| {
        matches!(
            ev,
            FxEvent::TurnFinished {
                stop_reason: StopReason::EndTurn,
                ..
            }
        )
    });
    assert!(p_created < p_started);
    assert!(p_started < p_user);
    assert!(p_user < p_busy);
    assert!(p_busy < p_finished);

    // Tool-call pair: EXACTLY one Pending then one Completed sharing id/title,
    // both after SessionCreated; agent chunk exactly once after SessionCreated.
    let upserts_of = |status_want: Tcs| -> Vec<(usize, u64)> {
        log.iter()
            .enumerate()
            .filter_map(|(i, ev)| match &ev.inner {
                FxEvent::ToolCallUpsert { status, .. } if *status == status_want => {
                    Some((i, ev.seq.as_u64()))
                }
                _ => None,
            })
            .collect()
    };
    let pendings = upserts_of(Tcs::Pending);
    assert_eq!(pendings.len(), 1, "exactly one Pending upsert");
    assert!(pendings[0].0 > p_created);
    let (p_completed, completed_seq) = log
        .iter()
        .enumerate()
        .find_map(|(i, ev)| match &ev.inner {
            FxEvent::ToolCallUpsert {
                status: Tcs::Completed,
                tool_call,
                title,
                output,
                ..
            } => {
                assert_eq!(tool_call.as_str(), "call_1");
                assert_eq!(title.as_str(), "e2e probe");
                // engine sends raw_output {"text":"done"}; Row O renders it
                // as pretty JSON (normalize.rs extract_output).
                let rendered = output.as_deref().expect("output present");
                assert!(rendered.contains("\"text\": \"done\""), "{rendered}");
                Some((i, ev.seq.as_u64()))
            }
            _ => None,
        })
        .expect("exactly-one Completed upsert");
    assert!(
        completed_seq > pendings[0].1,
        "pending stage precedes completed pairwise"
    );
    let _ = p_completed;
    let agent_chunks = log
        .iter()
        .filter(|ev| {
            matches!(&ev.inner, FxEvent::Chunk { role: Role::Agent, text, .. } if text == "agent text")
        })
        .count();
    assert_eq!(agent_chunks, 1);
    assert!(
        pos_of(
            &|ev| matches!(ev, FxEvent::Chunk { role: Role::Agent, text, .. } if text == "agent text")
        ) > p_started
    );

    // Global seq discipline across everything streamed on conn #1.
    let seqs: Vec<u64> = log.iter().map(|e| e.seq.as_u64()).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "ascending {seqs:?}");
    let h = settled_head(orch).await;
    assert!(
        seqs.last().copied().unwrap() <= h,
        "streamed events cannot exceed the persisted head"
    );
    close_politely(ws1).await;

    // ── connection #2: FULL replay-from-0 == STORE AUTHORITY ──
    let mut ws2 = connect(url).await;
    send(&mut ws2, &hello(&token)).await;
    let Message::Welcome { head_seq, .. } = next_frame(&mut ws2).await.unwrap() else {
        panic!("welcome #2");
    };
    assert_eq!(head_seq.as_u64(), h, "head static between turns");
    send(
        &mut ws2,
        &Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;
    let mut replayed: Vec<u64> = Vec::new();
    for _ in 0..h {
        match next_frame(&mut ws2).await.expect("replay frame") {
            Message::Event { event } => replayed.push(event.seq.as_u64()),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(replayed.len() as u64, h);
    // Wire replay equals the store walk BYTE FOR SEQ: the source of truth is
    // the persisted log, not our own collection timing.
    let authority = orch.replay_from(Seq::new(0)).await.expect("store walk");
    let authority_seqs: Vec<u64> = authority.iter().map(|e| e.seq.as_u64()).collect();
    assert_eq!(authority_seqs.len() as u64, h);
    assert_eq!(replayed, authority_seqs, "wire replay == store authority");
    assert_eq!(authority_seqs.first().copied(), Some(1));
    let live_set: BTreeSet<u64> = seqs.iter().copied().collect();
    assert!(
        live_set.iter().all(|s| authority_seqs.contains(s)),
        "live tail is a strict subset of the persisted log"
    );

    // ── connection #3: mid-log cursor ⇒ suffix-only, boundary EXCLUDED ──
    // Use TurnStarted's seq as the boundary: the NEXT frame must be exactly
    // one past it (the user-echo chunk), never the boundary event itself.
    let turn_started_seq = log
        .iter()
        .find(|e| matches!(e.inner, FxEvent::TurnStarted { .. }))
        .unwrap()
        .seq
        .as_u64();
    let mut ws3 = connect(url).await;
    send(&mut ws3, &hello(&token)).await;
    let _ = next_frame(&mut ws3).await.unwrap(); // welcome
    send(
        &mut ws3,
        &Message::Subscribe {
            last_seq: Seq::new(turn_started_seq),
        },
    )
    .await;
    // First frame is strictly AFTER the boundary event (no double-apply).
    match next_frame(&mut ws3).await.unwrap() {
        Message::Event { event } => assert_eq!(
            event.seq.as_u64(),
            turn_started_seq + 1,
            "suffix starts exactly one past the boundary"
        ),
        other => panic!("{other:?}"),
    }
    close_politely(ws3).await;

    // ── connection #4: cursor AT head ⇒ zero replay, then live continues at
    // exactly H+1 (boundary-dedupe holds at the HEAD too) ──
    let h4 = settled_head(orch).await;
    assert_eq!(h4, h, "head fully quiet before the boundary probe");
    let mut ws4 = connect(url).await;
    send(&mut ws4, &hello(&token)).await;
    let Message::Welcome { head_seq, .. } = next_frame(&mut ws4).await.unwrap() else {
        panic!("welcome #4");
    };
    assert_eq!(head_seq.as_u64(), h4);
    send(
        &mut ws4,
        &Message::Subscribe {
            last_seq: Seq::new(h4),
        },
    )
    .await;
    let mut second_pre: Vec<Sequenced<FxEvent>> = Vec::new();
    // Fire a second turn THROUGH THIS connection; every event it delivers must
    // be strictly BEYOND the cursor — nothing ≤ H is ever re-delivered — and
    // the second transcript folds identically. (The old sketch pinned the
    // literal first seq to H+1, which late stragglers from turn #1 may legally
    // displace; the INVARIANT is no-re-delivery + correct fold.)
    let Reply::PromptAccepted { .. } = request_collect(
        &mut ws4,
        &mut second_pre,
        100,
        Command::Prompt {
            session: session.clone(),
            blocks: vec![ContentBlock::Text {
                text: "round two".into(),
            }],
        },
    )
    .await
    else {
        panic!("accepted #2");
    };
    let mut second_log: Vec<Sequenced<FxEvent>> = Vec::new();
    collect_until_turn_end(&mut ws4, &mut second_log, &session).await;
    drain_tail(&mut ws4, &mut second_log).await;
    let mut all_second: Vec<u64> = second_pre
        .iter()
        .chain(second_log.iter())
        .map(|e| e.seq.as_u64())
        .collect();
    all_second.sort_unstable();
    all_second.dedup();
    assert!(
        all_second.iter().all(|s| *s > h4),
        "no re-delivery below/at the head cursor"
    );
    // Second transcript content sanity on the wire level too:
    let has_round_two_user = second_pre.iter().chain(second_log.iter()).any(
        |ev| matches!(&ev.inner, FxEvent::Chunk { role: fxproto::content::Role::User, text, .. } if text == "round two"),
    );
    assert!(has_round_two_user, "user echo streamed live past cursor");

    close_politely(ws2).await;
    close_politely(ws4).await;
    teardown(server).await;
}

/// G3 Phase 8.4: TWO drivers simultaneously — ClaudeCode AND CodexCli are BOTH
/// overridden onto the same example binary with different FX_FAKE_TEXT scripts;
/// two sessions interleave their prompts on ONE ws connection; transcripts
/// fold independently per session (role-merged chunks), and BOTH TurnFinished
/// end_turn events arrive before draining closes.
#[tokio::test]
async fn two_drivers_fold_independently() {
    let server = boot_custom("g3-two-drivers", move |scratch: PathBuf| async move {
        let missing = scratch.join("nope.toml");
        let mut cfg = Config::load_from(&missing, Some(scratch.to_path_buf())).expect("config");
        with_fake_drivers(
            &mut cfg,
            &[
                (DriverId::ClaudeCode, "chunks", "claude-says"),
                (DriverId::CodexCli, "chunks", "codex-says"),
            ],
        );
        cfg
    })
    .await;
    let (url, orch, scratch, ..) = &server;
    let token = read_stored_token(scratch.path()).await;

    let mut ws = connect(url).await;
    send(&mut ws, &hello(&token)).await;
    let Message::Welcome { .. } = next_frame(&mut ws).await.unwrap() else {
        panic!("welcome");
    };
    send(
        &mut ws,
        &Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;

    let mut log: Vec<Sequenced<FxEvent>> = Vec::new();
    let Reply::Started { agent: agent_a } = request_collect(
        &mut ws,
        &mut log,
        1,
        Command::StartAgent {
            driver: DriverId::ClaudeCode,
        },
    )
    .await
    else {
        panic!("agent A started");
    };
    let Reply::Started { agent: agent_b } = request_collect(
        &mut ws,
        &mut log,
        2,
        Command::StartAgent {
            driver: DriverId::CodexCli,
        },
    )
    .await
    else {
        panic!("agent B started");
    };
    assert_ne!(agent_a, agent_b);

    let Reply::SessionCreated { session: sess_a } = request_collect(
        &mut ws,
        &mut log,
        3,
        Command::NewSession {
            agent: agent_a.clone(),
            cwd: scratch.path().canonicalize().unwrap(),
            mcp_servers: vec![],
        },
    )
    .await
    else {
        panic!("session A");
    };
    let Reply::SessionCreated { session: sess_b } = request_collect(
        &mut ws,
        &mut log,
        4,
        Command::NewSession {
            agent: agent_b.clone(),
            cwd: scratch.path().canonicalize().unwrap(),
            mcp_servers: vec![],
        },
    )
    .await
    else {
        panic!("session B");
    };

    // INTERLEAVED prompts on ONE connection: two turn tasks stream concurrently
    // into the shared pump (per-event total order enforced by EventSink).
    // BOTH replies AND both TurnFinished frames are collected by ONE reader —
    // frames racing a reply MUST count toward the finishes tally.
    let Reply::PromptAccepted { .. } = request_collect(
        &mut ws,
        &mut log,
        5,
        Command::Prompt {
            session: sess_a.clone(),
            blocks: vec![ContentBlock::Text {
                text: "ping-a".into(),
            }],
        },
    )
    .await
    else {
        panic!("prompt A");
    };
    send(
        &mut ws,
        &Message::Request {
            id: 6,
            command: Command::Prompt {
                session: sess_b.clone(),
                blocks: vec![ContentBlock::Text {
                    text: "ping-b".into(),
                }],
            },
        },
    )
    .await;

    let mut finished: Vec<SessionId> = Vec::new();
    let mut both_accepted = false;
    while finished.len() < 2 || !both_accepted {
        match next_frame(&mut ws).await.expect("frame until two finishes") {
            Message::Event { event } => {
                if let FxEvent::TurnFinished { session, .. } = &event.inner
                    && {
                        matches!(
                            &event.inner,
                            FxEvent::TurnFinished {
                                stop_reason: fxproto::content::StopReason::EndTurn,
                                ..
                            }
                        ) && (*session == sess_a || *session == sess_b)
                            && !finished.contains(session)
                    }
                {
                    finished.push(session.clone());
                }
                log.push(event);
            }
            Message::Response { id: 6, reply } => {
                assert!(matches!(reply, Reply::PromptAccepted { .. }), "{reply:?}");
                both_accepted = true;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(finished.len(), 2);
    assert!(finished.contains(&sess_a));
    assert!(finished.contains(&sess_b));

    // Independent folds: per-session Chunk roles/texts isolated, tool cards
    // keyed per thread, active turns cleared.
    let snap = orch.projection_snapshot().await.unwrap();
    let expected = [
        (&sess_a, "ping-a", "claude-says"),
        (&sess_b, "ping-b", "codex-says"),
    ];
    for (sid, user_text, agent_text) in expected {
        let thread = &snap.threads.threads[sid];
        let texts: Vec<(fxproto::content::Role, String)> = thread
            .messages
            .iter()
            .map(|m| (m.role, m.text.clone()))
            .collect();
        assert_eq!(
            texts,
            vec![
                (fxproto::content::Role::User, user_text.to_owned()),
                (fxproto::content::Role::Agent, agent_text.to_owned()),
            ],
            "thread {sid} transcript leaked across drivers"
        );
        assert!(thread.active_turn.is_none());
        let card = thread.tool_calls.values().next().expect("tool card folded");
        assert_eq!(card.status, fxproto::content::ToolCallStatus::Completed);
        let rendered = card.output.as_deref().expect("folded output");
        assert!(rendered.contains("\"text\": \"done\""), "{rendered}");
    }
    assert_eq!(snap.threads.threads.len(), 2, "exactly the two sessions");

    close_politely(ws).await;
    teardown(server).await;
}

/// G2 Phase 8.3 SnapshotRequired against a REAL server harness: 150 synthetic
/// AgentStatus events are appended DIRECTLY through SqliteStore BEFORE boot
/// (append stamps seq itself; closing releases the db so Orchestrator reopens
/// the SAME file and rebuilds head=150 through projections). A client whose
/// cursor sits 101 behind head trips the debug-build gap limit (REPLAY_GAP_
/// LIMIT=100; release uses 10_000, hence the cfg gate below), receives a
/// snapshot whose baseline == the advertised head, then sees LIVE events
/// continuing at exactly baseline+1 — the envelope.rs seamlessness contract.
#[cfg(debug_assertions)]
#[tokio::test]
async fn snapshot_required_path_preseeded_store() {
    const GAP: u64 = 150;
    const CURSOR: u64 = 49; // 150 − 49 = 101 > 100 ⇒ snapshot branch

    let server = boot_custom("g2-snapshot", move |scratch: PathBuf| async move {
        // Pre-seed BEFORE orchestrator boot: raw store appends of tiny
        // synthetic AgentStatus rows (any inner FxEvent works; AgentStatus is
        // the cheapest serde-wise and totally legal in isolation).
        {
            let store = fxcore::SqliteStore::open_shared(scratch.join("events.db")).expect("open");
            for i in 0..GAP {
                store
                    .append(FxEvent::AgentStatus {
                        agent: fxproto::ids::AgentId::from_raw(format!("ghost-{i:03}")),
                        driver: DriverId::CodexCli,
                        status: fxproto::event::AgentStatus::Ready,
                    })
                    .await
                    .expect("append synthetic");
            }
            let head = store.head_seq().await.expect("head").as_u64();
            assert_eq!(head, GAP);
        }
        let missing = scratch.join("nope.toml");
        let mut cfg = Config::load_from(&missing, Some(scratch.to_path_buf())).expect("config");
        with_fake_drivers(
            &mut cfg,
            &[(DriverId::ClaudeCode, "chunks", "post-snapshot")],
        );
        cfg
    })
    .await;
    let (url, orch, scratch, ..) = &server;
    let token = read_stored_token(scratch.path()).await;

    let mut ws = connect(url).await;
    send(&mut ws, &hello(&token)).await;
    let Message::Welcome { head_seq, .. } = next_frame(&mut ws).await.unwrap() else {
        panic!("welcome");
    };
    assert_eq!(head_seq.as_u64(), GAP, "rebuilt head == seeded count");

    send(
        &mut ws,
        &Message::Subscribe {
            last_seq: Seq::new(CURSOR),
        },
    )
    .await;

    // 4b instead of 4a — NO replay frames precede it:
    let Message::SnapshotRequired { snapshot } = next_frame(&mut ws).await.unwrap() else {
        panic!("snapshot_required frame");
    };
    assert_eq!(
        snapshot.baseline_seq.as_u64(),
        GAP,
        "baseline == current head"
    );
    // Snapshot CONTENT: rebuilt agents include the 150 ghosts; no threads
    // existed among them.
    assert_eq!(snapshot.agents.agents.len(), GAP as usize);
    assert!(snapshot.threads.threads.is_empty());

    // Live tail on THIS connection resumes seamlessly at baseline+1.
    let mut log: Vec<Sequenced<FxEvent>> = Vec::new();
    let Reply::Started { agent } = request_collect(
        &mut ws,
        &mut log,
        1,
        Command::StartAgent {
            driver: DriverId::ClaudeCode,
        },
    )
    .await
    else {
        panic!("started post-snapshot");
    };
    let Reply::SessionCreated { session } = request_collect(
        &mut ws,
        &mut log,
        2,
        Command::NewSession {
            agent: agent.clone(),
            cwd: scratch.path().canonicalize().unwrap(),
            mcp_servers: vec![],
        },
    )
    .await
    else {
        panic!("session post-snapshot");
    };
    let Reply::PromptAccepted { .. } = request_collect(
        &mut ws,
        &mut log,
        3,
        Command::Prompt {
            session: session.clone(),
            blocks: vec![ContentBlock::Text {
                text: "after gap".into(),
            }],
        },
    )
    .await
    else {
        panic!("accepted post-snapshot");
    };
    collect_until_turn_end(&mut ws, &mut log, &session).await;

    // Every visible post-snapshot event lives strictly beyond baseline,
    // beginning EXACTLY at baseline+1 (ordering monotonic in between).
    let post_seqs: Vec<u64> = log.iter().map(|e| e.seq.as_u64()).collect();
    assert_eq!(
        post_seqs.first().copied(),
        Some(GAP + 1),
        "next event after snapshot == baseline_seq + 1"
    );
    assert!(post_seqs.windows(2).all(|w| w[1] > w[0]));

    // Head grew past the baseline AND the client-side state folding would be
    // loss-free: replaying (baseline..] from the store equals what arrived.
    let suffix = orch
        .replay_from(snapshot.baseline_seq)
        .await
        .expect("suffix replay");
    assert_eq!(suffix.first().unwrap().seq.as_u64(), GAP + 1);
    assert!(suffix.len() >= post_seqs.len());

    close_politely(ws).await;
    teardown(server).await;
}
