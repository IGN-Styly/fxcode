//! Agent-level projection: what processes exist and how are they doing.
//!
//! NOTE: no separate "view" types — this reuses `event::AgentStatus` verbatim.
//! A slimmed duplicate was considered and rejected: AgentStatus is already 5 small
//! variants; a second type would just drift.
//!
//! Trigger map covers ALL NINE FxEvent variants from event.rs (`AgentStatus |
//! SessionCreated | TurnStarted | Chunk | ToolCallUpsert | PlanUpdated |
//! PermissionRequested | PermissionResolved | TurnFinished`) so nobody has to wonder
//! about the ignored ones.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::driver::DriverId;
use crate::event::{AgentStatus, FxEvent};
use crate::ids::{AgentId, SessionId};

/// Derives REQUIRED (model/mod.rs derive rule): Serialize + Deserialize because
/// envelope.rs Snapshot serializes AgentsState whole; Default = boot/rebuild fold
/// target; Clone/Debug for UI; PartialEq for the mod.rs test checklist.
#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentsState {
    /// BTreeMap, not HashMap: deterministic iteration => stable sidebar order AND
    /// byte-stable Snapshot serialization for golden tests; AgentIds are uuid v7,
    /// so lexicographic order == spawn order.
    pub agents: BTreeMap<AgentId, AgentState>,
}

/// One agent PROCESS instance. Entries live forever once created: Crashed/Stopped
/// agents stay visible so the UI can offer restart. Nothing removes entries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    pub driver: DriverId,
    pub status: AgentStatus, // straight from event.rs — no wrapper
    /// Sessions opened on this agent, in SessionCreated arrival order (== chrono
    /// order given uuid v7). Append-only; the dup guard is rule S2.
    pub sessions: Vec<SessionId>,
}

/// Fold — the ONLY writer of driver/status/sessions anywhere.
///
///   S1  AgentStatus: upsert entry; event wins wholesale (driver AND status both from
///       the event); sessions list preserved; missing entry => create with empty list.
///   S2  SessionCreated, known agent: append iff absent (keyed idempotence); dup =>
///       debug log, original position kept.
///   S3  SessionCreated, unknown agent => debug log + IGNORE (no placeholder: DriverId
///       cannot be synthesized; StartAgent → Starting → NewSession ordering makes this
///       a garbled-log symptom where skipping loses least).
///   TurnStarted | Chunk | ToolCallUpsert | PlanUpdated | PermissionRequested |
///   PermissionResolved | TurnFinished                     => ignore (other folds own).
///
/// Expected status transitions (asserted in tests, not enforced here):
///   Starting → Ready | Crashed ; Ready ⇄ Busy ; any → Stopped.
///   Nothing leaves Stopped/Crashed except via a NEW AgentId.
pub fn apply_agent(state: &mut AgentsState, ev: &FxEvent) {
    match ev {
        FxEvent::AgentStatus {
            agent,
            driver,
            status,
        } => {
            let entry = state
                .agents
                .entry(agent.clone())
                .or_insert_with(|| AgentState {
                    driver: *driver,
                    status: status.clone(),
                    sessions: Vec::new(),
                });
            entry.driver = *driver;
            entry.status = status.clone();
        }
        FxEvent::SessionCreated { agent, session, .. } => match state.agents.get_mut(agent) {
            Some(a) => {
                if a.sessions.contains(session) {
                    tracing::debug!(
                        agent = %a.driver.label(),
                        session = %session,
                        "duplicate SessionCreated; keeping original position"
                    );
                } else {
                    a.sessions.push(session.clone());
                }
            }
            None => tracing::debug!(
                agent = %agent,
                session = %session,
                "SessionCreated for unknown agent; ignoring (no DriverId to synthesize)"
            ),
        },
        _ => {}
    }
}
