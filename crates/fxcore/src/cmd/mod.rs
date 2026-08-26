//! Command handlers — the ONLY mutators of orchestrator state.
//!
//! Each handler: validate against Projections → act on driver/conn → emit events
//! via the event sink (persist→broadcast) → return exactly one Reply.

pub mod perms;
pub mod session;

use std::collections::BTreeMap;
use std::sync::Arc;

use fxproto::command::Command;
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::{AgentId, SessionId};
use fxproto::reply::Reply;

use crate::bus::{BusReceiver, EventBus};
use crate::driver::DriverRegistry;
use crate::driver::acp::{AcpConnection, PermRegTx};
use crate::proj::Projections;
use crate::store::EventStore;

// ── The persist→project→broadcast pipeline ───────────────────────────────────
//
/// THE ONLY WAY any event enters the system. Ordering guarantees (all three by
/// construction):
///   G1 seq assignment: store.append runs FIRST and alone assigns Seq.
///   G2 projection visibility: projections.apply completes BEFORE bus.send and
///      BEFORE emit returns — later observers see state >= seq everywhere.
///   G3 total order == seq order: the whole body runs under ONE tokio Mutex,
///      so concurrent emitters cannot interleave out of seq order.
///
/// Failure semantics: step 1 failure ⇒ Err(StoreError) (mapped to
/// crate::Error::Store here), NEITHER projection NOR bus touched; steps 2–3
/// are infallible in-memory ops.
///
/// Lives HERE, not the driver layer: AcpConnections push raw FxEvents into a
/// channel (driver/acp EventTx) and ONE pump task drains through emit().
///
/// Mutex choice: tokio's async mutex because projection_snapshot() MUST hold
/// the same guard across an awaited head_seq() (envelope guarantee: next event
/// after the snapshot has seq == baseline_seq + 1). A sync mutex held across
/// await would be unsound-by-convention even if rarely contended.
#[derive(Clone)]
pub struct EventSink {
    inner: Arc<tokio::sync::Mutex<SinkInner>>,
    /// Cloned handle for synchronous subscribe() — the broadcast Sender is
    /// itself clonable, so subscription never needs to fight the emit guard.
    bus: EventBus,
}

struct SinkInner {
    store: Arc<dyn EventStore>,
    bus: EventBus,
    proj: Projections,
}

impl EventSink {
    pub fn new(store: Arc<dyn EventStore>, bus: EventBus, proj: Projections) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(SinkInner {
                store,
                bus: bus.clone(),
                proj,
            })),
            bus,
        }
    }

    /// append → project → broadcast under one guard (G1–G3).
    pub async fn emit(&self, ev: FxEvent) -> Result<Sequenced<FxEvent>, crate::Error> {
        let mut inner = self.inner.lock().await;
        let stamped = inner.store.append(ev).await.map_err(crate::Error::from)?;
        inner.proj.apply(&stamped);
        inner.bus.send(stamped.clone());
        Ok(stamped)
    }

    /// Read-only projection peek for handler validation (cheap; still behind
    /// the emit guard so G2 holds for readers too).
    pub async fn with_projections<R>(&self, f: impl FnOnce(&Projections) -> R) -> R {
        let inner = self.inner.lock().await;
        f(&inner.proj)
    }

    /// Snapshot recipe — CORRECTNESS-CRITICAL, do not reorder:
    ///   1. acquire sink guard        (blocks all appends)
    ///   2. clone agents/threads/perms
    ///   3. head = store.head_seq() inside the SAME guard: no append can
    ///      interleave between clone and read ⇒ states cover exactly ≤ head.
    ///
    /// Combined with append assigning monotonic rowids inside this same mutex,
    /// the next event after the frame has seq == baseline_seq + 1.
    pub(crate) async fn snapshot_locked(
        &self,
    ) -> Result<fxproto::envelope::Snapshot, crate::Error> {
        let inner = self.inner.lock().await;
        let baseline = inner.store.head_seq().await.map_err(crate::Error::from)?;
        Ok(fxproto::envelope::Snapshot {
            baseline_seq: baseline,
            agents: inner.proj.agents.clone(),
            threads: inner.proj.threads.clone(),
            perms: inner.proj.perms.clone(),
        })
    }
}

// ── Handler context ──────────────────────────────────────────────────────────

/// AgentId → live connection. BTreeMap for deterministic iteration on shutdown;
/// Arc<AcpConnection> because spawned turn tasks hold clones across awaits.
pub type ConnMap = BTreeMap<AgentId, Arc<AcpConnection>>;

/// Shared finish-claim between the turn task and any cancel watchdog: whoever
/// claims FIRST emits the force-finish TurnFinished; the loser no-ops (fold W7
/// would absorb doubles anyway; this avoids the warn noise).
#[derive(Debug, Default)]
pub struct FinishClaim {
    inner: std::sync::Mutex<bool>,
    /// Wake for late subscribers wanting to observe resolution (tests).
    pub notify: tokio::sync::Notify,
}

impl FinishClaim {
    /// True iff caller became THE emitter for the final TurnFinished.
    pub fn try_claim(&self) -> bool {
        let mut g = self.inner.lock().expect("finish claim");
        if *g {
            false
        } else {
            *g = true;
            self.notify.notify_waiters();
            true
        }
    }
}

/// Perms registry SHARED with turn tasks: conn-death sweeps must answer parked
/// responders WITHOUT the actor's &mut Ctx, so ownership sits in an async Mutex
/// everyone locks briefly. (DEVIATION from older draft placing the plain map
/// solely in the actor — flagged in docs.)
pub type PermShared = Arc<tokio::sync::Mutex<perms::PendingPerms>>;

/// Bookkeeping per running turn (session::prompt owns insertion; cancel owns
/// abort/watchdog; completion pops via an internal actor message).
#[derive(Debug)]
pub struct TurnHandle {
    pub turn: fxproto::ids::TurnId,
    pub abort: tokio::task::AbortHandle,
    pub claim: Arc<FinishClaim>,
}

/// Everything a command handler needs. Passed as `&mut Ctx` by the actor loop.
pub struct Ctx<'a> {
    /// Interior-mutex cache means handlers only ever need shared access.
    pub registry: &'a DriverRegistry,
    /// Live agent connections (see ConnMap note).
    pub conns: &'a mut ConnMap,
    /// Short non-awaiting reads go through ctx.sink.with_projections(..).
    pub sink: EventSink,
    pub pending_perms: &'a PermShared,
    /// Session→agent ownership for prompt/cancel routing. RUNTIME-ONLY index:
    /// rebuildable from AgentsState.sessions at boot; written by new_session.
    pub session_agent: &'a mut BTreeMap<SessionId, AgentId>,
    /// Live turn tasks (see TurnHandle note).
    pub turn_tasks: &'a mut BTreeMap<SessionId, TurnHandle>,
    /// Actor backchannel for internal bookkeeping (turn completions).
    pub job_tx: tokio::sync::mpsc::UnboundedSender<InternalCmd>,
    /// The ONLY minting source for turn ids in handlers (request ids mint in
    /// normalize.rs via its own clone; agent ids here). See ids.rs.
    pub idgen: &'a crate::ids::IdGen,
    /// Global pump sender cloned into every fresh connection.
    pub events_tx: crate::driver::acp::EventTx,
    /// Registration sender cloned into every started connection; parks flow up.
    pub permreg_tx: PermRegTx,
    /// Watchdog duration read once at boot (FX_CANCEL_WATCHDOG_MS override).
    pub cancel_watchdog: std::time::Duration,
}

impl Ctx<'_> {
    pub async fn projections<R>(&self, f: impl FnOnce(&Projections) -> R) -> R {
        self.sink.with_projections(f).await
    }
}

/// Internal actor messages beyond client Commands. fxproto is final — turn
/// completions travel THIS channel, not the wire enum.
pub enum InternalCmd {
    /// Turn task finished on its own: pop turn_tasks[s], keep counters honest.
    TurnDone { session: SessionId },
}

/// Exhaustive over Command. NOTE there is deliberately NO Subscribe arm:
/// subscription is envelope-level (Message::Subscribe handled by fxserver
/// net/handshake.rs). The match being exhaustive means any future fxproto
/// variant demands "what does fxcore do with it?" at compile time.
pub async fn dispatch(ctx: &mut Ctx<'_>, cmd: Command) -> Result<Reply, crate::Error> {
    match cmd {
        Command::DetectAgents => {
            // never errors: found:false rows are data
            Ok(Reply::DetectedAgents {
                drivers: ctx.registry.detect_all().await,
            })
        }
        Command::StartAgent { driver } => session::start_agent(ctx, driver).await,
        Command::NewSession {
            agent,
            cwd,
            mcp_servers,
        } => session::new_session(ctx, agent, cwd, mcp_servers).await,
        Command::Prompt { session, blocks } => session::prompt(ctx, session, blocks).await,
        Command::Cancel { session } => session::cancel(ctx, session).await,
        Command::PermissionResponse {
            request_id,
            option_id,
        } => perms::respond(ctx, request_id, option_id).await,
    }
}

impl EventSink {
    /// Post-persist fanout handle for one ws client (fxserver net/handshake).
    pub fn bus_subscribe(&self) -> BusReceiver {
        self.bus.subscribe()
    }
}
