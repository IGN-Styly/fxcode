//! The single v0 transport driver: one ACP connection per agent process,
//! many sessions per connection (mirrors ACP semantics).
//!
//! VERIFIED SDK FACTS (re-written against agent-client-protocol 1.3.0 +
//! schema 1.4.0 source during implementation):
//!
//! 1. TRANSPORT IS futures-io. Root-exported `acp_sdk::ByteStreams<OB, IB>`
//!    implements `ConnectTo<R>` where `OB: futures::AsyncWrite + Send +
//!    'static`, `IB: futures::AsyncRead + Send + 'static`. Tokio primitives
//!    bridge through hand-rolled `compat_*` adapters at the bottom of this
//!    file — tokio_util is NOT an fxcore dependency in the final manifest
//!    (flagged DEVIATION; semantics equal tokio_util::compat). Direction
//!    reality: outgoing half = OUR write end into the child's STDIN; incoming
//!    half = OUR read end of STDOUT (`acp_sdk::Stdio` wires OUR OWN process
//!    stdio — wrong tool here).
//! 2. LIFECYCLE IS ONE CLOSURE: `Client.builder()…connect_with(transport,
//!    main_fn)` drives everything until main_fn RETURNS, then shuts down.
//!    Handlers register BEFORE connect_with; callbacks run on the SDK dispatch
//!    loop which serializes message processing around each callback
//!    (concepts/ordering). `.block_task()` is legal in main_fn and in tasks
//!    spawned via ConnectionTo::spawn, ILLEGAL inside handler callbacks.
//!    Server->client requests arrive as typed handlers carrying an
//!    `acp_sdk::Responder<T>` consumed exactly once via `.respond(..)`.
//!    Our handlers therefore only FORWARD raw messages onto internal channels
//!    consumed by the single main_fn loop — normalization/bookkeeping stays in
//!    exactly one task per connection, no lock choreography.
//! 3. Client counterpart role is `Agent`; wire types live at
//!    `agent_client_protocol::schema::v1::*` (root re-exports cover jsonrpc
//!    infra only — adjusted scaffold claim).
//! 4. Unhandled server->client REQUESTS fall through automatically to
//!    method_not_found error responses (incoming_actor.rs): fs/terminal asks
//!    surface as protocol errors with zero wiring, matching v0 capability
//!    negation. Wired handlers: SessionNotification + RequestPermissionRequest.
//!
//! TEARDOWN DEVIATION (flagged): blueprint asked SIGTERM → grace → SIGKILL.
//! No signal-capable dependency ships in the final manifest and Cargo.toml
//! edits are off-limits, so the implemented ladder is: connection close (stdin
//! EOF) → agents exiting on EOF land immediately → SHUTDOWN_GRACE elapse →
//! `child.kill()` (SIGKILL). Semantics preserved: clean-close-first, bounded
//! grace, hard stop.

pub mod normalize;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_client_protocol as acp_sdk;
use agent_client_protocol::schema::v1 as s;

use fxproto::content::{ContentBlock, McpServerSpec, StopReason};
use fxproto::driver::{DriverId, DriverSpec};
use fxproto::event::{AgentStatus, FxEvent};
use fxproto::ids::{AgentId, OptionId, RequestId, SessionId, TurnId};

use crate::driver::SpawnPlan;
use crate::ids::IdGen;

/// How drivers hand events upward WITHOUT depending on cmd/orchestrator layers:
/// ONE global unbounded mpsc of raw FxEvents owned by the Orchestrator, drained
/// by ONE pump task into EventSink::emit (cmd/mod.rs PUMP-TASK OWNERSHIP).
///
/// NOTE (DEVIATION from older orchestrator.rs stub text): per-agent bounded
/// ConnEventRx channels + PumpCmd Attach plumbing are superseded by this decided
/// design in cmd/mod.rs; the two-state-owner contract survives intact.
///
/// Contract for emitters (this module):
///   - NEVER assign seq (store does), never touch projections/bus/store;
///   - events arrive here already-normalized (normalize.rs output);
///   - send() is effectively infallible-and-blocking-free (unbounded); a send
///     error means the Orchestrator dropped the receiver (shutting down) —
///     log once, stop producing quietly.
pub type EventTx = tokio::sync::mpsc::UnboundedSender<FxEvent>;

/// Tunables (single source of truth — tests import these):
pub const INIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Start-phase retry ladder (pre-Ready crashes ONLY): attempt delays
/// [500ms, 2000ms] before giving up ⇒ max START_ATTEMPTS=3 tries total.
/// NO AUTO-RESPAWN AFTER Ready — post-Ready child death publishes `Crashed`
/// exactly once and STOPS (sessions die with the process; recovery = user runs
/// StartAgent again → fresh AgentId per model/agents.rs resurrection rule).
pub const START_ATTEMPTS: u32 = 3;
pub const START_BACKOFF_MS: &[u64] = &[500, 2000];

/// Grace allowed between connection-close (stdin EOF) and the hard kill during
/// teardown. (SIGTERM step absent — see the TEARDOWN DEVIATION note.)
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Ceiling for outbound round-trips driven through this driver's public API,
/// so one stuck agent cannot wedge the command channel forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// ── Parked-permission surface ────────────────────────────────────────────────

/// Serde-clean pair produced by normalize::request_permission; the actor pairs
/// the core with the SDK Responder at callback time.
#[derive(Debug)]
pub struct PendingAcpRequestCore {
    pub our_id: RequestId,
    pub acp_session: String,
}

/// One parked permission ask handed UP to cmd/perms.rs via [`PermRegTx`].
/// Delivered strictly BEFORE the corresponding PermissionRequested event lands
/// on the pump (blueprint ordering contract): park first, then emit.
#[derive(Debug)]
pub struct ParkedPerm {
    /// Serde-clean identity half.
    pub core: PendingAcpRequestCore,
    /// REAL SDK responder — consume exactly once. Dropping it raw surfaces a
    /// JSON-RPC error frame to the agent, hence every removal path in
    /// cmd/perms.rs routes through respond_selected/respond_cancelled below.
    pub responder: acp_sdk::Responder<s::RequestPermissionResponse>,
    /// OUR SessionId — translated from the raw ACP string at park time via the
    /// adoption map (unknown ids never produce a park: warn+dropped upstream).
    pub session: SessionId,
}

impl ParkedPerm {
    /// User picked an option ⇒ Selected { option_id }. Consumes self.
    pub fn respond_selected(self, option_id: &OptionId) -> Result<(), String> {
        let outcome = normalize::respond_outcome(Some(option_id.clone()));
        self.responder
            .respond(s::RequestPermissionResponse::new(outcome))
            .map_err(|e| e.to_string())
    }

    /// Cancel/watchdog/sweep/crash ⇒ Outcome::Cancelled. Consumes self. ACP
    /// REQUIRES clients answer pending permission requests this way.
    pub fn respond_cancelled(self) -> Result<(), String> {
        let outcome = normalize::respond_outcome(None);
        self.responder
            .respond(s::RequestPermissionResponse::new(outcome))
            .map_err(|e| e.to_string())
    }
}

/// Channel wiring connection actors into cmd/perms.rs ownership: parked
/// registrations are consumed by the orchestrator actor's select loop.
pub type PermRegTx = tokio::sync::mpsc::UnboundedSender<ParkedPerm>;
pub type PermRegRx = tokio::sync::mpsc::UnboundedReceiver<ParkedPerm>;

// ── Commands / terminal facts ────────────────────────────────────────────────

enum ConnCmd {
    NewSession {
        cwd: PathBuf,
        mcp: Vec<McpServerSpec>,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    Prompt {
        our_session: SessionId,
        turn: TurnId,
        blocks: Vec<ContentBlock>,
        reply: tokio::sync::oneshot::Sender<Result<StopReason, String>>,
    },
    Cancel {
        acp_session: String,
    },
    /// Ordered teardown: break the loop (transports close ⇒ child sees stdin
    /// EOF) → grace → kill fallback → publish AgentStatus::Stopped.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminal {
    Alive,
    Stopped,
    Crashed(Option<i32>),
}

/// Interior view shared by inbound callbacks and the command loop. Lock scopes
/// stay tiny; per-agent inbound serialization comes free from the SDK dispatch
/// loop, leaving acknowledged race windows bounded to park/registration beats.
struct SharedState {
    /// Adopted sessions keyed by RAW ACP id → verbatim our-SessionId clone.
    sessions: Mutex<BTreeMap<String, SessionId>>,
    /// ACTIVE turn stamp per acp sid — inserted by the Prompt arm before the
    /// request flies, removed when its response lands or the connection ends.
    /// Chunk normalization consults this; turnless streams cannot fabricate
    /// fxproto TurnIds.
    turns: Mutex<BTreeMap<String, TurnId>>,
    /// Composed tool-call views (R4/R5 merge memory), keys
    /// "{acp_sid}\x1f{tool_call_id}".
    tool_views: Mutex<BTreeMap<String, normalize::ComposedToolCall>>,
}

impl SharedState {
    fn put_turn(&self, acp_sid: &str, turn: &TurnId) {
        if let Ok(mut t) = self.turns.lock() {
            t.insert(acp_sid.to_owned(), turn.clone());
        }
    }
    fn peek_turn(&self, acp_sid: &str) -> Option<TurnId> {
        self.turns.lock().ok()?.get(acp_sid).cloned()
    }
    fn resolve_our(&self, acp_sid: &str) -> Option<SessionId> {
        self.sessions.lock().ok()?.get(acp_sid).cloned()
    }
}

fn tool_view_key(acp_sid: &str, raw_tool_call_id: &str) -> String {
    format!("{acp_sid}\u{1f}{raw_tool_call_id}")
}

/// Public face of one agent connection. `&self` methods enqueue onto the single
/// consumer (the main_fn loop) or mutate interior arcs; soundness by channels.
pub struct AcpConnection {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<ConnCmd>,
    shared: Arc<SharedState>,
    terminal: Arc<Mutex<Terminal>>,
    join: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl AcpConnection {
    /// Register the adopted acp ↔ our-session pair (cmd/session.rs calls right
    /// after adoption succeeds). Unknown-acp inbound traffic stays warn+dropped
    /// downstream (adoption happens exactly once, HERE).
    pub fn register_session(&self, our: SessionId, acp_id: String) -> Result<(), String> {
        self.shared
            .sessions
            .lock()
            .map_err(|_| "session registry poisoned".to_owned())?
            .insert(acp_id, our);
        Ok(())
    }

    /// ACP session/new ⇒ the AGENT's sessionId string; caller adopts verbatim
    /// (fxproto WHO-MINTS-WHAT).
    pub async fn new_session(&self, cwd: &Path, mcp: &[McpServerSpec]) -> Result<String, String> {
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(ConnCmd::NewSession {
                cwd: cwd.to_owned(),
                mcp: mcp.to_vec(),
                reply,
            })
            .map_err(|_| "connection task gone".to_owned())?;
        match tokio::time::timeout(REQUEST_TIMEOUT, reply_rx).await {
            Ok(resolved) => resolved.unwrap_or_else(|_| Err("connection died mid-answer".into())),
            Err(_) => Err("session/new timed out".into()),
        }
    }

    /// session/prompt. Resolves ONLY at PromptResponse.stop_reason (whole turn
    /// finished) — streaming updates arrive separately as normalized FxEvents,
    /// so callers MUST treat Ok(_) as "turn finished", never "turn started".
    /// Failure ⇒ transport/connection lost; turn tasks convert that into
    /// TurnFinished{Cancelled} (error-path matrix in cmd/session.rs).
    pub async fn prompt(
        &self,
        our_session: &SessionId,
        turn: TurnId,
        blocks: Vec<ContentBlock>,
    ) -> Result<StopReason, String> {
        let registered = self
            .shared
            .sessions
            .lock()
            .map(|g| g.contains_key(our_session.as_str()))
            .unwrap_or(false);
        if !registered {
            return Err("not registered".into());
        }
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        self.cmd_tx
            .send(ConnCmd::Prompt {
                our_session: our_session.clone(),
                turn,
                blocks,
                reply,
            })
            .map_err(|_| "connection task gone".to_owned())?;
        reply_rx
            .await
            .unwrap_or_else(|_| Err("connection died mid-turn".into()))
    }

    /// session/cancel notification (fire-and-forget per ACP terms).
    pub async fn cancel(&self, acp_session: &str) {
        let _ = self.cmd_tx.send(ConnCmd::Cancel {
            acp_session: acp_session.to_owned(),
        });
    }

    /// Ordered teardown — first caller fires Shutdown and awaits completion;
    /// later callers return promptly once the terminal fact exists. Idempotent;
    /// safe after Crashed. Does NOT answer parked permissions — cmd/perms.rs
    /// sweeps those (audit rows must reflect reality already-told-to-agent).
    pub async fn shutdown(&self) {
        let taken = self.join.lock().ok().and_then(|mut g| g.take());
        match taken {
            Some(handle) => {
                let _ = self.cmd_tx.send(ConnCmd::Shutdown);
                let _ = handle.await;
            }
            None => {
                // Somebody else owns waiting: bounded wait on the terminal fact.
                let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE * 2;
                while tokio::time::Instant::now() < deadline && self.state_now() == Terminal::Alive
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    }

    fn state_now(&self) -> Terminal {
        self.terminal
            .lock()
            .map(|g| *g)
            .unwrap_or(Terminal::Stopped)
    }

    // ── construction paths ───────────────────────────────────────────────────

    /// Spawn + connect + initialize, with bounded retries. Steps IN ORDER:
    ///   1. tokio Command(program resolved by plan): args+env from spec,
    ///      stdin/stdout piped, stderr piped + forwarded to tracing (agents
    ///      log there; stdout is ACP-only traffic), kill_on_drop(true).
    ///   2. Manual ByteStreams transport over child pipes (SDK FACT #1); both
    ///      inbound handlers registered before any traffic flies.
    ///   3. InitializeRequest(V1) negotiated under INIT_TIMEOUT inside
    ///      main_fn before commands are served. Negotiated < V1 ⇒ fail;
    ///      NON-empty auth_methods ⇒ fail listing each AuthMethod id (v0 has
    ///      no auth UX — surface honestly).
    ///   4. Any failure while attempts remain ⇒ runner ends (kill_on_drop reaps
    ///      the attempt's child), sleep backoff, retry from step 1. Exhausted ⇒
    ///      Err(AgentStart); cmd/session.rs converts. NO lifecycle events here
    ///      pre-Ready (Starting was published before start() was even called).
    pub async fn start(
        agent: &AgentId,
        plan: &SpawnPlan,
        events: EventTx,
        idgen: IdGen,
        permreg: PermRegTx,
    ) -> Result<Self, crate::Error> {
        let program: PathBuf = plan
            .resolved_program
            .clone()
            .unwrap_or_else(|| PathBuf::from(&plan.spec.program));
        let mut last_err = String::new();

        for attempt in 0..START_ATTEMPTS {
            match spawn_child(agent, &program, &plan.spec) {
                Ok(mut child) => {
                    let attempt_result = match (
                        compat_write(child.stdin.take()),
                        compat_read(child.stdout.take()),
                    ) {
                        (Some(stdin), Some(stdout)) => {
                            let transport = acp_sdk::ByteStreams::new(stdin, stdout);
                            Self::build_inner(
                                agent,
                                agent_label(agent),
                                plan.driver,
                                transport,
                                events.clone(),
                                idgen.clone(),
                                permreg.clone(),
                                Some(child),
                            )
                            .await
                        }
                        _ => Err("child missing stdio pipes".into()),
                    };
                    match attempt_result {
                        Ok(conn) => return Ok(conn),
                        Err(err) => {
                            tracing::warn!(
                                target: "acp",
                                agent = %agent,
                                attempt = attempt + 1,
                                error = %err,
                                "initialize failed"
                            );
                            last_err = err;
                            // The ended runner already dropped the attempt's
                            // Child (kill_on_drop reaps it).
                        }
                    }
                }
                Err(spawn_err) => {
                    tracing::warn!(
                        target: "acp",
                        agent = %agent,
                        attempt = attempt + 1,
                        program = %program.display(),
                        error = %spawn_err,
                        "spawn failed"
                    );
                    last_err = spawn_err;
                }
            }
            if attempt + 1 < START_ATTEMPTS {
                tokio::time::sleep(Duration::from_millis(START_BACKOFF_MS[attempt as usize])).await;
            }
        }
        Err(crate::Error::AgentStart(last_err))
    }

    /// Transport-injection seam for the in-process FakeAgent harness (and any
    /// future socket transport): identical handshake/actor wiring minus the OS
    /// child. Hidden test/support surface.
    #[doc(hidden)]
    pub async fn start_over_transport(
        agent: &AgentId,
        driver: DriverId,
        transport: impl acp_sdk::ConnectTo<acp_sdk::Client> + 'static,
        events: EventTx,
        idgen: IdGen,
        permreg: PermRegTx,
    ) -> Result<Self, crate::Error> {
        Self::build_inner(
            agent,
            agent_label(agent),
            driver,
            transport,
            events,
            idgen,
            permreg,
            None,
        )
        .await
        .map_err(crate::Error::AgentStart)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_inner<T>(
        agent: &AgentId,
        name: String,
        driver: DriverId,
        transport: T,
        events: EventTx,
        idgen: IdGen,
        permreg: PermRegTx,
        child: Option<tokio::process::Child>,
    ) -> Result<Self, String>
    where
        T: acp_sdk::ConnectTo<acp_sdk::Client> + 'static,
    {
        let shared = Arc::new(SharedState {
            sessions: Mutex::new(BTreeMap::new()),
            turns: Mutex::new(BTreeMap::new()),
            tool_views: Mutex::new(BTreeMap::new()),
        });
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ready_tx, ready_rx) =
            tokio::sync::oneshot::channel::<Result<s::AgentCapabilities, String>>();
        let terminal = Arc::new(Mutex::new(Terminal::Alive));

        // Sole owner container of the Child; the dedicated supervisor task is
        // the ONLY code that touches the handle (single-owner rule — see the
        // run_connection tail notes for the reasoning trail).
        let child_cell: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>> =
            Arc::new(tokio::sync::Mutex::new(child));
        let has_child = child_cell.lock().await.is_some();
        let (sup_tx, sup_rx) = tokio::sync::mpsc::unbounded_channel::<SupReq>();

        if has_child {
            let sup_cell = Arc::clone(&child_cell);
            tokio::spawn(supervise_child(sup_cell, sup_rx));
        }

        let runner = tokio::spawn(run_connection(RunArgs {
            agent: agent.clone(),
            name,
            driver,
            shared: shared.clone(),
            terminal: terminal.clone(),
            events,
            permreg,
            cmd_rx,
            ready_tx,
            idgen,
            transport,
            has_child,
            _child_cell: child_cell,
            sup_tx,
        }));

        let caps = ready_rx
            .await
            .unwrap_or_else(|_| Err("connection died during initialize".into()))?;
        tracing::debug!(target: "acp", capabilities = ?caps, "agent initialized");

        Ok(Self {
            cmd_tx,
            shared,
            terminal,
            join: Mutex::new(Some(runner)),
        })
    }
}

// ── The connection task body ─────────────────────────────────────────────────

/// What main_fn ends with; transported out of connect_with via its result.
enum EndWhy {
    ShutdownRequested,
    TransportClosed,
}

type PermReqMsg = (
    s::RequestPermissionRequest,
    acp_sdk::Responder<s::RequestPermissionResponse>,
);

struct InFlightPrompt {
    reply: tokio::sync::oneshot::Sender<Result<StopReason, String>>,
}

/// Prompt-continuation bookkeeping message (spawned block_task → loop).
struct PromptDone {
    sid: String,
    outcome: Result<StopReason, String>,
}

/// Request channel into the child-supervisor task — sole owner of the handle.
type SupReqTx = tokio::sync::mpsc::UnboundedSender<SupReq>;

enum SupReq {
    /// Hard-stop ladder for the FINALIZE path: try_wait → bounded grace
    /// (50ms polls across SHUTDOWN_GRACE) → kill() → wait(); reports the
    /// resolved exit code. Never raced: supervisor is the single owner.
    Finalize(tokio::sync::oneshot::Sender<Option<i32>>),
}

async fn supervise_child(
    cell: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<SupReq>,
) {
    while let Some(SupReq::Finalize(reply)) = rx.recv().await {
        let code = {
            // Held through the whole ladder: enforces the single-owner rule.
            let mut guard = cell.lock().await;
            match guard.as_mut() {
                None => None,
                Some(kid) => Some(child_finalize(kid).await),
            }
        };
        let _ = reply.send(code.flatten());
    }
}

/// Grace ladder for ONE child handle: quick reap if already dead, bounded
/// SIGTERM-less grace polls otherwise, then the hard kill. Never called twice
/// concurrently (supervisor owns the cell guard).
async fn child_finalize(kid: &mut tokio::process::Child) -> Option<i32> {
    let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
    loop {
        match kid.try_wait() {
            Ok(Some(st)) => return st.code().or_else(|| negative_signal(&st)),
            Ok(None) => {}
            Err(_) => return Some(-1),
        }
        if tokio::time::Instant::now() >= deadline {
            if kid.kill().await.is_err() {
                return Some(-1);
            }
            return match kid.wait().await {
                Ok(st) => Some(st.code().or_else(|| negative_signal(&st)).unwrap_or(-1)),
                Err(_) => Some(-1),
            };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct RunArgs<T> {
    agent: AgentId,
    name: String,
    driver: DriverId,
    shared: Arc<SharedState>,
    terminal: Arc<Mutex<Terminal>>,
    events: EventTx,
    permreg: PermRegTx,
    cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ConnCmd>,
    ready_tx: tokio::sync::oneshot::Sender<Result<s::AgentCapabilities, String>>,
    idgen: IdGen,
    transport: T,
    has_child: bool,
    /// Sole-owner container; the supervisor task holds the only consuming
    /// clone, this one documents ownership (runner never touches the handle).
    _child_cell: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>,
    sup_tx: SupReqTx,
}

/// Full ACP client lifecycle against one transport:
///   1. connect_with(main_fn): initialize → single select loop until Shutdown
///      or transport death; ALL bookkeeping (turn stamps, tool-call views,
///      in-flight prompts) lives in THIS one task — no lock choreography.
///   2. AFTER connect_with unwinds (transports closed ⇒ child saw stdin EOF):
///      supervisor Finalize (grace ladder) for a real child, then ONE terminal
///      AgentStatus publication through the global pump. Never-Ready attempts
///      publish NOTHING (start()'s retry ladder owns pre-Ready reporting).
async fn run_connection<T>(args: RunArgs<T>)
where
    T: acp_sdk::ConnectTo<acp_sdk::Client> + 'static,
{
    let RunArgs {
        agent,
        name,
        driver,
        shared,
        terminal,
        events,
        permreg,
        cmd_rx,
        ready_tx,
        idgen,
        transport,
        has_child,
        _child_cell,
        sup_tx,
    } = args;

    // Inbound handlers forward raw SDK messages onto internal channels; the
    // main loop below is their only consumer (serialization by construction).
    // Receiver lifetimes == connection lifetime, so sends are infallible here.
    let (notif_tx, notif_rx) = tokio::sync::mpsc::unbounded_channel::<s::SessionNotification>();
    let (perm_req_tx, perm_req_rx) = tokio::sync::mpsc::unbounded_channel::<PermReqMsg>();

    let reached_ready = Arc::new(AtomicBool::new(false));
    let loop_shared = shared.clone();
    let loop_events = events.clone();
    let loop_ready_tx = ready_tx;
    let loop_reached = reached_ready.clone();
    let loop_idgen = idgen;

    let why_result = acp_sdk::Client
        .builder()
        .name(format!("{name} (client)"))
        .on_receive_notification(
            async move |note: s::SessionNotification, _cx| {
                let _ = notif_tx.send(note);
                Ok(())
            },
            acp_sdk::on_receive_notification!(),
        )
        .on_receive_request(
            async move |req: s::RequestPermissionRequest,
                        responder: acp_sdk::Responder<s::RequestPermissionResponse>,
                        _cx| {
                let _ = perm_req_tx.send((req, responder));
                Ok(())
            },
            acp_sdk::on_receive_request!(),
        )
        .connect_with(transport, move |cx| {
            main_loop(
                cx,
                MainLoopState {
                    shared: loop_shared,
                    events: loop_events,
                    permreg,
                    cmd_rx,
                    ready_tx: loop_ready_tx,
                    idgen: loop_idgen,
                    reached_ready: loop_reached,
                    notif_rx,
                    perm_req_rx,
                },
            )
        })
        .await;

    match &why_result {
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(target: "acp", agent = %agent, error = %err, "connection ended with error");
        }
    }

    if !reached_ready.load(Ordering::SeqCst) {
        // Never Ready ⇒ no public status was ever published as alive; the
        // start() retry ladder reports via Err(AgentStart). No events here.
        if let Ok(mut t) = terminal.lock() {
            *t = Terminal::Stopped; // internal bookkeeping only
        }
        return;
    }

    // Grace/kill ladder FIRST (audit rows reflect reality), publication SECOND.
    let exit_code: Option<i32> = if has_child {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if sup_tx.send(SupReq::Finalize(reply_tx)).is_ok() {
            match reply_rx.await {
                Ok(code) => code,
                Err(_) => Some(-1),
            }
        } else {
            Some(-1)
        }
    } else {
        None
    };

    let end = match why_result {
        Ok(EndWhy::ShutdownRequested) => Terminal::Stopped,
        _ => Terminal::Crashed(match exit_code {
            Some(c) => Some(c),
            // Processless EOF close (duplex fake-crash / harness teardown):
            None => Some(-1),
        }),
    };
    publish_terminal(&events, &agent, driver, end);

    if let Ok(mut t) = terminal.lock() {
        *t = end;
    }
}

/// Lifecycle facts beyond Ready come from exactly this fn (single site).
fn publish_terminal(events: &EventTx, agent: &AgentId, driver: DriverId, end: Terminal) {
    let status = match end {
        Terminal::Alive => return,
        Terminal::Stopped => AgentStatus::Stopped,
        Terminal::Crashed(code) => AgentStatus::Crashed { exit_code: code },
    };
    tracing::debug!(target: "acp", agent = %agent, ?status, "publishing terminal status");
    if events
        .send(FxEvent::AgentStatus {
            agent: agent.clone(),
            driver,
            status,
        })
        .is_err()
    {
        tracing::debug!(target: "acp", "terminal event dropped (orchestrator shutting down)");
    }
}

// ── main_fn ──────────────────────────────────────────────────────────────────

/// Everything the select loop needs, owned outright (named async fn keeps the
/// connect_with inference tractable; see scratch-validated pattern).
struct MainLoopState {
    shared: Arc<SharedState>,
    events: EventTx,
    permreg: PermRegTx,
    cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ConnCmd>,
    ready_tx: tokio::sync::oneshot::Sender<Result<s::AgentCapabilities, String>>,
    idgen: IdGen,
    reached_ready: Arc<AtomicBool>,
    notif_rx: tokio::sync::mpsc::UnboundedReceiver<s::SessionNotification>,
    perm_req_rx: tokio::sync::mpsc::UnboundedReceiver<PermReqMsg>,
}

async fn main_loop(
    cx: acp_sdk::ConnectionTo<acp_sdk::Agent>,
    mut st: MainLoopState,
) -> Result<EndWhy, acp_sdk::Error> {
    // ── initialize handshake ────────────────────────────────────────────────
    let init_fut = cx.send_request(s::InitializeRequest::new(
        acp_sdk::schema::ProtocolVersion::V1,
    ));
    let init_resp = match tokio::time::timeout(INIT_TIMEOUT, init_fut.block_task()).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            let msg = format!("initialize failed: {e}");
            let _ = st.ready_tx.send(Err(msg.clone()));
            return Err(acp_sdk::Error::internal_error().data(msg));
        }
        Err(_) => {
            let msg = "initialize timed out";
            let _ = st.ready_tx.send(Err(msg.to_owned()));
            return Err(acp_sdk::Error::internal_error().data(msg.to_owned()));
        }
    };

    // Negotiated < V1 refuses; non-empty auth_methods refuse honestly (v0 has
    // no auth UX).
    if init_resp.protocol_version.as_u16() < acp_sdk::schema::ProtocolVersion::V1.as_u16() {
        let msg = format!(
            "agent negotiated protocol version {} < V1",
            init_resp.protocol_version.as_u16()
        );
        let _ = st.ready_tx.send(Err(msg.clone()));
        return Err(acp_sdk::Error::invalid_request().data(msg));
    }
    if !init_resp.auth_methods.is_empty() {
        let ids: Vec<&str> = init_resp
            .auth_methods
            .iter()
            .map(|m| m.id().0.as_ref())
            .collect();
        let msg = format!("agent requires authentication: {ids:?}");
        let _ = st.ready_tx.send(Err(msg.clone()));
        return Err(acp_sdk::Error::invalid_request().data(msg));
    }

    if st.reached_ready.swap(true, Ordering::SeqCst) {
        unreachable!("handshake ran twice");
    }
    if st.ready_tx.send(Ok(init_resp.agent_capabilities)).is_err() {
        // Builder vanished before Ready consumed: treat as closure request.
        return Ok(EndWhy::TransportClosed);
    }

    // ── steady-state select loop ────────────────────────────────────────────
    let mut inflight: BTreeMap<String, InFlightPrompt> = BTreeMap::new();
    let (prompt_done_tx, mut prompt_done_rx) = tokio::sync::mpsc::unbounded_channel::<PromptDone>();

    Ok(loop {
        tokio::select! {
            cmd = st.cmd_rx.recv() => match cmd {
                None | Some(ConnCmd::Shutdown) => break EndWhy::ShutdownRequested,
                Some(ConnCmd::NewSession { cwd, mcp, reply }) => {
                    let fut = cx
                        .send_request(s::NewSessionRequest::new(cwd).mcp_servers(
                            mcp.iter().map(mcp_server_to_sdk).collect(),
                        ));
                    tokio::spawn(async move {
                        let res = match tokio::time::timeout(REQUEST_TIMEOUT, fut.block_task()).await {
                            Ok(Ok(resp)) => Ok(resp.session_id.to_string()),
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(_) => Err("session/new timed out".into()),
                        };
                        let _ = reply.send(res);
                        Ok::<(), acp_sdk::Error>(())
                    });
                }
                Some(ConnCmd::Prompt { our_session, turn, blocks, reply }) => {
                    let key = our_session.as_str().to_owned();
                    if let Some(prev) = inflight.remove(&key) {
                        // Defensive: server-side TurnNotActive guard should make
                        // this unreachable; absorb without wedging the channel.
                        let _ = prev.reply.send(Err("superseded".into()));
                    }
                    inflight.insert(key.clone(), InFlightPrompt { reply });
                    st.shared.put_turn(&key, &turn);

                    let sdk_blocks: Vec<s::ContentBlock> =
                        blocks.iter().map(content_block_to_sdk).collect();
                    let fut = cx.send_request(s::PromptRequest::new(key.clone(), sdk_blocks));
                    let done_tx = prompt_done_tx.clone();
                    let spawn_res = cx.spawn({
                        let sid = key.clone();
                        async move {
                            let outcome = fut.block_task().await;
                            let outcome = match outcome {
                                Ok(resp) => Ok(normalize::stop_reason(resp.stop_reason)),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = done_tx.send(PromptDone { sid, outcome });
                            Ok(())
                        }
                    });
                    if spawn_res.is_err() {
                        // Task system closing; incoming_closed arm will reap us.
                        continue;
                    }
                }
                Some(ConnCmd::Cancel { acp_session }) => {
                    if let Err(e) = cx.send_notification(s::CancelNotification::new(acp_session)) {
                        tracing::debug!(target: "acp", error = %e, "cancel notification failed");
                    }
                }
            },

            done = prompt_done_rx.recv(), if !inflight.is_empty() => match done {
                Some(done) => {
                    // NOTE: turn stamps are intentionally KEPT after completion.
                    // Agent→client transport is FIFO, but our completion notices
                    // take an extra hop (response router → spawned task →
                    // internal channel) and can land BEFORE streamed
                    // notifications are drained; dropping the stamp there would
                    // silently eat real transcript chunks. Attribution windows
                    // therefore end only when the next prompt replaces the
                    // stamp or the connection drains at teardown. Folds treat
                    // post-finish same-turn chunks as ordinary messages (W2).
                    if let Some(entry) = inflight.remove(&done.sid) {
                        let _ = entry.reply.send(done.outcome);
                    }
                }
                None => break EndWhy::TransportClosed,
            },

            note = st.notif_rx.recv() => match note {
                Some(note) => route_notification(note, &st.shared, &st.events),
                None => break EndWhy::TransportClosed,
            },

            perm = st.perm_req_rx.recv() => match perm {
                Some((req, responder)) => route_permission_request(
                    req,
                    responder,
                    &st.shared,
                    &st.events,
                    &st.permreg,
                    &st.idgen,
                ),
                None => break EndWhy::TransportClosed,
            },

            _ = cx.incoming_closed() => break EndWhy::TransportClosed,
        }
    })
}

// ── routing helpers (single consumer task, so plain fns suffice) ────────────

fn route_notification(note: s::SessionNotification, shared: &SharedState, events: &EventTx) {
    let acp_sid = note.session_id.to_string();
    let Some(our_session) = shared.resolve_our(&acp_sid) else {
        tracing::warn!(target: "acp", acp_session = %acp_sid, "notification for unknown session; dropping");
        return;
    };
    let turn = shared.peek_turn(&acp_sid);

    // Tool-call rows lock the composed-view map for their merge memory.
    let evs: Vec<FxEvent> = match &note.update {
        ref u @ (s::SessionUpdate::ToolCall(_) | s::SessionUpdate::ToolCallUpdate(_)) => {
            let raw_id = match u {
                s::SessionUpdate::ToolCall(t) => t.tool_call_id.to_string(),
                s::SessionUpdate::ToolCallUpdate(upd) => upd.tool_call_id.to_string(),
                _ => unreachable!(),
            };
            let key = tool_view_key(&acp_sid, &raw_id);
            let mut guard = match shared.tool_views.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let fresh = !guard.contains_key(&key);
            let view = guard.entry(key).or_default();
            if fresh && matches!(u, s::SessionUpdate::ToolCallUpdate(_)) {
                tracing::warn!(target: "normalize", acp_session=%acp_sid, tool_call=%raw_id, "tool_call_update before any tool_call: synthesized defaults");
            }
            normalize::session_update(&our_session, turn.as_ref(), u, Some(view))
        }
        _ => normalize::session_update(&our_session, turn.as_ref(), &note.update, None),
    };

    for ev in evs {
        if events.send(ev).is_err() {
            tracing::debug!(target: "acp", "event sink closed; stopping notification routing");
            break;
        }
    }
}

fn route_permission_request(
    req: s::RequestPermissionRequest,
    responder: acp_sdk::Responder<s::RequestPermissionResponse>,
    shared: &SharedState,
    events: &EventTx,
    permreg: &PermRegTx,
    idgen: &IdGen,
) {
    let acp_sid = req.session_id.to_string();
    let Some(our_session) = shared.resolve_our(&acp_sid) else {
        // Unknown session: answer cancelled politely and drop — never park.
        tracing::warn!(target: "acp", acp_session=%acp_sid, "permission request for unknown session; cancelling");
        let _ = responder.respond(s::RequestPermissionResponse::new(
            normalize::respond_outcome(None),
        ));
        return;
    };

    let (event, core) = normalize::request_permission(&req, &our_session, idgen);
    let parked = ParkedPerm {
        core,
        responder,
        session: our_session,
    };

    // ORDERING CONTRACT: park reaches cmd/perms.rs BEFORE the event becomes
    // visible on the pump.
    if permreg.send(parked).is_err() {
        tracing::warn!(target: "acp", "permreg closed; permission dropped (shutting down?)");
        return;
    }
    if events.send(event).is_err() {
        tracing::debug!(target: "acp", "event sink closed during permission emit");
    }
}

// ── fxproto ↔ SDK shape conversions (outbound side of the seam) ─────────────

pub(crate) fn content_block_to_sdk(b: &ContentBlock) -> s::ContentBlock {
    match b {
        ContentBlock::Text { text } => s::ContentBlock::Text(s::TextContent::new(text.clone())),
        ContentBlock::Image { media_type, data } => {
            s::ContentBlock::Image(s::ImageContent::new(data.clone(), media_type.clone()))
        }
        ContentBlock::Resource {
            uri,
            media_type,
            contents,
        } => s::ContentBlock::Resource(s::EmbeddedResource::new(match contents {
            fxproto::content::ResourceContents::Text { text } => {
                s::EmbeddedResourceResource::TextResourceContents(
                    s::TextResourceContents::new(text.clone(), uri.clone())
                        .mime_type(media_type.clone()),
                )
            }
            fxproto::content::ResourceContents::Blob { blob } => {
                s::EmbeddedResourceResource::BlobResourceContents(
                    s::BlobResourceContents::new(blob.clone(), uri.clone())
                        .mime_type(media_type.clone()),
                )
            }
        })),
    }
}

pub(crate) fn mcp_server_to_sdk(spec: &McpServerSpec) -> s::McpServer {
    s::McpServer::Stdio(
        s::McpServerStdio::new(spec.name.clone(), PathBuf::from(&spec.command))
            .args(spec.args.clone())
            .env(
                spec.env
                    .iter()
                    .map(|(k, v)| s::EnvVariable::new(k.clone(), v.clone()))
                    .collect(),
            ),
    )
}

// ── process spawning / tracing forwarder ─────────────────────────────────────

fn agent_label(a: &AgentId) -> String {
    format!("agent:{a}")
}

/// POSIX-only stance: killed statuses report as their NEGATIVE signal number
/// so audit trails distinguish SIGKILL (-9) from clean exits.
pub(crate) fn negative_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Spawn one agent child with piped stdio:
/// stdin/stdout are ACP-only traffic; stderr is PIPED AND FORWARDED to tracing
/// via a dedicated line-reader task. kill_on_drop(true) guarantees no orphans
/// across failed retries (start() owns the ladder).
fn spawn_child(
    agent: &AgentId,
    program: &Path,
    spec: &DriverSpec,
) -> Result<tokio::process::Child, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(&spec.args)
        .env_clear_heredoc(&spec.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;

    if let Some(stderr) = child.stderr.take() {
        let label = agent.to_string();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "acp-stderr", agent = %label, "{line}");
            }
        });
    }

    Ok(child)
}

trait EnvClearHeredoc {
    fn env_clear_heredoc(self, env: &std::collections::BTreeMap<String, String>) -> Self;
}
impl EnvClearHeredoc for &mut tokio::process::Command {
    /// Inherits our env (agents need PATH/HOME/etc.) then layers spec.env.
    fn env_clear_heredoc(self, env: &std::collections::BTreeMap<String, String>) -> Self {
        for (k, v) in env {
            self.env(k, v);
        }
        self
    }
}

// ── tokio ⇄ futures-io compat bridge ─────────────────────────────────────────
//
// The SDK speaks futures-io; our I/O primitives are tokio's (child pipes,
// duplexes). With tokio-util absent from the final manifest these tiny
// adapters do exactly what tokio_util::compat does — semantics verified
// against futures-io trait shapes during the scratch validation round.

pub struct CompatWrite<T>(pub T);
pub struct CompatRead<T>(pub T);

#[doc(hidden)]
pub fn compat_write<T: tokio::io::AsyncWrite + Unpin>(t: Option<T>) -> Option<CompatWrite<T>> {
    t.map(CompatWrite)
}

#[doc(hidden)]
pub fn compat_read<T: tokio::io::AsyncRead + Unpin>(t: Option<T>) -> Option<CompatRead<T>> {
    t.map(CompatRead)
}

/// Connection-construction indirection used by cmd/session::start_agent.
/// Production never registers one (falls through to AcpConnection::start);
/// integration tests bind FakeAgent duplex transports here.
pub type ConnFactory = Arc<
    dyn Fn(
            AgentId,
            SpawnPlan,
            EventTx,
            IdGen,
            PermRegTx,
        ) -> futures::future::BoxFuture<'static, Result<AcpConnection, crate::Error>>
        + Send
        + Sync,
>;

static CONN_FACTORY: std::sync::RwLock<Option<ConnFactory>> = std::sync::RwLock::new(None);

/// Test-only seam (global state ⇒ orchestrator tests must run serially when a
/// factory is installed; documented at both ends). Re-installation replaces the
/// previous factory so per-test isolation survives.
#[doc(hidden)]
pub fn set_connection_factory_for_tests(factory: ConnFactory) {
    *CONN_FACTORY.write().expect("conn factory lock") = Some(factory);
}

pub(crate) fn spawn_agent_connection(
    agent: &AgentId,
    plan: &SpawnPlan,
    events: EventTx,
    idgen: IdGen,
    permreg: PermRegTx,
) -> futures::future::BoxFuture<'static, Result<AcpConnection, crate::Error>> {
    let agent = agent.clone();
    let plan = plan.clone();
    Box::pin(async move {
        let factory = CONN_FACTORY.read().expect("conn factory lock").clone();
        match factory {
            Some(f) => f(agent, plan, events, idgen, permreg).await,
            None => AcpConnection::start(&agent, &plan, events, idgen, permreg).await,
        }
    })
}

/// Hidden test surface: in-process FakeAgent harness wires duplex halves with
/// these same adapters (tests/fake_agent.rs).
#[doc(hidden)]
pub mod __test_compat {
    pub use super::{CompatRead as Read, CompatWrite as Write, compat_read, compat_write};
}

impl<T: tokio::io::AsyncWrite + Unpin> futures::AsyncWrite for CompatWrite<T> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // futures-io closes via poll_close; tokio models close as shutdown.
        std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl<T: tokio::io::AsyncRead + Unpin> futures::AsyncRead for CompatRead<T> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut rb = tokio::io::ReadBuf::new(buf);
        match std::pin::Pin::new(&mut self.0).poll_read(cx, &mut rb) {
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(rb.filled().len())),
            other => match other {
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(Ok(())) => unreachable!("handled above"),
            },
        }
    }
}
