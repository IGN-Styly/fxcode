//! Handshake + subscription replay. The security boundary lives here.
//!
//! Subscription is ENVELOPE-LEVEL ONLY (locked decision): the client sends
//! Message::Subscribe { last_seq } right after Welcome; Command has no Subscribe
//! variant and Reply::Subscribed does not exist. This module owns the whole
//! Hello→Welcome→Subscribe→replay|snapshot branch; client.rs starts clean.
//!
//! Byte-level frame sequence (ONE WS text frame = one serde_json Message):
//!     1. C→S Hello { proto_version, token }        FIRST frame ONLY
//!     2a. S→C Welcome { server_version, head_seq }
//!     2b. version/token failure                    => WS Close, reason below
//!     3. C→S Subscribe { last_seq }                EXACTLY ONCE, next frame
//!     4a. gap ≤ REPLAY_GAP_LIMIT => Event×k replay (via AuthedClient) + live
//!     4b. gap > limit            => SnapshotRequired{snapshot} INSTEAD of replay
//! Post-handshake Subscribe attempts are FAIL-V2 but fire in net/client.rs
//! (READER treats every non-Request envelope as protocol violation).
//!
//! Close transport: WS Close frame carrying one of the EXACT reason strings
//! canonized in fxproto/src/envelope.rs ("auth_failed", "protocol_version",
//| plus "resubscribe" downstream), then drop the socket. Deliberately NO error
//! Reply variant exists — clients match the string for UX (fxapp views/connect.rs).
//! REASON_INTERNAL below is the single UNPINNED extra: it fires only AFTER auth
//! succeeded, when OUR OWN backing state broke; its payload leaks nothing.
//!
//! Testability split: [`run`] is GENERIC over sink/stream halves (any Sink<
//! ws-Message>/Stream pair), so unit tests drive it through two mpsc channels
//! instead of sockets. [`StateSource`] abstracts the fxcore calls (snapshot /
//! replay / subscribe) behind BoxFutures — mirroring EventStore's no-
//! #[async_trait] style — letting MockStateSource control replay data, bus
//! emission timing, and failure injection directly.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt, future::BoxFuture};
use subtle::{Choice, ConstantTimeEq};
use tracing::{debug, error, warn};

use fxcore::{BusError, BusReceiver, Orchestrator};
use fxproto::envelope::{Message, PROTO_VERSION, Snapshot};
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::Seq;

/// Server-side audit constant — NOT part of the pinned client-facing trio.
pub const REASON_INTERNAL: &str = "internal";
/// Pinned strings (envelope.rs) re-declared here as THE single spelling site.
pub const REASON_AUTH_FAILED: &str = "auth_failed";
pub const REASON_PROTOCOL_VERSION: &str = "protocol_version";

/// Per-stage wait budget: connect→Hello, Welcome→Subscribe (FAIL-T).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Gap threshold (EVENT COUNT): head_seq − last_seq beyond this => 4b.
///   dev/debug builds: 100 — tiny enough that impl.md Phase 8.3's "force
///     SnapshotRequired" happens by generating a burst locally.
///   release builds: 10_000 — replaying ≤10k small JSON frames is well under a
///     second from SQLite; beyond that a whole-state snapshot beats the stream.
/// Upgrade path: promote to Config::replay_gap_limit (flagged in fxcore) with
/// these as defaults; env override NOT planned.
pub const REPLAY_GAP_LIMIT: u64 = if cfg!(debug_assertions) { 100 } else { 10_000 };

const TOKEN_COMPARE_BUF: usize = 128;
/// Close sub-codes: strings carry the CONTRACT, codes aid tooling (RFC 6455:
/// 1002 protocol error / 1008 policy violation).
const CLOSE_CODE_PROTOCOL: u16 = 1002;
const CLOSE_CODE_POLICY: u16 = 1008;

type Wsf = axum::extract::ws::Message;

// ── Types ────────────────────────────────────────────────────────────────────

/// Which exchange blew up — pure audit/logging metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    AwaitingHello,
    AwaitingSubscribe,
    Attach,
}

/// Exhaustive-failure carrier (envelope.rs table FAIL-T/J/V1/V2): the canonical
/// Close has ALREADY been emitted before this propagates; callers must NOT
/// double-close.
#[derive(Debug)]
pub struct HandshakeClosed {
    pub reason: &'static str,
    pub stage: Stage,
}

/// Pure DATA handoff produced HERE, consumed by net/client.rs (which owns every
/// drop of execution below).
pub struct AuthedClient {
    /// Replay drained BEFORE any live forward, ascending, strictly after cursor.
    /// Empty on the snapshot path (4b).
    pub replay: Vec<Message>,
    /// Bus subscription taken BEFORE snapshot/replay begins (subscribe-first
    /// closes every loss window: on the snapshot path, events landing in
    /// (baseline_seq, snapshot-time] would otherwise be invisible forever,
    /// because seq never regresses and no replay follows).
    pub bus_rx: BusReceiver,
    /// Live events already buffered while replay/snapshot ran. The WRITER's
    /// warmup drains these, skipping every event with seq ≤ high_water (replay
    /// tail overlaps bus head BY DESIGN); afterwards pure passthrough.
    pub pending: Vec<Sequenced<FxEvent>>,
    /// Merge seed, set HERE so client.rs needs zero protocol knowledge:
    ///   replay path   => max(cursor last_seq, seq of last frame in `replay`)
    ///   snapshot path => snapshot.baseline_seq
    pub high_water: Seq,
}

/// Backing-state failure surfaced by StateSource even post-auth.
#[derive(Debug)]
pub struct StateError(pub String);
impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl From<fxcore::Error> for StateError {
    fn from(err: fxcore::Error) -> Self {
        Self(err.to_string())
    }
}

/// The three orchestrator powers the handshake needs — NOTHING else reaches past
/// this boundary (fxcore/lib.rs facade rule: wire layer calls orchestrator methods
/// only). Production impl sits right below; tests inject their own.
pub trait StateSource: Send + Sync {
    fn snapshot(&self) -> BoxFuture<'_, Result<Snapshot, StateError>>;
    fn replay(&self, after: Seq) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StateError>>;
    fn subscribe(&self) -> BusReceiver;
}

/// Production StateSource: straight delegation onto the Orchestrator methods.
impl StateSource for Arc<Orchestrator> {
    fn snapshot(&self) -> BoxFuture<'_, Result<Snapshot, StateError>> {
        Box::pin(async {
            Orchestrator::projection_snapshot(self)
                .await
                .map_err(StateError::from)
        })
    }
    fn replay(&self, after: Seq) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StateError>> {
        Box::pin(async move {
            Orchestrator::replay_from(self, after)
                .await
                .map_err(StateError::from)
        })
    }
    fn subscribe(&self) -> BusReceiver {
        Orchestrator::subscribe(self)
    }
}

/// Decision between 4a/4b; unit-pinned separately from IO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Replay,
    Snapshot,
}

fn plan_attach(head: Seq, last_seq: Seq, gap_limit: u64) -> Plan {
    // saturating_sub: a cursor claiming to be AHEAD of head is impossible-but-
    // handle (spec: "treat as gap check against head") => gap collapses to 0 =>
    // replay branch runs EMPTY and the live tail takes over immediately.
    let gap = head.as_u64().saturating_sub(last_seq.as_u64());
    if gap > gap_limit {
        Plan::Snapshot
    } else {
        Plan::Replay
    }
}

fn merge_seed(cursor: Seq, replay_tail: Option<Seq>) -> Seq {
    match replay_tail {
        Some(tail) if tail > cursor => tail,
        _ => cursor,
    }
}

/// CONSTANT-TIME token compare: equal-width pre-padded buffers keep TIME flat
/// across input lengths. Padding zeros differentiate unequal real bytes; inputs
/// larger than the buffer (attacker-controlled!) force-fail via the size gate.
/// stored side is always the shape pair.rs validated (64 lowercase hex).
fn tokens_match(presented: &str, stored: &str) -> bool {
    let mut pbuf = [0u8; TOKEN_COMPARE_BUF];
    let mut sbuf = [0u8; TOKEN_COMPARE_BUF];
    let fits = |s: &str| s.len() <= TOKEN_COMPARE_BUF;
    let copy = |dst: &mut [u8], src: &str| {
        // src is pure ASCII hex (validated server side) or attacker bytes — either
        // way we compare RAW bytes; byte length == char length here.
        let bytes = src.as_bytes();
        let n = bytes.len();
        dst[..n].copy_from_slice(&bytes[..n]);
    };
    if fits(presented) {
        copy(&mut pbuf, presented);
    }
    if fits(stored) {
        copy(&mut sbuf, stored);
    }
    let choice = pbuf.ct_eq(&sbuf) & Choice::from(u8::from(fits(presented) && fits(stored)));
    bool::from(choice)
}

// ── Core state machine ───────────────────────────────────────────────────────

/// Drives stages 1–4 against any sink/stream halves; on Ok the connection is
/// AUTHED + SUBSCRIBED and hands off to client::run together with these halves'
/// owners.
#[allow(clippy::too_many_arguments)]
pub async fn run<S, Tx, Rx>(
    source: S,
    stored_token: Option<&str>,
    server_version: &str,
    stage_timeout: Duration,
    gap_limit: u64,
    tx: &mut Tx,
    rx: &mut Rx,
) -> Result<AuthedClient, HandshakeClosed>
where
    S: StateSource,
    Tx: futures::Sink<Wsf> + Unpin,
    <Tx as futures::Sink<Wsf>>::Error: std::fmt::Display,
    Rx: futures::Stream<Item = Result<Wsf, axum::Error>> + Unpin,
{
    // ── Stage 1: FIRST frame MUST be Hello ──
    let hello = await_frame(tx, rx, stage_timeout, Stage::AwaitingHello).await?;
    let Message::Hello {
        proto_version,
        token,
    } = hello
    else {
        return Err(fail(
            tx,
            CLOSE_CODE_PROTOCOL,
            REASON_PROTOCOL_VERSION,
            Stage::AwaitingHello,
            "first frame was not Hello",
        )
        .await);
    };
    if proto_version != PROTO_VERSION {
        return Err(fail(
            tx,
            CLOSE_CODE_PROTOCOL,
            REASON_PROTOCOL_VERSION,
            Stage::AwaitingHello,
            "version mismatch",
        )
        .await);
    }
    // Own-token-load failures surface upstream as None; we still feed the
    // comparator SOMETHING in constant time so absence never answers by timing.
    let expected = stored_token.unwrap_or("");
    if !tokens_match(&token, expected) {
        return Err(fail(
            tx,
            CLOSE_CODE_POLICY,
            REASON_AUTH_FAILED,
            Stage::AwaitingHello,
            "token mismatch",
        )
        .await);
    }

    // SUBSCRIBE-FIRST (rationale on AuthedClient.bus_rx).
    let mut bus_rx = source.subscribe();

    // ONE snapshot serves BOTH roles: Welcome.head_seq carrier AND (on 4b) the
    // SnapshotRequired payload — guarantees baseline == head we advertised.
    // Store read failing => fail CLOSED (never guess a head out of thin air).
    let snapshot = match source.snapshot().await {
        Ok(snap) => snap,
        Err(err) => {
            error!(target:"handshake", %err, "projection_snapshot failed");
            return Err(HandshakeClosed {
                reason: REASON_INTERNAL,
                stage: Stage::Attach,
            });
        }
    };
    let head_seq = snapshot.baseline_seq;

    // ── Stage 2a ──
    let welcome = Message::Welcome {
        server_version: server_version.to_owned(),
        head_seq,
    };
    if send_json(tx, &welcome).await.is_err() {
        debug!("welcome send failed; peer gone");
        return Err(HandshakeClosed {
            reason: REASON_PROTOCOL_VERSION,
            stage: Stage::AwaitingSubscribe,
        });
    }

    // ── Stage 3: EXACTLY ONE Subscribe, immediately next frame ──
    let sub = await_frame(tx, rx, stage_timeout, Stage::AwaitingSubscribe).await?;
    let Message::Subscribe { last_seq } = sub else {
        return Err(fail(
            tx,
            CLOSE_CODE_PROTOCOL,
            REASON_PROTOCOL_VERSION,
            Stage::AwaitingSubscribe,
            "expected Subscribe",
        )
        .await);
    };

    // ── Stage 4: replay|snapshot ──
    match plan_attach(head_seq, last_seq, gap_limit) {
        Plan::Replay => {
            let events = match source.replay(last_seq).await {
                Ok(evts) => evts,
                Err(err) => {
                    error!(target:"handshake", %err, "replay failed");
                    return Err(HandshakeClosed {
                        reason: REASON_INTERNAL,
                        stage: Stage::Attach,
                    });
                }
            };
            debug_assert!(
                events.windows(2).all(|w| w[0].seq < w[1].seq),
                "store contract: replay strictly ascending"
            );
            let high_water = merge_seed(last_seq, events.last().map(|e| e.seq));
            let replay = events
                .into_iter()
                .map(|event| Message::Event { event })
                .collect();
            let pending = drain_pending(&mut bus_rx).await?;
            Ok(AuthedClient {
                replay,
                bus_rx,
                pending,
                high_water,
            })
        }
        Plan::Snapshot => {
            let snap_msg = Message::SnapshotRequired { snapshot };
            if send_json(tx, &snap_msg).await.is_err() {
                debug!("snapshot send failed; peer gone");
                return Err(HandshakeClosed {
                    reason: REASON_PROTOCOL_VERSION,
                    stage: Stage::Attach,
                });
            }
            let pending = drain_pending(&mut bus_rx).await?;
            Ok(AuthedClient {
                replay: Vec::new(),
                bus_rx,
                pending,
                high_water: head_seq,
            })
        }
    }
}

async fn send_json<Tx>(tx: &mut Tx, msg: &Message) -> Result<(), Tx::Error>
where
    Tx: futures::Sink<Wsf> + Unpin,
{
    let json = serde_json::to_string(msg).expect("fxproto Messages serialize unconditionally");
    tx.send(Wsf::text(json)).await
}

/// Receive ONE text frame enforcing FAIL-T (idle timeout) / FAIL-J (garbage),
/// auto-emitting the canonical close on violations.
async fn await_frame<Tx, Rx>(
    tx: &mut Tx,
    rx: &mut Rx,
    budget: Duration,
    stage: Stage,
) -> Result<Message, HandshakeClosed>
where
    Tx: futures::Sink<Wsf> + Unpin,
    <Tx as futures::Sink<Wsf>>::Error: std::fmt::Display,
    Rx: futures::Stream<Item = Result<Wsf, axum::Error>> + Unpin,
{
    let incoming = match tokio::time::timeout(budget, rx.next()).await {
        Err(_elapsed) => {
            return Err(fail(
                tx,
                CLOSE_CODE_PROTOCOL,
                REASON_PROTOCOL_VERSION,
                stage,
                "no frame within budget",
            )
            .await);
        }
        Ok(None) => {
            return Err(fail(
                tx,
                CLOSE_CODE_PROTOCOL,
                REASON_PROTOCOL_VERSION,
                stage,
                "peer closed early",
            )
            .await);
        }
        Ok(Some(Err(err))) => {
            warn!(%err, "transport error during handshake");
            return Err(HandshakeClosed {
                reason: REASON_PROTOCOL_VERSION,
                stage,
            });
        }
        Ok(Some(Ok(frame))) => frame,
    };
    let text = match incoming {
        Wsf::Text(t) => t.to_string(),
        other => {
            let kind = describe_frame(&other);
            return Err(fail(
                tx,
                CLOSE_CODE_PROTOCOL,
                REASON_PROTOCOL_VERSION,
                stage,
                kind,
            )
            .await);
        }
    };
    match serde_json::from_str::<Message>(&text) {
        Ok(msg) => Ok(msg),
        Err(err) => {
            warn!(%err, "undecodable handshake frame");
            Err(fail(
                tx,
                CLOSE_CODE_PROTOCOL,
                REASON_PROTOCOL_VERSION,
                stage,
                "garbage frame",
            )
            .await)
        }
    }
}

fn describe_frame(frame: &Wsf) -> &'static str {
    match frame {
        Wsf::Binary(_) => "binary frame",
        Wsf::Ping(_) | Wsf::Pong(_) => "control frame",
        Wsf::Close(_) => "early close",
        // Unreachable via await_frame's Text routing above; kept for exhaustiveness.
        Wsf::Text(_) => "unexpected text",
    }
}

/// Emit the canonical close frame + the AUDIT line (which stage, which reason —
/// this is the brute-force trail), then return the terminal error.
async fn fail<Tx>(
    tx: &mut Tx,
    code: u16,
    reason: &'static str,
    stage: Stage,
    why: &'static str,
) -> HandshakeClosed
where
    Tx: futures::Sink<Wsf> + Unpin,
    <Tx as futures::Sink<Wsf>>::Error: std::fmt::Display,
{
    warn!(%why, %reason, ?stage, "handshake rejected");
    let close = Wsf::Close(Some(axum::extract::ws::CloseFrame {
        code,
        reason: reason.into(),
    }));
    // Best-effort: a socket already dead cannot receive it; swallow errors.
    if let Err(err) = tx.send(close).await {
        debug!(%err, "close-frame delivery failed");
    }
    HandshakeClosed { reason, stage }
}

/// Drain events ALREADY seated in the bus buffer while replay/snapshot assembly
/// ran. Dedupe does NOT happen here — the writer warmup filters seq ≤ high_water
/// at exactly one site. A Lagged IN THIS WINDOW (>1024 events during a ms-scale
/// handshake) poisons the view: tear down so the peer resubscribes clean.
///
/// MUST be synchronous-flush: everything buffered NOW lands in `pending`, so
/// warmup filtering sees the full replay/live overlap before the pump switches
/// to unfiltered passthrough. A time-based probe would race the scheduler
/// (zero-duration timers resolve BEFORE the recv future ever polls); instead
/// each loop iteration polls one fresh recv() future EXACTLY ONCE — broadcast
/// semantics guarantee Ready-on-first-poll whenever events are retained, and
/// Pending genuinely means "empty at this instant".
async fn drain_pending(
    bus_rx: &mut BusReceiver,
) -> Result<Vec<Sequenced<FxEvent>>, HandshakeClosed> {
    let mut pending = Vec::new();
    loop {
        let recv = bus_rx.recv();
        futures::pin_mut!(recv);
        let outcome = std::future::poll_fn(|cx| match recv.as_mut().poll(cx) {
            std::task::Poll::Ready(value) => std::task::Poll::Ready(Some(value)),
            std::task::Poll::Pending => std::task::Poll::Ready(None), // buffer empty
        })
        .await;
        match outcome {
            Some(Ok(ev)) => pending.push(ev),
            Some(Err(BusError::Lagged(n))) => {
                warn!(skipped = n, "bus lagged during handshake");
                return Err(HandshakeClosed {
                    reason: "resubscribe",
                    stage: Stage::Attach,
                });
            }
            Some(Err(BusError::Closed)) | None => break,
        }
    }
    Ok(pending)
}

// ── Function-level tests (crates.md table row "handshake/auth unit tests") ──
// run() is GENERIC over sink/stream halves, so tests drive it with two mpsc
// channels + MockStateSource — NO sockets, NO orchestrator boot — controlling
// replay data, bus timing, and failure injection directly.

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;

    use tokio::sync::mpsc;

    use fxcore::EventBus;
    use fxproto::content::Role;
    use fxproto::ids::{SessionId, TurnId};
    use fxproto::model::agents::AgentsState;
    use fxproto::model::perms::PermsState;
    use fxproto::model::threads::ThreadsState;

    const SERVER_VERSION: &str = "test-server";
    const STORED_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TINY_BUDGET: Duration = Duration::from_millis(25);
    /// Mirrors REPLAY_GAP_LIMIT's debug value; injected per-drive for tripwires.
    const DEV_GAP_LIMIT: u64 = 100;

    fn chunk(seq: u64, text: &str) -> Sequenced<FxEvent> {
        Sequenced {
            seq: Seq::new(seq),
            inner: FxEvent::Chunk {
                session: SessionId::from_raw("s".into()),
                turn: TurnId::from_raw("t".into()),
                role: Role::Agent,
                text: text.into(),
            },
        }
    }

    // ── Mocks ──

    struct MockState {
        head: Seq,
        replay: Vec<Sequenced<FxEvent>>,
        /// Emitted onto the bus from INSIDE replay(): simulates appends racing
        /// the attach window AFTER subscribe-first captured the receiver.
        race_events: Vec<Sequenced<FxEvent>>,
        replay_err: Option<String>,
        snap_err: Option<String>,
        bus: EventBus,
    }
    impl Default for MockState {
        fn default() -> Self {
            Self {
                head: Seq::new(0),
                replay: vec![],
                race_events: vec![],
                replay_err: None,
                snap_err: None,
                bus: EventBus::new(1024),
            }
        }
    }
    impl StateSource for std::sync::Arc<MockState> {
        fn snapshot(&self) -> BoxFuture<'_, Result<Snapshot, StateError>> {
            (**self).snapshot()
        }
        fn replay(&self, after: Seq) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StateError>> {
            (**self).replay(after)
        }
        fn subscribe(&self) -> BusReceiver {
            (**self).subscribe()
        }
    }
    impl StateSource for MockState {
        fn snapshot(&self) -> BoxFuture<'_, Result<Snapshot, StateError>> {
            if let Some(err) = &self.snap_err {
                return Box::pin(std::future::ready(Err(StateError(err.clone()))));
            }
            Box::pin(std::future::ready(Ok(Snapshot {
                baseline_seq: self.head,
                agents: AgentsState::default(),
                threads: ThreadsState::default(),
                perms: PermsState::default(),
            })))
        }
        fn replay(
            &self,
            _after: Seq,
        ) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StateError>> {
            // Subscription exists ALREADY (subscribe-first) — anything sent
            // here provably lands in the receiver's retained buffer.
            for ev in &self.race_events {
                self.bus.send(ev.clone());
            }
            let err = self.replay_err.clone();
            let replay = self.replay.clone();
            Box::pin(async move {
                if let Some(err) = err {
                    return Err(StateError(err));
                }
                Ok(replay)
            })
        }
        fn subscribe(&self) -> BusReceiver {
            self.bus.subscribe()
        }
    }

    /// Owned infallible sink: forwards frames into a channel the test reads.
    struct OutSink(mpsc::UnboundedSender<Wsf>);
    impl futures::Sink<Wsf> for OutSink {
        type Error = std::convert::Infallible;
        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, item: Wsf) -> Result<(), Self::Error> {
            let this = self.get_mut();
            if this.0.send(item).is_err() {
                panic!("test receiver alive");
            }
            Ok(())
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Real-halves parity shim: never injects transport errors in these tests
    /// (that branch is exercised implicitly by EOF paths in the client loop).
    struct InStream(mpsc::UnboundedReceiver<Wsf>);
    impl futures::Stream for InStream {
        type Item = Result<Wsf, axum::Error>;
        fn poll_next(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            Pin::new(&mut self.0).poll_recv(cx).map(|opt| opt.map(Ok))
        }
    }

    // ── Drive helpers ──

    /// unwrap_err-style accessor WITHOUT requiring AuthedClient: Debug.
    trait TakeErr<T> {
        fn err_of(self) -> T;
    }
    impl TakeErr<HandshakeClosed> for Result<AuthedClient, HandshakeClosed> {
        fn err_of(self) -> HandshakeClosed {
            match self {
                Err(closed) => closed,
                Ok(authed) => panic!(
                    "expected close, got authed: {} replay frames",
                    authed.replay.len()
                ),
            }
        }
    }

    fn hello(token: &str) -> Wsf {
        Wsf::text(
            serde_json::to_string(&Message::Hello {
                proto_version: PROTO_VERSION,
                token: token.into(),
            })
            .unwrap(),
        )
    }
    fn subscribe(last_seq: u64) -> Wsf {
        Wsf::text(
            serde_json::to_string(&Message::Subscribe {
                last_seq: Seq::new(last_seq),
            })
            .unwrap(),
        )
    }

    struct Outcome {
        result: Result<AuthedClient, HandshakeClosed>,
        messages: Vec<Message>, // decoded Text frames IN ORDER
        closes: Vec<axum::extract::ws::CloseFrame>,
    }

    async fn full_run(
        state: MockState,
        script: &[Wsf],
        stored_token: Option<&str>,
        budget: Duration,
        gap_limit: u64,
    ) -> Outcome {
        let (client_tx, client_rx) = mpsc::unbounded_channel::<Wsf>();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel::<Wsf>();
        for frame in script {
            client_tx.send(frame.clone()).unwrap();
        }
        drop(client_tx); // scripted EOF

        let mut sink = OutSink(server_tx);
        let mut stream = InStream(client_rx);
        let result = run(
            Arc::new(state),
            stored_token,
            SERVER_VERSION,
            budget,
            gap_limit,
            &mut sink,
            &mut stream,
        )
        .await;

        drop(sink); // close lane so recv-cycles terminate
        let mut messages = Vec::new();
        let mut closes = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_millis(5), server_rx.recv()).await {
                Ok(Some(Wsf::Text(t))) => {
                    messages.push(serde_json::from_str(&t.to_string()).unwrap())
                }
                Ok(Some(Wsf::Close(frame))) => closes.push(frame.expect("close carries reason")),
                Ok(Some(_)) | Ok(None) => break,
                Err(_) => break,
            }
        }
        Outcome {
            result,
            messages,
            closes,
        }
    }

    async fn happy_script(state: MockState, last_seq: u64, gap_limit: u64) -> Outcome {
        full_run(
            state,
            &[hello(STORED_TOKEN), subscribe(last_seq)],
            Some(STORED_TOKEN),
            TINY_BUDGET * 20,
            gap_limit,
        )
        .await
    }

    // ── FAIL-V1 / FAIL-J / FAIL-T matrix ──

    #[tokio::test]
    async fn version_mismatch_closes_protocol_version() {
        let (client_tx, client_rx) = mpsc::unbounded_channel::<Wsf>();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel::<Wsf>();
        client_tx
            .send(Wsf::text(
                r#"{"type":"hello","proto_version":9999,"token":"t"}"#.to_owned(),
            ))
            .unwrap();
        drop(client_tx);

        let mut sink = OutSink(server_tx);
        let mut stream = InStream(client_rx);
        let err = run(
            MockState::default(),
            Some(STORED_TOKEN),
            SERVER_VERSION,
            TINY_BUDGET,
            DEV_GAP_LIMIT,
            &mut sink,
            &mut stream,
        )
        .await
        .err_of();

        assert_eq!(err.reason, REASON_PROTOCOL_VERSION);
        assert_eq!(err.stage, Stage::AwaitingHello);
        let Wsf::Close(frame) = tokio::time::timeout(Duration::ZERO, server_rx.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected close frame");
        };
        assert_eq!(frame.unwrap().reason.as_str(), REASON_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn wrong_token_closes_auth_failed_and_audits_stage() {
        let out = full_run(
            MockState::default(),
            &[hello("not-the-token")],
            Some(STORED_TOKEN),
            TINY_BUDGET,
            DEV_GAP_LIMIT,
        )
        .await;
        let err = out.result.err_of();
        assert_eq!(err.reason, REASON_AUTH_FAILED);
        assert_eq!(err.stage, Stage::AwaitingHello);
        assert_eq!(out.closes.len(), 1);
        assert_eq!(out.closes[0].reason.as_str(), REASON_AUTH_FAILED);
        assert_eq!(
            out.closes[0].code, 1008,
            "policy violation code rides with auth"
        );
        assert!(
            out.messages.is_empty(),
            "nothing flows to an unauthenticated peer"
        );
    }

    #[tokio::test]
    async fn unreadable_own_token_fails_closed_auth_failed() {
        // stored_token=None models pair::load_token failure upstream.
        let out = full_run(
            MockState::default(),
            &[hello(STORED_TOKEN)],
            None,
            TINY_BUDGET,
            DEV_GAP_LIMIT,
        )
        .await;
        assert_eq!(out.result.err_of().reason, REASON_AUTH_FAILED);
    }

    #[tokio::test]
    async fn first_frame_not_hello_is_protocol_version() {
        // Well-formed Subscribe arriving BEFORE Hello — stage-3 property only.
        let out = full_run(
            MockState::default(),
            &[subscribe(0)],
            Some(STORED_TOKEN),
            TINY_BUDGET,
            DEV_GAP_LIMIT,
        )
        .await;
        let err = out.result.err_of();
        assert_eq!(
            (err.reason, err.stage),
            (REASON_PROTOCOL_VERSION, Stage::AwaitingHello)
        );
        assert_eq!(out.closes.len(), 1);
    }

    #[tokio::test]
    async fn garbage_json_is_protocol_version_rejection() {
        let out = full_run(
            MockState::default(),
            &[Wsf::text("{not-json")],
            Some(STORED_TOKEN),
            TINY_BUDGET,
            DEV_GAP_LIMIT,
        )
        .await;
        assert_eq!(out.result.err_of().reason, REASON_PROTOCOL_VERSION);
        assert_eq!(out.closes.len(), 1);
    }

    #[tokio::test]
    async fn binary_frame_is_protocol_version_rejection() {
        let out = full_run(
            MockState::default(),
            &[Wsf::binary(vec![1, 2, 3])],
            Some(STORED_TOKEN),
            TINY_BUDGET,
            DEV_GAP_LIMIT,
        )
        .await;
        assert_eq!(out.result.err_of().reason, REASON_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn idle_peer_times_out_as_protocol_violation() {
        // No script at all: budget elapses against an OPEN (never-dropped) inbound
        // lane — pure FAIL-T.
        let (_client_tx, client_rx) = mpsc::unbounded_channel::<Wsf>();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel::<Wsf>();
        let mut sink = OutSink(server_tx);
        let mut stream = InStream(client_rx);
        let err = run(
            MockState::default(),
            Some(STORED_TOKEN),
            SERVER_VERSION,
            TINY_BUDGET,
            DEV_GAP_LIMIT,
            &mut sink,
            &mut stream,
        )
        .await
        .err_of();
        assert_eq!(err.reason, REASON_PROTOCOL_VERSION);
        let got = server_rx.try_recv().unwrap();
        let Wsf::Close(frame) = got else {
            panic!("{got:?}")
        };
        assert_eq!(frame.unwrap().reason.as_str(), REASON_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn missing_subscribe_after_welcome_times_out() {
        let out = full_run(
            MockState {
                head: Seq::new(4),
                ..MockState::default()
            },
            &[hello(STORED_TOKEN)],
            Some(STORED_TOKEN),
            TINY_BUDGET,
            DEV_GAP_LIMIT,
        )
        .await;
        // Welcome went OUT, then silence ⇒ FAIL-T at AwaitingSubscribe.
        assert!(
            matches!(out.messages.first(), Some(Message::Welcome { head_seq, .. }) if head_seq.as_u64() == 4),
            "{:?}",
            out.messages.first()
        );
        assert_eq!(out.result.err_of().stage, Stage::AwaitingSubscribe);
    }

    #[tokio::test]
    async fn non_subscribe_after_welcome_is_protocol_version() {
        let request = Message::Request {
            id: 1,
            command: fxproto::command::Command::DetectAgents,
        };
        let out = full_run(
            MockState {
                head: Seq::new(2),
                ..MockState::default()
            },
            &[
                hello(STORED_TOKEN),
                Wsf::text(serde_json::to_string(&request).unwrap()),
            ],
            Some(STORED_TOKEN),
            TINY_BUDGET * 20,
            DEV_GAP_LIMIT,
        )
        .await;
        assert!(matches!(
            out.messages.first(),
            Some(Message::Welcome { .. })
        ));
        let err = out.result.err_of();
        assert_eq!(
            (err.reason, err.stage),
            (REASON_PROTOCOL_VERSION, Stage::AwaitingSubscribe)
        );
    }

    // ── Happy paths + attach planning ──

    #[tokio::test]
    async fn ok_path_welcome_then_replay_then_buffered_pending() {
        let state = MockState {
            head: Seq::new(6),
            replay: vec![chunk(6, "r6")],
            // These race events fire from INSIDE replay(), i.e. after the
            // subscribe-first capture: 6 overlaps the replay tail (the exact
            // duplicate case the merge rule must swallow downstream), 7/8 are
            // genuinely live traffic that arrived during the attach window.
            race_events: vec![chunk(6, "dup"), chunk(7, "live-7"), chunk(8, "live-8")],
            ..Default::default()
        };
        let out = happy_script(state, 5, DEV_GAP_LIMIT).await;

        let welcome_json = serde_json::to_string(&out.messages[0]).unwrap();
        assert_eq!(
            welcome_json,
            r#"{"type":"welcome","server_version":"test-server","head_seq":6}"#
        );
        assert_eq!(
            out.messages.len(),
            1,
            "replay ships via AuthedClient, NOT inline"
        );
        let authed = out.result.expect("ok path");
        assert_eq!(
            authed.high_water,
            Seq::new(6),
            "merge seed = max(cursor, replay tail)"
        );
        assert_eq!(authed.replay.len(), 1);
        let pending_seqs: Vec<u64> = authed.pending.iter().map(|e| e.seq.as_u64()).collect();
        assert_eq!(
            pending_seqs,
            vec![6, 7, 8],
            "RAW pending capture; the WRITER warmup's high_water filter drops the dup 6"
        );
        // And the warmed stream really is gap-free: warmup (pure) over this
        // bundle emits exactly replay + {7,8}.
        let mut sink = Vec::new();
        crate::net::client::warmup_into(
            &mut sink,
            &authed.replay,
            &authed.pending,
            authed.high_water,
        );
        let emitted: Vec<u64> = sink
            .iter()
            .map(|m| match m {
                Message::Event { event } => event.seq.as_u64(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(emitted, vec![6, 7, 8], "no gap 5->6, no duplicated 6");
    }

    #[tokio::test]
    async fn snapshot_tripwire_at_threshold() {
        let state = MockState {
            head: Seq::new(200),
            ..Default::default()
        };
        let out = happy_script(state, 5, DEV_GAP_LIMIT).await; // gap 195 > 100

        assert_eq!(out.messages.len(), 2, "Welcome then SnapshotRequired");
        let Message::SnapshotRequired { snapshot } = &out.messages[1] else {
            panic!("expected snapshot_required, got {:?}", out.messages[1]);
        };
        assert_eq!(snapshot.baseline_seq.as_u64(), 200);
        assert!(
            matches!(out.messages.first(), Some(Message::Welcome { head_seq, .. }) if head_seq.as_u64() == 200)
        );
        let authed = out.result.expect("snapshot path still hands off");
        assert!(authed.replay.is_empty());
        assert_eq!(authed.high_water, Seq::new(200));
    }

    #[tokio::test]
    async fn cursor_at_head_is_pure_live_attach() {
        let out = happy_script(
            MockState {
                head: Seq::new(7),
                ..Default::default()
            },
            7,
            DEV_GAP_LIMIT,
        )
        .await;
        let authed = out.result.expect("ok");
        assert!(authed.replay.is_empty());
        assert_eq!(
            authed.high_water,
            Seq::new(7),
            "cursor wins over empty tail"
        );
    }

    #[tokio::test]
    async fn cursor_ahead_of_head_collapses_to_empty_replay() {
        // Impossible-but-handle: saturating gap check pins the decision at Replay.
        let out = happy_script(
            MockState {
                head: Seq::new(3),
                ..Default::default()
            },
            99,
            DEV_GAP_LIMIT,
        )
        .await;
        let authed = out.result.expect("no crash on future cursor");
        assert!(authed.replay.is_empty());
        assert_eq!(
            authed.high_water,
            Seq::new(99),
            "never regress the merge seed"
        );
    }

    #[tokio::test]
    async fn store_failure_maps_to_internal_reason_after_auth() {
        let out = happy_script(
            MockState {
                snap_err: Some("sqlite wedged".into()),
                ..Default::default()
            },
            5,
            DEV_GAP_LIMIT,
        )
        .await;
        assert_eq!(out.result.err_of().reason, REASON_INTERNAL);

        let replay_fail = full_run(
            MockState {
                head: Seq::new(3),
                replay_err: Some("boom".into()),
                ..Default::default()
            },
            &[hello(STORED_TOKEN), subscribe(1)],
            Some(STORED_TOKEN),
            TINY_BUDGET * 20,
            DEV_GAP_LIMIT,
        )
        .await;
        assert_eq!(replay_fail.result.err_of().reason, REASON_INTERNAL);
        assert!(
            matches!(replay_fail.messages[0], Message::Welcome { .. }),
            "welcome precedes the breakage"
        );
    }

    // ── Pure-decision witnesses ──

    #[test]
    fn plan_attach_matrix() {
        use Plan::*;
        let head = Seq::new(1000);
        assert_eq!(
            plan_attach(head, Seq::new(900), 100),
            Replay,
            "boundary == allowed"
        );
        assert_eq!(
            plan_attach(head, Seq::new(899), 100),
            Snapshot,
            "gap beyond limit trips"
        );
        assert_eq!(plan_attach(head, Seq::new(1000), 0), Replay);
        assert_eq!(
            plan_attach(head, Seq::new(5000), 1),
            Replay,
            "future cursor saturates to gap 0"
        );
    }

    #[test]
    fn merge_seed_takes_max() {
        assert_eq!(merge_seed(Seq::new(4), Some(Seq::new(9))), Seq::new(9));
        assert_eq!(
            merge_seed(Seq::new(4), Some(Seq::new(2))),
            Seq::new(4),
            "empty-gap cursor"
        );
        assert_eq!(merge_seed(Seq::new(4), None), Seq::new(4));
    }

    #[test]
    fn tokens_match_is_shape_indifferent_but_exact() {
        assert!(tokens_match(STORED_TOKEN, STORED_TOKEN));
        assert!(!tokens_match("wrong", STORED_TOKEN));
        assert!(!tokens_match("", STORED_TOKEN));
        assert!(
            !tokens_match(&"a".repeat(256), STORED_TOKEN),
            "oversized attacker input fails closed"
        );
    }
}
