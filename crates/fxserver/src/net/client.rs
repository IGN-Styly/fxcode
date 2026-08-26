//! Per-connection task pair after successful handshake.
//!
//! Precondition: handshake.rs already authenticated the client, drained replay (or
//! delivered SnapshotRequired) and holds a live bus subscription inside AuthedClient.
//! This file is transport plumbing only — zero protocol decisions beyond frame routing.

// Imports to restore as you implement:
// use std::sync::Arc;
//
// use axum::extract::ws::{Message as WsFrame, WebSocket};
// use futures::{SinkExt, StreamExt};
// use tokio::sync::mpsc;
//
// use fxcore::Orchestrator;
// use fxproto::envelope::Message;
//
// use super::handshake::AuthedClient;

// TODO: task-pair topology. ws.split() once; halves never migrate between tasks.
//
//                      ┌────────────────────────────────────────────┐
//   WS stream half ──▶ │ READER task                                │
//                      │   WsFrame → decode Message → match:        │
//                      │     Request { id, command }                │
//                      │       └─▶ orch.execute(command).await      │
//                      │           → out_tx: Response { id, reply } │
//                      │     anything else ⇒ FAIL-P (see below)     │
//                      └───────────────┬────────────────────────────┘
//                                      │ out_tx: mpsc<OutMsg>, cap 1024
//                                      ▼
//                      ┌────────────────────────────────────────────┐
//   WS sink half  ◀── │ WRITER task                                 │
//                      │   warmup steps 1–3 (the handshake.rs merge rule runs HERE):  │
//                      │   1. flush auth.replay frames in order                       │
//                      │   2. drain auth.pending, skipping ev.seq <= auth.high_water  │
//                      │      (seq comparison; replay tail / bus head overlap dies)   │
//                      │   3. loop on out_rx — passthrough from here on:              │
//                      │      OutMsg::Direct(Response|Close)                          │
//                      │      OutMsg::Event(seq'd) ← bus pump                         │
//                      └───────────────▲────────────────────────────┘
//                                      │ evt_tx: SAME mpsc as above (second clone)
//   orch.subscribe() ──▶ EVENT PUMP    │    (one bounded channel, two producers,
//   (AuthedClient.bus_rx)  lag ⇒ Close("resubscribe") + teardown)
//
// Channel inventory (all tokio mpsc, BOUNDED):
//   out: mpsc::channel::<OutMsg>(1024)   producers: READER (responses), EVENT PUMP
//                                        consumer: WRITER. Cap matches fxcore bus
//                                        capacity (bus.rs ~1024) so neither side is
//                                        the artificial bottleneck.
//   enum OutMsg { Direct(Message), Event(Sequenced<FxEvent>) }
//
// Lag / backpressure mapping (the ONLY close strings reachable here):
//   - EVENT PUMP sees broadcast::RecvError::Lagged(n) — server skipped n events for
//     THIS subscriber ⇒ push OutMsg::Direct(Close("resubscribe")), tear down.
//     Never backfill inline: the client's cursor makes reconnect+replay cheap and
//     correct; an inline gap-fill would race the live stream.
//   - out.try_send returns Full (client not reading fast enough for 1024 frames) ⇒
//     same treatment: Close("resubscribe") + teardown. Rationale: stalling READER to
//     apply TCP backpressure instead would let one dead client pin orchestrator
//     execute() slots; disconnecting is always safe because responses are correlated
//     by id and the client re-issues deliberately (it must NOT auto-requeue — see
//     fxapp conn/mod.rs correlation rules).
//   - READER sees a non-Request envelope post-handshake (Event/Subscribe/
//     SnapshotRequired/Hello/Welcome): protocol violation. Honest note replacing the
//     old "Subscribe rejected here" bullet: Command has NO Subscribe variant (locked
//     decision), so a client attempting the legacy command path dies at step ZERO —
//     serde cannot match `{"type": "subscribe", ...}` against any Command variant and
//     the frame never becomes a Command at all (structural rejection, rule below).
//     Well-formed envelope-level Subscribe frames CAN decode — they are handshake.rs
//     property (consumed exclusively at stage 3) — so their arrival here means a broken
//     or hostile client ⇒ OutMsg::Direct(Close("protocol_version")) + teardown.
//   - READER sees an INBOUND frame that fails decode at either layer: invalid JSON, or
//     valid JSON whose shape matches no Message variant — including any Request whose
//     `command` field decodes to no existing Command variant (the subscribe-command
//     case above is THIS rule, not a special one). No error Reply exists for malformed
//     input (the peer may not even speak our envelope) ⇒ OutMsg::Direct(
//     Close("protocol_version")) + teardown. Responding with JSON errors to garbage
//     would invite malformed-input loops.
//
// Teardown matrix (who cancels whom; run both tasks under one select!/JoinSet in
// client::run — first branch to finish wins, then aborts siblings):
//   | trigger                          | actor        | action                              |
//   |----------------------------------|--------------|-------------------------------------|
//   | read EOF (client went away)      | READER       | drop out_tx → WRITER drains queued  |
//   |                                  |              | frames then sees None → sends WS    |
//   |                                  |              | Close(1000) → exits                 |
//   | write error (sink.send fails:    | WRITER       | cancel READER via JoinSet abort;    |
//   | TCP reset, timeout)              |              | exit — socket is a corpse           |
//   | bus Lagged / out Full            | EVENT PUMP / | enqueue Close("resubscribe"), drop  |
//   |                                  | producer side| out_tx so it actually flushes, exit |
//   | FAIL-P (bad frame)               | READER       | enqueue Close("protocol_version"),  |
//   |                                  |              | drop out_tx, exit                   |
//   | server shutdown                  | serve()/mod  | CancellationToken fires: WRITER     |
//   |                                  | .rs          | sends Close(1001 going_away), both  |
//   |                                  |              | loops exit; orchestrator.shutdown() |
//   |                                  |              | proceeds independently              |
//   Nothing else needs cleanup: all durable state lives in the orchestrator; the
//   connection's only server-side residue is its bus subscription (dropped with rx).
//
// execute() ordering note: READER awaits orch.execute serially per connection, but
// connections are independent — total command ordering comes from the orchestrator's
// single actor (fxcore cmd/mod.rs), not from this loop.
//
// Latency badge data path (M0): READER timestamps Request-send vs Response-recv per
// correlation id into a tiny ring buffer exposed via AuthedClient/client stats handle;
// fxapp polls it through the Event stream? No — M0 badge uses WS ping RTT measured in
// THIS file instead (simpler): WRITER emits Ping every 25 s when idle, READER matches
// Pongs; last_rtt published through a watch channel for /healthz-adjacent debugging.
//
// TODO test (crates.md table): two concurrent clients each receive every event exactly
// once, in order; slow client gets "resubscribe" while fast client never stalls.
