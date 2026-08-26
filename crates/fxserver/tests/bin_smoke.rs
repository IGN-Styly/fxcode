//! REAL-BINARY smoke tests — the entire production boot path, end to end:
//! spawns the actual fxserver executable (`CARGO_BIN_EXE`), parses the pairing
//! token from the child's stderr (pair.rs print-once contract), and drives raw
//! WebSocket clients over real TCP. Exists because in-process router tests
//! cannot catch arg-parsing/env/token-file/binding drift — the exact class of
//! live bug reported ("binds nothing reachable", "protocol_version mismatch
//! with the right ip").
//!
//! Port strategy: bind+drop a probe listener to grab a free ephemeral port,
//! then hand `--bind 127.0.0.1:<that port>` to the child. The TOCTOU window is
//! real but tiny; the assert on the child's own "listening on" log line pins
//! whichever port we passed, so a collision fails loudly instead of silently.

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use async_tungstenite::tungstenite::Message as Ws;
use futures::StreamExt;
use fxproto::envelope::{Message, PROTO_VERSION};
use fxproto::ids::Seq;

const STARTUP_BUDGET: Duration = Duration::from_secs(30);
const STEP_BUDGET: Duration = Duration::from_secs(10);

type Session = async_tungstenite::WebSocketStream<async_tungstenite::tokio::ConnectStream>;

struct Server {
    child: Child,
    _drainer: std::thread::JoinHandle<()>,
    lines: mpsc::Receiver<String>,
    pub port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("probe listener binds")
}

fn scratch_dir() -> String {
    let dir = std::env::temp_dir().join(format!(
        "fxserver-smoke-{}-{}",
        std::process::id(),
        free_port()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("fxcode");
    std::fs::create_dir_all(&data).expect("scratch data dir");
    dir.to_string_lossy().into_owned()
}

fn spawn_server() -> Server {
    let exe = env!("CARGO_BIN_EXE_fxserver");
    let scratch = scratch_dir();
    let port = free_port();

    let mut child = std::process::Command::new(exe)
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .env("XDG_DATA_HOME", &scratch)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fxserver binary must spawn");

    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<String>();
    let drainer = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break; // test dropped Server; pipe closed
            }
        }
    });

    Server {
        child,
        _drainer: drainer,
        lines: rx,
        port,
    }
}

/// Wait for the pairing-token stderr line; extract its 64-hex payload.
/// Also asserts the child reports OUR bind address on its own listening line —
/// catches any drift between CLI parsing and the actual bind (the live-bug).
fn await_token_and_confirm_bind(server: &Server) -> String {
    let deadline = STARTUP_BUDGET;
    loop {
        match server.lines.recv_timeout(deadline) {
            Ok(line) => {
                if line.contains("listening on") {
                    assert!(
                        line.contains(&format!("127.0.0.1:{}", server.port)),
                        "child must report OUR bind addr, got: {line}"
                    );
                    continue;
                }
                if line.contains("pairing token") {
                    return extract_hex64(&line);
                }
            }
            Err(_) => panic!("no pairing token within {deadline:?}; server failed to boot or bind"),
        }
    }
}

fn extract_hex64(line: &str) -> String {
    // Token line shape (pair.rs): "... pairing token ... : <64 hex>".
    line.split_whitespace()
        .rev()
        .find(|w| w.len() == 64 && w.chars().all(|c| c.is_ascii_hexdigit()))
        .expect("token payload present on stderr line")
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn real_binary_full_cycle_over_real_socket() {
    let server = spawn_server();
    let token = await_token_and_confirm_bind(&server);

    // ── 1) happy handshake through the REAL process ─────────────────────────
    // Hello(PROTO_VERSION) → Welcome → Subscribe → correlated command Reply.
    let mut ws = dial(server.port).await;

    send(
        &mut ws,
        Message::Hello {
            proto_version: PROTO_VERSION,
            token: token.clone(),
        },
    )
    .await;
    expect_welcome_head_zero(&mut ws).await;

    send(
        &mut ws,
        Message::Subscribe {
            last_seq: Seq::new(0),
        },
    )
    .await;

    send(
        &mut ws,
        Message::Request {
            id: 1,
            command: fxproto::command::Command::DetectAgents,
        },
    )
    .await;
    match next_frame(&mut ws).await.expect("reply frame") {
        Message::Response { id: 1, .. } => {}
        other => panic!("expected correlated Response id=1, got {other:?}"),
    }

    // ── 2) WRONG VERSION from a fresh client ⇒ close reason protocol_version ──
    let mut bad_version = dial(server.port).await;
    send(
        &mut bad_version,
        Message::Hello {
            proto_version: PROTO_VERSION + 1,
            token: token.clone(),
        },
    )
    .await;
    assert_close_reason(&mut bad_version, "protocol_version").await;

    // ── 3) BAD TOKEN ⇒ close reason auth_failed ────────────────────────────────
    let mut nope = dial(server.port).await;
    send(
        &mut nope,
        Message::Hello {
            proto_version: PROTO_VERSION,
            token: "f".repeat(64),
        },
    )
    .await;
    assert_close_reason(&mut nope, "auth_failed").await;
}

// ---- tiny ws helpers --------------------------------------------------------

async fn dial(port: u16) -> Session {
    let url = format!("ws://127.0.0.1:{port}/ws");
    // Boot race: the token prints BEFORE net::serve binds the listener
    // (main.rs steps 6 vs 8), so poll instead of dial-once.
    let deadline = STARTUP_BUDGET;
    let started = std::time::Instant::now();
    loop {
        match async_tungstenite::tokio::connect_async(url.clone()).await {
            Ok((ws, _resp)) => return ws,
            Err(err) if started.elapsed() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = err; // refused/pre-upgrade responses are expected early
            }
            Err(err) => panic!("server never accepted within {deadline:?}: {err}"),
        }
    }
}

async fn send(ws: &mut Session, msg: Message) {
    let text = serde_json::to_string(&msg).expect("envelope serializes");
    ws.send(Ws::Text(text))
        .await
        .expect("frame send after upgrade");
}

/// Same idiom as e2e.rs's next_frame: one envelope per text frame; Close
/// surfaces as an Err(reason) so mismatch tests can branch on it.
async fn next_frame(ws: &mut Session) -> Result<Message, String> {
    loop {
        let ws = tokio::time::timeout(STEP_BUDGET, ws.next())
            .await
            .expect("frame within budget")
            .expect("stream not ended unexpectedly")
            .expect("ws transport ok");
        match ws {
            Ws::Text(t) => {
                return Ok(serde_json::from_str(&t).expect("server speaks our envelope"));
            }
            Ws::Close(frame) => {
                return Err(frame.map(|f| f.reason.to_string()).unwrap_or_default());
            }
            Ws::Ping(_) | Ws::Pong(_) => continue,
            other => panic!("unexpected transport frame: {other:?}"),
        }
    }
}

async fn expect_welcome_head_zero(ws: &mut Session) {
    match next_frame(ws).await {
        Ok(Message::Welcome { head_seq, .. }) if head_seq.as_u64() == 0 => {}
        other => panic!("expected Welcome(head=0), got {other:?}"),
    }
}

async fn assert_close_reason(ws: &mut Session, want: &'static str) {
    let got = loop {
        match next_frame(ws).await {
            Err(reason) => break reason,
            Ok(_) => continue, // ignore stragglers before the close lands
        }
    };
    assert_eq!(got, want, "close reason contract");
}
