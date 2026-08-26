//! ConnectionManager entity: owns the server connection lifecycle.
//!
//! Responsibilities:
//! - handshake (Hello/Welcome), token from connect screen or client-state
//! - send Command with correlation ids; route Reply to the awaiting caller
//! - receive Events → hand to AppState folds → bump last_seq cursor → notify stores
//! - reconnect loop w/ exponential backoff; on Ready re-Subscribe from stored last_seq
//!   (SnapshotRequired ⇒ clear projections, refold from snapshot)

pub mod cursor;
pub mod ws;

// Imports to restore as you implement:
// use std::collections::HashMap;
//
// use fxproto::command::Command;
// use fxproto::envelope::{Message, PROTO_VERSION};
// use fxproto::event::Sequenced;
// use fxproto::ids::Seq;
// use fxproto::reply::Reply;
//
// use crate::conn::cursor::{self, ClientState};
// use crate::store::AppState;

// NOTE: ConnStatus is DEFINED HERE ONLY (locked: single definition site). store/mod.rs
// imports it via crate::conn::ConnStatus; never a second copy in store/.

// TODO:
//
// /// Rendered by views/mod.rs (status chip) + connect.rs error line.
// #[derive(Clone, Debug, PartialEq)]
// pub enum ConnStatus {
//     /// fatal = None  => ordinary idle/off state (pre-first-connect).
//     /// fatal = Some  => retrying is USELESS; a human must act. Sets the reason the
//     ///                  ConnectScreen renders verbatim (mapping below).
//     Disconnected { fatal: Option<FatalError> },
//     Connecting { attempt: u32 },
//     Ready,
// }
//
// /// Terminal failures — exactly the canonized close strings that are NOT recoverable
// /// by reconnecting. Strings live in fxproto envelope.rs docs; map them here.
// #[derive(Clone, Copy, Debug, PartialEq, Eq)]
// pub enum FatalError { AuthFailed, ProtocolVersion }   // ← "auth_failed" / "protocol_version"
//
// pub struct ConnectionManager {
//     status: ConnStatus,
//     cmd_tx: Option<ws::CmdSender>,       // live while Ready
//     next_id: u64,                        // correlation counter; client-minted (envelope.rs),
//                                          // starts at 1, per-connection — reset on every dial
//     pending: HashMap<u64, flume::Sender<Reply>>,   // awaiting responses (flume bounded(1):
//                                          // async recv on GPUI's executor — same flavor as ws.rs)
//     url: String, token: String,
// }
//
// impl ConnectionManager {
//     /// Create entity + start the reconnect loop on GPUI's background executor.
//     /// Loads ClientState via cursor::load() itself — owns last_seq durability end-to-end.
//     pub fn spawn(cx: &mut App, url: String, token: String) -> Entity<Self>;
//
//     /// Correlate + await one Reply. NEVER queues while not Ready.
//     pub async fn send(&mut self, cmd: Command) -> Result<Reply, SendError>;
//     // internal: on_event(seq'd ev) → AppState folds → cursor persist → cx.notify()
//     // internal: on_status(s) → update + notify observers (banner/badge/chip)
// }
//
// #[derive(Clone, Copy, Debug, PartialEq, Eq)]
// pub enum SendError { NotReady, Transport, ConnectionLost }
//
// ---------------------------------------------------------------------------
// STATE MACHINE — three states, transitions exhaustive:
//
//   Disconnected{fatal:None} --Connect / auto-reconnect timer--> Connecting{attempt:1}
//   Connecting{n}
//     --dial ok ∧ Welcome ∧ Subscribe answered by first replay/snapshot frame-->
//         Ready   (attempt := 0, fatal := None, durable ClientState NOT touched —
//                  only event ingest moves last_seq)
//     --retryable failure--> Connecting{n+1} after BACKOFF_DELAY(n), see table
//     --terminal failure----> Disconnected{fatal:Some(_)}, loop PARKS until an explicit
//                             new Connect call comes from ConnectScreen
//   Ready --socket dies (EOF/reset/ws.rs dead-peer timeout)/Close("resubscribe")-->
//         Connecting{attempt:1}; ALL in-flight commands fail fast (CORRELATION below)
//
// FAILURE CLASSIFICATION (close strings are the canonized trio from envelope.rs):
//   | trigger                                            | class    | action                 |
//   |----------------------------------------------------|----------|------------------------|
//   | dial fail (DNS/TCP/refused/TLS/WS upgrade error)   | retryable| backoff → attempt+1    |
//   | Close "resubscribe"   (server lag-kicked us)       | retryable| backoff → attempt+1    |
//   | mid-session socket death (EOF, reset, ws.rs 60s    | retryable| backoff → attempt+1;   |
//   | dead-peer silence)                                 |          | fail-fast pending map  |
//   | Close "auth_failed"                                | TERMINAL | Disconnected{AuthFailed} |
//   | Close "protocol_version"                           | TERMINAL | Disconnected{ProtocolVersion} |
//   Terminal rationale: a bad token or version skew NEVER fixes itself via retries;
//   each blind redial just trains users to ignore the retry loop. Backoff schedule
//   (n = attempt number at failure): delay = min(250ms * 2^(n-1), 8_000 ms) ⇒ 250ms,
//   500ms, 1s, 2s, 4s, 8s, 8s… ±20% deterministic jitter (delay * (0.9 + 0.2*((n % 2)))
//   is enough anti-sync). Sleep happens on GPUI's background executor, NOT tokio.
//
// HANDSHAKE DUTY (while Connecting — frames flow through ws::WsHandle):
//   1. send Hello { proto_version: PROTO_VERSION, token }.
//   2. expect Welcome { server_version, head_seq }; anything else / close ⇒ classify above.
//      Record head_seq only for logging — replay correctness is SERVER-side cursored.
//   3. send Subscribe { last_seq: Seq::from_raw(cursor.last_seq) } — EXACTLY ONCE,
//      immediately after Welcome (fxserver handshake rejects a second one forever).
//   4. Frame intake from here to Ready:
//        Message::Event { event }            => see EVENT INGEST below.
//        Message::SnapshotRequired { snapshot} => REPLACE all three stores wholesale with
//             snapshot.{agents, threads, perms}, set last_seq =
//             snapshot.baseline_seq.as_u64(), cursor::save() once for the batch. This is
//             the ONLY place projections are replaced instead of folded (model/mod.rs
//             delivery contract). Then status → Ready.
//        first Event after Subscribe also completes entry to Ready.
//
// EVENT INGEST ORDERING (per Sequenced frame; order is load-bearing, do not reorder):
//   a. AppState fold(s) run on ev.inner (&FxEvent) — store/mod.rs owns this step.
//   b. last_seq := ev.seq.as_u64(); cursor::save() immediately (cursor.rs timing rules).
//   c. notify observers so views re-render.
//
// CORRELATION MAP LIFECYCLE — and THE DECIDED FAIL-FAST POLICY:
//   send(cmd): status != Ready ⇒ Err(NotReady) NOW (never queue against a future link).
//     id = next_id++ ; pending.insert(id, bounded(1) sender) ;
//     try_send Request { id, command } — channel Full ⇒ rollback remove(id) +
//     Err(Transport). Await receiver.async_recv():
//       Ok(reply)                       => Ok(reply)
//       Err (sender dropped = conn died) => Err(ConnectionLost).
//   Response { id, .. } arrives => pending.remove(id); absent id ⇒ tracing::warn!
//     (stale duplicate after our timeout?) + ignore — never route garbage to awaiters.
//   CONNECTION DROP ⇒ EVERY pending sender is dropped IMMEDIATELY, failing all live
//     send() calls with ConnectionLost. Requeue-on-reconnect was CONSIDERED AND REJECTED
//     (locked): (1) a requeued Prompt may have already executed server-side before the
//     drop — replaying it doubles the user's turn silently; (2) correlation ids are
//     per-connection (envelope.rs), so even a safely-parked command can't be matched to
//     its waiter across reconnects without extra bookkeeping that exists only to hide
//     the outage; (3) UI-facing errors ("send failed — retry") are honest and cheap;
//     hidden multi-second stalls followed by surprise execution are neither.
