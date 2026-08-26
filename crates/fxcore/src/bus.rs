//! Broadcast bus with an explicit lag policy.
//!
//! Thin wrapper over tokio broadcast so THE policy lives in exactly one place:
//! lag = drop + flag (never block, never buffer more), send never blocks. The
//! ordering guarantee rests on the pipeline contract in cmd/mod.rs: append to
//! the store assigns seq, THEN bus.send happens under the same sink mutex —
//! so any receiver sees seq strictly increasing per subscription.

use tokio::sync::broadcast;

use fxproto::event::{FxEvent, Sequenced};

/// Fanout capacity — the ONE number. fxserver's per-client out channel pairs
/// against it by importing THIS const (fxserver/src/net/client.rs documents
/// the pairing; do not hardcode 1024 there a second time).
pub const BUS_CAPACITY: usize = 1024;

/// Wrapper over tokio broadcast so the policy lives in one place:
/// - capacity: BUS_CAPACITY events (≈ one full replay-sized burst of headroom)
///   in real deployments; parameterized here so tests can force lag.
/// - on lag (RecvError::Lagged(n)): receiver SKIPPED n events — do NOT try to
///   paper over it, backfill inline, or buffer more. recv() surfaces
///   BusError::Lagged(n) EXACTLY ONCE, then keeps working on newer events.
///   The ws layer (fxserver/src/net/client.rs) maps that error to a WS Close
///   with reason string "resubscribe" and tears the connection down; the
///   client reconnects from its stored cursor, so nothing is lost. THE
///   LITERAL STRING IS fxserver'S CONTRACT WITH CLIENTS — this module only
///   guarantees the Lagged signal exists and is observable before silence.
/// - send NEVER blocks or drops for lack of subscribers: broadcast::Sender::
///   send fails only with Closed (= zero receivers), which is not an error
///   worth surfacing (early clients simply weren't attached yet).
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Sequenced<FxEvent>>,
}

impl EventBus {
    /// Real deployments pass crate::bus::BUS_CAPACITY; parameterized so tests
    /// can force lag with tiny values (e.g. capacity 2).
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Fire-and-forget: call ONLY with the freshly-sequenced event returned by
    /// EventSink::emit, while still holding the sink mutex — this is what makes
    /// "broadcast order == seq order globally" true (see cmd/mod.rs pipeline).
    /// Passing an unsequenced FxEvent here is a bug the type system now blocks:
    /// the old draft said `send(&self, ev: FxEvent)` — superseded deliberately.
    ///
    /// Closed (= zero receivers) is ignored on purpose: fire-and-forget means
    /// no subscriber yet is fine, not a failure worth a Result.
    pub fn send(&self, ev: Sequenced<FxEvent>) {
        let _ = self.tx.send(ev);
    }

    /// Attach one consumer. Safe any time (before first event too).
    pub fn subscribe(&self) -> BusReceiver {
        BusReceiver {
            rx: self.tx.subscribe(),
        }
    }
}

/// Thin wrapper exposing exactly one operation — keep it honest:
pub struct BusReceiver {
    rx: broadcast::Receiver<Sequenced<FxEvent>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BusError {
    /// This subscriber skipped N events. The consumer MUST treat its view as
    /// poisoned and disconnect/resubscribe — never attempt local repair.
    #[error("lagged: skipped {0} events")]
    Lagged(u64),
    /// EventBus dropped (orchestrator shutdown). Terminal.
    #[error("event bus closed")]
    Closed,
}

impl BusReceiver {
    /// Straight passthrough mapping RecvError::{Lagged ⇒ Lagged(n),
    /// Closed ⇒ Closed}. After Lagged, further recvs resume from the newest
    /// retained event (tokio semantics) — legal but pointless for ws clients,
    /// which exit at first Lagged per the contract above.
    pub async fn recv(&mut self) -> Result<Sequenced<FxEvent>, BusError> {
        match self.rx.recv().await {
            Ok(ev) => Ok(ev),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Err(BusError::Lagged(skipped)),
            Err(broadcast::error::RecvError::Closed) => Err(BusError::Closed),
        }
    }
}

// Ordering guarantee tests (impl.md Phase 2.3) — pin ALL THREE properties:
//   1. single subscriber + M emissions through one sink ⇒ seqs arrive 1..=M
//      strictly increasing, no dups;
//   2. N subscribers get IDENTICAL seq multisets;
//   3. tiny-capacity bus (2) + slow receiver (no recv while M=10 emitted)
//      ⇒ exactly one Lagged(8)-shaped error eventually observed.
// NOTE ordering guarantee rests on emit() assigning seq THEN sending under the
// same mutex — append-to-store assigns seq; THEN bus.send. So any receiver
// sees seq strictly increasing per subscription.
#[cfg(test)]
mod tests {
    use super::*;
    use fxproto::ids::{Seq, SessionId, TurnId};
    use std::collections::BTreeSet;

    fn chunk(seq: u64, text: &str) -> Sequenced<FxEvent> {
        Sequenced {
            seq: Seq::new(seq),
            inner: FxEvent::Chunk {
                session: SessionId::from_raw("s".into()),
                turn: TurnId::from_raw("t".into()),
                role: fxproto::content::Role::Agent,
                text: text.into(),
            },
        }
    }

    fn assert_strictly_increasing(events: &[Sequenced<FxEvent>]) {
        for pair in events.windows(2) {
            assert!(
                pair[1].seq > pair[0].seq,
                "seq regressed/duped: {} -> {}",
                pair[0].seq,
                pair[1].seq
            );
        }
    }

    #[tokio::test]
    async fn single_subscriber_sees_every_seq_exactly_once_ascending() {
        let bus = EventBus::new(BUS_CAPACITY);
        let mut rx = bus.subscribe();
        const M: u64 = 200;
        for i in 1..=M {
            bus.send(chunk(i, "x"));
        }
        let mut seen = Vec::new();
        for _ in 0..M {
            seen.push(rx.recv().await.unwrap());
        }
        assert_eq!(seen.len(), M as usize);
        assert_strictly_increasing(&seen);
        let seqs: BTreeSet<u64> = seen.iter().map(|e| e.seq.as_u64()).collect();
        assert_eq!(seqs, (1..=M).collect::<BTreeSet<_>>(), "no dups, no gaps");
    }

    #[tokio::test]
    async fn every_subscriber_gets_identical_order() {
        let bus = EventBus::new(BUS_CAPACITY);
        let mut receivers: Vec<_> = (0..5).map(|_| bus.subscribe()).collect();
        for i in 1..=50u64 {
            bus.send(chunk(i, "fanout"));
        }
        for rx in &mut receivers {
            let mut seen = Vec::new();
            for _ in 0..50 {
                seen.push(rx.recv().await.unwrap());
            }
            assert_strictly_increasing(&seen);
            let seqs: Vec<u64> = seen.iter().map(|e| e.seq.as_u64()).collect();
            assert_eq!(seqs, (1..=50).collect::<Vec<_>>());
        }
    }

    #[tokio::test]
    async fn forced_lag_surfaces_one_lagged_error_then_recovers_on_newer_events() {
        // Capacity 2 + 10 emissions while the subscriber never polls ⇒ it
        // missed 10 - 2 = 8 events; tokio retains the newest two.
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for i in 1..=10u64 {
            bus.send(chunk(i, "burst"));
        }
        let lag_err = rx.recv().await.unwrap_err();
        assert_eq!(lag_err, BusError::Lagged(8));

        // Retained tail arrives first, still strictly ordered...
        let kept_1 = rx.recv().await.unwrap();
        let kept_2 = rx.recv().await.unwrap();
        assert_eq!((kept_1.seq.as_u64(), kept_2.seq.as_u64()), (9, 10));
        assert!(kept_1.seq < kept_2.seq);

        // ...and subsequent recvs succeed with NEWER events, seq continuing
        // upward. Send/receive INTERLEAVED: a subscriber that goes quiet
        // again during another over-capacity burst gets ANOTHER Lagged by
        // the same policy (poison-then-resubscribe is the only legal reply),
        // so "keeps working on newer events" presumes a consumer that keeps
        // up. Exactly ONE Lagged surfaces across this whole session.
        for i in 11..=15u64 {
            bus.send(chunk(i, "post-lag"));
            let ev = rx.recv().await.unwrap();
            assert_eq!(ev.seq.as_u64(), i);
        }
        assert_eq!(lag_err, BusError::Lagged(8));
    }

    #[tokio::test]
    async fn send_with_zero_receivers_is_fire_and_forget_ok() {
        let bus = EventBus::new(8);
        // No subscriber exists: broadcast errors Closed internally, our send()
        // ignores it, nothing panics or blocks.
        bus.send(chunk(1, "into-the-void"));
        bus.send(chunk(2, "still-void"));
        // First late subscriber does NOT get pre-subscription history — safe
        // any time, but replay is the store's job, not the bus's.
        let mut rx = bus.subscribe();
        bus.send(chunk(3, "subscribed-in-time"));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.seq.as_u64(), 3);
    }

    #[tokio::test]
    async fn dropped_bus_reports_closed_to_live_receiver() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        drop(bus);
        assert!(matches!(rx.recv().await, Err(BusError::Closed)));
    }
}
