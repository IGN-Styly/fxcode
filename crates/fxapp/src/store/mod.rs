//! AppState global: the client-side projections. Views NEVER see raw events.

use fxproto::event::{FxEvent, Sequenced};
use fxproto::model::{agents::AgentsState, perms::PermsState, threads::ThreadsState};

// TODO:
//
// pub struct AppState {
//     pub conn_status: ConnStatus,             // from ConnectionManager
//     pub agents: AgentsState,
//     pub threads: ThreadsState,
//     pub perms: PermsState,
// }
//
// impl Global for AppState {}                // cx.set_global / cx.global::<AppState>()
//
// impl AppState {
//     /// The single mutation entrypoint:
//     /// apply fold → persist cursor → cx.notify() relevant entities.
//     /// (Consider Entity<Store> per domain instead of one global if notify granularity
//     /// becomes a perf issue — start simple.)
//     pub fn apply(&mut self, ev: &Sequenced<FxEvent>, cx: &mut App);
// }
