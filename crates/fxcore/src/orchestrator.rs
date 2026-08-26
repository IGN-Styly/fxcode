//! Orchestrator — THE fxcore entrypoint. Everything else serves this.
//!
//! Concurrency model, resolved (this paragraph is normative; lib.rs summarizes):
//! commands run on ONE actor task fed by a single mpsc — no locks protect
//! orchestrator-owned mutable state because only that task ever touches it.
//! Long-running work (turns) is delegated to spawned tasks that NEVER receive
//! `&mut` anything; they communicate exclusively via (a) EventSink::emit,
//! (b) the permit-registration channel, (c) per-turn CancellationToken watches.
//! A second long-lived task (`cmd::ConnPump`) drains agent connections; like
//! turn tasks it only emits through EventSink. So: exactly two state owners —
//! the ACTOR (conns, pending_perms, turn registry) and EventSink's internal
//! mutex (store append order + projections + bus). Nothing else mutates.

// Imports to restore as you define the types:
// use std::sync::Arc;
//
// use futures::future::BoxFuture;
// use fxproto::command::Command;
// use fxproto::envelope::Snapshot;
// use fxproto::event::{FxEvent, Sequenced};
// use fxproto::ids::{AgentId, Seq};
// use fxproto::reply::Reply;
// use tokio::sync::{mpsc, oneshot};
// use tokio_util? NO — cancellation uses plain tokio::sync::watch/Notify or the
//                 CancellationToken from tokio_util ONLY if that dep is added;
//                 v0 picks tokio::sync::Notify (no new dep).
//
// use crate::bus::{BusReceiver, EventBus, BUS_CAPACITY};
// use crate::cmd::{ConnPump, EventSink};
// use crate::config::Config;
// use crate::driver::DriverRegistry;
// use crate::driver::acp::PendingAcpRequest;
// use crate::ids::IdGen;
// use crate::proj::Projections;
// use crate::store::EventStore;
// use crate::store::sqlite::SqliteStore;

// TODO:
//
// // ── Channels & capacities (all named consts here; one tuning table) ──────
// const JOB_CAP: usize = 256;        // command queue across ALL clients
// const PERMREG_CAP: usize = 64;     // parked-permission registrations/second burst
// const PUMPCMD_CAP: usize = 16;     // pump control messages (Attach only in v0)
// const CONN_EVENT_CAP: usize = 256; // per-connection raw-event buffer
//                                    // (bus capacity lives in bus.rs = BUS_CAPACITY)
//
// /// One queued command + how to answer it. Both fields mandatory.
// struct Job {
//     cmd: Command,
//     reply_tx: oneshot::Sender<Result<Reply, crate::Error>>,
// }
//
// /// Permit-registration channel payload alias. The wire-half type is owned by
// /// cmd/perms.rs; this alias exists so AcpConnection::start's signature
// /// (driver/acp/mod.rs) can name it without importing orchestrator internals.
// pub type PermRegTx = mpsc::Sender<PendingAcpRequest>;
//
// /// Channel a spawned connection forwards inbound raw FxEvents into. Per-agent:
// /// one fresh channel per started agent (NOT one global) so a chatty/stalled
// /// agent backpressures only itself — its conn actor parks awaiting send while
// /// other agents keep flowing. Cap = CONN_EVENT_CAP.
// pub type ConnEventRx = mpsc::Receiver<FxEvent>;
//
// pub struct Orchestrator {
//     cmd_tx: mpsc::Sender<Job>,
//     store: Arc<dyn EventStore>,        // same Arc inside every EventSink clone
//     registry: Arc<DriverRegistry>,     // &self methods w/ interior Mutex cache
//     sink: EventSink,                   // Clone-able; holds store+bus+projections
//     pump_ctl: mpsc::Sender<PumpCmd>,   // register/unregister conn channels
//     actor_join: tokio::task::JoinHandle<()>,
// }
//
// enum PumpCmd { Attach(AgentId, ConnEventRx) }   // Detach implicit: closed channel
//
// impl Orchestrator {
//     /// Boots everything in THIS order (each step sees the previous alive):
//     ///   1. SqliteStore::open(cfg.data_dir/"events.db") → Arc<dyn EventStore>
//     ///   2. Projections::rebuild(&*store).await   (strict sequential fold)
//     ///   3. EventBus::new(BUS_CAPACITY); EventSink::new(store.clone(), bus, Arc<RwLock<Projections>>)
//     ///      — projections live behind Arc<std::sync::RwLock<_>>: handlers read via
//     ///      short non-awaiting critical sections; EventSink writes inside its mutex.
//     ///   4. DriverRegistry::new(cfg.drivers); IdGen::production()
//     ///   5. spawn ConnPump (returns ctl pair); spawn actor loop (below)
//     pub async fn new(cfg: Config) -> Result<Self>;
//     /// Test seam for deterministic ids (see ids.rs): identical to new() except
//     /// step 4 uses the given IdGen.
//     pub async fn new_with_ids(cfg: Config, id_gen: IdGen) -> Result<Self>;
//
//     /// Queue a command; returns when its handler completes. Exactly one Reply.
//     /// Both failure spots map to Error::ShuttingDown (→ Internal): .send failing
//     /// (actor gone/closed) and oneshot recv Canceled (actor died mid-handler).
//     pub async fn execute(&self, cmd: Command) -> Result<Reply>;
//
//     /// Post-persist fanout handle for one ws client (fxserver net/handshake).
//     pub fn subscribe(&self) -> BusReceiver;
//
//     /// Handshake replay leg. Thin await over store.replay(after). fxserver MUST
//     /// consult the gap threshold FIRST and fall back to projection_snapshot()
//     /// instead of calling this with a far-behind cursor (bounded-memory rule:
//     /// replay materializes the whole tail; docs/crates.md still implies direct
//     /// store access from handshake — flagged there).
//     pub async fn replay_from(&self, after: Seq) -> Result<Vec<Sequenced<fxproto::event::FxEvent>>>;
//
//     /// Snapshot assembly for SnapshotRequired (fxserver builds the envelope
//     /// frame around what this returns). Ordering recipe — CORRECTNESS-CRITICAL,
//     /// do not reorder:
//     ///   1. acquire EventSink's internal mutex          (blocks all appends)
//     ///   2. clone agents/threads/perms under projections.read()  (nested lock,
//     ///      never awaits while held)
//     ///   3. head = store.head_seq().await               (still under sink mutex:
//     ///      no append can interleave between clone and read ⇒ states cover
//     ///      exactly events ≤ head)
//     ///   4. release; return Snapshot { baseline_seq: head, ..clones }
//     /// Combined with append assigning monotonic rowids INSIDE the same mutex,
//     /// the next event after this frame has seq == baseline_seq + 1 — the
//     /// guarantee envelope.rs documents.
//     pub async fn projection_snapshot(&self) -> Result<Snapshot>;
//
//     /// Graceful shutdown, fully drained, in EXACTLY this order (all steps
//     /// bounded; fxserver calls on SIGTERM/Ctrl-C):
//     ///   1. close Job intake (drop self.cmd_tx sender side). execute() from
//     ///      here on → ShuttingDown; ALREADY-QUEUED jobs drain normally so their
//     ///      replies arrive. Await actor_join with SHUTDOWN_GRACE (2 s).
//     ///   2. kill children: for each Arc<AcpConnection> snapshot of conns
//     ///      (taken by the actor before exiting): conn.shutdown() concurrently
//     ///      — SIGTERM → AGENT_KILL_GRACE (5 s) → SIGKILL, idempotent.
//     ///   3. let turn tasks finalize: killing conns resolves their pending
//     ///      conn.prompt() calls, which emit TurnFinished{Cancelled} +
//     ///      AgentStatus::Crashed themselves. JoinSet join with TURN_FLUSH_GRACE
//     ///      (2 s), then abort_all() — aborted tasks simply emit nothing; next
//     ///      boot's fold absorbs the missing TurnFinished (threads.rs W7 warn).
//     ///   4. drop PumpCtl sender → pump sees all Attach'd channels close → exits.
//     ///      Join pump task.
//     ///   5. drop EventBus/store Arcs (SqliteStore::drop closes WAL cleanly).
//     ///   Events already emitted are durable; no store-side buffering exists to
//     ///   flush (Option A write-through, sqlite.rs).
//     pub async fn shutdown(self);
// }
//
// // ── Actor loop (implement here OR cmd/mod.rs; cmd owns dispatch) ────────────
// //
// // struct ActorState {                          // lives wholly inside one task
// //     job_rx: mpsc::Receiver<Job>,
// //     permreg_rx: PermRegRx,                   // mpsc<PendingAcpRequest>
// //     conns: BTreeMap<AgentId, Arc<AcpConnection>>,   // == cmd::ConnMap; NOT a
// //             // concurrent map: single-task owner (the "DashMap-ish" hand-wave
// //             // in older drafts resolved here — BTreeMap chosen for shutdown-
// //             // ordering determinism; see cmd/mod.rs type alias note)
// //     pending_perms: PendingPerms,             // populated ONLY from permreg_rx
// //     turns: BTreeMap<SessionId, TurnHandle>,  // { Notify for cancel, JoinHandle }
// //     sinks/refs cloned out of Orchestrator at spawn time
// // }
// //
// // tokio::select! over THREE branches each iteration:
// //   job_rx.recv()    → cmd::dispatch(ctx built from &mut state fields, cmd)
// //                      — handlers may themselves await conn RPCs (new_session,
// //                      permission responder completion); that SERIALIZES behind
// //                      other jobs by design (total command order, T3-style);
// //                      prompt()/cancel() are the exceptions that return early
// //                      after spawning turn tasks.
// //   permreg_rx.recv()→ pending_perms.insert(request.our_id, request) — this is
// //                      why PermissionRequested bookkeeping needs no lock: the
// //                      conn pushes here immediately BEFORE forwarding the event
// //                      into its ConnEventTx, so a respond() job always finds it.
// //   None-anywhere    → intake closed ⇒ break to shutdown step 2.
// // Note token pruning: finished turn handles removed when cancel fires or task
// // completes (JoinSet-less tracking via this map keeps ownership explicit).
