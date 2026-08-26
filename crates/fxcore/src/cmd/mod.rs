//! Command handlers — the ONLY mutators of orchestrator state.
//!
//! Each handler: validate against Projections → act on driver/conn → emit events via
//! the event sink (persist→broadcast) → return exactly one Reply.

pub mod perms;
pub mod session;

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
// use std::sync::Arc;
//
// use fxproto::command::Command;
// use fxproto::event::{FxEvent, Sequenced};
// use fxproto::ids::{AgentId, SessionId};
// use fxproto::reply::Reply;
//
// use crate::bus::EventBus;
// use crate::driver::DriverRegistry;
// use crate::driver::acp::{AcpConnection, PendingAcpRequest};
// use crate::proj::Projections;
// use crate::store::EventStore;
//
// /// Everything a handler needs. Passed as `&mut Ctx` by the actor loop.
// pub struct Ctx<'a> {
//     pub store: &'a dyn EventStore,
//     pub registry: &'a DriverRegistry,
//     /// Live agent connections. BTreeMap (not HashMap) for deterministic iteration
//     /// on shutdown; Arc<AcpConnection> because spawned turn tasks hold a clone
//     /// across .await points.
//     pub conns: &'a mut ConnMap,
//     pub projections: &'a Projections,
//     pub pending_perms: &'a mut PendingPerms,
//     pub sink: EventSink,                 // append + project + broadcast in one call
// }
//
// /// AgentId → live connection.
// pub type ConnMap = BTreeMap<AgentId, Arc<AcpConnection>>;
//
// pub async fn dispatch(ctx: &mut Ctx, cmd: Command) -> Result<Reply>;
//     match cmd {
//         DetectAgents        → registry.detect_all()
//         StartAgent{..}      → session::start_agent(...)
//         NewSession{..}      → session::new_session(...)  // emits SessionCreated on success
//         Prompt{..}          → session::prompt(...)   // spawns the turn task
//         Cancel{..}          → session::cancel(...)   // + sweeps pending perms
//         PermissionResponse  → perms::respond(...)
//         Subscribe{..}       → handled at fxserver layer, error here (defensive)
//     }
//
// /// The persist→project→broadcast pipeline — THE ONLY WAY any event enters the system:
// ///   store.append(ev) assigns seq  →  projections.apply(seq'd)  →  bus.send(seq'd)
// /// Lives HERE (cmd layer), not in the driver layer: AcpConnections push raw FxEvents
// /// into an mpsc channel (driver/acp/mod.rs EventTx) and this module's pump task drains
// /// all connection channels through emit(). Drivers never assign seq or touch stores.
// pub struct EventSink { /* store ref + bus */ }
// impl EventSink {
//     pub async fn emit(&self, ev: FxEvent) -> Sequenced<FxEvent>;
// }
