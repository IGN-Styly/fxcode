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
//!
//! M2 G1 NOTE: the Script/Step/run_steps engine moved VERBATIM into
//! `examples/support/testing_agent.rs` so `examples/fake_agent_stdio.rs`
//! reuses it over real stdio pipes. This file keeps only the DUPLEX wiring.

#![allow(dead_code)]

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1 as s;

use fxproto::content::Role;

#[path = "../examples/support/testing_agent.rs"]
mod testing_agent;
pub use testing_agent::{ObservedRequest, Script, Step};

type Harness = testing_agent::Engine;

/// One-call duplex wiring: build the two pairs, spawn the agent task binding
/// the ac*/ca_agent ends through the SHARED engine, hand back observation
/// channels plus the CLIENT pipe ends.
fn start_harness(script: Script) -> (Harness, Arc<Mutex<Option<ClientEnds>>>) {
    let (ca_client_end, ca_agent_end) = tokio::io::duplex(64 * 1024);
    let (ac_agent_end, ac_client_end) = tokio::io::duplex(64 * 1024);

    let agent_transport = agent_client_protocol::ByteStreams::new(
        testing_agent::compat_wrap(ac_agent_end),
        testing_agent::compat_read(ca_agent_end),
    );
    let engine = testing_agent::connect_engine(script, agent_transport, "fake-agent", None);

    (
        engine,
        Arc::new(Mutex::new(Some(ClientEnds {
            out: ca_client_end,
            inn: ac_client_end,
        }))),
    )
}

/// The client-side pipe halves, handed to the test connection factory (opaque
/// transports are not Clone, so ends are stored raw and wrapped lazily).
struct ClientEnds {
    out: tokio::io::DuplexStream, // client writes here
    inn: tokio::io::DuplexStream, // client reads here
}

impl ClientEnds {
    fn into_transport(
        self,
    ) -> impl agent_client_protocol::ConnectTo<agent_client_protocol::Client> + 'static {
        agent_client_protocol::ByteStreams::new(
            testing_agent::compat_wrap(self.out),
            testing_agent::compat_read(self.inn),
        )
    }
}

// ── Tests: harness sanity (handshake gate — impl.md 4.3) ─────────────────────

use fxcore::Orchestrator;
use fxcore::config::Config;
use fxcore::driver::acp as client_acp;
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
