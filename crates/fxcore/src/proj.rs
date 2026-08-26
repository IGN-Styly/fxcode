//! Boot-time projection rebuild + the projections side of command validation.

// Imports to restore as you define the types:
// use fxproto::event::Sequenced;
// use fxproto::ids::{AgentId, RequestId, SessionId};
// use fxproto::model::{agents::AgentsState, perms::PermsState, threads::ThreadsState};
//
// use crate::store::EventStore;

// TODO:
//
// /// Server-side mirror of what clients compute with the same fold fns.
// /// Derives: Debug (tracing dumps). NOT Clone — Orchestrator hands out cloned
// /// top-level STATES, never a second mutable Projections.
// pub struct Projections {
//     pub agents: AgentsState,
//     pub threads: ThreadsState,
//     pub perms: PermsState,
// }
//
// /// Rebuild page size: replay_batch limit while walking the log at boot.
// /// 10k events ≈ single-digit MB JSON per batch; far below any allocation-cliff
// /// while keeping SQLite round-trips (~µs each) negligible even for million-event
// /// logs. Pure memory bound on rebuild = O(PAGE × event size) transient.
// const REBUILD_PAGE: usize = 10_000;
//
// impl Projections {
//     /// Fold the whole log at boot. EXACT algorithm:
//     ///   let mut p = Self::default();      // all three states derive Default
//     ///   let mut cursor = Seq(0);
//     ///   loop {
//     ///       let batch = store.replay_batch(cursor, REBUILD_PAGE).await?;
//     ///       for ev in &batch { p.apply(ev); }
//     ///       if batch.len() < REBUILD_PAGE { break; }
//     ///       cursor = batch.last().expect("non-empty by contract").seq;
//     ///   }
//     ///   Ok(p)
//     /// STRICTLY SEQUENTIAL — no parallel chunk folds: folds are order-dependent
//     /// (Chunk merging), and v0 log sizes make parallelism pointless. With
//     /// pagination, boot memory is bounded regardless of log size; if boot TIME
//     /// ever matters, add a projection-snapshot table instead of optimizing this.
//     pub async fn rebuild(store: &dyn EventStore) -> Result<Self, crate::store::StoreError>;
//
//     /// Apply one freshly-sequenced event post-append. Called ONLY from
//     /// EventSink::emit (cmd/mod.rs) between "seq assigned" and "bus.send",
//     /// under the sink mutex ⇒ every emitter (actor handlers, turn tasks, conn
//     /// pump) converges here serialized; no internal locking needed.
//     ///
//     /// Body is three line delegations — folds take &FxEvent per the locked
//     /// fxproto model contract; `seq` is deliberately unread here (cursor
//     /// bookkeeping lives in callers):
//     ///     fxproto::model::apply_agent(&mut self.agents, &ev.inner);
//     ///     fxproto::model::apply_thread(&mut self.threads, &ev.inner);
//     ///     fxproto::model::apply_perms(&mut self.perms, &ev.inner);
//     pub fn apply(&mut self, ev: &Sequenced<fxproto::event::FxEvent>);
//
//     // ── Validation helpers — cheap reads; THE list cmd/*.rs actually calls.
//     //    Polarity + destination error pinned per consumer so nobody guesses:
//
//     /// cmd/session.rs new_session  → absent ⇒ Reply Error(AgentNotFound).
//     /// true iff known AND status ∈ {Ready, Busy} (Starting/Crashed/Stopped are
//     /// not sessionable).
//     pub fn agent_running(&self, agent: &AgentId) -> bool;
//
//     /// cmd/session.rs prompt / cancel → absent ⇒ Error(SessionNotFound).
//     pub fn session_exists(&self, id: &SessionId) -> bool;
//
//     /// cmd/session.rs prompt / cancel routing: which live connection serves
//     /// this session. Linear scan of agents' sessions vecs — O(#agents×#sessions),
//     /// irrelevant at human scale; switch to an aux index only if profiling ever
//     /// disagrees. None ⇒ treated as SessionNotFound by callers.
//     pub fn session_owner(&self, id: &SessionId) -> Option<AgentId>;
//
//     /// Neutral predicate; POLARITY differs by caller — do not "simplify":
//     ///   prompt: active  ⇒ Error(TurnNotActive)   (reject concurrent turns)
//     ///   cancel: !active ⇒ Error(TurnNotActive)   (nothing to cancel)
//     pub fn turn_active(&self, session: &SessionId) -> bool;
//
//     /// cmd/perms.rs respond → absent ⇒ Error(PermissionNotFound). One check
//     /// covers unknown AND already-resolved (perms.rs R-note).
//     pub fn permission_pending(&self, request_id: &RequestId) -> bool;
//
//     /// Enumerates projected-pending requests for one session. SECONDARY source:
//     /// the authoritative sweep input is cmd/perms.rs's runtime PendingPerms map
//     /// (it owns the ACP responders projections never see). Use this for parity
//     /// assertions in tests/debug tooling; the two can disagree only inside tiny
//     /// registration/projection race windows (see orchestrator.rs actor notes).
//     pub fn pending_for_session(&self, session: &SessionId) -> Vec<RequestId>;
// }
