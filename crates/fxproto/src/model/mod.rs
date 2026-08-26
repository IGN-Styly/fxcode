//! Canonical projections + fold functions — the shared brain.
//!
//! BOTH sides run these:
//! - fxserver (fxcore/src/proj.rs): rebuilds state at boot by folding the event log,
//!   and uses it to validate commands (e.g. reject Prompt for unknown session).
//! - fxapp (src/store/mod.rs): applies live events to UI stores.
//!
//! Contract rules:
//!
//! - Signature: `fn apply_*(state: &mut XState, ev: &FxEvent)` — plain `&FxEvent`,
//!   NOT `&Sequenced<FxEvent>`: folds never read `seq`. Cursor bookkeeping belongs
//!   to callers (fxcore `Projections::apply` post-append; fxapp `AppState::apply`
//!   before persisting `last_seq`). An earlier draft of this file said `Sequenced`;
//!   the three per-file signatures were the majority — this line now matches them.
//! - Ownership: the three `apply_*` fns are the ONLY mutators of these states on
//    either side. Callers pass states in and render them; they never poke fields.
//! - Folds are TOTAL: any event applied to any state is defined, never panics.
//!   Unknown parents follow two fixed policies (below), never ad-hoc choices.
//! - Delivery contract: both callers consume each `Sequenced<FxEvent>` EXACTLY ONCE
//!   (replay-from-cursor then live attach; `SnapshotRequired` REPLACES state rather
//!   than replaying overlap). Folds therefore carry no applied-seq watermarks, and
//    "idempotent" in the per-file maps means: re-applying KEYED/upsert-shaped
//!   events is a no-op. Append-shaped payloads (Chunk text merges, sessions-list
//!   pushes) WOULD duplicate under double-apply — that is expected and covered by
//!   the exactly-once contract, not by fold logic. impl.md Phase 1.2's "re-applying
//!   same event => no dupes" must be read as scoped to keyed events; the checklist
//!   below encodes exactly that scope.
//! - Auto-vivify policy (threads + perms): any event carrying a `session` ensures
//!   the ThreadState exists first (get-or-create with defaults), so replays that
//!   start mid-session and snapshot baselines still render. AGENTS IS THE EXCEPTION:
//    SessionCreated naming an unknown AgentId is logged and ignored — AgentState
//!   needs a DriverId, which cannot be synthesized, and protocol ordering
//!   (StartAgent -> AgentStatus(Starting) -> NewSession -> SessionCreated) makes a
//!   missing parent a garbled-log symptom where ignoring is least damaging.
//! - States are INDEPENDENT: no apply_* reads another state. `PermissionResolved`
//!   is deliberately derived TWICE — perms.rs records the audit row, threads.rs
//!   stamps the tool-card badge — so replay order across states can never matter
//    and neither side can drift off the other.
//! - Derives: EVERY state type in this module derives `Serialize` + `Deserialize`
//    because envelope.rs `Snapshot` serializes the three top-level states
//    (AgentsState / ThreadsState / PermsState) WHOLE — clients deserialize them
//!   straight into their stores. Top-level states additionally derive `Default`
//    (boot/rebuild fold target), `Clone` + `Debug` (UI/test ergonomics) and
//!   `PartialEq` (checklist asserts state equality). Nested types derive what their
//!   owner needs; each file's TODO block lists them explicitly — do not guess.
//! - Logging levels: unknown-parent / dropped-event => `tracing::debug!`;
//!   anomalies indicating a protocol bug (double TurnFinished, overwritten
//!   active turn) => `tracing::warn!`.
//! - No I/O, no clocks, no randomness — pure functions of (state, event).

pub mod agents;
pub mod perms;
pub mod threads;

// Re-exported public surface (exhaustive — adding a type means adding it here too;
// downstream crates use both `fxproto::model::X` and these):
pub use self::agents::{AgentState, AgentsState, apply_agent};
pub use self::perms::{
    RECENT_CAP, PendingPermission, PermsState, ResolvedPermission, apply_perms,
};
pub use self::threads::{
    FlowItem, Message, PermOutcome, ThreadState, ThreadsState, ToolCall, apply_thread,
};

// TODO(tests — impl.md Phase 1.2; run with `cargo test -p fxproto`. Property style:
// one checklist line = at least one test fn. Helpers: fresh default states; ev()
// constructors per variant; apply one event per step.)
//
// agents (apply_agent) — see agents.rs rules S1–S3:
//   A1  AgentStatus on empty state => entry created with event's driver + status,
//       sessions empty.
//   A2  Re-apply identical AgentStatus => state unchanged (PartialEq; keyed
//       idempotence per the delivery contract).
//   A3  Starting -> Ready -> Busy -> Ready sequence => status tracks last event.
//   A4  Crashed { exit_code: Some(-9) } survives serde round-trip.
//   A5  SessionCreated for known agent appends once; re-apply => still once.
//   A6  SessionCreated for UNKNOWN agent => state unchanged (debug log, no
//       placeholder entry).
//   A7  Each of the other seven variants individually => state unchanged.
//   A8  Whole AgentsState serde round-trip (empty + populated) byte-stable.
//
// threads (apply_thread) — see threads.rs rules W0–W8:
//   T1  Chunk for unseen session auto-vivifies the thread; chunk lands at
//       messages[0] / flow[0].
//   T2  Consecutive same-role chunks MERGE into one Message (len == 1, texts
//       concatenated in arrival order).
//   T3  Role flip (User then Agent) starts a NEW Message; text never merges
//       across roles.
//   T4  Chunk AFTER a tool call does not merge into the pre-tool message even
//       when roles match — merge compares ONLY flow.last().
//   T5  ToolCallUpsert BEFORE any message => flow[0] is FlowItem::Tool, messages
//       still empty; nothing synthetic is invented.
//   T6  ToolCallUpsert twice, same id => map holds 1 entry whose fields equal the
//       LATEST event, exactly one flow item at the first-seen position, `perm`
//       preserved across overwrite.
//   T7  Two distinct tool ids => two map entries; flow order == first-appearance
//       order regardless of upsert update order afterwards.
//   T8  TurnStarted sets active_turn; TurnFinished for the SAME turn clears it.
//   T9  Second TurnFinished (already cleared) => warn, state unchanged.
//   T10 TurnFinished for a stale/different turn id => active_turn untouched.
//   T11 PlanUpdated REPLACES wholesale: second update with fewer entries shrinks
//       the plan (no merge ghosts).
//   T12 PermissionResolved with a recorded mapping and an upserted tool =>
//       tool.perm set to Chosen/Cancelled per `chosen`, mapping entry removed;
//       re-apply => no further change.
//   T13 PermissionResolved for an unknown request_id => state unchanged.
//   T14 PermissionRequested records the id bridge even when the tool has not been
//       upserted yet (annotation then skips gracefully per W6).
//   T15 Under a long randomized event mix, messages never shrink (append-only
//       invariant keeps every FlowItem::Message index valid).
//   T16 Whole ThreadsState serde round-trip byte-stable (BTreeMaps give
//       deterministic field order).
//   T17 Fuzz: interleave all nine variants over random sessions in random order
//       => no panic (totality) + invariants T2/T4/T6/T15 hold at the end.
//
// perms (apply_perms) — see perms.rs rules R1–R3:
//   P1  PermissionRequested inserts into pending keyed by request_id.
//   P2  Same request_id requested again => single entry, fields = latest.
//   P3  PermissionResolved removes from pending AND appends to recent carrying
//       `chosen`.
//   P4  chosen = None lands in recent as a Cancelled audit row (never dropped
//       silently).
//   P5  Resolution for a never-requested id => still appended to recent; pending
//       untouched.
//   P6  Inserting RECENT_CAP + 10 resolutions => oldest 10 evicted, newest 50
//       retained in resolution order (bound is exactly RECENT_CAP == 50).
//   P7  Re-applying the same PermissionResolved => recent holds ONE entry for that
//       id (dedupe-then-push); recent is idempotent unlike Chunk.
//   P8  Any of the seven non-permission variants => state unchanged.
//   P9  Whole PermsState serde round-trip byte-stable.
