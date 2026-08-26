//! Command handlers — the ONLY mutators of orchestrator state.
//!
//! Each handler: validate against Projections → act on driver/conn → emit events
//! via the event sink (persist→broadcast) → return exactly one Reply.

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
// use super::perms::PendingPerms;

// TODO:
//
// /// Everything a handler needs. Passed as `&mut Ctx` by the actor loop.
// pub struct Ctx<'a> {
//     pub store: &'a dyn EventStore,
//     pub registry: &'a mut DriverRegistry,
//     /// Live agent connections. BTreeMap (not HashMap) for deterministic
//     /// iteration on shutdown; Arc<AcpConnection> because spawned turn tasks
//     /// hold a clone across .await points.
//     pub conns: &'a mut ConnMap,
//     pub projections: &'a Projections,
//     pub pending_perms: &'a mut PendingPerms,
//     pub sink: EventSink,                 // append + project + broadcast in one call
//     /// Session→agent ownership map for prompt/cancel routing. RUNTIME-ONLY
//     /// bookkeeping: rebuildable from AgentsState.sessions at boot by walking
//     /// every agent's sessions vec (fold data is authoritative; this is just
//     /// the inverted index so handlers need no O(n) scans). Written by
//     /// new_session; never persisted separately.
//     pub session_agent: &'a mut BTreeMap<SessionId, AgentId>,
//     /// Live turn tasks: SessionId → (TurnId, JoinHandle/AbortHandle). Driven by
//     /// session::prompt / cancel; aborts are canceled-turn watchdog business.
//     pub turn_tasks: &'a mut BTreeMap<SessionId, TurnHandle>,
//     /// IdGen clone — the ONLY minting source for turn ids in handlers (request
//     /// ids mint in normalize.rs via its own clone; agent ids here). See ids.rs.
//     pub idgen: &'a crate::ids::IdGen,
//     /// Raw FxEvents from driver actors flow out through this; the pump task
//     /// (spawned once, below) drains it into sink.emit. Kept on Ctx ONLY so
//     /// start_agent can hand fresh connections their clone.
//     pub events_tx: crate::driver::acp::EventTx,
// }
//
// /// AgentId → live connection.
// pub type ConnMap = BTreeMap<AgentId, Arc<AcpConnection>>;
//
// /// Bookkeeping per running turn (session::prompt owns insertion; cancel owns
// /// abort/watchdog; removal happens when the turn task finishes).
// pub struct TurnHandle { pub turn: fxproto::ids::TurnId, pub abort: tokio::task::AbortHandle }
//
// pub async fn dispatch(ctx: &mut Ctx, cmd: Command) -> Result<Reply>;
//     match cmd {
//         DetectAgents        → registry.detect_all()   // never errors: found:false rows are data
//         StartAgent{..}      → session::start_agent(...)  // AgentStatus trail always
//         NewSession{..}      → session::new_session(...)  // emits SessionCreated on success
//         Prompt{..}          → session::prompt(...)       // spawns the turn task
//         Cancel{..}          → session::cancel(...)       // + sweeps pending perms
//         PermissionResponse  → perms::respond(...)
//     }
//     NOTE there is deliberately NO Subscribe arm: Command::Subscribe was DELETED
//     from fxproto command.rs — subscription is envelope-level (Message::Subscribe
//     after Welcome, handled by fxserver net/handshake.rs replay-then-live attach;
//     its resync twin SnapshotRequired lives only there too). The match above is
//     exhaustive over Command, so if fxproto ever grows a variant we must answer
//     "what does fxcore do with it?" at compile time. That is the whole story —
//     no defensive runtime rejection exists or should exist.
//
// /// The persist→project→broadcast pipeline — THE ONLY WAY any event enters the
// /// system. Ordering guarantees (all three hold by construction):
// ///   G1 seq assignment: store.append runs FIRST and alone assigns Seq
// ///      (SQLite AUTOINCREMENT behind the store's single-writer path).
// ///   G2 projection visibility: projections.apply(seq'd) completes BEFORE
// ///      bus.send and BEFORE emit returns, so any later observer (handler
// ///      validation read, bus subscriber) sees state >= seq everywhere.
// ///   G3 total order == seq order: the WHOLE three-step body runs under one
// ///      mutex inside EventSink, so two concurrent emitters cannot append out
// ///      of order relative to project/broadcast (lib.rs pins this invariant).
// /// Failure semantics: step 1 failure => Err(StoreError) returned to caller and
// /// NEITHER projection NOR bus touched (atomic-ish all-or-nothing); steps 2–3
// /// are infallible in-memory operations.
// /// Lives HERE (cmd layer), not in the driver layer: AcpConnections push raw
// /// FxEvents into the channel (driver/acp/mod.rs EventTx) and ONE pump task
// /// drains it through emit(). Drivers never assign seq or touch stores.
// pub struct EventSink {
//     // store ref + projections handle + bus. Internal Mutex guards G3.
// }
// impl EventSink {
//     pub async fn emit(&self, ev: FxEvent) -> Sequenced<FxEvent>;
// }
//
// PUMP-TASK OWNERSHIP (DECIDED): ONE global pump task, spawned by
// Orchestrator::new, draining ONE global unbounded mpsc whose Sender clones
// into every AcpConnection (`Ctx.events_tx`). NOT per-connection pumps.
// Rationale:
//   - Correctness needs exactly-once serial application of every event; that
//     already requires the sink mutex (G3). N pumps would add N-1 scheduling
//     surfaces around the same lock without removing it.
//   - One consumer = one place to observe pipeline health (seq watermark,
//     tracing spans), and one join target during shutdown ordering.
//   - Per-conn bounded channels were considered for backpressure against a hung
//     agent's notification flood; rejected because ACP traffic volume is text-
//     sized, unbounded channels cannot drop transcripts, and a fair-later M5
//     soak test (impl.md 10.2) can promote per-conn bounds without changing
//     driver-facing types beyond `pub type EventTx`.
