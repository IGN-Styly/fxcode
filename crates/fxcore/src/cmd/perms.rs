//! Permission request bookkeeping: parking ACP requests until the human answers.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
// use std::sync::Arc;
//
// use tracing::{debug, warn};
//
// use super::Ctx;
// use fxproto::event::FxEvent;
// use fxproto::ids::{OptionId, RequestId, SessionId};
//
// use crate::driver::acp::PendingAcpRequest;

// TODO:
//
// /// One parked entry: everything needed to answer + audit a permission ask.
// /// (our SessionId rides HERE, not inside PendingAcpRequest — the ACP type only
// /// knows the RAW acp session string; the translation happened when cmd layer
// /// received the park from the connection actor.)
// pub struct PermsEntry {
//     pub request: PendingAcpRequest,
//     pub session: SessionId,
// }
//
// pub struct PendingPerms {
//     /// BTreeMap on purpose: uuid-v7 RequestIds iterate oldest-first, so
//     /// sweep_cancelled resolves in ASK order — deterministic test assertions
//     /// and fair modal queues. Authority for "is this pending?": THIS map.
//     /// projections.perms.pending mirrors it for replays/UI but respond()
//     /// never consults it (races resolved by map removal alone; see respond
//     /// step 1).
//     map: BTreeMap<RequestId, PermsEntry>,
// }
//
// impl PendingPerms {
//     pub fn insert(&mut self, id: RequestId, entry: PermsEntry);
// }
//
// PermissionResponse → Reply::PermissionRecorded | Error(PermissionNotFound | Internal)
// pub async fn respond(ctx: &mut Ctx, request_id: RequestId, option_id: OptionId)
//     -> Result<Reply>;
//   1. entry = ctx.pending_perms.map.remove(&request_id)
//        None => return Error(PermissionNotFound). ONE check covers unknown AND
//                already-resolved AND swept — exactly what fxproto reply.rs pins
//                ("no expiry timestamps here"; removal IS the resolution mark).
//   2. entry.request.respond_selected(option_id)   // answers the agent FIRST:
//                // recorded audit rows should describe reality already-told-to-
//                // agent; also unblocks the waiting turn fastest. If the send
//                // fails (conn died between park and now):
//   2b. Err => warn!(...); emit PermissionResolved { request_id,
//                chosen: None }   // honest outcome: nobody chose anything
//            return Error(Internal, "agent connection lost before answer landed").
//            (Entry was already removed at step 1 => no sweep can double-emit.)
//   3. emit PermissionResolved { request_id, chosen: Some(option_id) };
//   4. return Reply::PermissionRecorded.
//
// User cancel / watchdog path for one session (called by session::cancel).
// pub async fn sweep_cancelled(ctx: &mut Ctx, session: SessionId);
//   1. drained: Vec<_> = ctx.pending_perms.map.iter().filter(session).collect();
//      empty => return immediately (zero events — cancel with no open perms is
//      NOT an event-worthy fact; TurnFinished carries that story already).
//   2. for each in ask order: entry.request.respond_cancelled();          // agent
//                             emit PermissionResolved { request_id, chosen: None };
//                             remove from map.                            // ours
//      (fxproto model contract satisfied twice INDEPENDENTLY: threads.rs W6
//      stamps the card badge, perms.rs R3 records the audit row — events arrive
//      via the normal pipeline like any other.)
//
// Conn-death twin of sweep_cancelled, callable WITHOUT Ctx (turn task context —
// see cmd/session.rs step 8b): takes (&Arc<PendingPerms>, &EventSink, SessionId),
// same order: responders answered Outcome::Cancelled first, then one
// PermissionResolved { chosen: None } event each, then map removal.
// pub async fn sweep_cancelled_for_conn_death(perms, sink, session);
//
// GROWTH/BOUND ANALYSIS (why no TTL here despite unbounded map):
// - entries leave ONLY via respond/sweeps above; three sweeps cover every exit:
//   user cancel (sweep_cancelled), watchdog force-finish (same call), process
//   death (conn-death twin), plus Orchestrator-boot reconciliation below.
// - per-turn inflow is small (tool calls ×1 asks); BTreeMap it is.
//
// BOOT GAP (cross-file obligation, flagged to orchestrator.rs owner): after a
// restart, parked Responders are gone forever while replayed events may show
// `pending` in projections.perms. Orchestrator::new must emit synthetic
// PermissionResolved { chosen: None } for every projected-pending request_id
// so both projections and runtime start clean. Until implemented, respond()
// after restart would correctly say PermissionNotFound (map is authority) and
// UI would show phantom modals — acceptable transient, documented here rather
// than silently ignored.
