//! Broadcast bus with an explicit lag policy.

// Imports to restore as you define the types:
// use tokio::sync::broadcast;
//
// use fxproto::event::{FxEvent, Sequenced};

// TODO:
//
// /// Fanout capacity — the ONE number. fxserver's per-client out channel pairs
// /// against it by importing THIS const (fxserver/src/net/client.rs documents
// /// the pairing; do not hardcode 1024 there a second time).
// pub const BUS_CAPACITY: usize = 1024;
//
// /// Wrapper over tokio broadcast so the policy lives in one place:
// /// - capacity: BUS_CAPACITY events (≈ one full replay-sized burst of headroom)
// /// - on lag (RecvError::Lagged(n)): receiver SKIPPED n events — do NOT try to
// ///   paper over it, backfill inline, or buffer more. recv() surfaces
// ///   BusError::Lagged(n) EXACTLY ONCE, then keeps working on newer events.
// ///   The ws layer (fxserver/src/net/client.rs) maps that error to a WS Close
// ///   with reason string "resubscribe" and tears the connection down; the
// ///   client reconnects from its stored cursor, so nothing is lost. THE
// ///   LITERAL STRING IS fxserver'S CONTRACT WITH CLIENTS — bus.rs only
// ///   guarantees the Lagged signal exists and is observable before silence.
// /// - send NEVER blocks or drops for lack of subscribers: broadcast::Sender::
// ///   send fails only with Closed (= zero receivers), which is not an error
// ///   worth surfacing (early clients simply weren't attached yet).
// #[derive(Clone)]
// pub struct EventBus { tx: broadcast::Sender<Sequenced<FxEvent>> }
//
// impl EventBus {
//     /// Real deployments pass crate::bus::BUS_CAPACITY; parameterized so tests
//     /// can force lag with tiny values (e.g. capacity 2).
//     pub fn new(capacity: usize) -> Self;
//
//     /// Fire-and-forget: call ONLY with the freshly-sequenced event returned by
//     /// EventSink::emit, while still holding the sink mutex — this is what makes
//     /// "broadcast order == seq order globally" true (see cmd/mod.rs pipeline).
//     /// Passing an unsequenced FxEvent here is a bug the type system now blocks:
//     /// the old draft said `send(&self, ev: FxEvent)` — superseded deliberately.
//     pub fn send(&self, ev: Sequenced<FxEvent>);   // `.ok()`-style ignore on Closed
//
//     /// Attach one consumer. Safe any time (before first event too).
//     pub fn subscribe(&self) -> BusReceiver;
// }
//
// /// Thin wrapper exposing exactly one operation — keep it honest:
// pub struct BusReceiver { rx: broadcast::Receiver<Sequenced<FxEvent>> }
//
// #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
// pub enum BusError {
//     /// This subscriber skipped N events. The consumer MUST treat its view as
//     /// poisoned and disconnect/resubscribe — never attempt local repair.
//     #[error("lagged: skipped {0} events")]
//     Lagged(u64),
//     /// EventBus dropped (orchestrator shutdown). Terminal.
//     #[error("event bus closed")]
//     Closed,
// }
//
// impl BusReceiver {
//     /// Straight passthrough mapping RecvError::{Lagged ⇒ Lagged(n),
//     /// Closed ⇒ Closed}. After Lagged, further recvs resume from the newest
//     /// retained event (tokio semantics) — legal but pointless for ws clients,
//     /// which exit at first Lagged per the contract above.
//     pub async fn recv(&mut self) -> Result<Sequenced<FxEvent>, BusError>;
// }
//
// // Ordering guarantee test (impl.md Phase 2.3) — pin BOTH properties:
// //   1. single subscriber + M emissions through one sink ⇒ seqs arrive 1..=M
// //      strictly increasing, no dups;
// //   2. N subscribers get IDENTICAL seq multisets;
// //   3. tiny-capacity bus (2) + slow receiver (no recv while M=10 emitted)
// //      ⇒ exactly one Lagged(8)-shaped error eventually observed.
// // NOTE ordering guarantee rests on emit() assigning seq THEN sending under the
// // same mutex — append-to-store assigns seq; THEN bus.send. So any receiver
// // sees seq strictly increasing per subscription.
