//! In-process fake ACP agent for integration tests — NO real CLIs in CI.
//!
//! Built on the SAME official `agent-client-protocol` crate, implementing the
//! AGENT side over in-memory duplex streams instead of stdio: wire framing,
//! JSON-RPC ids and ndjson line discipline are REAL — only the OS pipe is fake.
//!
//! Transport wiring (validated against SDK 1.3.0): two duplex pairs with roles
//! explicit — ca* = client→agent wire, ac* = agent→client wire; each side wraps
//! its halves through fxcore's own compat adapters.
//!
//! Handler model: EVERY scripted step runs inline inside the typed
//! PromptRequest handler (the SDK dispatch loop stays unblocked because the
//! permission gate schedules its continuation via on_receiving_result instead
//! of awaiting block_task inside a callback). The agent's main_fn parks until
//! either a Step::Crash flips the watch or transports reach EOF.
//! `block_task()` remains ILLEGAL inside callbacks per SDK docs.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_client_protocol as acp_sdk;
use agent_client_protocol::schema::v1 as s;

use fxcore::driver::acp as client_acp;
use fxproto::content::Role;

// ── Script engine ────────────────────────────────────────────────────────────

/// Deterministic behavior for one fake agent instance. Steps fire IN ORDER per
/// session/prompt received; each prompt restarts from step 0.
#[derive(Debug, Clone)]
pub struct Script(pub Vec<Step>);

impl Script {
    fn materialize(&self) -> Vec<Step> {
        self.0.clone()
    }
}

#[derive(Debug, Clone)]
pub enum Step {
    /// One streamed text chunk notification.
    Chunk(Role, String),
    /// tool_call(pending by default).
    ToolCall { id: String, title: String },
    /// tool_call_update upsert; Some(output) rides raw_output.
    ToolCallUpdate {
        id: String,
        status: s::ToolCallStatus,
        output: Option<String>,
    },
    /// Full plan snapshot replace.
    Plan(Vec<s::PlanEntry>),
    /// session/request_permission; script halts until the outcome arrives.
    AskPermission(Vec<s::PermissionOption>),
    /// Close the connection mid-turn WITHOUT answering anything pending.
    Crash,
    /// Never respond to this prompt (FX_CANCEL_WATCHDOG_MS override tests only).
    Stall,
    /// End the turn with this stop reason (the ONLY terminal step).
    Stop(s::StopReason),
}

/// What tests observe (agent-side traffic log):
#[derive(Debug)]
pub enum ObservedRequest {
    NewSession {
        cwd: std::path::PathBuf,
        mcp_servers: Vec<s::McpServer>,
    },
    Prompt {
        session_id: String,
        blocks: Vec<s::ContentBlock>,
    },
    Cancelled {
        session_id: String,
    },
    Outcome {
        session_id: String,
        outcome: Option<s::RequestPermissionOutcome>,
    },
}

pub struct Harness {
    pub observed_rx: tokio::sync::mpsc::UnboundedReceiver<ObservedRequest>,
    pub session_ids_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

struct Core {
    script: std::sync::Mutex<Vec<Step>>,
    counter: AtomicU64,
    observed_tx: tokio::sync::mpsc::UnboundedSender<ObservedRequest>,
    sessions_tx: tokio::sync::mpsc::UnboundedSender<String>,
    crash_tx: tokio::sync::watch::Sender<bool>,
}

fn note_chunk(sid: &str, role: Role, text: &str) -> s::SessionNotification {
    let update = match role {
        Role::User => s::SessionUpdate::UserMessageChunk(s::ContentChunk::new(
            s::ContentBlock::from(text.to_owned()),
        )),
        Role::Agent => s::SessionUpdate::AgentMessageChunk(s::ContentChunk::new(
            s::ContentBlock::from(text.to_owned()),
        )),
    };
    s::SessionNotification::new(s::SessionId::new(sid.to_owned()), update)
}

/// One-pass runner used by BOTH the handler segment before any permission and
/// the post-outcome continuation. Owned values all the way down.
fn run_steps(
    cx: acp_sdk::ConnectionTo<acp_sdk::Client>,
    sid: String,
    steps: Vec<Step>,
    prompt_responder: Option<acp_sdk::Responder<s::PromptResponse>>,
    observed_tx: tokio::sync::mpsc::UnboundedSender<ObservedRequest>,
    crash_tx: Option<tokio::sync::watch::Sender<bool>>,
) -> futures::future::BoxFuture<'static, Result<(), acp_sdk::Error>> {
    Box::pin(run_steps_inner(
        cx,
        sid,
        steps,
        prompt_responder,
        observed_tx,
        crash_tx,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_steps_inner(
    cx: acp_sdk::ConnectionTo<acp_sdk::Client>,
    sid: String,
    steps: Vec<Step>,
    prompt_responder: Option<acp_sdk::Responder<s::PromptResponse>>,
    observed_tx: tokio::sync::mpsc::UnboundedSender<ObservedRequest>,
    crash_tx: Option<tokio::sync::watch::Sender<bool>>,
) -> Result<(), acp_sdk::Error> {
    let mut it = steps.into_iter();
    while let Some(step) = it.next() {
        match step {
            Step::Chunk(role, text) => {
                cx.send_notification(note_chunk(&sid, role, &text))?;
            }
            Step::ToolCall { id, title } => {
                cx.send_notification(s::SessionNotification::new(
                    s::SessionId::new(sid.clone()),
                    s::SessionUpdate::ToolCall(s::ToolCall::new(id, title)),
                ))?;
            }
            Step::ToolCallUpdate { id, status, output } => {
                let mut fields = s::ToolCallUpdateFields::new().status(status);
                if let Some(out) = output {
                    fields = fields.raw_output(serde_json::json!({ "text": out }));
                }
                cx.send_notification(s::SessionNotification::new(
                    s::SessionId::new(sid.clone()),
                    s::SessionUpdate::ToolCallUpdate(s::ToolCallUpdate::new(id, fields)),
                ))?;
            }
            Step::Plan(entries) => {
                cx.send_notification(s::SessionNotification::new(
                    s::SessionId::new(sid.clone()),
                    s::SessionUpdate::Plan(s::Plan::new(entries)),
                ))?;
            }
            Step::AskPermission(opts) => {
                // Real ACP agents stream the tool_call card BEFORE asking for
                // permission on it; mirror that so the W6 badge fold has a
                // card to stamp.
                cx.send_notification(s::SessionNotification::new(
                    s::SessionId::new(sid.clone()),
                    s::SessionUpdate::ToolCall(s::ToolCall::new(
                        "perm_call_1".to_owned(),
                        "permission ask".to_owned(),
                    )),
                ))?;
                let perm = s::RequestPermissionRequest::new(
                    s::SessionId::new(sid.clone()),
                    s::ToolCallUpdate::new(
                        "perm_call_1".to_owned(),
                        s::ToolCallUpdateFields::new().title("permission ask"),
                    ),
                    opts,
                );
                let remaining: Vec<Step> = it.collect();
                let cx2 = cx.clone();
                let obs2 = observed_tx.clone();
                let sid2 = sid.clone();
                cx.send_request(perm)
                    .on_receiving_result(move |outcome| async move {
                        let _ = obs2.send(ObservedRequest::Outcome {
                            session_id: sid2.clone(),
                            outcome: outcome.as_ref().ok().map(|r| r.outcome.clone()),
                        });
                        run_steps(cx2, sid2, remaining, prompt_responder, obs2, None).await?;
                        Ok(())
                    })?;
                // Rest runs after the outcome; permission gating ends this pass.
                return Ok(());
            }
            Step::Crash => {
                if let Some(tx) = crash_tx.clone() {
                    let _ = tx.send(true); // main_fn breaks ⇒ EOF to the client
                }
                return Ok(()); // prompt deliberately NEVER answered
            }
            Step::Stall => {
                std::future::pending::<()>().await;
                unreachable!("pending never resolves");
            }
            Step::Stop(reason) => {
                if let Some(resp) = prompt_responder {
                    resp.respond(s::PromptResponse::new(reason))?;
                }
                return Ok(());
            }
        }
    }
    // Script exhausted without an explicit Stop ⇒ end benignly.
    if let Some(resp) = prompt_responder {
        resp.respond(s::PromptResponse::new(s::StopReason::EndTurn))?;
    }
    Ok(())
}

/// The client-side pipe halves, handed to the test connection factory (opaque
/// transports are not Clone, so ends are stored raw and wrapped lazily).
pub struct ClientEnds {
    pub out: tokio::io::DuplexStream, // client writes here
    pub inn: tokio::io::DuplexStream, // client reads here
}

impl ClientEnds {
    pub fn into_transport(self) -> impl acp_sdk::ConnectTo<acp_sdk::Client> + 'static {
        acp_sdk::ByteStreams::new(compat_wrap(self.out), compat_read(self.inn))
    }
}

/// One-call wiring:
///   1. build the two duplex pairs,
///   2. spawn the agent task binding the ac*/ca_agent ends,
///   3. hand back observation channels plus the CLIENT pipe ends.
pub fn start_harness(script: Script) -> (Harness, Arc<std::sync::Mutex<Option<ClientEnds>>>) {
    let (ca_client_end, ca_agent_end) = tokio::io::duplex(64 * 1024);
    let (ac_agent_end, ac_client_end) = tokio::io::duplex(64 * 1024);

    let (observed_tx, observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (sessions_tx, session_ids_rx) = tokio::sync::mpsc::unbounded_channel();
    let (crash_tx, crash_rx) = tokio::sync::watch::channel(false);

    let counter_for_prompt = Arc::new(AtomicU64::new(0));

    let agent_transport =
        acp_sdk::ByteStreams::new(compat_wrap(ac_agent_end), compat_read(ca_agent_end));

    tokio::spawn(async move {
        let steps_cell = Arc::new(std::sync::Mutex::new(script.materialize()));
        let core = Arc::new(Core {
            script: std::sync::Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
            observed_tx,
            sessions_tx,
            crash_tx: crash_tx.clone(),
        });
        let core_new = Arc::clone(&core);
        let core_prompt = Arc::clone(&core);
        let core_cancel = Arc::clone(&core);
        let _ = &core;
        let result = acp_sdk::Agent
            .builder()
            .name("fake-agent")
            .on_receive_request(
                async |req: s::InitializeRequest, responder, _cx| {
                    responder.respond(s::InitializeResponse::new(req.protocol_version))?;
                    Ok(())
                },
                acp_sdk::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: s::NewSessionRequest, responder, _cx| {
                    let core = core_new.clone();
                    let n = counter_for_prompt.fetch_add(1, Ordering::SeqCst);
                    let id = format!("sess-{n:06}");
                    let _ = core.sessions_tx.send(id.clone());
                    let _ = core.observed_tx.send(ObservedRequest::NewSession {
                        cwd: req.cwd.clone(),
                        mcp_servers: req.mcp_servers.clone(),
                    });
                    responder.respond(s::NewSessionResponse::new(s::SessionId::new(id)))?;
                    Ok(())
                },
                acp_sdk::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: s::PromptRequest, responder, cx| {
                    let core = core_prompt.clone();
                    let sid = req.session_id.to_string();
                    let _ = core.observed_tx.send(ObservedRequest::Prompt {
                        session_id: sid.clone(),
                        blocks: req.prompt.clone(),
                    });
                    let steps = steps_cell.lock().expect("script lock").clone();
                    // First pass executes right here; permission continuation
                    // finishes via run_steps' on_receiving_result arm.
                    run_steps(
                        cx.clone(),
                        sid,
                        steps,
                        Some(responder),
                        core.observed_tx.clone(),
                        Some(core.crash_tx.clone()),
                    )
                    .await
                },
                acp_sdk::on_receive_request!(),
            )
            .on_receive_notification(
                async move |note: s::CancelNotification, _cx| {
                    let _ = core_cancel.observed_tx.send(ObservedRequest::Cancelled {
                        session_id: note.session_id.to_string(),
                    });
                    Ok(())
                },
                acp_sdk::on_receive_notification!(),
            )
            .connect_with(
                agent_transport,
                move |cx: acp_sdk::ConnectionTo<acp_sdk::Client>| async move {
                    let mut crash = crash_rx;
                    loop {
                        if *crash.borrow_and_update() {
                            break Ok::<_, acp_sdk::Error>(());
                        }
                        tokio::select! {
                            changed = crash.changed() => {
                                if changed.is_err() || *crash.borrow_and_update() {
                                    break Ok(());
                                }
                            }
                            _ = cx.incoming_closed() => break Ok(()),
                        }
                    }
                },
            )
            .await;
        if let Err(e) = result {
            eprintln!("fake-agent ended: {e:?}");
        }
    });

    (
        Harness {
            observed_rx,
            session_ids_rx,
        },
        Arc::new(std::sync::Mutex::new(Some(ClientEnds {
            out: ca_client_end,
            inn: ac_client_end,
        }))),
    )
}

fn observed_tx_for_core(
    tx: &tokio::sync::mpsc::UnboundedSender<ObservedRequest>,
) -> tokio::sync::mpsc::UnboundedSender<ObservedRequest> {
    tx.clone()
}

// ── compat re-export aliases ─────────────────────────────────────────────────

/// Local shims so this test crate reads symmetrically about both directions.
fn compat_wrap<T: tokio::io::AsyncWrite + Unpin>(t: T) -> fxcore::driver::acp::CompatWrite<T> {
    fxcore::driver::acp::compat_write(Some(t)).unwrap()
}

fn compat_read<T: tokio::io::AsyncRead + Unpin>(t: T) -> fxcore::driver::acp::CompatRead<T> {
    fxcore::driver::acp::compat_read(Some(t)).unwrap()
}

// ── Tests: harness sanity (handshake gate — impl.md 4.3) ─────────────────────

use fxcore::Orchestrator;
use fxcore::config::Config;
use fxproto::command::Command;
use fxproto::event::FxEvent;
use fxproto::reply::Reply;

/// Cross-scenario serializer (see ensure_serial docs). A tokio mutex because
/// the guard legitimately spans awaits inside each scenario.
static SERIAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
async fn serial<'a>() -> tokio::sync::MutexGuard<'a, ()> {
    SERIAL_LOCK.lock().await
}

async fn next_with_timeout<T>(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<T>,
    what: &'static str,
) -> T {
    match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
        Ok(Some(v)) => v,
        _ => panic!("expected {what} within 5s"),
    }
}

fn script_basic() -> Script {
    Script(vec![
        Step::Chunk(Role::Agent, "Hel".into()),
        Step::Chunk(Role::Agent, "lo ".into()),
        Step::Chunk(Role::Agent, "world".to_string()),
        Step::Stop(s::StopReason::EndTurn),
    ])
}

/// end-to-end through Orchestrator + shared architecture, no OS processes.
/// Isolation probe: FakeAgent + fxcore AcpConnection WITHOUT orchestrator.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn probe_direct_connection() {
    let (_harness, ends_cell_raw) = start_harness(Script(vec![
        Step::Chunk(Role::Agent, "hi-agent".to_string()),
        Step::Stop(s::StopReason::EndTurn),
    ]));
    let ends = ends_cell_raw.lock().unwrap().take().unwrap();
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let (perm_tx, _perm_rx) = tokio::sync::mpsc::unbounded_channel();

    let idgen = fxcore::ids::IdGen::deterministic("t");
    let conn = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        client_acp::AcpConnection::start_over_transport(
            &fxproto::ids::AgentId::from_raw("probe-a".into()),
            fxproto::driver::DriverId::ClaudeCode,
            ends.into_transport(),
            events_tx,
            idgen.clone(),
            perm_tx,
        ),
    )
    .await
    .expect("probe start within 6s")
    .expect("start ok");
    let session = conn
        .new_session(std::path::Path::new("/tmp/w"), &[])
        .await
        .expect("session/new");
    assert_eq!(session, "sess-000000");
    conn.register_session(
        fxproto::ids::SessionId::from_raw(session.clone()),
        session.clone(),
    )
    .expect("register");
    let reason = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        conn.prompt(
            &fxproto::ids::SessionId::from_raw(session.clone()),
            idgen.turn(),
            vec![fxproto::content::ContentBlock::Text {
                text: "ping".into(),
            }],
        ),
    )
    .await
    .expect("prompt in time")
    .expect("prompt ok");
    assert_eq!(reason, fxproto::content::StopReason::EndTurn);

    let got_chunk = std::sync::atomic::AtomicBool::new(false);
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(300), events_rx.recv()).await
    {
        if let FxEvent::Chunk {
            text: t2,
            role: fxproto::content::Role::Agent,
            ..
        } = ev
            && t2 == "hi-agent"
        {
            got_chunk.store(true, Ordering::SeqCst);
        }
    }
    assert!(got_chunk.load(Ordering::SeqCst));
    drop(conn);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orchestrator_happy_turn() {
    let _serial_guard = serial().await;
    let tmp = std::env::temp_dir().join(format!(
        "fxcore-happy-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();

    // Watchdog shortening is process-global; serial execution via single test.
    unsafe { std::env::set_var("FX_CANCEL_WATCHDOG_MS", "250") };

    let cfg = Config {
        data_dir: tmp.clone(),
        bind_override: None,
        drivers: Default::default(),
    };
    let orch = Orchestrator::new_with_ids(cfg, fxcore::ids::IdGen::deterministic("t"))
        .await
        .expect("boot");
    let mut sub = orch.subscribe();

    // Harness BEFORE StartAgent so nothing is missed on the agent side:
    let (mut harness, ends_cell) = start_harness(script_basic());

    // Connection injection: factory pops the prepared pipe ends (once).
    client_acp::set_connection_factory_for_tests(Arc::new(
        move |agent_id: fxproto::ids::AgentId, plan, events, idgen, permreg| {
            let ends_cell = ends_cell.clone();
            Box::pin(async move {
                let ends = ends_cell
                    .lock()
                    .expect("ends cell")
                    .take()
                    .expect("single StartAgent");
                client_acp::AcpConnection::start_over_transport(
                    &agent_id,
                    plan.driver,
                    ends.into_transport(),
                    events,
                    idgen,
                    permreg,
                )
                .await
            })
        },
    ));

    // StartAgent → Started reply + Starting/Ready events subscribed-before?
    let reply_started = orch
        .execute(Command::StartAgent {
            driver: fxproto::driver::DriverId::ClaudeCode,
        })
        .await
        .unwrap();
    let Reply::Started { agent } = reply_started else {
        panic!("expected Started, got {reply_started:?}");
    };

    // SessionCreated round-trip (adopts sess-000000 verbatim):
    let reply_session = orch
        .execute(Command::NewSession {
            agent: agent.clone(),
            // SDK-side v1 compat validates that session/new's cwd EXISTS.
            cwd: std::env::temp_dir().canonicalize().unwrap(),
            mcp_servers: vec![],
        })
        .await
        .unwrap();
    let Reply::SessionCreated { session } = reply_session else {
        panic!("expected SessionCreated, got {reply_session:?}");
    };
    let minted = next_with_timeout(&mut harness.session_ids_rx, "agent minted session").await;
    assert_eq!(session.as_str(), minted.as_str(), "verbatim adoption");

    // Prompt → PromptAccepted with deterministic turn id t-000000? Ids share a
    // counter: two agents? one minted agent (t-000000) then turn ⇒ t-000001.
    let reply_prompt = orch
        .execute(Command::Prompt {
            session: session.clone(),
            blocks: vec![fxproto::content::ContentBlock::Text { text: "hi".into() }],
        })
        .await
        .unwrap();
    let Reply::PromptAccepted { turn } = reply_prompt else {
        panic!("expected PromptAccepted, got {reply_prompt:?}");
    };
    assert_eq!(turn.as_str(), "t-000001");

    // Stream verification from subscribe(): drain until TurnFinished.
    let mut seqs = Vec::new();
    let mut user_echo = false;
    loop {
        let ev = match tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv()).await {
            Ok(Ok(ev)) => ev,
            other => panic!("subscription ended early: {other:?}"),
        };
        seqs.push(ev.seq.as_u64());
        match &ev.inner {
            FxEvent::Chunk {
                role: fxproto::content::Role::User,
                text,
                ..
            } => {
                assert_eq!(text, "hi");
                user_echo = true;
            }
            FxEvent::TurnFinished {
                stop_reason: fxproto::content::StopReason::EndTurn,
                ..
            } => break,
            _ => {}
        }
    }
    for w in seqs.windows(2) {
        assert!(w[1] > w[0], "seq regression");
    }
    assert!(user_echo, "user echo missing");
    assert!(seqs.iter().any(|&n| n > 0), "turn finished stream observed");

    // Tail-drain: notifications may still be mid-flight when TurnFinished
    // arrives (single-writer pump lags by a scheduler tick); collect everything
    // during a short quiet window before asserting on the snapshot.
    let mut quiet: u32 = 0;
    while quiet < 5 {
        match tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await {
            Ok(Ok(_)) => quiet = 0,
            _ => quiet += 1,
        }
    }

    // Folding everything into ThreadsState clears active_turn (T8).
    // (head snapshot approach: projection_snapshot)
    let snap = orch.projection_snapshot().await.unwrap();
    assert!(snap.threads.threads[&session].active_turn.is_none());
    // Exactly TWO messages: (User "hi") merged? no—role flips: user echo then
    // Agent "Hello world" merged from three chunks.
    let msgs = &snap.threads.threads[&session].messages;
    assert_eq!(
        msgs.iter()
            .map(|m| format!("{}:{:?}", m.text, m.role))
            .collect::<Vec<_>>(),
        // Agent chunks merge W2-style across one streamed flow; the trailing
        // space in the script is intentional (tests exact concat semantics).
        vec![
            format!("hi:{:?}", fxproto::content::Role::User),
            format!("Hello world:{:?}", fxproto::content::Role::Agent),
        ]
    );

    drop(orch);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), collect_shutdown()).await;
    cleanup_dir(tmp);
}

use std::sync::Once;

static SERIAL: Once = Once::new();

/// Serial-execution guard for ORCHESTRATOR scenarios in this file: they share
/// process-global state (FX_CANCEL_WATCHDOG_MS env + the connection factory
/// seam), so parallel runs cross-contaminate. The blueprint documents running
/// with --test-threads=1; the mutex makes that hold even when a runner forgets.
pub fn ensure_serial() {}

async fn collect_shutdown() {}

fn perm_options() -> Vec<s::PermissionOption> {
    vec![
        s::PermissionOption::new("opt-allow", "Allow", s::PermissionOptionKind::AllowOnce),
        s::PermissionOption::new("opt-deny", "Deny", s::PermissionOptionKind::RejectOnce),
    ]
}

/// permission_roundtrip: ask → human answers allow → script continues →
/// PermissionResolved{Some} recorded, agent observed the selection, card badge
/// stamped Chosen via W6 fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orchestrator_permission_roundtrip() {
    let _serial_guard = serial().await;
    let tmp = std::env::temp_dir().join(format!(
        "fxcore-perm-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe { std::env::set_var("FX_CANCEL_WATCHDOG_MS", "250") };

    let orch = Orchestrator::new_with_ids(
        Config {
            data_dir: tmp.clone(),
            bind_override: None,
            drivers: Default::default(),
        },
        fxcore::ids::IdGen::deterministic("t"),
    )
    .await
    .expect("boot");
    let mut sub = orch.subscribe();

    let (mut harness, ends_cell) = start_harness(Script(vec![
        Step::AskPermission(perm_options()),
        Step::Chunk(Role::Agent, "granted".to_string()),
        Step::Stop(s::StopReason::EndTurn),
    ]));

    client_acp::set_connection_factory_for_tests(Arc::new(
        move |agent_id: fxproto::ids::AgentId, plan, events, idgen, permreg| {
            let ends_cell = ends_cell.clone();
            Box::pin(async move {
                let ends = ends_cell.lock().unwrap().take().expect("single StartAgent");
                client_acp::AcpConnection::start_over_transport(
                    &agent_id,
                    plan.driver,
                    ends.into_transport(),
                    events,
                    idgen,
                    permreg,
                )
                .await
            })
        },
    ));

    let Reply::Started { agent } = orch
        .execute(Command::StartAgent {
            driver: fxproto::driver::DriverId::ClaudeCode,
        })
        .await
        .unwrap()
    else {
        panic!("expected Started");
    };
    let Reply::SessionCreated { session } = orch
        .execute(Command::NewSession {
            agent: agent.clone(),
            cwd: std::env::temp_dir().canonicalize().unwrap(),
            mcp_servers: vec![],
        })
        .await
        .unwrap()
    else {
        panic!("expected SessionCreated");
    };
    assert!(matches!(
        orch.execute(Command::Prompt {
            session: session.clone(),
            blocks: vec![fxproto::content::ContentBlock::Text { text: "go".into() }],
        })
        .await
        .unwrap(),
        Reply::PromptAccepted { .. }
    ));

    // Wait for PermissionRequested carrying our verbatim options.
    let request_id = loop {
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv())
            .await
            .expect("event in time")
            .expect("bus alive");
        if let FxEvent::PermissionRequested {
            request_id,
            options,
            ..
        } = ev.inner
        {
            assert_eq!(options.len(), 2);
            assert_eq!(options[0].option_id.as_str(), "opt-allow");
            break request_id;
        }
    };

    let reply = orch
        .execute(Command::PermissionResponse {
            request_id,
            option_id: fxproto::ids::OptionId::from_raw("opt-allow".into()),
        })
        .await
        .unwrap();
    assert_eq!(reply, Reply::PermissionRecorded);

    // Agent saw the selected outcome.
    loop {
        match next_with_timeout(&mut harness.observed_rx, "outcome").await {
            ObservedRequest::Outcome { outcome, .. } => {
                let outcome = outcome.expect("outcome present");
                assert!(matches!(outcome, s::RequestPermissionOutcome::Selected(_)));
                break;
            }
            _ => continue, // skip queued NewSession/Prompt observations
        }
    }

    // Turn completes; PermissionResolved{Some} in stream; badges stamped.
    let mut resolved_chosen = false;
    loop {
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv())
            .await
            .expect("event in time")
            .expect("bus alive");
        match &ev.inner {
            FxEvent::PermissionResolved { chosen, .. } => {
                assert!(chosen.is_some());
                assert_eq!(chosen.as_ref().unwrap().as_str(), "opt-allow");
                resolved_chosen = true;
            }
            FxEvent::TurnFinished {
                stop_reason: fxproto::content::StopReason::EndTurn,
                ..
            } => break,
            _ => {}
        }
    }
    assert!(resolved_chosen);

    let snap = orch.projection_snapshot().await.unwrap();
    assert_eq!(snap.perms.recent.len(), 1);
    assert_eq!(
        snap.perms.recent[0].chosen.as_ref().map(|o| o.as_str()),
        Some("opt-allow")
    );
    let thread = &snap.threads.threads[&session];
    let card = thread
        .tool_calls
        .values()
        .find(|c| c.perm.is_some())
        .expect("card badge stamped");
    assert!(matches!(
        card.perm,
        Some(fxproto::model::PermOutcome::Chosen(_))
    ));

    cleanup_dir(tmp);
}

/// cancel_sweeps_pending_permissions + watchdog force-finish (script never
/// answers because Stall blocks forever; FX_CANCEL_WATCHDOG_MS=250 governs).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orchestrator_cancel_sweeps_and_watchdog_finishes() {
    let _serial_guard = serial().await;
    let tmp = std::env::temp_dir().join(format!(
        "fxcore-cancel-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe { std::env::set_var("FX_CANCEL_WATCHDOG_MS", "250") };

    let orch = Orchestrator::new_with_ids(
        Config {
            data_dir: tmp.clone(),
            bind_override: None,
            drivers: Default::default(),
        },
        fxcore::ids::IdGen::deterministic("t"),
    )
    .await
    .expect("boot");
    let mut sub = orch.subscribe();

    let (mut harness, ends_cell) = start_harness(Script(vec![
        Step::AskPermission(perm_options()),
        Step::Stall,
    ]));

    client_acp::set_connection_factory_for_tests(Arc::new(
        move |agent_id: fxproto::ids::AgentId, plan, events, idgen, permreg| {
            let ends_cell = ends_cell.clone();
            Box::pin(async move {
                let ends = ends_cell.lock().unwrap().take().expect("single StartAgent");
                client_acp::AcpConnection::start_over_transport(
                    &agent_id,
                    plan.driver,
                    ends.into_transport(),
                    events,
                    idgen,
                    permreg,
                )
                .await
            })
        },
    ));

    let Reply::Started { agent } = orch
        .execute(Command::StartAgent {
            driver: fxproto::driver::DriverId::ClaudeCode,
        })
        .await
        .unwrap()
    else {
        panic!("expected Started");
    };
    let Reply::SessionCreated { session } = orch
        .execute(Command::NewSession {
            agent: agent.clone(),
            cwd: std::env::temp_dir().canonicalize().unwrap(),
            mcp_servers: vec![],
        })
        .await
        .unwrap()
    else {
        panic!("expected SessionCreated");
    };
    assert!(matches!(
        orch.execute(Command::Prompt {
            session: session.clone(),
            blocks: vec![fxproto::content::ContentBlock::Text { text: "go".into() }],
        })
        .await
        .unwrap(),
        Reply::PromptAccepted { .. }
    ));

    let request_id = loop {
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), sub.recv())
            .await
            .expect("event in time")
            .expect("bus alive");
        if let FxEvent::PermissionRequested { request_id, .. } = ev.inner {
            break request_id;
        }
    };
    drop(request_id);

    // Cancel promptly (<1s ack by construction — no sleeps in handler).
    let started = std::time::Instant::now();
    let reply = orch
        .execute(Command::Cancel {
            session: session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(reply, Reply::Cancelled);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "cancel must ack fast"
    );

    // Watchdog path (250ms) emits TurnFinished{Cancelled}; sweep emitted one
    // PermissionResolved{None} BEFORE it.
    let saw_sweep = Arc::new(tokio::sync::Mutex::new(false));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut finished_cancelled = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(200), sub.recv()).await {
            Ok(Ok(ev)) => match ev.inner {
                FxEvent::PermissionResolved { chosen, .. } => {
                    assert_eq!(chosen, None, "swept ⇒ cancelled audit row");
                    *saw_sweep.lock().await = true;
                }
                FxEvent::TurnFinished {
                    stop_reason: fxproto::content::StopReason::Cancelled,
                    ..
                } => {
                    finished_cancelled = true;
                    break;
                }
                _ => {}
            },
            Ok(Err(e)) => panic!("bus died: {e:?}"),
            Err(_) => {}
        }
    }
    assert!(*saw_sweep.lock().await, "pending permission swept");
    assert!(finished_cancelled, "watchdog force-finished as cancelled");

    // Agent received Outcome{cancelled} for its parked ask.
    loop {
        match next_with_timeout(&mut harness.observed_rx, "cancelled outcome").await {
            ObservedRequest::Outcome { outcome, .. } => {
                assert_eq!(outcome, Some(s::RequestPermissionOutcome::Cancelled));
                break;
            }
            _ => continue,
        }
    }

    // Session reusable afterwards (TurnNotActive guard released).
    let second_prompt = orch
        .execute(Command::Prompt {
            session: session.clone(),
            blocks: vec![fxproto::content::ContentBlock::Text {
                text: "again".into(),
            }],
        })
        .await
        .unwrap();
    assert!(matches!(second_prompt, Reply::PromptAccepted { .. }));

    cleanup_dir(tmp);
}

fn cleanup_dir(dir: std::path::PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}
