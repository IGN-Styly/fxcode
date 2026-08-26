//! Broadcast bus with an explicit lag policy.

// Imports to restore as you define the types:
// use tokio::sync::broadcast;
// use fxproto::event::{FxEvent, Sequenced};

// TODO:
//
// /// Wrapper over tokio broadcast so the policy lives in one place:
// /// - capacity: bounded (start ~1024)
// /// - on lag (RecvError::Lagged): receiver SKIPPED events — do NOT try to paper over it.
// ///   The ws client layer detects lag, disconnects that client with a Resubscribe notice;
// ///   cursor-based replay makes reconnect cheap. Never block the orchestrator for slow
// ///   consumers.
// #[derive(Clone)]
// pub struct EventBus { tx: broadcast::Sender<Sequenced<FxEvent>> }
//
// impl EventBus {
//     pub fn new(capacity: usize) -> Self;
//     pub fn send(&self, ev: FxEvent) /* fire-and-forget; persist happens BEFORE send */;
//     pub fn subscribe(&self) -> BusReceiver;  // thin wrapper exposing recv() -> Sequenced<FxEvent>
// }
//
// NOTE ordering guarantee: append-to-store assigns seq; THEN bus.send. So any receiver
// sees seq strictly increasing per subscription. Write a test pinning this.
