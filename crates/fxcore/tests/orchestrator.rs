//! Orchestrator integration tests — impl.md Phase 8.5 durability suite:
//! crash_and_replay / cursor_replay / ordering_guarantee (**M2 exit**).
//!
//! Split of transports used here (deliberate, one per concern):
//! - crash_and_replay spawns the REAL `examples/fake_agent_stdio` binary via
//!   Config.drivers overrides (no connection-factory seam!) so the exit-code
//!   chain is genuine OS reality: child exits(7) mid-turn ⇒ the client's
//!   finalize ladder reaps Some(7) ⇒ AgentStatus{Crashed{exit_code:Some(7)}}.
//! - cursor_replay / ordering_guarantee use the in-process duplex FakeAgent
//!   harness (same engine module as tests/fake_agent.rs) — those scenarios pin
//!   LOG semantics, not process-death semantics.
//!
//! Serial execution contract: the three scenarios share process-global state
//! (FX_CANCEL_WATCHDOG_MS env + the CONN_FACTORY seam), so they serialize on
//! one async mutex even though `[tokio::test]` defaults to parallel runners.
//! NEVER remove the guards without changing this to --test-threads=1 CI-wide.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1 as s;
use futures::FutureExt;

use fxcore::Orchestrator;
use fxcore::config::Config;
use fxcore::driver::acp as client_acp;
use fxproto::command::Command;
use fxproto::content::{ContentBlock, Role, StopReason};
use fxproto::driver::{DriverId, DriverSpec};
use fxproto::event::{AgentStatus, FxEvent, Sequenced};
use fxproto::ids::Seq;
use fxproto::reply::Reply;

#[path = "../examples/support/testing_agent.rs"]
mod testing_agent;
use testing_agent::Script;

// ── Duplex FakeAgent wiring (verbatim twin of tests/fake_agent.rs post-G1) ──

fn start_harness(script: Script) -> (testing_agent::Engine, Arc<Mutex<Option<ClientEnds>>>) {
    let (ca_client_end, ca_agent_end) = tokio::io::duplex(64 * 1024);
    let (ac_agent_end, ac_client_end) = tokio::io::duplex(64 * 1024);
    let transport = agent_client_protocol::ByteStreams::new(
        testing_agent::compat_wrap(ac_agent_end),
        testing_agent::compat_read(ca_agent_end),
    );
    let engine = testing_agent::connect_engine(script, transport, "fake-agent", None);
    (
        engine,
        Arc::new(Mutex::new(Some(ClientEnds {
            out: ca_client_end,
            inn: ac_client_end,
        }))),
    )
}

/// Local copy of the duplex client-half wrapper (tests/fake_agent.rs owns the
/// original); named distinctly to avoid colliding in this flat test crate.
struct ClientEnds {
    out: tokio::io::DuplexStream,
    inn: tokio::io::DuplexStream,
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

// ── Shared scenario helpers ─────────────────────────────────────────────────

/// Absolute path of the stdio fake agent EXAMPLE binary. Runtime lookup so a
/// missing build shows an instructive panic instead of a cryptic compile-time
/// `env!` failure; cargo sets CARGO_BIN_EXE_<name> (example targets included)
/// whenever it builds this package's integration tests — fxcore owns it.
pub fn fake_agent_stdio_exe() -> String {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_fake_agent_stdio") {
        return path.to_string_lossy().into_owned();
    }
    // Fallback for runners outside `cargo test`: workspace dev-profile
    // artifact location relative to our own test binary (.../deps/...).
    let guess = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .map(|deps| deps.join("../examples/fake_agent_stdio"))
        .filter(|p| p.is_file());
    guess
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            panic!(
                "CARGO_BIN_EXE_fake_agent_stdio unset; run `cargo build -p fxcore --examples` first"
            )
        })
}

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("fxcore-orch-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn base_config(data_dir: &std::path::Path) -> Config {
    Config {
        data_dir: data_dir.to_path_buf(),
        bind_override: None,
        drivers: Default::default(),
    }
}

/// Cross-scenario serializer (see module header). Tokio mutex because the
/// guard legitimately spans awaits inside each scenario.
static SERIAL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn serial<'a>() -> tokio::sync::MutexGuard<'a, ()> {
    SERIAL_LOCK.lock().await
}

const EVENT_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);

/// Bus receive with the common timeout envelope; panics loudly on lag/closure.
async fn next_event(rx: &mut fxcore::BusReceiver) -> Sequenced<FxEvent> {
    match tokio::time::timeout(EVENT_BUDGET, rx.recv()).await {
        Ok(Ok(ev)) => ev,
        other => panic!("expected event within budget, got {other:?}"),
    }
}

/// Keep receiving for a quiet window: late notifications trail their causal
/// event by scheduler ticks; drain everything during a short idle gap before
/// snapshotting/replaying (same recipe as tests/fake_agent.rs).
async fn quiet_drain(rx: &mut fxcore::BusReceiver, collect: &mut Vec<u64>) {
    let mut quiet: u32 = 0;
    while quiet < 5 {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                collect.push(ev.seq.as_u64());
                quiet = 0;
            }
            _ => quiet += 1,
        }
    }
}

fn ascending_strict(events: &[u64]) -> bool {
    events.windows(2).all(|w| w[1] > w[0])
}

fn driver_spec(program: &str, env: &[(&str, &str)]) -> DriverSpec {
    DriverSpec {
        program: program.to_owned(),
        args: vec![],
        env: env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

/// Standard StartAgent→NewSession round-trip returning raw id strings.
async fn open_session(orch: &Orchestrator, agent: Option<DriverId>) -> (String, String) {
    let Reply::Started { agent } = orch
        .execute(Command::StartAgent {
            driver: agent.unwrap_or(DriverId::ClaudeCode),
        })
        .await
        .expect("StartAgent")
    else {
        panic!("expected Started");
    };
    let agent_string = agent.clone().to_string();
    // SDK-side v1 compat validates that session/new's cwd EXISTS.
    let cwd = std::env::temp_dir().canonicalize().expect("cwd");
    let Reply::SessionCreated { session } = orch
        .execute(Command::NewSession {
            agent,
            cwd,
            mcp_servers: vec![],
        })
        .await
        .expect("NewSession")
    else {
        panic!("expected SessionCreated");
    };
    (agent_string, session.to_string())
}

fn opt_allow_reject() -> Vec<s::PermissionOption> {
    vec![
        s::PermissionOption::new("opt-allow", "Allow", s::PermissionOptionKind::AllowOnce),
        s::PermissionOption::new("opt-reject", "Reject", s::PermissionOptionKind::RejectOnce),
    ]
}

/// Reset the PROCESS-GLOBAL connection-factory seam back to production
/// behavior. The factory is a single static slot (driver/acp/mod.rs), so a
/// duplex scenario that leaves its one-shot harness armed would otherwise
/// HIJACK a later real-spawn scenario's StartAgent (its ends-cell returns
/// None ⇒ panic inside the actor task ⇒ Error::ShuttingDown flake observed
/// during M2 G4 bring-up). Reinstalling an exact passthrough replica of the
/// factory-less path restores parity without touching fxcore sources.
fn disarm_connection_factory() {
    client_acp::set_connection_factory_for_tests(Arc::new(
        |agent_id: fxproto::ids::AgentId, plan, events, idgen, permreg| {
            Box::pin(async move {
                client_acp::AcpConnection::start(&agent_id, &plan, events, idgen, permreg).await
            })
        },
    ));
}

// ── crash_and_replay ─────────────────────────────────────────────────────────

/// REAL process exits 7 mid-turn:
///   1. Streamed partial transcript lands BEFORE death (user echo + agent
///      chunk persisted through the sink pipeline),
///   2. AgentStatus{Crashed{exit_code: Some(7)}} surfaces exactly once
///      (post-Ready crash published by the AcpConnection runner's finalize
///      ladder reading the true child status),
///   3. TurnFinished{Cancelled} arrives (turn-task error matrix). Its ORDER vs
///      Crashed is deliberately NOT pinned: two independent emitters race the
///      same pump and either may win a given scheduler roll (documented; no
///      contract exists between those two events).
///   4. RESTART on the same store: head identical, projections rebuilt by pure
///      replay fold contain the partial transcript + Crashed audit row, and
///      the next StartAgent mints a FRESH agent id (resurrection rule).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_and_replay() {
    let _guard = serial().await;
    let scratch = Scratch::new("crash");

    let cfg = Config {
        drivers: BTreeMap::from([(
            DriverId::ClaudeCode,
            driver_spec(
                &fake_agent_stdio_exe(),
                &[("FX_FAKE_MODE", "crash_after_chunk")],
            ),
        )]),
        ..base_config(scratch.0.as_path())
    };

    let orch = Orchestrator::new(cfg.clone()).await.expect("boot A");
    let mut sub = orch.subscribe();

    let (agent_a, session) = open_session(&orch, None).await;
    let session_typed = fxproto::ids::SessionId::from_raw(session.clone());

    let Reply::PromptAccepted { .. } = orch
        .execute(Command::Prompt {
            session: session_typed.clone(),
            blocks: vec![ContentBlock::Text { text: "go".into() }],
        })
        .await
        .expect("prompt")
    else {
        panic!("expected PromptAccepted");
    };

    // Collect until crash evidence + finish pair appear.
    let deadline = std::time::Instant::now() + EVENT_BUDGET;
    let mut seen_seqs: Vec<u64> = Vec::new();
    let mut crashed_7: Option<Option<i32>> = None;
    let mut finished_cancelled = false;
    let mut partial_text = false;
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(
            deadline.saturating_duration_since(std::time::Instant::now()),
            sub.recv(),
        )
        .await
        {
            Ok(Ok(ev)) => {
                match &ev.inner {
                    FxEvent::AgentStatus {
                        status: AgentStatus::Crashed { exit_code },
                        ..
                    } => crashed_7 = Some(*exit_code),
                    FxEvent::TurnFinished {
                        stop_reason: StopReason::Cancelled,
                        ..
                    } => finished_cancelled = true,
                    FxEvent::Chunk { role, text, .. }
                        if matches!(role, Role::Agent) && text.contains("-partial") =>
                    {
                        partial_text = true;
                    }
                    _ => {}
                }
                seen_seqs.push(ev.seq.as_u64());
                if crashed_7.is_some() && finished_cancelled {
                    break;
                }
            }
            other => panic!("bus ended early: {other:?}"),
        }
    }
    assert_eq!(crashed_7, Some(Some(7)), "REAL exit code 7 surfaced");
    assert!(finished_cancelled, "turn closed cancelled");
    assert!(partial_text, "partial agent chunk streamed before crash");

    // Nothing emitted out of order through the single-sink pipeline.
    assert!(
        ascending_strict(&seen_seqs),
        "strictly increasing seqs across bus"
    );

    let head_pre = orch
        .projection_snapshot()
        .await
        .expect("snap A")
        .baseline_seq
        .as_u64();
    orch.shutdown().await;

    // ── restart on the SAME store ──
    let orch_b = Orchestrator::new(base_config(scratch.0.as_path()))
        .await
        .expect("boot B (same store)");
    let snap_b = orch_b.projection_snapshot().await.expect("snap B");
    assert_eq!(
        snap_b.baseline_seq.as_u64(),
        head_pre,
        "reopened store head equals pre-crash head (no loss/dup)"
    );

    // FULL replay-from-0 folded cleanly into Projections at boot: transcript
    // rebuilt purely from the persisted log contains the partial turn.
    let thread = &snap_b.threads.threads[&session_typed];
    assert!(
        thread.active_turn.is_none(),
        "cancelled turn cleared on fold"
    );
    let msgs: Vec<(String, Role)> = thread
        .messages
        .iter()
        .map(|m| (m.text.clone(), m.role))
        .collect();
    assert!(
        msgs.contains(&("go".to_owned(), Role::User)),
        "user echo survived restart: {msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|(t, r)| matches!(r, Role::Agent) && t.contains("-partial")),
        "agent partial text survived restart: {msgs:?}"
    );
    // The Crashed audit row too (exit code rides the fold):
    assert!(
        snap_b
            .agents
            .agents
            .values()
            .any(|a| matches!(a.status, AgentStatus::Crashed { exit_code: Some(7) })),
        "Crashed{{Some(7)}} visible in rebuilt AgentsState"
    );

    // NEW StartAgent gets a FRESH id (production IdGen ⇒ uuid-v7; equality
    // would mean resurrection violated the who-mints-what contract).
    let Reply::Started { agent: agent_b } = orch_b
        .execute(Command::StartAgent {
            driver: DriverId::ClaudeCode,
        })
        .await
        .expect("StartAgent B")
    else {
        panic!("expected Started on B");
    };
    assert_ne!(
        agent_b.to_string(),
        agent_a,
        "fresh id per resurrection rule"
    );

    // Reaped with the orchestrator (child never prompted: stdin EOF ⇒ clean exit).
    orch_b.shutdown().await;
}

// ── cursor_replay ────────────────────────────────────────────────────────────

/// Client-resubscribe semantics stay GAPLESS and CURSOR-EXCLUSIVE:
/// after two full turns (head H), replay_from(H/2) returns EXACTLY the tail
/// window (H/2+1 ..= H) — the event AT the boundary must NOT double-apply —
/// and replay_from(0) covers 1..=H once with zero gaps. A live subscriber
/// attached BEFORE the final turn stitches onto the suffix without overlap
/// surprises: its seqs are strictly ascending and land inside ≤ H.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursor_replay() {
    let _guard = serial().await;
    let scratch = Scratch::new("cursor");

    let orch = Orchestrator::new_with_ids(
        base_config(scratch.0.as_path()),
        fxcore::ids::IdGen::deterministic("c"),
    )
    .await
    .expect("boot");
    let mut first_sub = orch.subscribe();

    // Connection injection: factory pops the prepared pipe ends ONCE; the same
    // conn serves both turns (steps restart per prompt by engine contract).
    let (_harness, ends_cell) = start_harness(Script(vec![
        testing_agent::Step::Chunk(Role::Agent, "a1".into()),
        testing_agent::Step::Stop(s::StopReason::EndTurn),
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

    let (_agent, session) = open_session(&orch, None).await;
    let session_typed = fxproto::ids::SessionId::from_raw(session.clone());
    let prompt = |text: &'static str| Command::Prompt {
        session: session_typed.clone(),
        blocks: vec![ContentBlock::Text { text: text.into() }],
    };

    // Turn 1 (first subscriber), then LIVE attach before turn 2.
    orch.execute(prompt("t1")).await.expect("prompt 1");
    loop {
        let ev = next_event(&mut first_sub).await;
        if matches!(
            ev.inner,
            FxEvent::TurnFinished {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ) {
            break;
        }
    }

    let mut live = orch.subscribe();
    let mut live_seqs: Vec<u64> = Vec::new();
    orch.execute(prompt("t2")).await.expect("prompt 2");
    loop {
        let ev = next_event(&mut live).await;
        live_seqs.push(ev.seq.as_u64());
        if matches!(
            ev.inner,
            FxEvent::TurnFinished {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ) {
            break;
        }
    }
    quiet_drain(&mut live, &mut live_seqs).await;

    assert!(ascending_strict(&live_seqs), "live seq order");
    let h = orch
        .projection_snapshot()
        .await
        .unwrap()
        .baseline_seq
        .as_u64();
    assert!(!live_seqs.is_empty() && live_seqs.iter().all(|s| *s <= h));

    // FULL walk: exact 1..=H, gapless.
    let full = orch.replay_from(Seq::new(0)).await.expect("replay 0");
    assert_eq!(full.len() as u64, h, "log size == head");
    for (i, e) in full.iter().enumerate() {
        assert_eq!(e.seq.as_u64(), i as u64 + 1, "seq gapless position {i}");
    }

    // Mid-log resubscribe: EXACT remaining suffix, strictly-after the cursor
    // (dedupe-no-double-apply of the SAME seq boundary event).
    let half = h / 2;
    let suffix = orch.replay_from(Seq::new(half)).await.expect("replay half");
    assert_eq!(
        suffix.first().map(|e| e.seq.as_u64()),
        Some(half + 1),
        "boundary event excluded — cursor-exclusive"
    );
    assert_eq!(suffix.len(), (h - half) as usize, "exact remaining count");
    for (i, e) in suffix.iter().enumerate() {
        assert_eq!(e.seq.as_u64(), half + 1 + i as u64, "suffix gapless");
    }

    // Union identity (handshake stitching contract): every persisted seq is
    // covered exactly once by full ++ nothing-extra; the live tail is a subset
    // of the log whose union with replay-from(any cursor ≥ min(live)) stays
    // duplicate-free because the writer warmup filters seq ≤ high_water.
    // Functionally pinned here by: live_seqs ⊆ 1..=H unique-ascending AND
    // suffix == (half+1)..=H ⇒ stitching suffix then live-minus-(≤ tail seen)
    // cannot double-count.
    let live_set: std::collections::BTreeSet<u64> = live_seqs.iter().copied().collect();
    assert_eq!(live_set.len(), live_seqs.len(), "live seqs unique");

    orch.shutdown().await;
    disarm_connection_factory();
}

// ── ordering_guarantee ───────────────────────────────────────────────────────

/// Cancel DURING an active turn with a pending permission ask produces, IN SEQ
/// ORDER in the appended log:
///     PermissionResolved{chosen: None}     (the sweep, cancelled-audit row)
/// BEFORE
///     TurnFinished{stop_reason: Cancelled} (watchdog force-finish at 250ms).
/// Rapid-fire Prompt→Cancel→Prompt racing is deliberately NOT attempted here:
/// command validation rejects prompt-while-active with TurnNotActive BY DESIGN
/// (guard suite in tests/fake_agent.rs); what IS pinned here is the total-
/// order machinery itself: strictly ascending, globally unique seqs across
/// BOTH the appended log and the bus subscription.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordering_guarantee() {
    let _guard = serial().await;
    // Watchdog shortening is process-global; ONLY set while the serial lock
    // guarantees no sibling scenario is concurrently driving turns.
    unsafe { std::env::set_var("FX_CANCEL_WATCHDOG_MS", "250") };

    let scratch = Scratch::new("ordering");
    let orch = Orchestrator::new_with_ids(
        base_config(scratch.0.as_path()),
        fxcore::ids::IdGen::deterministic("o"),
    )
    .await
    .expect("boot");
    let mut sub = orch.subscribe();

    let (_harness, ends_cell) = start_harness(Script(vec![
        testing_agent::Step::AskPermission(opt_allow_reject()),
        testing_agent::Step::Stall,
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

    let (_agent, session) = open_session(&orch, None).await;
    let session_typed = fxproto::ids::SessionId::from_raw(session.clone());

    orch.execute(Command::Prompt {
        session: session_typed.clone(),
        blocks: vec![ContentBlock::Text { text: "go".into() }],
    })
    .await
    .expect("prompt");
    loop {
        let ev = next_event(&mut sub).await;
        if let FxEvent::PermissionRequested { options, .. } = ev.inner {
            assert_eq!(options.len(), 2);
            break;
        }
    }

    // Cancel promptly (<1s ack by construction).
    let started = std::time::Instant::now();
    let reply = orch
        .execute(Command::Cancel {
            session: session_typed.clone(),
        })
        .await
        .expect("cancel");
    assert_eq!(reply, Reply::Cancelled);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "cancel must ack fast"
    );

    // Drain until watchdog force-finish.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match tokio::time::timeout(
            deadline.saturating_duration_since(std::time::Instant::now()),
            sub.recv(),
        )
        .await
        {
            Ok(Ok(ev)) => {
                if let FxEvent::TurnFinished {
                    stop_reason: StopReason::Cancelled,
                    ..
                } = ev.inner
                {
                    break;
                }
            }
            other => panic!("watchdog finish never arrived: {other:?}"),
        }
    }
    quiet_drain(&mut sub, &mut Vec::new()).await;

    // ── AUTHORITY CHECK: the appended log itself, not the bus view ──
    let log = orch.replay_from(Seq::new(0)).await.expect("log");
    let finished_pos = log
        .iter()
        .position(|e| {
            matches!(
                e.inner,
                FxEvent::TurnFinished {
                    stop_reason: StopReason::Cancelled,
                    ..
                }
            )
        })
        .expect("finished in log");
    let swept_pos = log
        .iter()
        .position(|e| matches!(e.inner, FxEvent::PermissionResolved { chosen: None, .. }))
        .expect("swept permission row in log");
    assert!(
        swept_pos < finished_pos,
        "PermissionResolved(None) seq-position {swept_pos} must precede \
         TurnFinished(Cancelled) {finished_pos}"
    );

    // Strict ascending, gapless, unique in the AUTHORITATIVE log.
    for (i, e) in log.iter().enumerate() {
        assert_eq!(e.seq.as_u64(), i as u64 + 1, "log gapless position {i}");
    }
    assert!(ascending_strict(
        &(1..=log.len() as u64).collect::<Vec<_>>()
    ));

    // Guard released: session reusable afterwards.
    let second = orch
        .execute(Command::Prompt {
            session: session_typed.clone(),
            blocks: vec![ContentBlock::Text {
                text: "again".into(),
            }],
        })
        .await
        .expect("second prompt");
    assert!(matches!(second, Reply::PromptAccepted { .. }));

    orch.shutdown().await;
    disarm_connection_factory();
}

// ── unused-import tripwire shim ─────────────────────────────────────────────
// FutureExt carries now_or_never used nowhere today (kept out of hot paths to
// avoid cross-test blocking reads of broadcast receivers); exercised trivially
// so the import can't rot silently.
#[test]
fn now_or_never_helper_is_alive() {
    let ready = futures::future::ready(7u8).now_or_never();
    assert_eq!(ready, Some(7));
}
