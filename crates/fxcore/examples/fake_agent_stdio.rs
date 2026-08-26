//! stdio ACP fake agent binary — the G1 "live events over real sockets" leg.
//!
//! Same scripted engine as tests/fake_agent.rs (shared via
//! `examples/support/testing_agent.rs`), but served over OUR OWN process
//! stdio via `acp_sdk::Stdio` instead of duplex pipes: fxcore's
//! `AcpConnection::start` spawns THIS FILE as a plain child process, so the
//! wire framing, JSON-RPC ids and ndjson line discipline are exercised against
//! genuine OS pipes — only the agent's BEHAVIOR is scripted.
//!
//! Usage contract (env knobs only; no flags besides --version):
//!   FX_FAKE_MODE: chunks (default) | perm | crash_after_chunk | hang
//!     chunks           [Chunk(Agent,text), ToolCall pair, Stop(EndTurn)]
//!     perm             AskPermission(allow/reject) then the chunks script
//!     crash_after_chunk Chunk(Agent,"partial"), dwell, then CLOSE mid-turn
//!                      WITHOUT answering the pending prompt. As a REAL
//!                      process this means exit(7) — the client's finalize
//!                      ladder reads the true status ⇒ Crashed{Some(7)}.
//!     hang             never respond (watchdog/cancel scenarios)
//!   FX_FAKE_PERM=1     same as FX_FAKE_MODE=perm (both spellings supported)
//!   FX_FAKE_TEXT       override the agent chunk text (multi-driver e2e uses
//!                      distinct texts per driver slot)
//!   --version          print one line and exit 0 — detect.rs probe_version
//!                      calls `<program> --version`; answering instantly keeps
//!                      StartAgent off its 2s probe timeout.
//!
//! The client is expected to send an EMPTY prompt payload (fxcore turn_task
//! passes Vec::new(); the orchestrator emits the user echo itself), so the
//! script echoes nothing user-side.

use agent_client_protocol as acp_sdk;

#[path = "support/testing_agent.rs"]
mod testing_agent;
use testing_agent::{Script, Step};

/// Dwell between the last notification and the process exit in
/// crash_after_chunk mode: gives the SDK writer task time to flush buffered
/// ndjson lines to stdout before connect_with teardown races them away.
const CRASH_FLUSH_DWELL: std::time::Duration = std::time::Duration::from_millis(300);

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Optional diagnostic dwell between the last streamed update and Stop.
/// EMPIRICAL FINDING (M2 G1 debugging): adding a dwell does NOT change the
/// order in which the CLIENT surfaces these events — the prompt response
/// resolution consistently beats transport-borne session updates regardless
/// of agent-side pacing (matching the completion-note asymmetry documented in
/// fxcore/driver/acp/mod.rs main_loop, whose turn-stamp retention exists
/// exactly for this). Kept ONLY as a manual-probing knob; DEFAULT 0 so tests
/// stay fast. crash/hang modes ignore it.
const DEFAULT_STOP_DELAY_MS: u64 = 0;

fn stop_delay_from_env() -> Option<std::time::Duration> {
    let raw = std::env::var("FX_FAKE_STOP_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_STOP_DELAY_MS);
    (raw > 0).then_some(std::time::Duration::from_millis(raw))
}

fn script_for(mode: &str) -> Script {
    let text = env_or("FX_FAKE_TEXT", "agent text");
    let wants_perm = mode == "perm" || std::env::var("FX_FAKE_PERM").as_deref() == Ok("1");
    let mut steps: Vec<Step> = Vec::new();
    if wants_perm {
        steps.push(Step::AskPermission(vec![
            acp_sdk::schema::v1::PermissionOption::new(
                "opt-allow",
                "Allow",
                acp_sdk::schema::v1::PermissionOptionKind::AllowOnce,
            ),
            acp_sdk::schema::v1::PermissionOption::new(
                "opt-reject",
                "Reject",
                acp_sdk::schema::v1::PermissionOptionKind::RejectOnce,
            ),
        ]));
    }
    match mode {
        "crash_after_chunk" => {
            // Bounded flush dwell BEFORE Crash — see CRASH_FLUSH_DWELL note.
            steps.push(Step::Chunk(
                fxproto::content::Role::Agent,
                format!("{text}-partial"),
            ));
            steps.push(Step::Pause(CRASH_FLUSH_DWELL));
            steps.push(Step::Crash);
        }
        "hang" => {
            steps.push(Step::Stall);
        }
        // "perm" and unknown modes fall through to the canonical chunks shape
        // (unknown mode names must fail OPEN into a usable agent, matching how
        // tests treat typo'd knobs as default-chunks).
        _ => {
            steps.push(Step::Chunk(fxproto::content::Role::Agent, text.clone()));
            steps.push(Step::ToolCall {
                id: "call_1".to_owned(),
                title: "e2e probe".to_owned(),
            });
            steps.push(Step::ToolCallUpdate {
                id: "call_1".to_owned(),
                status: acp_sdk::schema::v1::ToolCallStatus::Completed,
                output: Some("done".to_owned()),
            });
            if let Some(dwell) = stop_delay_from_env() {
                // Restore true streaming precedence (see DEFAULT_STOP_DELAY_MS):
                // guarantees on-the-wire observers see ToolCallUpsert stages
                // BEFORE TurnFinished(end_turn), deterministically.
                steps.push(Step::Pause(dwell));
            }
        }
    }
    if mode != "crash_after_chunk" && mode != "hang" {
        // Terminal step; script exhaustion would end benignly anyway but be
        // explicit so the reply stop_reason is deterministic end_turn.
        steps.push(Step::Stop(acp_sdk::schema::v1::StopReason::EndTurn));
    }
    Script(steps)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // detect.rs probe fast-path — MUST precede any stdio ACP traffic.
    let argv: Vec<String> = std::env::args().collect();
    if argv.iter().any(|a| a == "--version") {
        println!("fake-agent-stdio {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let mode = env_or("FX_FAKE_MODE", "chunks");
    eprintln!(
        "fake-agent-stdio starting mode={mode} perm={}",
        std::env::var("FX_FAKE_PERM").unwrap_or_else(|_| "-".into())
    );

    // Env hygiene ONLY for our own knobs: engine reads FX_FAKE_* here already,
    // children never spawn again — pass the resolved values down explicitly by
    // NOT relying on ambient state further.
    // Per-instance suffix keeps session ids unique across CONCURRENT fake
    // agents (M2 G3 drives two driver slots at once; verbatim adoption would
    // otherwise collide both on sess-000000). pid+boot-nanos is plenty.
    let unique_suffix = format!(
        "{:x}{:04x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_nanos() & 0xffff) as u16)
            .unwrap_or(0)
    );
    let engine = testing_agent::connect_engine(
        script_for(&mode),
        acp_sdk::Stdio::new(),
        "fake-agent-stdio",
        Some(unique_suffix),
    );

    let () = match engine.join.await {
        Ok(()) => (),
        Err(e) => eprintln!("fake-agent-stdio dispatch panicked: {e}"),
    };
    // Step::Crash over a REAL transport = process death with a REAL code. The
    // client's supervisor ladder reaps 7 from try_wait (post-Ready crash ⇒
    // exactly ONE AgentStatus{Crashed{exit_code:Some(7)}} publication).
    if *engine.crash_watch.borrow() {
        std::process::exit(7);
    }
}
