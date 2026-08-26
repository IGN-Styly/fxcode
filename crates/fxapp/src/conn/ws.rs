//! THE ONLY FILE THAT KNOWS TOKIO EXISTS (docs/crates.md rule).
//!
//! Owns a small embedded tokio Runtime running async-tungstenite; bridges frames to
//! GPUI's executor via channels. If GPUI-native async ever suffices, only this file dies.
//!
//! Channel flavor DECIDED — FLUME, bounded. One-line why: flume receivers await
//! natively on ANY executor, so GPUI's smol-based side does zero-copy async recv while the
//! quarantined tokio runtime pushes with plain sync sends — the std-mpsc-plus-GPUI-timer
//! alternative would inject timer-tick latency into every streamed chunk and burn UI-thread
//! polls. (Same reasoning makes bounded-flume-as-oneshot right for conn/mod.rs replies.)
//!
//! Channel inventory (flume):
//!   out_tx: flume::bounded::<Message>(16)   GPUI → runtime. Commands are human-paced
//!                                           (Prompt/Cancel/PermissionResponse); 16 slots
//!                                           ≫ burst need. try_send Full ⇒ the link is
//!                                           effectively unusable ⇒ surface Err up to
//!                                           send() as Transport rather than queue blindly.
//!   in_rx:  flume::bounded::<WsEvent>(1024) runtime → GPUI. Capacity MATCHES the fxcore
//!                                           bus cap (~1024) so this bridge is never the
//!                                           artificial bottleneck; send().await
//!                                           backpressures tungstenite's reads → TCP backs
//!                                           up → fxserver's out-Full rule kicks us with
//!                                           Close("resubscribe"). Coherent.
//!
//! DEVIATION vs. the original channel sketch (documented): inbound items are NOT plain
//! `Message`. Protocol-level failures ride WS Close frames carrying reason strings
//! ("auth_failed" / "protocol_version" / "resubscribe") with NO envelope::Message shape —
//! yet conn/mod.rs MUST classify them (FAILURE CLASSIFICATION table). So in_rx items are
//! [`WsEvent`] (`Message | Closed(reason)`); outbound stays bare `Message`.
//!
//! Pump shape — TWO tasks per handle instead of three (adaptation note):
//!   T1 WRITE+KEEPALIVE owns out_rx and the shared sink half exclusively, and
//!      tokio::select!s between outgoing frames and the 20s tick (which pings
//!      and enforces the DEAD-PEER RULE against last_inbound_ms). Merging
//!      keepalive into the writer keeps exactly ONE owner of the sink, so
//!      dropping the handle deterministically tears the socket down instead of
//!      racing two holders over close().
//!   T2 READ owns the stream half and updates last_inbound_ms for EVERY inbound
//!      frame (Pong included — liveness counts all traffic). Decodes Text
//!      frames into envelope Messages, records RTT from Pong payloads (they
//!      echo our timestamp bytes), forwards Closed(reason) verbatim.
//!   Both tasks select! against a tokio watch<bool> shutdown: whichever decides
//!      the link is dead flips it, the other unwinds promptly, and its dropped
//!      half closes the socket. Dropping [`WsHandle`] ends T1 via its out_rx
//!      going Disconnected ⇒ T1 flips shutdown ⇒ T2 follows.

use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures::{SinkExt, StreamExt};
use tokio::runtime::Runtime;

use fxproto::envelope::Message;

// DEVIATION vs. an idiomatic `async_tungstenite::tokio::connect_async` call (documented):
// the fixed workspace dep list pulls async-tungstenite WITHOUT its `tokio-runtime`
// feature, so the pre-built TokioAdapter module is compiled out. Rather than editing
// any Cargo.toml, this file ships a ~80-line futures-IO ↔ tokio-IO bridge below —
// still fully quarantined here. Plaintext (`ws://`) only: fxserver's documented stance
// is Tailscale/no-TLS, so `wss://` dials fail loudly rather than pretending.

use std::{
    io,
    pin::Pin,
    task::{Context as IoContext, Poll},
};

/// Bridge so tokio sockets can feed async-tungstenite's runtime-generic core.
#[derive(Debug)]
struct TokioIoCompat(tokio::net::TcpStream);

impl futures::AsyncRead for TokioIoCompat {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut IoContext<'_>,
        dst: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        use tokio::io::AsyncRead as _;
        let mut buf = tokio::io::ReadBuf::new(dst);
        if let Poll::Ready(result) = Pin::new(&mut self.0).poll_read(cx, &mut buf) {
            result?;
            return Poll::Ready(Ok(buf.filled().len()));
        }
        Poll::Pending
    }
}

impl futures::AsyncWrite for TokioIoCompat {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut IoContext<'_>,
        src: &[u8],
    ) -> Poll<io::Result<usize>> {
        use tokio::io::AsyncWrite as _;
        Pin::new(&mut self.0).poll_write(cx, src)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut IoContext<'_>) -> Poll<io::Result<()>> {
        use tokio::io::AsyncWrite as _;
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut IoContext<'_>) -> Poll<io::Result<()>> {
        // futures' shutdown hook == tokio's half-close.
        use tokio::io::AsyncWrite as _;
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// Default dial port — MUST stay in lockstep with fxserver's
/// ifaddr::DEFAULT_PORT (golden port; single drift risk, noted on both sides).
pub const DEFAULT_PORT: u16 = 8949;

/// KEEPALIVE cadence: 20s keeps NAT/tailscale mappings warm at negligible cost.
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// DEAD-PEER RULE: no inbound frame OF ANY KIND (Pong included) for 60s (= 3
/// intervals) ⇒ declare death. Tolerates 2 lost probes before giving up.
const DEAD_PEER_MS: u64 = 60_000;
/// Outstanding-ping history cap (RTT matching table never grows unboundedly).
const PING_HISTORY_CAP: usize = 16;

// ---------------------------------------------------------------------------
// Normalized URL
// ---------------------------------------------------------------------------

/// A validated `ws://host[:port]/ws` address produced by [`normalize_url`].
/// Carries its pieces because plain-TCP dialing needs them separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    /// Canonical rendering kept precomputed (Display/as_str never re-parse).
    wire: String,
    /// Canonical lowercase wire scheme ("ws" only until TLS exists — see the
    /// DEVIATION note; wss parses but refuses to DIAL).
    scheme: String,
    /// Host WITHOUT brackets (IPv6 literals render brackets in Display).
    host: String,
    port: u16,
}

impl Url {
    pub fn as_str(&self) -> &str {
        &self.wire
    }

    pub fn is_wss(&self) -> bool {
        self.scheme == "wss"
    }

    /// What a TCP socket dials: (hostname-or-literal, port).
    pub fn dial_target(&self) -> (&str, u16) {
        (&self.host, self.port)
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}:{}/ws",
            self.scheme,
            render_host(&self.host),
            self.port
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlError {
    /// Scheme not exactly `ws`/`wss` (missing scheme, http/https, anything else).
    /// NEVER guessed or auto-prepended: silently defaulting the scheme risks
    /// pointing a pairing token at a plaintext endpoint the user did not type.
    UnsupportedScheme { got: String },
    /// Host part missing/empty (also userinfo present — rejected outright so
    /// pasted credentials never ship around unnoticed).
    MissingHost,
    /// Any path beyond ""|"/", any query `?…`, any fragment `#…`, bad port.
    BadPath { path: String },
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UrlError::UnsupportedScheme { got } => write!(
                f,
                "unsupported address scheme {got:?} — must start with ws:// or wss://"
            ),
            UrlError::MissingHost => write!(f, "missing host — expected ws://<host>[:<port>]"),
            UrlError::BadPath { path } => {
                write!(f, "unsupported location {path:?} — only /ws is served")
            }
        }
    }
}

/// Runs BEFORE dialing; ConnectScreen renders the Err message verbatim.
///
/// Rules (one accept + one reject row per rule in the tests below):
///   - scheme MUST be exactly `ws` or `wss` (ASCII case-insensitive); anything
///     else is Err, NEVER guessed or auto-prepended.
///   - host REQUIRED (IPv4/IPv6 literal/hostname; empty ⇒ Err).
///   - port OPTIONAL: absent ⇒ DEFAULT_PORT substitution.
///   - path ""|"/"|"/ws" normalize to "/ws"; any other path/query/fragment ⇒ Err.
pub fn normalize_url(input: &str) -> Result<Url, UrlError> {
    let input = input.trim();

    let Some((scheme_raw, tail)) = input.split_once(':') else {
        return Err(UrlError::UnsupportedScheme {
            got: "<none>".into(),
        });
    };
    if !scheme_raw.eq_ignore_ascii_case("ws") && !scheme_raw.eq_ignore_ascii_case("wss") {
        return Err(UrlError::UnsupportedScheme {
            got: scheme_raw.to_string(),
        });
    }

    // A scheme IS followed by an authority here, which requires `//`.
    let Some(after_slashes) = tail.strip_prefix("//") else {
        return Err(UrlError::BadPath {
            path: tail.to_string(),
        });
    };

    // Reject query/fragment outright, wherever they appear.
    if after_slashes.contains('?') || after_slashes.contains('#') {
        return Err(UrlError::BadPath {
            path: after_slashes.to_string(),
        });
    }

    // Authority runs until the first '/'; the remainder is the path.
    let (authority, path) = match after_slashes.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (after_slashes, String::from("/")),
    };

    // Userinfo (`user:pass@host`) rejected — a token pasted into the host
    // field must not be silently shipped around.
    if authority.contains('@') {
        return Err(UrlError::MissingHost);
    }

    let (bracketed_host, port_part) = split_authority(authority)?;

    let host = bracketed_host
        .trim_matches(|c| c == '"' || c == '\'')
        .trim();
    if host.is_empty() {
        return Err(UrlError::MissingHost);
    }

    let port: u16 = match port_part {
        Some(p) if !p.is_empty() => p.parse().map_err(|_| UrlError::BadPath {
            path: format!(":{p}"),
        })?,
        _ => DEFAULT_PORT, // absent OR explicitly empty (`host:`)
    };

    match path.as_str() {
        // Accept "", "/" AND an explicitly-typed "/ws" (idempotent normalization).
        // Interpretation note vs the sketch's "anything else rejected": users will
        // paste the exact endpoint fxserver prints; refusing it would be hostile.
        "/" | "/ws" => {
            let scheme = scheme_raw.to_ascii_lowercase();
            let url = Url {
                wire: format!("{}://{}:{port}/ws", scheme, render_host(host)),
                scheme,
                host: host.to_string(),
                port,
            };
            Ok(url)
        }
        other => Err(UrlError::BadPath {
            path: other.to_string(),
        }),
    }
}

fn render_host(host: &str) -> String {
    // IPv6 literals carry brackets; regular hosts do not.
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// Returns `(host-part-WITHOUT-brackets, Option<port-text>)`.
fn split_authority(authority: &str) -> Result<(&str, Option<&str>), UrlError> {
    // Bracketed IPv6 literal FIRST: the colon after ']' (if any) splits the port.
    if let Some(inner) = authority.strip_prefix('[') {
        let (host, rest) = inner.split_once(']').ok_or(UrlError::MissingHost)?;
        return match rest {
            "" => Ok((host, None)),
            _ => match rest.strip_prefix(':') {
                Some(port) => Ok((host, Some(port))),
                None => Err(UrlError::MissingHost), // junk after the bracket
            },
        };
    }
    Ok(match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    })
}

// ---------------------------------------------------------------------------
// Handle surface
// ---------------------------------------------------------------------------

/// What the read pump hands GPUI-side consumers.
#[derive(Clone, Debug)]
pub enum WsEvent {
    /// A decoded protocol frame.
    Message(Message),
    /// Server sent a WS Close carrying its reason string ("auth_failed",
    /// "protocol_version", "resubscribe"). Classification is conn/mod.rs's job.
    /// The recv stream ends right after this event (all senders drop).
    Closed(Option<String>),
}

#[derive(Debug)]
pub struct DialError(pub String);

impl fmt::Display for DialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<async_tungstenite::tungstenite::Error> for DialError {
    fn from(err: async_tungstenite::tungstenite::Error) -> Self {
        Self(err.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrySendError {
    Full,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("connection closed")
    }
}

/// The concrete WS socket this client runs over (tokio TCP bridged via the
/// local compat adapter — see the DEVIATION note at the top of the file).
type RawSocket = async_tungstenite::WebSocketStream<TokioIoCompat>;

type RawSink = futures::stream::SplitSink<RawSocket, async_tungstenite::tungstenite::Message>;

/// Clean handle over one connection: what conn/mod.rs holds while Ready.
///
/// Dropping WsHandle drops the channels ⇒ pump tasks see disconnect and close
/// the WS. No explicit close() API needed for v0.
pub struct WsHandle {
    out_tx: flume::Sender<Message>,  // main → runtime task
    in_rx: flume::Receiver<WsEvent>, // runtime task → main
    rtt_ms: Arc<AtomicU64>,          // latest ping RTT for the M0 latency badge
    broken: Arc<AtomicBool>,         // set by pumps on fatal socket error
}

impl WsHandle {
    /// Connect + return handle. DNS/TCP/WS-upgrade errors surface as Err HERE;
    /// protocol-level auth/version failures arrive via inbound events as
    /// [`WsEvent::Closed`] — classification is CONN/MOD.RS's job.
    ///
    /// The dial itself is synchronous (blocking); callers run it off the UI
    /// thread (conn/mod.rs parks it on its background executor).
    pub fn connect(url: &Url) -> Result<Self, DialError> {
        let target = url.clone();
        let (out_tx, out_rx) = flume::bounded::<Message>(16);
        let (in_tx, in_rx) = flume::bounded::<WsEvent>(1024);
        let rtt_ms = Arc::new(AtomicU64::new(0));
        let broken = Arc::new(AtomicBool::new(false));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Clones taken BEFORE the move-closure so the handle keeps its own Arcs.
        let pumps_rtt_ms = rtt_ms.clone();
        let pumps_broken = broken.clone();
        let pumps_shutdown_tx = shutdown_tx.clone();
        let pumps_shutdown_rx = shutdown_rx.clone();

        runtime().block_on(async move {
            if target.is_wss() {
                // fxserver runs Tailscale/plaintext by design; no TLS backend is
                // wired into the fixed dep list. Fail loudly instead of guessing.
                return Err(DialError(
                    "wss:// addresses are not supported — fxserver is plaintext/Tailscale-only"
                        .to_string(),
                ));
            }
            let (host, port) = target.dial_target();
            let tcp = tokio::net::TcpStream::connect((host, port))
                .await
                .map_err(|error| DialError(error.to_string()))?;
            let (socket, _response) =
                async_tungstenite::client_async(target.as_str(), TokioIoCompat(tcp)).await?;
            spawn_pumps(
                socket,
                out_rx,
                in_tx,
                pumps_rtt_ms,
                pumps_broken,
                pumps_shutdown_tx,
                pumps_shutdown_rx,
            );
            Ok::<_, DialError>(())
        })?;

        Ok(Self {
            out_tx,
            in_rx,
            rtt_ms,
            broken,
        })
    }

    /// Queue an outgoing frame. Full ⇒ Err (link unusable); Disconnected ⇒ Err.
    pub fn try_send(&self, msg: Message) -> Result<(), TrySendError> {
        self.out_tx.try_send(msg).map_err(|err| match err {
            flume::TrySendError::Full(_) => TrySendError::Full,
            flume::TrySendError::Disconnected(_) => TrySendError::Disconnected,
        })
    }

    /// Cheap clone of the outbound conduit — callers push handshake frames
    /// (Hello before Ready) and keep them alive independently of try_send().
    pub fn sender(&self) -> flume::Sender<Message> {
        self.out_tx.clone()
    }

    /// Next inbound item; resolves when one arrives or when every pump has
    /// dropped their senders (Err ⇒ socket gone).
    pub async fn recv(&self) -> Result<WsEvent, RecvError> {
        self.in_rx.recv_async().await.map_err(|_| RecvError)
    }

    /// Latest measured ping RTT in ms (0 = no sample yet).
    pub fn rtt_ms(&self) -> u64 {
        self.rtt_ms.load(Ordering::Relaxed)
    }

    /// True once any pump hit a fatal socket condition.
    pub fn broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Runtime + pumps
// ---------------------------------------------------------------------------

/// ONE lazily-initialized Runtime for the whole process. Reconnects build
/// fresh WsHandles over the SAME runtime — never per-connection runtimes.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("failed to start embedded tokio runtime"))
}

#[allow(clippy::too_many_arguments)]
fn spawn_pumps(
    socket: RawSocket,
    out_rx: flume::Receiver<Message>,
    in_tx: flume::Sender<WsEvent>,
    rtt_ms: Arc<AtomicU64>,
    broken: Arc<AtomicBool>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    // Both pumps watch death independently; whichever unwinds first triggers
    // the other through a flip of the shared watch channel.
    let shutdown_tx_reader = shutdown_tx.clone();
    let shutdown_tx_writer = shutdown_tx;
    let shutdown_rx_writer = shutdown_rx.clone();
    let shutdown_rx_reader = shutdown_rx;
    let (sink_half, stream_half) = socket.split();
    let sink: Arc<tokio::sync::Mutex<RawSink>> = Arc::new(tokio::sync::Mutex::new(sink_half));
    let last_inbound_ms = Arc::new(AtomicU64::new(now_millis()));
    let pending_pings: Arc<tokio::sync::Mutex<HashMap<Vec<u8>, Instant>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // ---------------- T1: WRITE + KEEPALIVE --------------------------------
    // Encode envelope → Text frames; encode/socket errors flip broken and shut
    // everything down.
    {
        let sink = Arc::clone(&sink);
        let broken = broken.clone();
        let last_inbound_ms = last_inbound_ms.clone();
        let pending_pings = Arc::clone(&pending_pings);
        let mut shutdown_rx = shutdown_rx_writer;
        let shutdown_tx = shutdown_tx_writer;
        let _task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(PING_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut reason = "writable side finished".to_string();

            'writer: loop {
                let action = tokio::select! {
                    _ = shutdown_rx.changed() => Action::Stop,
                    item = out_rx.recv_async() => match item {
                        Ok(msg) => Action::Send(msg),
                        Err(_) => Action::Stop, // handle dropped → close the WS
                    },
                    _ = tick.tick() => Action::KeepaliveTick,
                };

                match action {
                    Action::Send(msg) => match serde_json::to_string(&msg) {
                        Ok(json) => {
                            if sink
                                .lock()
                                .await
                                .send(async_tungstenite::tungstenite::Message::Text(json))
                                .await
                                .is_err()
                            {
                                broken.store(true, Ordering::SeqCst);
                                reason = "socket write failed".to_string();
                                break 'writer;
                            }
                        }
                        Err(err) => {
                            tracing::error!(error = %err, "undecodable outbound frame");
                            broken.store(true, Ordering::SeqCst);
                            break 'writer;
                        }
                    },
                    Action::KeepaliveTick => {
                        let elapsed =
                            now_millis().saturating_sub(last_inbound_ms.load(Ordering::SeqCst));
                        if dead_peer_elapsed(elapsed) {
                            tracing::warn!("no inbound frames for 60s; declaring peer dead");
                            broken.store(true, Ordering::SeqCst);
                            reason = "dead peer".to_string();
                            break 'writer;
                        }
                        let payload = encode_ping_payload();
                        trim_ping_history(&mut *pending_pings.lock().await);
                        pending_pings
                            .lock()
                            .await
                            .insert(payload.clone(), Instant::now());
                        if sink
                            .lock()
                            .await
                            .send(async_tungstenite::tungstenite::Message::Ping(
                                payload.clone(),
                            ))
                            .await
                            .is_err()
                        {
                            broken.store(true, Ordering::SeqCst);
                            reason = "keepalive ping failed".to_string();
                            break 'writer;
                        }
                    }
                    Action::Stop => break 'writer,
                }
            }

            tracing::debug!(reason = %reason, "write pump exited");
            drop(sink); // our half gone; T2 handles the rest of teardown
            let _ = shutdown_tx.send(true);
        });
    }

    // ---------------- T2: READ ---------------------------------------------
    // Frame → envelope → in_tx. UNDECODABLE frame from our own server is an
    // integration bug, not client UX: error! + fatal. Server-initiated Pings
    // are answered by tungstenite automatically — no code here.
    {
        let broken = broken.clone();
        let last_inbound_ms = last_inbound_ms.clone();
        let pending_pings = Arc::clone(&pending_pings);
        let mut shutdown_rx = shutdown_rx_reader;
        let _task = tokio::spawn(async move {
            let mut stream = stream_half;
            'reader: loop {
                let next = tokio::select! {
                    _ = shutdown_rx.changed() => break 'reader,
                    next = stream.next() => next,
                };

                let message = match next {
                    Some(Ok(message)) => {
                        last_inbound_ms.store(now_millis(), Ordering::SeqCst);
                        message
                    }
                    Some(Err(err)) => {
                        tracing::debug!(error = %err, "ws read ended with error");
                        broken.store(true, Ordering::SeqCst);
                        break 'reader;
                    }
                    // Stream closed without a Close frame: EOF/reset path.
                    None => break 'reader,
                };

                match message {
                    async_tungstenite::tungstenite::Message::Text(text) => {
                        match serde_json::from_str::<Message>(&text) {
                            Ok(msg) => {
                                if in_tx.send_async(WsEvent::Message(msg)).await.is_err() {
                                    break 'reader; // consumer went away
                                }
                            }
                            Err(err) => {
                                tracing::error!(error = %err, "undecodable inbound frame");
                                broken.store(true, Ordering::SeqCst);
                                break 'reader;
                            }
                        }
                    }
                    async_tungstenite::tungstenite::Message::Pong(payload) => {
                        record_rtt(&pending_pings, payload.as_ref(), &rtt_ms).await;
                    }
                    async_tungstenite::tungstenite::Message::Close(frame) => {
                        let reason = frame.as_ref().map(|f| f.reason.to_string());
                        // Best-effort: if the consumer is gone nobody is left
                        // to hear about the close anyway.
                        let _ = in_tx.try_send(WsEvent::Closed(reason));
                        break 'reader;
                    }
                    // Binary/Ping frames carry no protocol meaning here.
                    _ => {}
                }
            }

            // `stream` (our owned stream half) drops here: teardown completes.
            let _ = shutdown_tx_reader.send(true);
        });
    }
}

enum Action {
    Send(Message),
    KeepaliveTick,
    Stop,
}

async fn record_rtt(
    pending_pings: &tokio::sync::Mutex<HashMap<Vec<u8>, Instant>>,
    payload: &[u8],
    rtt_ms: &Arc<AtomicU64>,
) {
    let mut pings = pending_pings.lock().await;
    if let Some(sent_at) = pings.remove(payload) {
        rtt_ms.store(
            u64::try_from(sent_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

fn trim_ping_history(pings: &mut HashMap<Vec<u8>, Instant>) {
    if pings.len() < PING_HISTORY_CAP {
        return;
    }
    // Keep the NEWEST half by probe time (HashMap iteration order alone would be
    // nondeterministic); RTT matching only needs recent probes anyway.
    let mut by_age: Vec<(Vec<u8>, Instant)> = pings
        .iter()
        .map(|(key, sent_at)| (key.clone(), *sent_at))
        .collect();
    by_age.sort_by_key(|(_, sent_at)| *sent_at);
    let stale_count = pings.len() - PING_HISTORY_CAP / 2;
    for (key, _) in by_age.into_iter().take(stale_count) {
        pings.remove(&key);
    }
}

/// Monotonic process clock in milliseconds for keepalive bookkeeping.
fn now_millis() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Pure seam extracted from the keepalive task so the rule is unit-testable
/// without fake clocks: elapsed-since-last-inbound ≥ DEAD_PEER_MS declares death.
fn dead_peer_elapsed(elapsed_since_last_inbound_ms: u64) -> bool {
    elapsed_since_last_inbound_ms >= DEAD_PEER_MS
}

fn encode_ping_payload() -> Vec<u8> {
    now_millis().to_le_bytes().to_vec()
}

// ---------------------------------------------------------------------------
// Tests — pure helpers + pump validation against a LOCAL echo server (no
// fxserver needed yet, no axum: bare async-tungstenite over a tokio
// TcpListener inside the embedded runtime).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use fxproto::envelope::PROTO_VERSION;

    // ---------------- normalize_url table --------------------------------

    fn accept(input: &str) -> String {
        match normalize_url(input) {
            Ok(url) => url.to_string(),
            Err(err) => panic!("{input:?} should be accepted, got {err}"),
        }
    }

    fn expect(input: &str, err: &UrlError) {
        assert_eq!(normalize_url(input).err().as_ref(), Some(err), "{input:?}");
    }

    #[test]
    fn accepts_canonical_address_unchanged() {
        assert_eq!(accept("ws://127.0.0.1:8949/ws"), "ws://127.0.0.1:8949/ws");
    }

    #[test]
    fn defaults_port_and_normalizes_empty_path() {
        assert_eq!(accept("ws://localhost"), "ws://localhost:8949/ws");
        assert_eq!(accept("ws://localhost/"), "ws://localhost:8949/ws");
        assert_eq!(accept("WS://LOCALHOST"), "ws://LOCALHOST:8949/ws");
    }

    #[test]
    fn keeps_wss_scheme_and_explicit_port() {
        assert_eq!(
            accept("wss://fx.example.com:9443"),
            "wss://fx.example.com:9443/ws"
        );
    }

    #[test]
    fn brackets_ipv6_literals() {
        assert_eq!(accept("ws://[::1]:7000"), "ws://[::1]:7000/ws");
        assert_eq!(accept("ws://[::1]"), "ws://[::1]:8949/ws");
    }

    #[test]
    fn rejects_missing_or_foreign_schemes() {
        // The classic wrong-shape input "host:port": split once reads the host
        // as a scheme. Never guessed.
        expect(
            "localhost:8949",
            &UrlError::UnsupportedScheme {
                got: "localhost".into(),
            },
        );
        expect(
            "//127.0.0.1:8949",
            &UrlError::UnsupportedScheme {
                got: "//127.0.0.1".into(),
            },
        );
        expect(
            "http://127.0.0.1:8949/ws",
            &UrlError::UnsupportedScheme { got: "http".into() },
        );
        expect(
            "https://127.0.0.1:8949/ws",
            &UrlError::UnsupportedScheme {
                got: "https".into(),
            },
        );
    }

    #[test]
    fn rejects_missing_host() {
        expect("ws://", &UrlError::MissingHost);
        expect("ws:///ws", &UrlError::MissingHost);
        expect("ws://user@127.0.0.1", &UrlError::MissingHost); // userinfo never shipped silently
        expect("ws://[::1", &UrlError::MissingHost); // unclosed bracket
    }

    #[test]
    fn rejects_bad_paths_queries_fragments_ports() {
        expect(
            "ws://127.0.0.1:8949/app",
            &UrlError::BadPath {
                path: "/app".into(),
            },
        );
        expect(
            "ws://127.0.0.1?token=1",
            &UrlError::BadPath {
                path: "127.0.0.1?token=1".into(),
            },
        );
        expect(
            "ws://127.0.0.1/#frag",
            &UrlError::BadPath {
                path: "127.0.0.1/#frag".into(),
            },
        );
        expect(
            "ws://127.0.0.1:notaport/ws",
            &UrlError::BadPath {
                path: ":notaport".into(),
            },
        );
    }

    #[test]
    fn explicit_empty_port_reads_as_default_port() {
        assert_eq!(accept("ws://127.0.0.1:/"), "ws://127.0.0.1:8949/ws");
    }

    // ---------------- dead-peer + ping bookkeeping -----------------------

    #[test]
    fn dead_peer_rule_bounds_elapsed_silence() {
        assert!(!dead_peer_elapsed(0));
        assert!(!dead_peer_elapsed(DEAD_PEER_MS - 1));
        assert!(dead_peer_elapsed(DEAD_PEER_MS));
    }

    #[test]
    fn ping_history_trim_keeps_recent_half() {
        let start = Instant::now();
        let mut pings: HashMap<Vec<u8>, Instant> = HashMap::new();
        for i in 0..(PING_HISTORY_CAP as u64) {
            // Monotonic probe times so the trim's sort is deterministic.
            pings.insert(
                i.to_le_bytes().to_vec(),
                start + Duration::from_millis(i * 10),
            );
        }
        trim_ping_history(&mut pings);
        assert_eq!(pings.len(), PING_HISTORY_CAP / 2);
        // Oldest half gone.
        for i in 0..(PING_HISTORY_CAP / 2) as u64 {
            let key = i.to_le_bytes();
            assert!(
                !pings
                    .keys()
                    .any(|stored| stored.as_slice() == key.as_slice()),
                "oldest half must be gone"
            );
        }
    }

    #[test]
    fn ping_payload_round_trips_via_rtt_table() {
        let table = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let rtt = Arc::new(AtomicU64::new(0));
        let payload = encode_ping_payload();

        runtime().block_on({
            let table = table.clone();
            let rtt = rtt.clone();
            async move {
                table.lock().await.insert(payload.clone(), Instant::now());
                record_rtt(&table, payload.as_ref(), &rtt).await;
            }
        });
        assert!(rtt.load(Ordering::Relaxed) <= DEAD_PEER_MS);
    }

    // ---------------- pump behavior vs. local echo server -----------------

    type RawServerStream = RawSocket;

    enum ServerBehavior {
        Echo,
        CloseWith(&'static str),
    }

    /// Bind a TCP listener NOW (sync), then run the WS server task on the same
    /// embedded runtime the client pumps use.
    async fn serve(listener: std::net::TcpListener, behavior: ServerBehavior) {
        listener
            .set_nonblocking(true)
            .expect("listener handed to tokio");
        let listener = tokio::net::TcpListener::from_std(listener).expect("register with runtime");
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let mut ws: RawServerStream =
                match async_tungstenite::accept_async(TokioIoCompat(stream)).await {
                    Ok(ws) => ws,
                    Err(_) => return,
                };

            use async_tungstenite::tungstenite::Message as Ws;
            loop {
                if let ServerBehavior::CloseWith(reason) = behavior {
                    let _ = ws
                        .close(Some(async_tungstenite::tungstenite::protocol::CloseFrame {
                            code: 1000.into(),
                            reason: reason.into(),
                        }))
                        .await;
                    break;
                }
                match ws.next().await {
                    Some(Ok(Ws::Text(text))) => {
                        if ws.send(Ws::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Ws::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        });
    }

    fn bind_local() -> std::net::TcpListener {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        listener
            .set_nonblocking(true)
            .expect("listener handed to tokio");
        listener
    }

    fn local_url(addr: std::net::SocketAddr) -> Url {
        normalize_url(&format!("ws://{addr}/ws")).expect("local address normalizes")
    }

    #[test]
    fn echo_round_trip_pumps_both_directions() {
        let listener = bind_local();
        let addr = listener.local_addr().unwrap();
        runtime().block_on(async {
            serve(listener, ServerBehavior::Echo).await;
        });

        let handle = WsHandle::connect(&local_url(addr)).expect("dial local echo server");

        handle
            .try_send(Message::Hello {
                proto_version: PROTO_VERSION,
                token: "pair-token".into(),
            })
            .expect("queued");
        match futures::executor::block_on(handle.recv()).expect_ok() {
            WsEvent::Message(Message::Hello {
                proto_version,
                token,
            }) => {
                assert_eq!(proto_version, PROTO_VERSION);
                assert_eq!(token.as_str(), "pair-token");
            }
            other => panic!("expected echoed Hello, got {other:?}"),
        }

        handle
            .try_send(Message::Subscribe {
                last_seq: fxproto::ids::Seq::new(7),
            })
            .expect("queued");
        match futures::executor::block_on(handle.recv()).expect_ok() {
            WsEvent::Message(Message::Subscribe { last_seq }) => {
                assert_eq!(last_seq, fxproto::ids::Seq::new(7));
            }
            other => panic!("expected echoed Subscribe, got {other:?}"),
        }

        assert!(!handle.broken());
    }

    #[test]
    fn server_close_reason_surfaces_through_recv() {
        let listener = bind_local();
        let addr = listener.local_addr().unwrap();
        runtime().block_on(async {
            serve(listener, ServerBehavior::CloseWith("auth_failed")).await;
        });

        let handle = WsHandle::connect(&local_url(addr)).expect("dial succeeded pre-close");

        match futures::executor::block_on(handle.recv()).expect_ok() {
            WsEvent::Closed(Some(reason)) => assert_eq!(reason, "auth_failed"),
            other => panic!("expected Closed(Some(auth_failed)), got {other:?}"),
        }
        // After the close every sender is dropped: recv ends.
        assert!(matches!(
            futures::executor::block_on(handle.recv()),
            Err(RecvError)
        ));
    }

    #[test]
    fn dial_failure_surfaces_here_not_via_events() {
        // Bind then drop: port briefly refuses connections — real dial error
        // without any network dependency.
        let listener = bind_local();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let url = local_url(addr);
        let outcome = WsHandle::connect(&url);
        assert!(outcome.is_err());
    }

    /// Tiny sugar to keep assertions readable; recv errors are asserted
    /// explicitly at the one place they are expected.
    trait ExpectOk {
        fn expect_ok(self) -> WsEvent;
    }
    impl ExpectOk for Result<WsEvent, RecvError> {
        fn expect_ok(self) -> WsEvent {
            self.expect("recv should deliver an event")
        }
    }
}
