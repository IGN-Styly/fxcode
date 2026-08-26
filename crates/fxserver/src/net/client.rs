//! Per-connection task pair after successful handshake.
//!
//! Precondition: handshake.rs already authenticated the client, drained replay
//! (or delivered SnapshotRequired) and holds a live bus subscription inside
//! AuthedClient. This file is transport plumbing only — zero protocol decisions
//! beyond frame routing.
//!
//! Task topology (ws.split() ONCE; halves never migrate between tasks):
//!
//! ```text
//!                       ┌──────────────────────────────────────────────┐
//!   WS stream half ──▶ │ READER                                        │
//!                      │   WsFrame → decode Message → Request ONLY:    │
//!                      │     orch.execute(command).await               │
//!                      │      → out_tx Response{id, reply}             │
//!                      │   anything else ⇒ ctrl-lane fail (below)      │
//!                      └───────────────┬───────────────────────────────┘
//!                                      ▼
//!   WS sink half  ◀──────────────────────────────────────────────────────────
//!                      ┌──────────────────────────────────────────────┐
//!                      │ WRITER                                        │
//!                      │   warmup steps 1–2 — THE merge rule:          │
//!                      │     1. flush auth.replay in order             │
//!                      │     2. drain auth.pending skipping            │
//!                      │        seq ≤ auth.high_water                  │
//!                      │     3. passthrough: out lane + periodic pings │
//!                      │   owns BOTH close-frame emitters (out-EOF +   │
//!                      │   ctrl lane); its completion ends the session │
//!                      └───────────────▲──────────────▲────────────────┘
//!                                      │              │
//!   orch.subscribe() ──▶ EVENT PUMP ───┘   ctrl lane ─┘  (separate mpsc<1>)
//! ```
//!
//! Channel inventory (all BOUNDED):
//!   out:  mpsc::channel::<OutMsg>(OUT_CAP) — cap PAIRS WITH fxcore::bus::
//!         BUS_CAPACITY (imported below, deliberately not re-hardcoded): the
//!         event fanout lane and this per-client queue saturate together, so
//!         neither becomes an artificial bottleneck.
//!         Producers: READER (responses) + EVENT PUMP (seq'd events).
//!   ctrl: mpsc::channel::<WsFrame>(1) — TEARDOWN LANE reserved for canonical
//!         close frames. Why: a resubscribe/protocol close MUST reach the peer
//!         even when `out` is wedged full of events (the very condition that
//!         CAUSES the close); the writer's biased select services ctrl ahead
//!         of everything so a packed event queue cannot starve the goodbye.
//!
//! Lag / backpressure mapping (the ONLY close strings reachable here):
//!   - EVENT PUMP sees BusError::Lagged(n): server skipped n events FOR THIS
//!     subscriber ⇒ Ctrl(close("resubscribe")) + teardown. NEVER backfill
//!     inline: the client's cursor makes reconnect+replay cheap and correct;
//!     an inline gap-fill would race the live stream.
//!   - out.try_send == Full (client slower than BUS_CAPACITY frames): SAME
//!     treatment ("resubscribe"). Applying TCP backpressure instead would pin
//!     orchestrator execute() slots behind one wedged client; disconnecting is
//!     always safe because responses correlate by id and clients re-issue
//!     deliberately (fxapp conn/mod.rs rules — never auto-requeue).
//!   - Non-Request envelope post-handshake (Event/Subscribe/SnapshotRequired/
//!     Hello/Welcome): protocol violation ⇒ close("protocol_version"). A
//!     well-formed envelope-level Subscribe DECODES here — it is handshake.rs
//!     stage-3 property exclusively, once-per-connection by construction — so
//!     its arrival means a broken/hostile client. A legacy subscribe-as-
//!     command dies EARLIER still: Command has no Subscribe variant (locked),
//!     serde cannot map `{"type":"subscribe", ...}` onto any Command, and the
//!     frame never becomes a Request at all (structural rejection, next rule).
//!   - Undecodable inbound (invalid JSON / unknown Message shape / unknown
//!     Command inside a Request): close("protocol_version") + teardown. No
//!     JSON error replies to garbage — that invites malformed-input loops.
//!
//! Teardown matrix (who cancels whom):
//!   | trigger                        | actor    | action                            |
//!   |--------------------------------|----------|-----------------------------------|
//!   | read EOF (client away)         | READER   | DieWithLocal guard cancels local  |
//!   |                                |          | → pump exits → tx drains empty →  |
//!   |                                |          | WRITER flushes residual then      |
//!   |                                |          | Close(1000) → supervisor reaps    |
//!   | write error                    | WRITER   | socket corpse: cancel local, exit |
//!   | bus Lagged / out Full          | producers| Ctrl(close"resubscribe"), Die     |
//!   | FAIL-P (bad frame)             | READER   | Ctrl(close"protocol_version"),Die |
//!   | server shutdown                | mod.rs   | global token: WRITER sends        |
//!   |                                |          | Close(1001 going_away), loops     |
//!   |                                |          | exit; orchestrator proceeds alone |
//!   Nothing else needs cleanup: durable state lives in the orchestrator; the
//!   connection's only residue is its bus subscription (dropped with rx).
//!
//! Ordering note: READER awaits orchestrator.execute SERIALLY per connection;
//! connections are independent — total ordering comes from fxcore's single
//! actor (cmd dispatch), not this loop.
//!
//! Latency badge data path (M0): WRITER emits a Ping every 25 s carrying an
//! epoch-nanos token; READER matches Pongs by that token and publishes the RTT
//! through a watch channel for /healthz-adjacent debugging.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::Message as Wsf;
use futures::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use fxcore::{BUS_CAPACITY, BusError, BusReceiver, Orchestrator};
use fxproto::envelope::Message;
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::Seq;
use fxproto::reply::{FxError, FxErrorCode, Reply};

use super::handshake::AuthedClient;

const CLOSE_CODE_NORMAL: u16 = 1000;
const CLOSE_CODE_AWAY: u16 = 1001;
const CLOSE_CODE_PROTOCOL: u16 = 1002;
const CLOSE_CODE_POLICY: u16 = 1008;

pub const REASON_RESUBSCRIBE: &str = "resubscribe";
pub const REASON_PROTOCOL_VERSION: &str = "protocol_version";

/// Ping cadence (M0 badge); embedded epoch-nanos token makes samples absolute.
const PING_INTERVAL: Duration = Duration::from_secs(25);
/// Bounded reap budget for sibling tasks after the WRITER terminates.
const SIBLING_JOIN_GRACE: Duration = Duration::from_secs(2);

/// Cap pairing against fxcore::bus::BUS_CAPACITY — see module doc inventory.
const OUT_CAP: usize = BUS_CAPACITY;

enum OutMsg {
    /// Correlated responses. Event flow rides the dedicated arm so the writer
    /// keeps ONE serialization site.
    Direct(Message),
    Event(Sequenced<FxEvent>),
    /// Answer to an inbound transport Ping (RFC 6455 mandates the Pong echo).
    /// Rides the ctrl lane? No: ctrl is a 1-slot TEARDOWN lane — stuffing
    /// keepalives there could starve real closes; it gets its own bounded slot.
    Pong(axum::body::Bytes),
}

fn close_frame(code: u16, reason: &'static str) -> Wsf {
    Wsf::Close(Some(axum::extract::ws::CloseFrame {
        code,
        reason: reason.into(),
    }))
}

fn text_frame(msg: &Message) -> Wsf {
    Wsf::text(serde_json::to_string(msg).expect("fxproto Messages serialize unconditionally"))
}

/// Command-execution seam: prod = Arc<Orchestrator> delegation; tests inject a
/// canned Commander so loop-level tests need NO store/agent boot.
pub(crate) trait Commander: Send + Sync {
    fn execute(
        &self,
        command: fxproto::command::Command,
    ) -> futures::future::BoxFuture<'_, Result<Reply, fxcore::Error>>;
}

impl Commander for Orchestrator {
    fn execute(
        &self,
        command: fxproto::command::Command,
    ) -> futures::future::BoxFuture<'_, Result<Reply, fxcore::Error>> {
        Box::pin(async move { Orchestrator::execute(self, command).await })
    }
}

/// Session entrypoint: upgraded socket HALVES (split exactly once upstream in
/// conn_entrypoint — axum cannot reassemble post-split) + post-auth bundles →
/// task trio until either side tears down (matrix above). Generic over the
/// sink/stream pair so loop-level unit tests drive shims identically to prod.
pub(super) async fn run<Si, St>(
    mut sink: Si,
    mut stream: St,
    commander: Arc<dyn Commander>,
    authed: AuthedClient,
    cancel: CancellationToken,
) where
    Si: futures::Sink<Wsf> + Unpin + Send + 'static,
    <Si as futures::Sink<Wsf>>::Error: std::fmt::Display,
    St: futures::Stream<Item = Result<Wsf, axum::Error>> + Unpin + Send + 'static,
{
    let local = CancellationToken::new();

    // One bounded consumer-facing channel with TWO producers + the reserved
    // 1-slot control lane the writer services first.
    let (out_tx, mut out_rx) = mpsc::channel::<OutMsg>(OUT_CAP);
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<Wsf>(1);

    // Destructure BEFORE spawning: bus_rx belongs to the pump EXCLUSIVELY,
    // while writer/reader consume their slices by reference/move.
    let AuthedClient {
        replay,
        pending,
        high_water,
        bus_rx,
    } = authed;

    let (latency_tx, _latency_rx) = watch::channel(None::<Duration>);
    // M0 seam: the watch is published-only today (future debug endpoint reads
    // it); keeping the receiver bound suppresses nothing-nobody-lints noise.

    // ── WRITER ──
    let writer_local = local.clone();
    let writer_cancel = cancel.clone();
    let writer_handle = tokio::spawn(async move {
        // Steps 1–2 precede ANY live passthrough: history-before-live. Sink
        // errors mean corpse-socket; kill the session.
        if let Err(err) = flush_warmup(&mut sink, &replay, &pending, high_water).await {
            debug!(%err, "warmup write failed");
            writer_local.cancel();
            return;
        }

        let mut ping_tick = tokio::time::interval(PING_INTERVAL);
        ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;

                // Teardown lane FIRST — resubscribe/goodbye must win over a
                // packed event backlog by design (see ctrl rationale above).
                routed = ctrl_rx.recv() => match routed {
                    Some(frame) => {
                        if let Err(err) = sink.send(frame).await {
                            debug!(%err, "terminal close delivery failed");
                        }
                        break;
                    }
                    None => break,
                },

                routed = out_rx.recv() => match routed {
                    // All producers finished without a ctrl close (clean EOF or
                    // orchestrator-stopped bus): natural close.
                    None => {
                        let _ = sink.send(close_frame(CLOSE_CODE_NORMAL, "")).await;
                        break;
                    }
                    Some(OutMsg::Pong(payload)) => {
                        if sink.send(Wsf::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(OutMsg::Direct(msg)) => {
                        if sink.send(text_frame(&msg)).await.is_err() {
                            break;
                        }
                    }
                    Some(OutMsg::Event(event)) => {
                        if sink.send(text_frame(&Message::Event { event })).await.is_err() {
                            break;
                        }
                    }
                },

                _ = writer_local.cancelled() => {
                    // Internal death (sibling died). On PROCESS shutdown the
                    // going-away close still ships; plain connection death skips it.
                    if writer_cancel.is_cancelled() {
                        let _ = sink.send(close_frame(CLOSE_CODE_AWAY, "going_away")).await;
                    }
                    break;
                }

                _ = writer_cancel.cancelled() => {
                    // Server shutdown: going-away close, exit. Clients reconnect
                    // and replay from cursors — no drain ceremony (main.rs spec).
                    let _ = sink.send(close_frame(CLOSE_CODE_AWAY, "going_away")).await;
                    break;
                }

                _ = ping_tick.tick() => {
                    let stamp = epoch_nanos();
                    if sink.send(Wsf::Ping(axum::body::Bytes::copy_from_slice(&stamp.to_be_bytes()))).await.is_err() {
                        break;
                    }
                }
            }
        }
        writer_local.cancel();
    });

    // ── READER ──
    let reader_out = out_tx.clone();
    let reader_ctrl = ctrl_tx.clone();
    let reader_local = local.clone();
    let reader_cancel = cancel.clone();
    let reader_latency = latency_tx;
    let reader_handle = tokio::spawn(async move {
        // Whatever way this scope ends, siblings must hear THIS poll-cycle:
        // EOF-without-frame would otherwise wedge the writer behind a quiet bus.
        let _die_with_scope = DieWithLocal(reader_local.clone());

        loop {
            tokio::select! {
                biased;
                _ = reader_local.cancelled() => break,
                _ = reader_cancel.cancelled() => break,

                frame = stream.next() => {
                    match frame {
                        None | Some(Ok(Wsf::Close(_))) => break,
                        Some(Err(err)) => { debug!(%err, "read error"); break; }

                        Some(Ok(Wsf::Text(text))) => match serde_json::from_str::<Message>(&text.to_string()) {
                            Ok(Message::Request { id, command }) => {
                                let reply = match commander.execute(command).await {
                                    Ok(reply) => reply,
                                    // execute only errs on infrastructure races
                                    // (ShuttingDown mid-handler): Internal data reply.
                                    Err(err) => Reply::Error(FxError {
                                        code: FxErrorCode::Internal,
                                        message: err.to_string(),
                                    }),
                                };
                                let response = OutMsg::Direct(Message::Response { id, reply });
                                if reader_out.try_send(response).is_err() {
                                    // Full = slow client resubscribes; Closed =
                                    // writer already gone. Same either way.
                                    let _ = reader_ctrl.try_send(close_frame(CLOSE_CODE_POLICY, REASON_RESUBSCRIBE));
                                    break;
                                }
                            }
                            // Envelope decoded but wrong KIND for steady state.
                            Ok(_) => {
                                let _ = reader_ctrl.try_send(close_frame(CLOSE_CODE_PROTOCOL, REASON_PROTOCOL_VERSION));
                                break;
                            }
                            Err(err) => {
                                debug!(%err, "undecodable steady-state frame");
                                let _ = reader_ctrl.try_send(close_frame(CLOSE_CODE_PROTOCOL, REASON_PROTOCOL_VERSION));
                                break;
                            }
                        },

                        Some(Ok(Wsf::Pong(payload))) => {
                            if let Some(rtt) = pong_rtt(&payload) {
                                reader_latency.send_replace(Some(rtt));
                            }
                        }

                        // RFC 6455: inbound Ping MUST be answered with its own
                        // payload and NEVER tears down the session (browser tabs,
                        // proxies and fxapp itself all rely on this). Binary IS a
                        // protocol violation in v0 (envelope-only wire).
                        Some(Ok(Wsf::Ping(payload))) => {
                            if reader_out.try_send(OutMsg::Pong(payload)).is_err() {
                                break;
                            }
                        }
                        Some(Ok(Wsf::Binary(_))) => {
                            let _ = reader_ctrl.try_send(close_frame(CLOSE_CODE_PROTOCOL, REASON_PROTOCOL_VERSION));
                            break;
                        }
                    }
                }
            }
        }
    });

    // ── EVENT PUMP ──
    let pump_out = out_tx;
    let pump_local = local.clone();
    let pump_handle = tokio::spawn(event_pump(bus_rx, pump_out, ctrl_tx, pump_local, cancel));

    // ── SUPERVISOR ──
    // The WRITER owns every close-frame emitter, so its completion IS the
    // session end. Force-stop lingering siblings within grace afterwards.
    let _ = writer_handle.await;
    local.cancel();
    let _ = tokio::time::timeout(SIBLING_JOIN_GRACE, async {
        let _ = reader_handle.await;
        let _ = pump_handle.await;
    })
    .await;
}

/// Drop-guard: whichever way a sibling scope ends, downstream must observe it
/// within one poll-cycle (see teardown matrix's EOF row).
struct DieWithLocal(CancellationToken);
impl Drop for DieWithLocal {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn epoch_nanos() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    )
    .unwrap_or_default()
}

/// Pong correlation: parse the echoed epoch-nanos ping token into an RTT.
fn pong_rtt(payload: &[u8]) -> Option<Duration> {
    if payload.len() != 8 {
        return None;
    }
    let sent_nanos = u64::from_be_bytes(payload.try_into().ok()?);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .checked_sub(Duration::from_nanos(sent_nanos))
}

/// Writer warmup steps 1–2, extracted for unit testing (crates.md table row:
/// "replay-then-live ordering"). Flushes replay IN ORDER, then pending skipping
/// seq ≤ high_water — THE single dedupe site in the system.
///
/// Signature note: takes slices instead of &AuthedClient so unit tests need no
/// synthetic BusReceiver plumbing.
/// PURE core of the merge rule (unit-testable without a Sink): replay frames
/// verbatim, then pending minus everything seq ≤ high_water.
pub(crate) fn warmup_into(
    out: &mut Vec<Message>,
    replay: &[Message],
    pending: &[Sequenced<FxEvent>],
    high_water: Seq,
) {
    out.extend(replay.iter().cloned());
    for event in pending.iter().filter(|e| e.seq > high_water) {
        out.push(Message::Event {
            event: event.clone(),
        });
    }
}

pub(super) async fn flush_warmup<S>(
    sink: &mut S,
    replay: &[Message],
    pending: &[Sequenced<FxEvent>],
    high_water: Seq,
) -> Result<(), S::Error>
where
    S: futures::Sink<Wsf> + Unpin,
{
    let mut queue = Vec::with_capacity(replay.len() + pending.len());
    warmup_into(&mut queue, replay, pending, high_water);
    for msg in &queue {
        sink.send(text_frame(msg)).await?;
    }
    Ok(())
}

/// EVENT PUMP body (extracted so tests can drive a REAL EventBus directly —
/// fxcore exposes EventBus::new(cap)/subscribe() precisely for that ability).
/// Lag policy recap: Lagged ⇒ poisoned view ⇒ Ctrl("resubscribe"); Closed ⇒ the
/// orchestrator is going down, global cancel lands momentarily regardless.
async fn event_pump(
    mut bus_rx: BusReceiver,
    out_tx: mpsc::Sender<OutMsg>,
    ctrl_tx: mpsc::Sender<Wsf>,
    local: CancellationToken,
    global: CancellationToken,
) {
    // Guard holds its own handle so the loop below keeps polling `local`.
    let _die_with_scope = DieWithLocal(local.clone());
    loop {
        tokio::select! {
            biased;
            _ = local.cancelled() => break,
            _ = global.cancelled() => break,

            polled = bus_rx.recv() => {
                match polled {
                    Ok(seq_ev) => {
                        if out_tx.try_send(OutMsg::Event(seq_ev)).is_err() {
                            // Full (slow client) or Closed (writer dead): same
                            // UX — disconnect and let the cursor repair things.
                            let _ = ctrl_tx.try_send(close_frame(CLOSE_CODE_POLICY, REASON_RESUBSCRIBE));
                            break;
                        }
                    }
                    Err(BusError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "subscriber fell behind; forcing resubscribe");
                        let _ = ctrl_tx.try_send(close_frame(CLOSE_CODE_POLICY, REASON_RESUBSCRIBE));
                        break;
                    }
                    Err(BusError::Closed) => break,
                }
            }
        }
    }
}

// ── Loop-level tests (crates.md: task-pair plumbing, no external daemons) ──
// Same trick as handshake.rs: the GENERIC halves let tests feed a scripted
// InStream + collect from an OutSink. The Commander seam removes any need for
// a real orchestrator; event flow still uses a REAL fxcore EventBus.

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;

    use fxcore::EventBus;

    use fxproto::command::Command;
    use fxproto::content::Role;
    use fxproto::ids::{Seq, SessionId, TurnId};

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

    fn text_frame(msg: &Message) -> Wsf {
        Wsf::text(serde_json::to_string(msg).unwrap())
    }

    /// Canned Commander: every execute() returns a fixed Reply; calls recorded.
    struct Canned {
        reply: Reply,
        seen: std::sync::Mutex<Vec<Command>>,
    }
    impl Commander for Canned {
        fn execute(
            &self,
            command: Command,
        ) -> futures::future::BoxFuture<'_, Result<Reply, fxcore::Error>> {
            self.seen.lock().unwrap().push(command);
            let reply = self.reply.clone();
            Box::pin(std::future::ready(Ok(reply)))
        }
    }

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
            if self.get_mut().0.send(item).is_err() {
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

    fn authed() -> AuthedClient {
        AuthedClient {
            replay: vec![],
            bus_rx: EventBus::new(8).subscribe(),
            pending: vec![],
            high_water: Seq::new(0),
        }
    }

    fn decode(frame: &Wsf) -> Message {
        let Wsf::Text(t) = frame else {
            panic!("text expected: {frame:?}")
        };
        serde_json::from_str(t.to_string().as_str()).expect("writer-built envelopes are valid")
    }

    async fn drain(rx: &mut mpsc::UnboundedReceiver<Wsf>) -> Vec<Wsf> {
        let mut out = Vec::new();
        while let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await
        {
            out.push(frame);
        }
        out
    }

    const IDLE_JOIN: Duration = Duration::from_millis(250);

    /// Reap a session task within budget: Elapsed => wedged sibling (a REAL
    /// failure); JoinError would mean the runtime itself is dying.
    async fn reap(session: tokio::task::JoinHandle<()>) {
        let _ = tokio::time::timeout(IDLE_JOIN, session)
            .await
            .unwrap_or_else(|_| panic!("session did not end within join grace"));
    }

    // ── Correlation round-trip ──

    #[tokio::test]
    async fn request_response_preserves_correlation_id() {
        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        let canned = Arc::new(Canned {
            reply: Reply::PermissionRecorded,
            seen: Default::default(),
        });

        let session = tokio::spawn(run(
            OutSink(server_tx),
            InStream(client_rx),
            Arc::clone(&canned) as Arc<dyn Commander>,
            authed(),
            CancellationToken::new(),
        ));

        let _ = client_tx.send(text_frame(&Message::Request {
            id: 7,
            command: Command::DetectAgents,
        }));

        let frames = drain(&mut server_rx).await;
        let responses: Vec<Message> = frames
            .iter()
            .filter(|f| matches!(f, Wsf::Text(_)))
            .map(decode)
            .collect();
        assert_eq!(responses.len(), 1, "exactly one correlated response");
        assert_eq!(
            serde_json::to_string(&responses[0]).unwrap(),
            serde_json::to_string(&Message::Response {
                id: 7,
                reply: Reply::PermissionRecorded
            })
            .unwrap(),
            "id echoes verbatim per envelope.rs correlation rule"
        );
        assert_eq!(canned.seen.lock().unwrap()[0], Command::DetectAgents);

        drop(client_tx); // EOF → normal close after residuals
        reap(session).await;
    }

    #[tokio::test]
    async fn infrastructure_error_maps_to_internal_reply_data() {
        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        let failing = Arc::new(FailingCommander);
        let session = tokio::spawn(run(
            OutSink(server_tx),
            InStream(client_rx),
            failing,
            authed(),
            CancellationToken::new(),
        ));
        let _ = client_tx.send(text_frame(&Message::Request {
            id: 3,
            command: Command::DetectAgents,
        }));
        let frames = drain(&mut server_rx).await;
        let Message::Response { id, reply } = decode(&frames[0]) else {
            panic!()
        };
        assert_eq!(id, 3);
        assert!(
            matches!(reply, Reply::Error(ref e) if e.code == FxErrorCode::Internal),
            "{reply:?}"
        );
        drop(client_tx);
        reap(session).await;
    }

    struct FailingCommander;
    impl Commander for FailingCommander {
        fn execute(
            &self,
            _: Command,
        ) -> futures::future::BoxFuture<'_, Result<Reply, fxcore::Error>> {
            Box::pin(std::future::ready(Err(fxcore::Error::ShuttingDown)))
        }
    }

    // ── FAIL-P matrix ──

    #[tokio::test]
    async fn post_handshake_subscribe_is_protocol_violation() {
        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        let noop = Arc::new(Canned {
            reply: Reply::Cancelled,
            seen: Default::default(),
        });
        let session = tokio::spawn(run(
            OutSink(server_tx),
            InStream(client_rx),
            noop as Arc<dyn Commander>,
            authed(),
            CancellationToken::new(),
        ));
        let _ = client_tx.send(text_frame(&Message::Subscribe {
            last_seq: Seq::new(0),
        }));
        let close = expect_close(&mut server_rx).await;
        assert_eq!(close.reason.as_str(), REASON_PROTOCOL_VERSION);
        reap(session).await;
    }

    #[tokio::test]
    async fn malformed_steady_state_json_is_protocol_violation() {
        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        let noop: Arc<dyn Commander> = Arc::new(Canned {
            reply: Reply::Cancelled,
            seen: Default::default(),
        });
        let session = tokio::spawn(run(
            OutSink(server_tx),
            InStream(client_rx),
            noop,
            authed(),
            CancellationToken::new(),
        ));
        let _ = client_tx.send(Wsf::Text("{{{".into()));
        let close = expect_close(&mut server_rx).await;
        assert_eq!(close.reason.as_str(), REASON_PROTOCOL_VERSION);
        reap(session).await;
    }

    #[tokio::test]
    async fn legacy_subscribe_as_command_dies_structurally() {
        // {"type":"subscribe"} matches NO Command variant ⇒ Request decode fails
        // at the ENVELOPE layer — same protocol_version answer.
        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        let noop: Arc<dyn Commander> = Arc::new(Canned {
            reply: Reply::Cancelled,
            seen: Default::default(),
        });
        let session = tokio::spawn(run(
            OutSink(server_tx),
            InStream(client_rx),
            noop,
            authed(),
            CancellationToken::new(),
        ));
        let _ = client_tx.send(Wsf::Text(
            r#"{"type":"request","id":1,"command":{"type":"subscribe"}}"#.into(),
        ));
        let close = expect_close(&mut server_rx).await;
        assert_eq!(close.reason.as_str(), REASON_PROTOCOL_VERSION);
        reap(session).await;
    }

    async fn expect_close(rx: &mut mpsc::UnboundedReceiver<Wsf>) -> axum::extract::ws::CloseFrame {
        while let Some(frame) = tokio::time::timeout(IDLE_JOIN, rx.recv())
            .await
            .ok()
            .flatten()
        {
            if let Wsf::Close(Some(close)) = frame {
                return close;
            }
        }
        panic!("no close frame observed");
    }

    // ── Global-cancel / warmup ordering through the WHOLE trio ──

    #[tokio::test]
    async fn cancel_token_ships_going_away_close() {
        let (_client_tx, client_rx) = mpsc::unbounded_channel();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        let global = CancellationToken::new();
        let noop: Arc<dyn Commander> = Arc::new(Canned {
            reply: Reply::Cancelled,
            seen: Default::default(),
        });
        let session = tokio::spawn(run(
            OutSink(server_tx),
            InStream(client_rx),
            noop,
            authed(),
            global.clone(),
        ));
        global.cancel();
        let close = expect_close(&mut server_rx).await;
        assert_eq!(close.code, CLOSE_CODE_AWAY, "going away on shutdown");
        assert_eq!(close.reason.as_str(), "going_away");
        reap(session).await;
    }

    #[tokio::test]
    async fn replay_then_pending_then_live_through_full_trio() {
        // ONE genuine fxcore EventBus drives BOTH writer inputs:
        //   pending: events buffered during "replay" (authed.pending)
        //   live:    events emitted AFTER attach via the pump branch.
        let bus = EventBus::new(16);
        for i in 6..=7u64 {
            bus.send(chunk(i, "in-flight")); // captured into pending below
        }

        let bus_for_pump = bus.clone();
        let authed_bundle = AuthedClient {
            replay: vec![Message::Event {
                event: chunk(4, "r4"),
            }],
            // Consume + snapshot what's already in the buffer to simulate the
            // handshake's subscribe-first capture:
            pending: drain_bus(bus.subscribe()).await,
            high_water: Seq::new(7),
            bus_rx: bus.subscribe(),
        };

        let (client_tx, client_rx) = mpsc::unbounded_channel();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel();
        let noop: Arc<dyn Commander> = Arc::new(Canned {
            reply: Reply::Cancelled,
            seen: Default::default(),
        });
        let session = tokio::spawn(run(
            OutSink(server_tx),
            InStream(client_rx),
            noop,
            authed_bundle,
            CancellationToken::new(),
        ));

        // Give the pump a beat, then emit LIVE traffic past high_water.
        bus_for_pump.send(chunk(8, "live-8"));
        bus_for_pump.send(chunk(9, "live-9"));

        let seq_stream: Vec<u64> = drain(&mut server_rx)
            .await
            .iter()
            .filter_map(|f| match f {
                Wsf::Text(_) => match decode(f) {
                    Message::Event { event } => Some(event.seq.as_u64()),
                    other => panic!("only events expected: {other:?}"),
                },
                Wsf::Ping(_) => None,
                other => panic!("unexpected {other:?}"),
            })
            .collect();

        assert_eq!(
            seq_stream,
            vec![4, /*pending dupes ≤7 die here*/ 8, 9],
            "replay → deduped pending → live passthrough, ascending"
        );

        drop(client_tx);
        reap(session).await;
    }

    async fn drain_bus(mut rx: BusReceiver) -> Vec<Sequenced<FxEvent>> {
        // Mirrors handshake::drain_pending's poll-once pattern (single-poll
        // Ready when events are retained; Pending means truly empty).
        let mut out = Vec::new();
        loop {
            let recv = rx.recv();
            futures::pin_mut!(recv);
            let probed = std::future::poll_fn(|cx| match recv.as_mut().poll(cx) {
                std::task::Poll::Ready(v) => std::task::Poll::Ready(Some(v)),
                std::task::Poll::Pending => std::task::Poll::Ready(None),
            })
            .await;
            match probed {
                Some(Ok(ev)) => out.push(ev),
                _ => break,
            }
        }
        out
    }
}
