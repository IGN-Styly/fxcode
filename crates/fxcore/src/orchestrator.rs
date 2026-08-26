//! Orchestrator — THE fxcore entrypoint. Everything else serves this.
//!
//! Concurrency model (normative; lib.rs summarizes): commands run on ONE actor
//! task fed by a single mpsc — no locks protect orchestrator-owned mutable
//! state because only that task ever touches it. Long-running work (turns) is
//! delegated to spawned tasks that NEVER receive `&mut` anything; they
//! communicate exclusively via (a) EventSink::emit, (b) the permit-registration
//! channel, (c) FinishClaim/watches. ONE pump task drains connection events
//! into the same sink. Exactly two state owners: the ACTOR and EventSink's
//! mutex. (`Arc<Mutex<PendingPerms>>` is additionally shared with turn-task
//! sweeps — flagged DEVIATION documented in cmd/mod.rs.)

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fxproto::command::Command;
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::{AgentId, Seq, SessionId};
use fxproto::reply::Reply;
use tokio::sync::{mpsc, oneshot};

use crate::bus::{BUS_CAPACITY, BusReceiver, EventBus};
use crate::cmd::{Ctx, EventSink, InternalCmd, PermShared, session as cmd_session};
use crate::config::Config;
use crate::driver::DriverRegistry;
use crate::driver::acp::{AcpConnection, ParkedPerm, PermRegRx, PermRegTx};
use crate::ids::IdGen;
use crate::proj::Projections;
use crate::store::EventStore;

// ── Channel & capacity tuning table ──────────────────────────────────────────
/// Command queue across ALL clients.
pub const JOB_CAP: usize = 256;
/// Parked-permission burst ceiling for registrations (unbounded mpsc beneath;
/// this const documents expected throughput, enforced nowhere in v0 — parks are
/// one-shot tool asks, not streaming).
pub const PERMREG_BURST_DOC: usize = 64;
/// Bounded delay budget for actor drain during shutdown.
const ACTOR_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Child kill ladder grace (inside AcpConnection::shutdown; referenced here so
/// the tuning story has one visible table).
const AGENT_KILL_GRACE_REF: Duration = Duration::from_secs(5);
/// Time allotted to stray turn tasks to finish persisting their last events
/// before the actor force-aborts them at shutdown.
const TURN_FLUSH_GRACE: Duration = Duration::from_secs(2);

enum ActorMsg {
    Command {
        cmd: Command,
        reply_tx: oneshot::Sender<Result<Reply, crate::Error>>,
    },
    /// Deprecated alias arm kept for exhaustive matches during wiring; not
    /// constructed anymore (internal + parked ride their own lanes).
    #[allow(dead_code)]
    Legacy,
}

pub struct Orchestrator {
    cmd_tx: mpsc::Sender<ActorMsg>,
    internal_tx: mpsc::UnboundedSender<InternalCmd>,
    /// Same Arc inside every EventSink clone.
    #[allow(dead_code)]
    store: Arc<dyn EventStore>,
    registry: Arc<DriverRegistry>,
    sink: EventSink,
    perms: PermShared,
    events_tx: crate::driver::acp::EventTx,
    pub(super) pump_join: Option<tokio::task::JoinHandle<()>>,
    pub(super) actor_join: Option<tokio::task::JoinHandle<()>>,
}

impl Orchestrator {
    /// Boots everything IN THIS ORDER (each step sees the previous alive):
    ///   1. SqliteStore::open_shared(cfg.data_dir/"events.db")
    ///   2. Projections::rebuild(&*store)  (strict sequential fold, paged)
    ///   3. EventBus + EventSink wired onto rebuilt projections
    ///      (+ BOOT GAP reconciliation, see below)
    ///   4. DriverRegistry::new(cfg.drivers); IdGen (injected or production)
    ///   5. global event pump + single actor task
    pub async fn new(cfg: Config) -> Result<Self, crate::Error> {
        Self::boot(cfg, IdGen::production()).await
    }

    /// Test seam for deterministic ids (see ids.rs): identical boot except
    /// step 4 uses the given IdGen.
    pub async fn new_with_ids(cfg: Config, id_gen: IdGen) -> Result<Self, crate::Error> {
        Self::boot(cfg, id_gen).await
    }

    async fn boot(cfg: Config, idgen: IdGen) -> Result<Self, crate::Error> {
        // 1.
        let store_path = cfg.data_dir.join("events.db");
        let store: Arc<dyn EventStore> =
            crate::store::sqlite::SqliteStore::open_shared(&store_path)?;

        // 2. Whole-log fold, strictly sequential.
        let projections = Projections::rebuild(store.as_ref()).await?;

        // 3. Sink FIRST (synthetic reconciliation emits through the pipeline),
        //    then the boot-gap sweep: parked Responders died with the old
        //    process while replayed perms show `pending`. Emitting resolved{None}
        //    keeps projections and runtime starting clean (perms.rs BOOT GAP).
        let bus = EventBus::new(BUS_CAPACITY);
        let pending_at_boot = projections.all_pending_ids();
        if !pending_at_boot.is_empty() {
            tracing::info!(
                count = pending_at_boot.len(),
                "reconciling permissions left pending by a previous process"
            );
        }
        let mut proj_rebuilt = projections;
        let sink = EventSink::new(store.clone(), bus, std::mem::take(&mut proj_rebuilt));
        // Boot-gap reconciliation emits through the SAME pipeline (persist →
        // apply → broadcast) so projections and runtime start clean.
        for request_id in pending_at_boot {
            if let Err(err) = sink
                .emit(FxEvent::PermissionResolved {
                    request_id,
                    chosen: None,
                })
                .await
            {
                tracing::warn!(error=?err, "boot-gap reconciliation append failed");
            }
        }

        // 4.
        let registry = Arc::new(DriverRegistry::new(cfg.drivers));

        // 5. Global pump: ONE unbounded channel drained by ONE task (cmd/mod.rs
        //    PUMP-TASK OWNERSHIP decision).
        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<FxEvent>();
        let pump_sink = sink.clone();
        let pump_join = tokio::spawn(async move {
            while let Some(ev) = events_rx.recv().await {
                if let Err(err) = pump_sink.emit(ev).await {
                    tracing::error!(target:"pump", error=?err, "event emit failed");
                }
            }
            tracing::debug!(target:"pump", "drained; exiting");
        });

        // Actor: ONE consumer of commands + internal messages + permission
        // registrations. Internal messages ride their own unbounded lane (turn
        // completions must never drop when the bounded command queue is full).
        let (internal_tx, internal_rx) = mpsc::unbounded_channel::<InternalCmd>();
        let (msg_tx, msg_rx) = mpsc::channel::<ActorMsg>(JOB_CAP);
        let (permreg_tx, permreg_rx) = mpsc::unbounded_channel::<ParkedPerm>();
        let perms_shared: PermShared = Arc::default();

        let actor_state = ActorState {
            job_rx: msg_rx,
            internal_rx,
            permreg_rx,
            conns: BTreeMap::new(),
            session_agent: BTreeMap::new(),
            turn_tasks: BTreeMap::new(),
            job_tx: internal_tx.clone(),
            sink: sink.clone(),
            registry: Arc::clone(&registry),
            events_tx: events_tx.clone(),
            permreg_tx,
            perms_registry: perms_shared.clone(),
            idgen: idgen.clone(),
            watchdog: cmd_session::watchdog_from_env(),
        };
        let actor_join = tokio::spawn(actor_loop(actor_state));

        Ok(Self {
            cmd_tx: msg_tx,
            internal_tx,
            store,
            registry,
            sink,
            perms: perms_shared,
            events_tx,
            pump_join: Some(pump_join),
            actor_join: Some(actor_join),
        })
    }

    /// Queue a command; returns when its handler completes. Exactly one Reply.
    /// Both failure spots map to Error::ShuttingDown (→ Internal): .send failing
    /// (actor gone/closed) and oneshot recv Canceled (actor died mid-handler).
    pub async fn execute(&self, cmd: Command) -> Result<Reply, crate::Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(ActorMsg::Command { cmd, reply_tx })
            .await
            .map_err(|_| crate::Error::ShuttingDown)?;
        reply_rx.await.map_err(|_| crate::Error::ShuttingDown)?
    }

    /// Post-persist fanout handle for one ws client (fxserver net/handshake).
    pub fn subscribe(&self) -> BusReceiver {
        self.sink.bus_subscribe()
    }

    /// Handshake replay leg. Thin await over store.replay(after). fxserver MUST
    /// consult the gap threshold FIRST and fall back to projection_snapshot()
    /// instead of calling this with a far-behind cursor (bounded-memory rule).
    pub async fn replay_from(&self, after: Seq) -> Result<Vec<Sequenced<FxEvent>>, crate::Error> {
        self.store.replay(after).await.map_err(crate::Error::from)
    }

    /// Snapshot assembly for SnapshotRequired. Guard-across-await recipe lives
    /// on EventSink::snapshot_locked (head_seq awaited INSIDE the guard).
    pub async fn projection_snapshot(&self) -> Result<fxproto::envelope::Snapshot, crate::Error> {
        self.sink.snapshot_locked().await
    }

    /// Graceful shutdown, fully drained, order documented inline. fxserver
    /// calls on SIGTERM/Ctrl-C.
    pub async fn shutdown(self) {
        // 1. Close intake. execute() from here on ⇒ ShuttingDown; ALREADY-
        //    QUEUED jobs drain normally inside the actor so their replies land.
        //    (Dropping our Sender half plus expecting rx-closure works because
        //    the actor holds no other ref to this specific sender.)
        //    The channel halves are separate fields, so simply dropping self's
        //    cmd_tx by taking ownership via destructuring:
        let Self {
            cmd_tx,
            internal_tx,
            store,
            registry,
            sink,
            perms,
            events_tx,
            pump_join,
            actor_join,
        } = self;
        drop(cmd_tx);
        // Closing the internal lane too: actor exits once BOTH lanes empty.
        drop(internal_tx);

        // 2. Await actor exit (bounded). The actor DRAINS queued jobs first,
        //    then kills every child concurrently (each respecting the kill
        //    ladder), force-aborts stray turn tasks, and finally notifies.
        if let Some(handle) = actor_join
            && tokio::time::timeout(ACTOR_DRAIN_GRACE, handle)
                .await
                .is_err()
        {
            tracing::warn!("orchestrator actor exceeded drain grace");
        }

        // 3. Stray turn tasks were aborted BY the actor; anything still hanging
        //    gets TURN_FLUSH_GRACE implicitly via process teardown timing. Join
        //    the pump last so terminal statuses/flushes make it onto the bus.
        drop(events_tx);
        drop(sink);
        drop(perms);
        drop(registry);
        drop(store);
        if let Some(handle) = pump_join
            && tokio::time::timeout(TURN_FLUSH_GRACE.max(AGENT_KILL_GRACE_REF), handle)
                .await
                .is_err()
        {
            tracing::warn!("event pump did not stop cleanly");
        }
        // Events already emitted are durable; no store-side buffering exists
        // (write-through SQLite, Option A).
    }
}

// ── Actor loop ───────────────────────────────────────────────────────────────

struct ActorState {
    job_rx: mpsc::Receiver<ActorMsg>,
    internal_rx: mpsc::UnboundedReceiver<InternalCmd>,
    permreg_rx: PermRegRx,
    job_tx: mpsc::UnboundedSender<InternalCmd>,
    /// NOT a concurrent map: single-task owner ("DashMap-ish hand-wave from
    /// older drafts resolves HERE"; BTreeMap for deterministic shutdown order).
    conns: BTreeMap<AgentId, Arc<AcpConnection>>,
    /// Runtime-only inverted index (see Ctx.session_agent docs).
    session_agent: BTreeMap<SessionId, AgentId>,
    /// SessionId → handle. Removal happens on InternalCmd::TurnDone.
    turn_tasks: BTreeMap<SessionId, crate::cmd::TurnHandle>,
    sink: EventSink,
    registry: Arc<DriverRegistry>,
    events_tx: crate::driver::acp::EventTx,
    /// Cloned into every started agent; parks flow back through permreg_rx.
    permreg_tx: PermRegTx,
    /// Shared sweeps registry (actor inserts via permreg deliveries; tasks
    /// sweep on conn death).
    perms_registry: PermShared,
    idgen: IdGen,
    watchdog: Duration,
}

async fn actor_loop(mut st: ActorState) {
    loop {
        tokio::select! {
            msg = st.job_rx.recv() => match msg {
                None => break,
                Some(ActorMsg::Command { cmd, reply_tx }) => {
                    let reply = handle_command(&mut st, cmd).await;
                    let _ = reply_tx.send(reply);
                }
                Some(ActorMsg::Legacy) => {}
            },
            internal = st.internal_rx.recv() => match internal {
                Some(msg) => handle_internal(&mut st, msg),
                None => break, // orchestrated shutdown closed this lane too
            },
            parked = st.permreg_rx.recv() => match parked {
                Some(parked) => register_park(&mut st, parked).await,
                None => {
                    // No connections exist to park anything; keep serving — the
                    // command lane governs shutdown exclusively.
                }
            },
        }
    }

    // ── Shutdown sequence (runs on intake closure) ───────────────────────────
    // Kill children CONCURRENTLY — idempotent ladders; each publishes its own
    // Stopped/Crashed terminal through the pump.
    let kill_futs: Vec<_> = st.conns.values().map(|c| c.shutdown()).collect();
    futures::future::join_all(kill_futs).await;

    // Force-finish stragglers: aborted tasks simply emit nothing further.
    for (_, handle) in std::mem::take(&mut st.turn_tasks) {
        handle.abort.abort();
    }

    st.conns.clear();
    tracing::debug!(target:"actor", "closed");
}

async fn handle_command(st: &mut ActorState, cmd: Command) -> Result<Reply, crate::Error> {
    let mut ctx = Ctx {
        registry: &st.registry,
        conns: &mut st.conns,
        sink: st.sink.clone(),
        pending_perms: &st.perms_registry,
        session_agent: &mut st.session_agent,
        turn_tasks: &mut st.turn_tasks,
        job_tx: st.job_tx.clone(),
        idgen: &st.idgen,
        events_tx: st.events_tx.clone(),
        permreg_tx: st.permreg_tx.clone(),
        cancel_watchdog: st.watchdog,
    };
    // Long ops (session/new RPCs, permission responder completions) serialize
    // behind other jobs BY DESIGN (total command order); prompt/cancel return
    // early after spawning turn tasks.
    crate::cmd::dispatch(&mut ctx, cmd).await
}

fn handle_internal(st: &mut ActorState, msg: InternalCmd) {
    match msg {
        InternalCmd::TurnDone { session } => {
            // Token pruning: finished-turn handles removed here; the claim
            // object outlives inside whoever still references it harmlessly.
            st.turn_tasks.remove(&session);
        }
    }
}

async fn register_park(st: &mut ActorState, parked: ParkedPerm) {
    // Registration lands BEFORE the corresponding event becomes visible via
    // the pump (park-first contract) — response jobs always find entries.
    st.perms_registry.lock().await.insert_parked(parked);
}
