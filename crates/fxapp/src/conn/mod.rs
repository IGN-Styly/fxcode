//! ConnectionManager entity: owns the server connection lifecycle.
//!
//! Responsibilities:
//! - handshake (Hello/Welcome), token from connect screen or client-state
//! - send Command with correlation ids; route Reply to the awaiting caller
//! - receive Events → hand to AppState folds → bump last_seq cursor → notify stores
//! - reconnect loop w/ exponential backoff; on Ready re-Subscribe from stored last_seq
//!   (SnapshotRequired ⇒ clear projections, refold from snapshot)
//!
//! Threading model (kept simple on purpose): ONE foreground actor task owns the whole
//! state machine across dials — GPUI entities are main-thread objects, so every
//! mutation funnels through `WeakEntity::update`. Blocking parts (WS dial, backoff
//! sleeps) are parked on the background executor; frame intake awaits the flume
//! channel of conn/ws.rs, which awaits natively here.
//!
//! ---------------------------------------------------------------------------
//! STATE MACHINE — three states, transitions exhaustive:
//!
//!   Disconnected{fatal:None} --Connect / auto-reconnect timer--> Connecting{attempt:1}
//!   Connecting{n}
//!     --dial ok ∧ Welcome ∧ Subscribe answered by first replay/snapshot frame-->
//!         Ready   (attempt ladder resets; durable ClientState NOT touched on the
//!                  transition — only event ingest moves last_seq)
//!     --retryable failure--> Connecting{n+1} after BACKOFF_DELAY(n)
//!     --terminal failure----> Disconnected{fatal:Some(_)}; loop PARKS until a
//!                             fresh Connect call replaces this manager
//!   Ready --socket dies (EOF/reset/ws.rs dead-peer timeout)/Close("resubscribe")-->
//!         Connecting{attempt:1}; ALL in-flight commands fail fast (CORRELATION below)
//!
//! FAILURE CLASSIFICATION (close strings are the canonized trio from envelope.rs):
//!   | trigger                                            | class    | action                 |
//!   |----------------------------------------------------|----------|------------------------|
//!   | dial fail (DNS/TCP/refused/TLS/WS upgrade error)   | retryable| backoff → attempt+1    |
//!   | Close "resubscribe"   (server lag-kicked us)       | retryable| backoff → attempt+1    |
//!   | mid-session socket death (EOF, reset, ws.rs 60s    | retryable| backoff → attempt+1;   |
//!   | dead-peer silence)                                 |          | fail-fast pending map  |
//!   | Close "auth_failed"                                | TERMINAL | Disconnected{AuthFailed}      |
//!   | Close "protocol_version"                           | TERMINAL | Disconnected{ProtocolVersion} |
//!   Terminal rationale: a bad token or version skew NEVER fixes itself via retries;
//!   each blind redial just trains users to ignore the retry loop. Close reasons arrive
//!   via ws.rs as `WsEvent::Closed(reason)`; EOF shows up as recv Err. Unknown reason
//!   strings are treated retryable + warned (conservative).
//!
//! HANDSHAKE DUTY (while Connecting):
//!   1. send Hello { proto_version: PROTO_VERSION, token }.
//!   2. expect Welcome { server_version, head_seq }; anything else / close ⇒ classify.
//!      head_seq is only logged — replay correctness is SERVER-side cursored.
//!   3. send Subscribe { last_seq: Seq::from_raw(stored.last_seq) } — EXACTLY ONCE,
//!      immediately after Welcome (fxserver rejects a second one forever).
//!   4. First Event after Subscribe completes entry to Ready, as does
//!      SnapshotRequired after its wholesale replace.
//!
//! EVENT INGEST ORDERING (per Sequenced frame; load-bearing, cursor.rs rules):
//!   a. AppState fold(s) run on ev.inner (store/mod.rs::apply).
//!   b. last_seq := ev.seq.as_u64(); cursor save immediately.
//!   c. notify observers so views re-render.
//!
//! CORRELATION MAP LIFECYCLE — and THE DECIDED FAIL-FAST POLICY:
//!   send(cmd): status != Ready ⇒ Err(NotReady) NOW (never queue against a future link).
//!     id = next_id++ ; pending.insert(id, bounded(1) sender);
//!     try_send Request { id, command } — Full/Dropped ⇒ rollback remove(id) +
//!     Err(Transport). The returned Task awaits receiver.async_recv():
//!       Ok(reply)                       => Ok(reply)
//!       Err (sender dropped = conn died) => Err(ConnectionLost).
//!   Response { id, .. } arrives => pending.remove(id); absent id ⇒ warn! + ignore —
//!     never route garbage to awaiters. CONNECTION DROP clears the whole map at once,
//!     failing all live waiters with ConnectionLost. Requeue-on-reconnect was
//!     CONSIDERED AND REJECTED (locked): (1) a requeued Prompt may have already executed
//!     server-side before the drop — replaying it doubles the user's turn silently;
//!     (2) correlation ids are per-connection, so safely-parked commands cannot be
//!     matched across reconnects without bookkeeping that exists only to hide the
//!     outage; (3) UI-facing errors ("send failed — retry") are honest and cheap.

pub mod cursor;
pub mod ws;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, AppContext, Entity, Task};

use fxproto::command::Command;
use fxproto::envelope::{Message, PROTO_VERSION};
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::Seq;
use fxproto::reply::Reply;

use crate::conn::cursor::ClientState;
use crate::store::AppState;

// NOTE: ConnStatus is DEFINED HERE ONLY (locked: single definition site). store/mod.rs
// imports it via crate::conn::ConnStatus; never a second copy in store/.

/// Rendered by views/mod.rs (status chip) + connect.rs error line.
#[derive(Clone, Debug, PartialEq)]
pub enum ConnStatus {
    /// fatal = None  => ordinary idle/off state (pre-first-connect).
    /// fatal = Some  => retrying is USELESS; a human must act. Sets the reason the
    ///                  ConnectScreen renders verbatim (mapping lives there).
    Disconnected {
        fatal: Option<FatalError>,
    },
    Connecting {
        attempt: u32,
    },
    Ready,
}

/// Terminal failures — exactly the canonized close strings that are NOT recoverable
/// by reconnecting. Strings live in fxproto envelope.rs docs; mapped here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FatalError {
    /// ← "auth_failed"
    AuthFailed,
    /// ← "protocol_version"
    ProtocolVersion,
}

impl FatalError {
    pub fn from_close_reason(reason: &str) -> Option<Self> {
        match reason {
            "auth_failed" => Some(Self::AuthFailed),
            "protocol_version" => Some(Self::ProtocolVersion),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// Status != Ready at call time (never queue against a future link).
    NotReady,
    /// Outbound queue Full or dropped mid-push.
    Transport,
    /// Awaited reply went nowhere: pending sender dropped (connection died).
    ConnectionLost,
}

/// Backoff schedule (n = attempt number at failure):
/// delay = min(250ms * 2^(n-1), 8_000 ms) ⇒ 250ms, 500ms, 1s, 2s, 4s, 8s… plus ±20%
/// deterministic jitter (parity-based anti-sync). Sleeps run on GPUI's background
/// executor, NOT tokio.
const BACKOFF_BASE_MS: u64 = 250;
const BACKOFF_CAP_MS: u64 = 8_000;

pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    let raw = BACKOFF_BASE_MS
        .saturating_mul(1_u64 << exponent)
        .min(BACKOFF_CAP_MS);
    // Integer thousandths of the blueprint factor 0.9 + 0.2·(n % 2).
    let factor_milli: u64 = if attempt.is_multiple_of(2) { 900 } else { 1100 };
    Duration::from_millis(raw * factor_milli / 1000)
}

/// Correlation ids are CLIENT-minted per CONNECTION: start at 1, monotonically
/// increase, reset on every dial (fxproto envelope.rs).
pub(crate) fn mint_correlation(counter: &mut u64) -> u64 {
    *counter += 1;
    *counter
}

/// Outcome classification of whatever ended the current socket.
#[derive(Debug, PartialEq, Eq)]
enum SessionEnd {
    Retryable,
    Terminal(FatalError),
}

pub struct ConnectionManager {
    status: ConnStatus,
    cmd_tx: Option<flume::Sender<Message>>, // live while a link exists; Ready drives it
    next_id: u64,                           // mint_correlation counter (resets per dial)
    pending: HashMap<u64, flume::Sender<Reply>>, // awaiting responses (flume bounded(1))

    url: Option<String>,
    token: Option<String>,
    /// One DetectAgents per link (refreshed on reconnect so newly installed
    /// agents appear without app restart).
    detect_requested: bool,
    state_file_dir: PathBuf, // injectable seam for tests; default ~/.fxcode
    stored: ClientState,     // cursor durability seed; mutated on ingest

    /// Mirror of the live link's ping RTT for the M0 latency badge (status bar
    /// reads rtt_ms(); never used for protocol decisions here).
    rtt_ms: u64,

    /// Set once a link ever flipped the status to Ready during ITS lifetime,
    /// so the attempt ladder resets to 1 on the next socket death.
    had_ready_link: bool,
}

impl ConnectionManager {
    /// Boot-seeding constructor (`ConnectionManager::spawn` of the blueprint).
    ///
    /// Reads ClientState via cursor::load() itself — owns last_seq durability end-to-end.
    /// Explicit args beat remembered state; overriding values refresh the file once
    /// immediately (last_seq untouched — only event ingest moves it).
    ///
    /// With neither args nor remembered state we boot IDLE
    /// (`Disconnected { fatal: None }`) without dialing; ConnectScreen drives from
    /// there via [`Self::connect`].
    ///
    /// "Exactly one manager per process" is enforced at the VIEW layer:
    /// WorkspaceView holds the sole handle and replaces it wholesale on a fresh
    /// Connect click (calling this never double-dials on its own).
    pub fn spawn(cx: &mut App, url: Option<String>, token: Option<String>) -> Entity<Self> {
        let state_file_dir = cursor::default_dir();
        let stored = cursor::load(&state_file_dir);

        let url = url.or_else(|| stored.server_url.clone());
        let token = token.or_else(|| stored.token.clone());

        if let (Some(url), Some(token)) = (&url, &token)
            && (stored.server_url.as_deref() != Some(url.as_str())
                || stored.token.as_deref() != Some(token.as_str()))
        {
            let refreshed = ClientState {
                server_url: Some(url.clone()),
                token: Some(token.clone()),
                ..stored.clone()
            };
            if let Err(error) = refreshed.save(&state_file_dir) {
                tracing::warn!(error = %error, "could not persist refreshed client state");
            }
        }

        let start = url.is_some() && token.is_some();
        cx.new(|cx| {
            let manager = Self {
                status: ConnStatus::Disconnected { fatal: None },
                cmd_tx: None,
                next_id: 0,
                pending: HashMap::new(),
                url,
                token,
                detect_requested: false,
                state_file_dir,
                stored,
                rtt_ms: 0,
                had_ready_link: false,
            };
            if start {
                run_session_loop(cx.entity().downgrade(), cx);
            }
            manager
        })
    }

    /// Fresh user-initiated connection from ConnectScreen: unlike [`Self::spawn`]
    /// the pair here is authoritative (already normalize_url-validated upstream);
    /// replacing an existing manager is done by dropping the old Entity handle.
    pub fn connect(cx: &mut App, url: String, token: String) -> Entity<Self> {
        Self::spawn(cx, Some(url), Some(token))
    }

    #[allow(dead_code)] // single accessor site for tests/diagnostics
    pub fn status(&self) -> &ConnStatus {
        &self.status
    }

    /// Latest mirrored ping RTT in ms (0 = none measured yet).
    pub fn rtt_ms(&self) -> u64 {
        self.rtt_ms
    }

    /// Resume-cursor value as far as this entity knows it (diagnostics/tests).
    #[allow(dead_code)]
    pub fn last_seq(&self) -> u64 {
        self.stored.last_seq
    }

    // -----------------------------------------------------------------------
    // Commands (correlation map entrypoint)
    // -----------------------------------------------------------------------

    /// Correlate + await one Reply. NEVER queues while not Ready.
    ///
    /// DEVIATION vs the sketchy `pub async fn send(&mut self)`: GPUI entities are
    /// main-thread objects and cannot be borrowed across `.await`; the async body
    /// therefore lives INSIDE the returned Task (the awaiter part is unchanged:
    /// bounded(1) receiver resolves with exactly one reply or ConnectionLost).
    /// Call it like:
    /// ```ignore
    /// let reply = manager.update(cx, |m, cx| m.send(command, cx));
    /// ```
    pub fn send(
        &mut self,
        command: Command,
        cx: &mut gpui::Context<Self>,
    ) -> Result<Task<Result<Reply, SendError>>, SendError> {
        if self.status != ConnStatus::Ready || !self.subscribed_gate_open() {
            return Err(SendError::NotReady);
        }
        let id = mint_correlation(&mut self.next_id);

        let (waiter, receiver) = flume::bounded::<Reply>(1);
        self.pending.insert(id, waiter);

        let frame = Message::Request { id, command };
        match self.cmd_tx.as_ref() {
            Some(out) if out.try_send(frame).is_ok() => {
                Ok(cx.background_executor().spawn(async move {
                    receiver
                        .recv_async()
                        .await
                        .map_err(|_| SendError::ConnectionLost)
                }))
            }
            _ => {
                self.pending.remove(&id); // rollback
                Err(SendError::Transport)
            }
        }
    }

    fn subscribed_gate_open(&self) -> bool {
        self.status == ConnStatus::Ready
    }

    // -----------------------------------------------------------------------
    // Internal transitions (all on the main thread, Context<Self>)
    // -----------------------------------------------------------------------

    fn set_status(&mut self, status: ConnStatus, cx: &mut gpui::Context<Self>) {
        if self.status != status {
            self.status = status.clone();
            cx.global_mut::<AppState>().conn_status = status;
            cx.notify();
        }
    }

    fn note_dial_failure(&mut self, attempt: u32, cx: &mut gpui::Context<Self>) {
        self.set_status(ConnStatus::Connecting { attempt }, cx);
    }

    fn current_attempt(&self) -> u32 {
        match &self.status {
            ConnStatus::Connecting { attempt } => *attempt,
            _ => 0,
        }
    }

    /// New physical link: correlation counters reset per connection.
    fn begin_link(&mut self, out_tx: flume::Sender<Message>) {
        self.next_id = 0;
        self.pending.clear();
        self.cmd_tx = Some(out_tx);
        self.rtt_ms = 0;
        self.detect_requested = false;
    }

    /// Fire-and-forget DetectAgents ONCE per link. Called exactly when Ready is
    /// promoted so the composer picker has data by first paint of a thread.
    fn request_detect_agents(&mut self, cx: &mut gpui::Context<Self>) {
        if self.detect_requested {
            return;
        }
        self.detect_requested = true;
        if let Ok(task) = self.send(Command::DetectAgents, cx) {
            task.detach();
        }
    }

    /// Fail-fast policy application point; also tears transient link state down
    /// so a subsequent Connect-style call finds pristine fields.
    fn teardown_link(&mut self, end: SessionEnd, cx: &mut gpui::Context<Self>) {
        // EVERY pending waiter gets ConnectionLost BEFORE any status change shows.
        self.pending.clear();
        self.cmd_tx = None;
        self.rtt_ms = 0;
        match end {
            SessionEnd::Terminal(fatal) => {
                self.set_status(ConnStatus::Disconnected { fatal: Some(fatal) }, cx)
            }
            SessionEnd::Retryable => {} // caller continues the loop
        }
    }

    fn observed_frame_rtt(&mut self, rtt_snapshot: u64) {
        self.rtt_ms = rtt_snapshot;
    }

    /// Route one inbound frame while a link is up. Returns whether THIS frame
    /// flipped the link into Ready (attempt-ladder reset signal).
    fn route_frame(
        &mut self,
        message: Message,
        rtt_snapshot: u64,
        cx: &mut gpui::Context<Self>,
    ) -> Result<bool, SessionEnd> {
        self.observed_frame_rtt(rtt_snapshot);

        match message {
            Message::Welcome {
                server_version,
                head_seq,
            } => {
                tracing::info!(
                    server_version = %server_version,
                    head = %head_seq,
                    "welcome received (head recorded for logging only)"
                );
                Ok(false)
            }
            Message::Event { event } => self.ingest(event, cx),
            Message::SnapshotRequired { snapshot } => {
                // THE ONLY place projections are replaced instead of folded
                // (model/mod.rs delivery contract). Then status → Ready.
                let baseline = cx.global_mut::<AppState>().replace_all(&snapshot);
                self.stored.last_seq = baseline;
                if let Err(error) = self.stored.save(&self.state_file_dir) {
                    tracing::warn!(error = %error, "snapshot cursor save failed");
                }
                self.set_status(ConnStatus::Ready, cx);
                self.had_ready_link = true;
                self.request_detect_agents(cx);
                Ok(true)
            }
            Message::Response { id, reply } => {
                if let Reply::DetectedAgents { drivers } = &reply {
                    // Central stash so ANY view (not just the awaiting caller)
                    // renders the picker immediately; waiter still resolves.
                    cx.global_mut::<AppState>().record_detected(drivers.clone());
                }
                match self.pending.remove(&id) {
                    Some(waiter) => {
                        // bounded(1): buffer absorbs the reply even though the
                        // awaiter may not have polled yet.
                        if waiter.send(reply).is_err() {
                            tracing::debug!(id, "reply arrived for abandoned awaiter");
                        }
                    }
                    None => {
                        tracing::warn!(id, "response for unknown/stale correlation id; ignored")
                    }
                }
                Ok(false)
            }
            Message::Hello { .. } | Message::Subscribe { .. } | Message::Request { .. } => {
                // Server speaking client frames: garbled-log symptom; kill the
                // link rather than guess what came off bytewise.
                tracing::error!("protocol violation: unexpected client-side frame");
                Err(SessionEnd::Retryable)
            }
        }
    }

    /// EVENT INGEST ORDERING: fold FIRST, then cursor advance + save, then Ready.
    fn ingest(
        &mut self,
        ev: Sequenced<FxEvent>,
        cx: &mut gpui::Context<Self>,
    ) -> Result<bool, SessionEnd> {
        // Fold(s) FIRST — b/c below depend on them having run (cursor rules).
        let seq_done = cx.global_mut::<AppState>().apply(&ev);

        self.stored.advance_to_seq(seq_done);
        if let Err(error) = self.stored.save(&self.state_file_dir) {
            // Under-cursor persistence merely replays forward safely next boot.
            tracing::warn!(error = %error, "cursor save failed");
        }

        self.request_detect_agents(cx);
        if self.status == ConnStatus::Ready {
            Ok(false)
        } else {
            self.set_status(ConnStatus::Ready, cx);
            self.had_ready_link = true;
            Ok(true)
        }
    }
}

// ---------------------------------------------------------------------------
// Session/reconnect loop (the foreground actor)
// ---------------------------------------------------------------------------

fn run_session_loop(
    self_: gpui::WeakEntity<ConnectionManager>,
    cx: &mut gpui::Context<ConnectionManager>,
) {
    let _ = self_; // weak handle is captured through the spawn closure below
    cx.spawn(async move |this: gpui::WeakEntity<ConnectionManager>, cx| {
        'sessions: loop {
            // Config read every cycle so a replaced Manager stops cleanly.
            let Some((url, token)) = this
                .update(cx, |m, _| m.url.clone().zip(m.token.clone()))
                .ok()
                .flatten()
            else {
                return; // released or parked-idle; nothing to drive
            };

            let mut attempt: u32 = this
                .update(cx, |m, _| m.current_attempt())
                .unwrap_or_default();

            attempt += 1;
            this.update(cx, |m, cx| {
                m.set_status(ConnStatus::Connecting { attempt }, cx)
            })
            .ok();

            // Normalize defensively — normally pre-validated upstream.
            let target = match ws::normalize_url(&url) {
                Ok(u) => u,
                Err(error) => {
                    // Programming/config error: NEVER guessed; park instead of spinning.
                    tracing::error!(error = %error, url = %url, "cannot dial invalid url");
                    this.update(cx, |m, cx| {
                        m.teardown_link(SessionEnd::Retryable, cx); // drop pendings
                        m.url = None;
                        m.token = None;
                        m.set_status(ConnStatus::Disconnected { fatal: None }, cx);
                    })
                    .ok();
                    return;
                }
            };

            // DIAL — blocking call parked onto the background executor.
            let dial_target = target.clone();
            let handle = match cx
                .background_executor()
                .spawn(async move { ws::WsHandle::connect(&dial_target) })
                .await
            {
                Ok(handle) => handle,
                Err(error) => {
                    tracing::debug!(error = %error, url = %target.to_string(),
                        "dial failed; backing off");
                    this.update(cx, |m, cx| m.note_dial_failure(attempt, cx))
                        .ok();
                    sleep_backoff(cx, attempt).await;
                    continue 'sessions;
                }
            };

            let out_tx = handle.sender();
            this.update(cx, |m, _| m.begin_link(out_tx.clone())).ok();

            // HANDSHAKE step 1: Hello (ws pumps already running, incl. keepalive).
            if out_tx
                .send(Message::Hello {
                    proto_version: PROTO_VERSION,
                    token: token.clone(),
                })
                .is_err()
            {
                finish_session(&this, SessionEnd::Retryable, cx).await;
                sleep_backoff(cx, attempt).await;
                continue 'sessions;
            }

            let mut was_ready = false;
            let end = drive_session(&this, handle, &mut was_ready, cx).await;
            if was_ready {
                attempt = 0; // ladder restarts on next death from Ready
            }
            finish_session(&this, end, cx).await;

            if let ConnStatus::Disconnected { fatal: Some(_) } = this
                .update(cx, |m, _| m.status.clone())
                .unwrap_or_else(|_| terminal_default())
            {
                return; // loop PARKS
            }
            sleep_backoff(cx, attempt.max(1)).await;
        }
    })
    .detach();
}

fn terminal_default() -> ConnStatus {
    ConnStatus::Disconnected { fatal: None }
}

async fn sleep_backoff(cx: &mut gpui::AsyncApp, attempt: u32) {
    cx.background_executor()
        .timer(backoff_delay(attempt.max(1)))
        .await;
}

async fn finish_session(
    this: &gpui::WeakEntity<ConnectionManager>,
    end: SessionEnd,
    cx: &mut gpui::AsyncApp,
) {
    // drive_session owns the WsHandle; when it returns the handle is dropped,
    // which kills the pumps (out side Disconnected ⇒ shutdown watch flips).
    this.update(cx, |m, cx| m.teardown_link(end, cx)).ok();
}

/// Intake for ONE established link: Welcome → Subscribe (exactly once) →
/// replay/live tail until the socket ends. Never exits early on success.
async fn drive_session(
    this: &gpui::WeakEntity<ConnectionManager>,
    handle: ws::WsHandle,
    was_ready: &mut bool,
    cx: &mut gpui::AsyncApp,
) -> SessionEnd {
    use ws::WsEvent;

    // ---- Phase A: Welcome (exactly ONE frame expected before we may Subscribe) ---
    let last_seq: u64 = match handle.recv().await {
        Err(_) => return SessionEnd::Retryable, // EOF/reset before any close
        Ok(WsEvent::Closed(reason)) => return classify_close(reason.as_deref()),
        Ok(WsEvent::Message(message @ Message::Welcome { .. })) => {
            let stepped = if let Ok(step) =
                this.update(cx, |m, cx| m.route_frame(message, handle.rtt_ms(), cx))
            {
                step
            } else {
                return SessionEnd::Retryable; // entity released
            };
            debug_assert!(stepped.is_ok(), "welcome cannot end a session");
            this.update(cx, |m, _| m.stored.last_seq).unwrap_or(0)
        }
        Ok(WsEvent::Message(other)) => {
            tracing::error!(frame = ?other_kind(&other),
                "handshake violated: expected Welcome first");
            return SessionEnd::Retryable;
        }
    };

    // ---- Phase B: Subscribe EXACTLY ONCE ------------------------------------
    // Liveness rule: the link is Ready the moment OUR Subscribe is on the wire.
    // The server guarantees replay-then-live in stream order, so any early
    // events fold while we sit at Ready (ingest keeps that promotion as a
    // belt-and-braces for snapshot paths). Pinning Ready to the FIRST event
    // instead wedged fresh servers with EMPTY logs forever — Welcome followed
    // by silence is a valid terminal handshake state (2026-08 live bug: the
    // app sat on Connecting against a brand-new data dir).
    let subscribed = handle.try_send(Message::Subscribe {
        last_seq: Seq::new(last_seq),
    });
    if let Err(_send_err) = subscribed {
        return SessionEnd::Retryable;
    }
    this.update(cx, |m, cx| {
        if m.status != ConnStatus::Ready {
            m.set_status(ConnStatus::Ready, cx);
            m.had_ready_link = true;
            *was_ready = true;
        }
        m.request_detect_agents(cx);
    })
    .ok();

    // ---- Phase C: live tail (replay batch first, then steady stream) --------
    loop {
        if handle.broken() {
            return SessionEnd::Retryable; // dead-peer/keepalive declared death
        }
        match handle.recv().await {
            Err(_) => return SessionEnd::Retryable,
            Ok(WsEvent::Closed(reason)) => return classify_close(reason.as_deref()),
            Ok(WsEvent::Message(message)) => {
                let reached = match this
                    .update(cx, |m, cx| m.route_frame(message, handle.rtt_ms(), cx))
                    .ok()
                {
                    Some(Ok(reached)) => reached,
                    Some(Err(end)) => return end,
                    None => return SessionEnd::Retryable, // entity released
                };
                *was_ready |= reached;
            }
        }
    }
}

fn classify_close(reason: Option<&str>) -> SessionEnd {
    match FatalError::from_close_reason(reason.unwrap_or_default()) {
        Some(fatal) => {
            tracing::warn!(reason = reason, "terminal close; parking manager");
            SessionEnd::Terminal(fatal)
        }
        None => {
            tracing::warn!(reason = ?reason, "connection closed; treating as retryable");
            SessionEnd::Retryable
        }
    }
}

fn other_kind(message: &Message) -> &'static str {
    match message {
        Message::Hello { .. } => "hello",
        Message::Welcome { .. } => "welcome",
        Message::Request { .. } => "request",
        Message::Response { .. } => "response",
        Message::Event { .. } => "event",
        Message::Subscribe { .. } => "subscribe",
        Message::SnapshotRequired { .. } => "snapshot_required",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Backoff ladder: 250ms, 500ms, 1s, 2s, 4s, 8s capped, with parity jitter
    // (odd attempts run 1.1×, even attempts 0.9×).
    #[test]
    fn backoff_ladder_doubles_and_caps_with_parity_jitter() {
        let expect = |n: u32, base_ms: u64| {
            assert_eq!(
                backoff_delay(n),
                Duration::from_millis(base_ms),
                "attempt {n}"
            );
        };
        // attempt=1 ⇒ base 250ms × 1.1
        expect(1, 275);
        expect(2, 450); // 500 × 0.9
        expect(3, 1100); // 1000 × 1.1
        expect(4, 1800); // 2000 × 0.9
        expect(5, 4400); // 4000 × 1.1
        expect(6, 7200); // 8000 × 0.9 (cap applied before jitter)
        expect(7, 8800); // cap again, jitter up
    }

    #[test]
    fn correlation_ids_start_at_one_and_are_monotonic() {
        let mut counter = 0;
        assert_eq!(mint_correlation(&mut counter), 1);
        for expected in 2..=7 {
            assert_eq!(mint_correlation(&mut counter), expected);
        }
        // Reset-on-dial is the caller's job; the mint itself never restarts.
        assert_eq!(counter, 7);
    }

    #[test]
    fn close_reason_classification_matches_envelope_canon() {
        assert_eq!(
            FatalError::from_close_reason("auth_failed"),
            Some(FatalError::AuthFailed)
        );
        assert_eq!(
            FatalError::from_close_reason("protocol_version"),
            Some(FatalError::ProtocolVersion)
        );
        assert_eq!(
            FatalError::from_close_reason("resubscribe"),
            None,
            "retryable"
        );
        assert_eq!(FatalError::from_close_reason("anything-else"), None);
        assert_eq!(
            classify_close(Some("auth_failed")),
            SessionEnd::Terminal(FatalError::AuthFailed)
        );
        assert_eq!(
            classify_close(Some("resubscribe")),
            SessionEnd::Retryable,
            "lag kick = retry"
        );
        assert_eq!(classify_close(None), SessionEnd::Retryable, "silent EOF");
    }
}
