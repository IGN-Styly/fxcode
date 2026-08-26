//! Handshake + subscription replay. The security boundary lives here.
//!
//! Subscription is ENVELOPE-LEVEL ONLY (locked decision): the client sends
//! Message::Subscribe { last_seq } right after Welcome; Command has no Subscribe
//! variant and Reply::Subscribed does not exist. This module owns the whole
//! Hello→Welcome→Subscribe→replay|snapshot branch; client.rs starts clean.

// Imports to restore as you implement:
// use std::sync::Arc;
//
// use subtle::ConstantTimeEq;
//
// use crate::pair;                     // token source
// use fxcore::Orchestrator;
// use fxproto::envelope::{Message, Snapshot, PROTO_VERSION};
// use fxproto::ids::Seq;

// TODO: gap threshold. Gap = head_seq - last_seq (EVENT COUNT, not bytes — cheap,
// deterministic). Strategy:
//   const REPLAY_GAP_LIMIT: u64 = if cfg!(debug_assertions) { 100 } else { 10_000 };
//   - dev/debug builds: 100 — tiny enough that impl.md Phase 8.3's "force
//     SnapshotRequired" happens by just generating a burst of events locally.
//   - release builds: 10_000 — replay of ≤10k small JSON frames is well under a
//     second from SQLite; beyond that a whole-state snapshot is cheaper than the
//     stream.
//   Upgrade path: make it Config::replay_gap_limit (fxcore/config.rs addition —
//   flagged there) with these as defaults; env override NOT planned.

// TODO: AuthedClient — pure DATA struct, produced HERE, consumed by client::run (which
// owns every drop of execution below):
//
// pub struct AuthedClient {
//     /// Replay drained BEFORE any live forward, ascending, strictly after cursor.
//     /// Empty on the snapshot path (4b).
//     pub replay: Vec<Message>,          // Message::Event { .. } frames, pre-encoded
//     /// Bus subscription taken BEFORE replay begins (subscribe-first closes the
//     /// gap between "replay ended" and "live attached"). Overflow during replay
//     /// lands in `pending`; dedupe rule below.
//     pub bus_rx: <Orchestrator::subscribe() receiver>,
//     pub pending: Vec<Sequenced<FxEvent>>,  // buffered live events seen during replay
//     /// Merge seed, set HERE so client.rs needs zero protocol knowledge:
//     ///   replay path  => max(cursor last_seq, seq of last frame in `replay`)
//     ///   snapshot path=> snapshot.baseline_seq
//     pub high_water: Seq,
// }
// Dedupe/merge rule (seq comparison — locked decision): forward all `replay`, then
// drain `pending` skipping every event with seq <= high_water (replay tail can overlap
// bus head), then switch to pure passthrough. Execution site pinned in client.rs WRITER
// warmup steps 1–3. Guarantee (envelope.rs): first live seq == last replayed + 1, or ==
// baseline_seq + 1 on the snapshot path — no gap, no overlap, ever.

// TODO: byte-level frame sequence. One WS text frame = one serde_json::Message.
// Numbered exactly; S→C alternatives shown as 2a/4a/4b:
//
//   1. C→S Hello { proto_version, token }
//        FIRST frame only. Anything else (Request/Subscribe/garbage/binary frame)
//        => FAIL-V1 below.
//   2a. S→C Welcome { server_version, head_seq }          … continue at 3
//   2b. proto_version != PROTO_VERSION                    => FAIL-V1 ("protocol_version")
//       token mismatch (constant-time compare, see below)
//       server cannot read ITS OWN token file             => FAIL-V1 ("auth_failed")
//                                                              + error! server-side;
//                                                              fail closed, never
//                                                              leak internals to an
//                                                              unauthenticated peer
//   3. C→S Subscribe { last_seq } — EXACTLY ONCE, next frame after Welcome.
//        Any other frame here (Request before subscribing, a second Hello) => FAIL-V2.
//   4a. gap = head_seq − last_seq ≤ REPLAY_GAP_LIMIT:
//         S→C Event × k   (replay of seqs last_seq+1 ..= head, ascending)
//         then live Event frames from bus_rx (after pending-drain dedupe above)
//         → hand (ws sink, stream, AuthedClient) to client::run. STAYS OPEN.
//   4b. gap > REPLAY_GAP_LIMIT (or last_seq > head_seq — impossible-but-handle:
//         treat as gap check against head):
//         S→C SnapshotRequired { snapshot: Snapshot {
//                baseline_seq: Seq(head AT SNAPSHOT TIME),   // sole baseline carrier
//                agents, threads, perms } }                  // states serialized WHOLE
//         Live attach resumes at baseline_seq + 1 via the same subscribe-first +
//         pending-drain rule (skip nothing: baseline covers everything ≤ itself).
//         → client::run with empty `replay`. STAYS OPEN.
//   5. Steady state (owned by client.rs): Request/Response traffic + live Events.
//        A Message::Subscribe arriving NOW (post-handshake re-subscribe attempt)
//        => FAIL-V2 — subscription is once-per-connection by construction.
//
// Failure modes (FAIL-*), exhaustive:
//   | id     | condition                                   | close reason        | socket |
//   |--------|---------------------------------------------|---------------------|--------|
//   | FAIL-T | no frame within HANDSHAKE_TIMEOUT = 10 s    | "protocol_version"  | CLOSE  |
//   |        | (of connect, of Welcome, per stage)         | (protocol violation)|        |
//   | FAIL-J | undecodable JSON / wrong envelope shape     | "protocol_version"  | CLOSE  |
//   | FAIL-V1| version mismatch / auth failure             | see 2b              | CLOSE  |
//   | FAIL-V2| wrong frame for current handshake stage,    | "protocol_version"  | CLOSE  |
//   |        | incl. post-handshake Subscribe              |                     |        |
//   Close = WS Close frame with that EXACT reason string (canonized in envelope.rs),
//   then drop the socket. There is deliberately NO error Reply variant for these —
//   clients match the string for UX (fxapp views/connect.rs).
//
// Token compare: load_token() once per connection attempt; compare presented vs stored
// with subtle::ConstantTimeEq over bytes AFTER hex-decode (or over the raw strings —
// equal length enforced by pair.rs validation); mismatch => FAIL-V1. Never early-return
// on length difference alone (pad-or-compare-anyway keeps timing flat).
//
// After ANY close above: log outcome (which stage, which reason) at info/warn — this
// line is the audit trail for brute-force attempts.
//
// TODO tests (crates.md table): wrong version / wrong token / first-frame-not-Hello /
// timeout / replay-then-live ordering / snapshot path / post-handshake Subscribe.
