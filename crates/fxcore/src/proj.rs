//! Boot-time projection rebuild + the projections side of command validation.

// Imports to restore as you define the types:
// use fxproto::model::{agents::AgentsState, perms::PermsState, threads::ThreadsState};
// use fxproto::ids::{RequestId, SessionId};
// use fxproto::event::Sequenced;
//
// use crate::store::EventStore;

// TODO:
//
// /// Server-side mirror of what clients compute with the same fold fns.
// /// Lives behind RwLock inside Orchestrator; updated AFTER each successful append.
// pub struct Projections {
//     pub agents: AgentsState,
//     pub threads: ThreadsState,
//     pub perms: PermsState,
// }
//
// impl Projections {
//     /// Fold the entire log at boot: store.replay(Seq(0)) → apply_* per event.
//     /// For huge logs this is one pass over SQLite — fine for v0; add snapshotting
//     /// later if boot time ever matters.
//     pub async fn rebuild(store: &dyn EventStore) -> Result<Self>;
//
//     /// Apply one freshly-sequenced event post-append. Called by the actor/turn tasks.
//     pub fn apply(&mut self, ev: &Sequenced<FxEvent>);
//
//     // Validation helpers used by cmd handlers — cheap reads, keep them here:
//     pub fn session_exists(&self, id: &SessionId) -> bool;
//     pub fn turn_active(&self, session: &SessionId) -> bool;
//     pub fn permission_pending(&self, request_id: &RequestId) -> bool;
// }
