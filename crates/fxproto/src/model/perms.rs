//! Permission projection: pending asks + their resolutions.
//!
//! Cross-state rule: this file NEVER reads ThreadsState or AgentsState. On Cancel
//! the server emits PermissionResolved { chosen: None } for every swept pending
//! request (architecture.md permission pump; ACP requires answering pending
//! requests) plus TurnFinished { stop_reason: Cancelled }. This fold just processes
//! those like any other events — no special casing, no ordering assumptions, and
//  the "cancelled" story splits cleanly: perms.rs owns the audit row in `recent`,
//  threads.rs owns the tool-card badge (PermOutcome::Cancelled).

// Imports to restore as you define the types:
// use std::collections::{BTreeMap, VecDeque};
//
// use serde::{Deserialize, Serialize};
// use tracing::debug;
//
// use crate::event::{FxEvent, PermissionOption, ToolCallSummary};
// use crate::ids::{OptionId, RequestId, SessionId};

// TODO: define:
//
// /// Cap for the recent-resolution ring. EXACTLY 50 — not "say 50": one screen of
// /// audit rows in views/perms.rs, small enough that every Snapshot carries it for
// /// free. Named const so tests import it instead of hardcoding.
// pub const RECENT_CAP: usize = 50;
//
// /// Derives REQUIRED (model/mod.rs derive rule): Serialize + Deserialize because
// /// envelope.rs Snapshot serializes PermsState whole; Default/Clone/Debug/
// /// PartialEq per checklist.
// #[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct PermsState {
//     /// Pending = needs a modal on screen right now. Keyed by RequestId (uuid v7
//     /// => oldest-first iteration feeds the modal queue). The KEY owns the id:
//     /// PendingPermission deliberately does NOT repeat it — values are only ever
//     /// reached through this map.
//     pub pending: BTreeMap<RequestId, PendingPermission>,
//     /// Recent resolutions: newest pushed at back, oldest evicted from front once
//     /// len exceeds RECENT_CAP. Here the id IS repeated inside the value, because
//     /// an entry outlives its map slot.
//     pub recent: VecDeque<ResolvedPermission>,
// }
//
// #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct PendingPermission {
//     pub session: SessionId,
//     pub summary: ToolCallSummary,
//     pub options: Vec<PermissionOption>,
// }
//
// #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct ResolvedPermission {
//     pub request_id: RequestId,
//     /// None = cancelled (user dismiss, watchdog timeout, or turn-cancel sweep).
//     /// Cancelled rows are kept — they are audit facts, not noise.
//     pub chosen: Option<OptionId>,
// }
//
// /// Fold — EXACT trigger map, all nine variants (ownership: apply_perms is the
// /// only writer of PermsState; it stays PASSIVE — sweeps happen upstream in
// /// fxcore cmd/perms.rs and arrive here as ordinary PermissionResolved events):
// ///
// ///   PermissionRequested { request_id, session, tool_call, options }
// ///     R1  pending.insert(request_id, PendingPermission { .. }). Duplicate id =>
// ///         overwrite + debug log (idempotent by construction). Unbounded growth
// ///         is prevented upstream (cancel-turn sweep + watchdog both emit
// ///         Resolution(None)); no TTL lives in this fold.
// ///
// ///   PermissionResolved { request_id, chosen }
// ///     R2  pending.remove(request_id).
// ///     R3  Build ResolvedPermission, then DEDUPE-THEN-PUSH into `recent`:
// ///           retain only entries whose request_id != this one,
// ///           push_back(new entry),
// ///           while recent.len() > RECENT_CAP { pop_front(); }
// ///         Dedupe makes `recent` idempotent under re-delivery (unlike Chunk);
// ///         the push works whether or not R2 found anything, so resolutions for
// ///         unknown/expired ids still get audited.
// ///
// ///   AgentStatus | SessionCreated | TurnStarted | Chunk | ToolCallUpsert |
// ///   PlanUpdated | TurnFinished                                   => ignore
// ///        (agents.rs / threads.rs own those)
// ///
// /// Server-side extra duty: command handlers consult `pending` to gate
// /// Command::PermissionResponse — request_id absent from `pending` => error reply.
// /// One check covers unknown AND already-resolved (no expiry timestamps here).
// pub fn apply_perms(state: &mut PermsState, ev: &FxEvent);
//
// Test checklist: model/mod.rs block "perms (apply_perms)", items P1–P9.
