//! Shared scripted-agent engine — ONE source for two consumers:
//!   1. `examples/fake_agent_stdio.rs` — REAL stdio ACP agent binary (G1).
//!   2. `tests/fake_agent.rs` + `tests/orchestrator.rs` — in-process duplex
//!      harness (moved verbatim out of tests/fake_agent.rs during M2 G1 so the
//!      example cannot drift from the tests: same Script/Step/run_steps core,
//!      same wire framing discipline against SDK 1.3.0).
//!
//! Included via `#[path = "../examples/support/testing_agent.rs"] mod
//! testing_agent;` in both consumers (pre-2024-rustc examples can't share
//! modules any other way; a `src/` pub-module route was rejected because lib
//! sources are off-limits to test-support additions).
//!
//! See tests/fake_agent.rs header for the transport/handler-model contract:
//! every step runs inline inside typed PromptRequest handlers; block_task()
//! stays ILLEGAL inside callbacks; the agent main_fn parks until Step::Crash
//! flips the watch or transports reach EOF.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use agent_client_protocol as acp_sdk;
use agent_client_protocol::schema::v1 as s;

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
    /// Plain await dwell (NOT a protocol event). Origin note: added when the
    /// stdio binary needed bounded time for the SDK writer to flush buffered
    /// notification lines BEFORE exiting the process on Crash — duplex harness
    /// never needs it. Handlers may await freely; block_task() is the only
    /// illegal call there.
    Pause(std::time::Duration),
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

/// Handle returned by [`connect_engine`] for the party that owns the OTHER
/// side of the transport:
/// - observed_rx/session_ids_rx mirror tests/fake_agent.rs's Harness contract;
/// - crash_watch ends true when a Step::Crash fired (the stdio binary maps
///   that witness onto process exit(7));
/// - join resolves once the SDK dispatch loop fully unwound.
pub struct Engine {
    pub observed_rx: tokio::sync::mpsc::UnboundedReceiver<ObservedRequest>,
    pub session_ids_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    pub crash_watch: tokio::sync::watch::Receiver<bool>,
    pub join: tokio::task::JoinHandle<()>,
}

struct Core {
    script: std::sync::Mutex<Vec<Step>>,
    counter: AtomicU64,
    observed_tx: tokio::sync::mpsc::UnboundedSender<ObservedRequest>,
    sessions_tx: tokio::sync::mpsc::UnboundedSender<String>,
    crash_tx: tokio::sync::watch::Sender<bool>,
    /// When Some, minted session ids get a PER-INSTANCE unique suffix so
    /// several fake-agent processes running concurrently (M2 G3: two drivers)
    /// cannot collide on verbatim-adopted ids. Tests keep the exact
    /// "sess-00000N" contract via None.
    unique_session_suffix: Option<String>,
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
            Step::Pause(dur) => {
                tokio::time::sleep(dur).await;
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

// ── Compat re-export aliases ─────────────────────────────────────────────────

/// Local shims (origin: tests/fake_agent.rs compat section) so both consumers
/// read symmetrically about both directions.
pub fn compat_wrap<T: tokio::io::AsyncWrite + Unpin>(t: T) -> fxcore::driver::acp::CompatWrite<T> {
    fxcore::driver::acp::compat_write(Some(t)).unwrap()
}

pub fn compat_read<T: tokio::io::AsyncRead + Unpin>(t: T) -> fxcore::driver::acp::CompatRead<T> {
    fxcore::driver::acp::compat_read(Some(t)).unwrap()
}

// ── Wiring ───────────────────────────────────────────────────────────────────

/// Bind the scripted agent onto ANY ConnectTo<Client> transport and spawn its
/// dispatch loop. Duplex harness passes ByteStreams over duplex halves; the
/// stdio binary passes `acp_sdk::Stdio::new()`. Returns observation channels +
/// crash watch + join immediately (spawned internally — callers must already
/// sit inside a tokio runtime; the SDK spawns ITS tasks off this one too).
/// Bind the scripted agent onto ANY ConnectTo<Client> transport and spawn its
/// dispatch loop. Duplex harness passes ByteStreams over duplex halves; the
/// stdio binary passes `acp_sdk::Stdio::new()`. Returns observation channels +
/// crash watch + join immediately (spawned internally — callers must already
/// sit inside a tokio runtime; the SDK spawns ITS tasks off this one too).
///
/// `unique_session_suffix`: Some(tag) makes minted session ids
/// "sess-{n:06}-{tag}" — REQUIRED when multiple engine instances serve one
/// orchestrator (verbatim adoption would otherwise collide on sess-000000);
/// tests pass None for their exact-id assertions.
pub fn connect_engine(
    script: Script,
    transport: impl acp_sdk::ConnectTo<acp_sdk::Agent> + 'static,
    name: &'static str,
    unique_session_suffix: Option<String>,
) -> Engine {
    let (observed_tx, observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (sessions_tx, session_ids_rx) = tokio::sync::mpsc::unbounded_channel();
    let (crash_tx, crash_watch) = tokio::sync::watch::channel(false);

    let counter_for_prompt = Arc::new(AtomicU64::new(0));

    let crash_watch_clone = crash_watch.clone();
    let join = tokio::spawn(async move {
        let steps_cell = Arc::new(std::sync::Mutex::new(script.materialize()));
        let core = Arc::new(Core {
            script: std::sync::Mutex::new(Vec::new()),
            counter: AtomicU64::new(0),
            observed_tx,
            sessions_tx,
            crash_tx: crash_tx.clone(),
            unique_session_suffix: unique_session_suffix.clone(),
        });
        let core_new = Arc::clone(&core);
        let core_prompt = Arc::clone(&core);
        let core_cancel = Arc::clone(&core);
        let result = acp_sdk::Agent
            .builder()
            .name(name)
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
                    let mut id = format!("sess-{n:06}");
                    if let Some(tag) = &core.unique_session_suffix {
                        id.push('-');
                        id.push_str(tag);
                    }
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
                transport,
                move |cx: acp_sdk::ConnectionTo<acp_sdk::Client>| async move {
                    let mut crash = crash_watch_clone;
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
            eprintln!("{name} ended: {e:?}");
        }
    });

    Engine {
        observed_rx,
        session_ids_rx,
        crash_watch,
        join,
    }
}
