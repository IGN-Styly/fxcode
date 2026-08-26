//! Agent-level projection: what processes exist and how are they doing.
//!
//! NOTE: no separate "view" types — this reuses `event::AgentStatus` verbatim.
//! A slimmed duplicate was considered and rejected: AgentStatus is already 5 small
//! variants; a second type would just drift.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
//
// use crate::event::{AgentStatus, FxEvent};
// use crate::driver::DriverId;
// use crate::ids::{AgentId, SessionId};

// TODO: define:
//
// #[derive(Default, Serialize, Deserialize)]      // Serialize: needed for Snapshot
// pub struct AgentsState {
//     pub agents: BTreeMap<AgentId, AgentState>,   // ordered for stable UI rendering
// }
//
// #[derive(Serialize, Deserialize)]
// pub struct AgentState {
//     pub driver: DriverId,
//     pub status: AgentStatus,                     // straight from event.rs — no wrapper
//     pub sessions: Vec<SessionId>,
// }
//
// /// Fold — EXACT trigger map, no hand-waving:
// ///   AgentStatus   { agent, .. }        → entry.insert(agent, AgentState { status, .. }) ;
// ///                                        existing entry keeps driver+sessions (merge!)
// ///   SessionCreated{ session, agent,..} → push session onto that agent if absent;
// ///                                        unknown agent ⇒ create placeholder w/ status Unknown?
// ///                                        NO — log + ignore (agent event will arrive first
// ///                                        because StartAgent precedes NewSession)
// ///   everything else                    → ignore
// ///
// /// Status transitions to expect (assert-able in tests):
// ///   Starting → Ready | Crashed ; Ready ⇄ Busy ; any → Stopped (shutdown)
// pub fn apply_agent(state: &mut AgentsState, ev: &FxEvent);
