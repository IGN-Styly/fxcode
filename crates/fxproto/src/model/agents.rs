//! Agent-level projection: what processes exist and how are they doing.
//!
//! NOTE: no separate "view" types — this reuses `event::AgentStatus` verbatim.
//! A slimmed duplicate was considered and rejected: AgentStatus is already 5 small
//! variants; a second type would just drift.
//!
//! The trigger map below covers ALL NINE FxEvent variants from event.rs
//! (`AgentStatus | SessionCreated | TurnStarted | Chunk | ToolCallUpsert |
//! PlanUpdated | PermissionRequested | PermissionResolved | TurnFinished`) so nobody
//! has to wonder about the ignored ones. No stale names: McpAttached is gone,
//! SessionCreated is current.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
//
// use serde::{Deserialize, Serialize};
// use tracing::debug;
//
// use crate::driver::DriverId;
// use crate::event::{AgentStatus, FxEvent};
// use crate::ids::{AgentId, SessionId};

// TODO: define:
//
// /// Derives REQUIRED (model/mod.rs derive rule): Serialize + Deserialize because
// /// envelope.rs Snapshot serializes AgentsState whole; Default = boot/rebuild fold
// /// target; Clone/Debug for UI; PartialEq for the mod.rs test checklist.
// #[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct AgentsState {
//     /// BTreeMap, not HashMap: deterministic iteration => stable sidebar order AND
//     /// byte-stable Snapshot serialization for golden tests; AgentIds are uuid v7,
//     /// so lexicographic order == spawn order.
//     pub agents: BTreeMap<AgentId, AgentState>,
// }
//
// /// One agent PROCESS instance. Entries live forever once created: Crashed/Stopped
// /// agents stay visible so the UI can offer restart. Nothing removes entries.
// #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct AgentState {
//     pub driver: DriverId,
//     pub status: AgentStatus,                     // straight from event.rs — no wrapper
//     /// Sessions opened on this agent, in SessionCreated arrival order (== chrono
//     /// order given uuid v7). Append-only; the dup guard is rule S2.
//     pub sessions: Vec<SessionId>,
// }
//
// /// Fold — EXACT trigger map, all nine variants (ownership: these rules are the
// /// only writers of driver/status/sessions anywhere):
// ///
// ///   AgentStatus { agent, driver, status }
// ///     S1 Upsert. Set entry.driver = ev.driver AND entry.status = ev.status — the
// ///        event carries driver, so the event wins wholesale ("keep old driver" in
// ///        an earlier draft was wrong). Sessions list always preserved. Missing
// ///        entry => create it with empty sessions (auto-vivify is safe here: both
// ///        fields come from the event).
// ///
// ///   SessionCreated { session, agent, .. }
// ///     S2 Known agent: push session onto .sessions ONLY IF absent (keyed
// ///        idempotence). Already present => debug log, keep original position.
// ///     S3 Unknown agent => debug log + IGNORE. Do NOT create a placeholder:
// ///        DriverId cannot be synthesized and a half-filled entry would render as
// ///        a lie. Safe because StartAgent -> AgentStatus(Starting) -> NewSession ->
// ///        SessionCreated is protocol-ordered; hitting S3 during replay means a
// ///        truncated/garbled log and skipping loses least.
// ///
// ///   TurnStarted | Chunk | ToolCallUpsert | PlanUpdated            => ignore
// ///        (session-scoped transcript events; threads.rs owns them)
// ///   PermissionRequested | PermissionResolved                      => ignore
// ///        (perms.rs owns them)
// ///   TurnFinished                                                  => ignore
// ///
// /// Status transitions to EXPECT (assertable in tests; the fold enforces nothing —
// /// it records whatever the server emitted):
// ///   Starting -> Ready | Crashed ; Ready <=> Busy ; any -> Stopped (shutdown).
// ///   Nothing leaves Stopped/Crashed except via a NEW AgentId (ids.rs mints fresh
// ///   ids per process start), so no resurrection handling exists here.
// pub fn apply_agent(state: &mut AgentsState, ev: &FxEvent);
//
// Test checklist: model/mod.rs block "agents (apply_agent)", items A1–A8.
