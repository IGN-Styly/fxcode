//! Boot-time projection rebuild + the projections side of command validation.

use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::{AgentId, RequestId, Seq, SessionId};
use fxproto::model::{agents::AgentsState, perms::PermsState, threads::ThreadsState};

use crate::store::EventStore;

const REBUILD_PAGE: usize = 10_000;

/// Server-side mirror of what clients compute with the same fold fns.
/// Not Clone — Orchestrator hands out cloned top-level STATES, never a second
/// mutable Projections.
#[derive(Debug, Default)]
pub struct Projections {
    pub agents: AgentsState,
    pub threads: ThreadsState,
    pub perms: PermsState,
}

impl Projections {
    /// Rebuild page size: replay_batch limit while walking the log at boot.
    /// 10k events ≈ single-digit MB JSON per batch; far below any allocation-cliff
    /// while keeping SQLite round-trips (~µs each) negligible even for million-event
    /// logs. Pure memory bound on rebuild = O(PAGE × event size) transient.
    fn page() -> usize {
        REBUILD_PAGE
    }
    /// Fold the whole log at boot. EXACT algorithm:
    ///   let mut p = Self::default();
    ///   let mut cursor = Seq(0);
    ///   loop {
    ///       let batch = store.replay_batch(cursor, REBUILD_PAGE).await?;
    ///       for ev in &batch { p.apply(ev); }
    ///       if batch.len() < REBUILD_PAGE { break; }
    ///       cursor = batch.last().expect("non-empty by contract").seq;
    ///   }
    ///   Ok(p)
    /// STRICTLY SEQUENTIAL — folds are order-dependent (Chunk merging), and v0
    /// log sizes make parallelism pointless. With pagination, boot memory is
    /// bounded regardless of log size.
    pub async fn rebuild(store: &dyn EventStore) -> Result<Self, crate::store::StoreError> {
        let mut p = Self::default();
        let mut cursor = Seq::new(0);
        loop {
            let batch = store.replay_batch(cursor, Self::page()).await?;
            let done = batch.len() < Self::page();
            for ev in &batch {
                p.apply(ev);
            }
            if done {
                break;
            }
            cursor = batch.last().expect("non-empty by contract").seq;
        }
        Ok(p)
    }

    /// Apply one freshly-sequenced event post-append. Called ONLY from
    /// EventSink::emit between "seq assigned" and "bus.send", under the sink
    /// mutex ⇒ every emitter converges here serialized; no internal locking.
    ///
    /// Body is three line delegations — folds take &FxEvent per the locked
    /// fxproto model contract; `seq` stays unread here (fold contract:
    /// cursors belong to callers; the sink's snapshot baseline uses the seq
    /// stamped by append, not anything computed here).
    pub fn apply(&mut self, ev: &Sequenced<FxEvent>) {
        fxproto::model::apply_agent(&mut self.agents, &ev.inner);
        fxproto::model::apply_thread(&mut self.threads, &ev.inner);
        fxproto::model::apply_perms(&mut self.perms, &ev.inner);
    }

    // ── Validation helpers — cheap reads; THE list cmd/*.rs actually calls.
    //    Polarity + destination error pinned per consumer so nobody guesses:

    /// cmd/session.rs new_session → absent ⇒ Reply Error(AgentNotFound).
    /// true iff known AND status ∈ {Ready, Busy} (Starting/Crashed/Stopped are
    /// not sessionable).
    pub fn agent_running(&self, agent: &AgentId) -> bool {
        self.agents.agents.get(agent).is_some_and(|a| {
            matches!(
                a.status,
                fxproto::event::AgentStatus::Ready | fxproto::event::AgentStatus::Busy
            )
        })
    }

    /// cmd/session.rs prompt / cancel → absent ⇒ Error(SessionNotFound).
    pub fn session_exists(&self, id: &SessionId) -> bool {
        self.threads.threads.contains_key(id)
    }

    /// cmd/session.rs prompt / cancel routing: which live connection serves
    /// this session. Linear scan of agents' sessions vecs — O(#agents×#sessions),
    /// irrelevant at human scale. None ⇒ treated as SessionNotFound by callers.
    pub fn session_owner(&self, id: &SessionId) -> Option<AgentId> {
        for (agent, state) in &self.agents.agents {
            if state.sessions.contains(id) {
                return Some(agent.clone());
            }
        }
        None
    }

    /// Neutral predicate; POLARITY differs by caller — do not "simplify":
    ///   prompt: active  ⇒ Error(TurnNotActive)   (reject concurrent turns)
    ///   cancel: !active ⇒ Error(TurnNotActive)   (nothing to cancel)
    pub fn turn_active(&self, session: &SessionId) -> bool {
        self.threads
            .threads
            .get(session)
            .and_then(|t| t.active_turn.as_ref())
            .is_some()
    }

    /// cmd/perms.rs respond → absent ⇒ Error(PermissionNotFound). One check
    /// covers unknown AND already-resolved (perms.rs R-note).
    pub fn permission_pending(&self, request_id: &RequestId) -> bool {
        self.perms.pending.contains_key(request_id)
    }

    /// Enumerates projected-pending requests for one session. SECONDARY source:
    /// the authoritative sweep input is cmd/perms.rs's runtime PendingPerms map.
    /// Used for parity assertions and the boot-gap reconciliation.
    pub fn pending_for_session(&self, session: &SessionId) -> Vec<RequestId> {
        self.perms
            .pending
            .iter()
            .filter(|(_, p)| &p.session == session)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Every projected-pending request id regardless of session — boot-time
    /// reconciliation input (perms.rs BOOT GAP note).
    pub fn all_pending_ids(&self) -> Vec<RequestId> {
        self.perms.pending.keys().cloned().collect()
    }
}
