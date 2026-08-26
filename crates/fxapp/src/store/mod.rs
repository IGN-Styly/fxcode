//! AppState global: the client-side projections. Views NEVER see raw events.

use fxproto::event::{FxEvent, Sequenced};
use fxproto::model::{agents::AgentsState, perms::PermsState, threads::ThreadsState};

// Imports to restore as you implement:
// use gpui::{App, Global};
//
// use fxproto::model::agents::apply_agent;
// use fxproto::model::perms::apply_perms;
// use fxproto::model::threads::apply_thread;
//
// use crate::conn::ConnStatus;      // SINGLE definition lives in conn/mod.rs — import it

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
//     /// The single mutation entrypoint (nobody else mutates the three states):
//     ///   1. run the owning fold(s) on ev.inner — folds take &FxEvent per model/mod.rs;
//     ///   2. hand back ev.seq.as_u64() so conn/mod.rs advances + persists the cursor
//     ///      AFTER the fold ran (cursor.rs timing rules); then this fn notifies.
//     /// Variant → owner map (static, mirrors model/* ownership rules exactly):
//     ///   AgentStatus                                   → apply_agent (agents state only)
//     ///   SessionCreated | TurnStarted | Chunk | ToolCallUpsert | PlanUpdated |
//     ///   PermissionRequested | PermissionResolved | TurnFinished
//     ///                                                 → apply_thread (everything else)
//     ///   PermissionRequested | PermissionResolved      → apply_perms AS WELL — the same
//     ///     event deliberately folds into BOTH states independently ("derived twice",
//     ///     model/mod.rs); neither state reads the other.
//     /// Total per event: owners are {agents} or {threads} or {threads, perms} — never all three.
//     pub fn apply(&mut self, ev: &Sequenced<FxEvent>) -> u64;
// }
//
// NOTIFY-GRANULARITY v0 PLAN (decided — coarse now, seams pre-cut for cheap refinement):
//   v0 ships ONE notification after every apply(): all observing views re-render. Folds
//   are O(event), gpui batches redraws within a frame — per-event UI cost is negligible
//   at chat rates, so spend zero complexity here until data says otherwise.
//   SEAMS ALREADY IN PLACE for the upgrade: states are separate structs TODAY and apply()
//   knows each variant's owner set (above). Upgrade = replace "notify all" inside
//   apply()/its caller with per-owner Entity notifies behind ONE facade method
//   (AppState::notify_owners(owner_set)) — a single-site change by construction.
//   UPGRADE TRIGGER CONDITION (concrete numbers, not vibes — either fires ⇒ refactor):
//     (a) profiler shows any interactive frame >16 ms during streaming turns attributable
//         to ≥2 passive views re-rendering on foreign-domain events, OR
//     (b) sustained ingest >200 Sequenced events/s for >10 s (bursty tool storms count).
//
// NO-STORES-YET note for SnapshotRequired: conn/mod.rs REPLACES these fields wholesale
// from snapshot.{agents, threads, perms} (assignment, not folding) before resuming ingest.
