//! Serde golden tests — impl.md step 0.7.
//!
//! Fixture rule from ids.rs/content.rs docs: goldens must contain BARE strings/u64
//! (never newtype wrappers) and every enum must land on its pinned snake_case tag.
//! Two directions are asserted per fixture:
//!   1. `serde_json::to_string(&T)` == exact pinned byte string  (wire shape)
//!   2. `serde_json::from_str::<T>(pinned)` == API-built value    (lossless in)
//!
//! These run through the crate's PUBLIC surface (integration test), so this doubles
//! as a compatibility canary for what fxcore/fxapp consume.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fxproto::command::Command;
use fxproto::content::{
    ContentBlock, McpServerSpec, PlanEntry, PlanEntryStatus, PlanPriority, Role, StopReason,
    ToolCallKind, ToolCallStatus,
};
use fxproto::driver::{DriverId, DriverSpec};
use fxproto::envelope::{Message, PROTO_VERSION};
use fxproto::event::{
    AgentStatus, FxEvent, PermissionOption, PermissionOptionKind, Sequenced, ToolCallSummary,
};
use fxproto::ids::{
    AgentId, OptionId, RequestId, Seq, SessionId, ToolCallId, TurnId,
};
use fxproto::model::{PermsState, ThreadsState};
use fxproto::reply::{DetectedDriver, FxError, FxErrorCode, Reply};

fn rt<T: serde::Serialize + serde::de::DeserializeOwned>(value: T, pinned: &str) {
    let out = serde_json::to_string(&value).expect("serialize");
    assert_eq!(out, pinned, "wire bytes drifted");
    // Lossless "in" direction: parsing the fixture back must land on the same wire
    // bytes again (structural equality via canonical serialization, no PartialEq
    // needed on types whose blueprint deliberately skips those derives).
    let back: T = serde_json::from_str(pinned).expect("deserialize fixture");
    let out2 = serde_json::to_string(&back).expect("re-serialize");
    assert_eq!(out2, pinned, "fixture did not survive the trip");
}

// ---------- Seq + id newtypes ----------

#[test]
fn seq_is_a_bare_u64() {
    rt(Seq::new(42), "42");
}

#[test]
fn ids_are_bare_strings() {
    rt(AgentId::from_raw("a".into()), "\"a\"");
    rt(SessionId::from_raw("s".into()), "\"s\"");
    rt(TurnId::from_raw("t".into()), "\"t\"");
    rt(ToolCallId::from_raw("tc".into()), "\"tc\"");
    rt(RequestId::from_raw("r".into()), "\"r\"");
    rt(OptionId::from_raw("o".into()), "\"o\"");
}

// ---------- content.rs ----------

#[test]
fn content_blocks() {
    rt(
        ContentBlock::Text { text: "hello world".into() },
        r#"{"type":"text","text":"hello world"}"#,
    );
    rt(
        ContentBlock::Image { media_type: "image/png".into(), data: "iVBORw==".into() },
        r#"{"type":"image","media_type":"image/png","data":"iVBORw=="}"#,
    );
}

#[test]
fn stop_reasons_and_roles() {
    rt(Role::User, "\"user\"");
    rt(Role::Agent, "\"agent\"");
    rt(StopReason::EndTurn, "\"end_turn\"");
    rt(StopReason::MaxTokens, "\"max_tokens\"");
    rt(StopReason::MaxTurnRequests, "\"max_turn_requests\"");
    rt(StopReason::Refusal, "\"refusal\"");
    rt(StopReason::Cancelled, "\"cancelled\"");
}

#[test]
fn mcp_server_spec_env_ordering_is_byte_stable() {
    let mut env = BTreeMap::new();
    env.insert("A".to_string(), "1".to_string());
    env.insert("Z".to_string(), "26".to_string());
    let spec = McpServerSpec {
        name: "fs".into(),
        command: "mcp-fs".into(),
        args: vec!["--root".into()],
        env,
    };
    rt(
        spec,
        r#"{"name":"fs","command":"mcp-fs","args":["--root"],"env":{"A":"1","Z":"26"}}"#,
    );
}

#[test]
fn plan_entries_round_trip_with_optional_priority() {
    rt(
        PlanEntry { content: "write fold".into(), status: PlanEntryStatus::InProgress, priority: Some(PlanPriority::High) },
        r#"{"content":"write fold","status":"in_progress","priority":"high"}"#,
    );
    rt(
        PlanEntry { content: "shrink".into(), status: PlanEntryStatus::Pending, priority: None },
        r#"{"content":"shrink","status":"pending","priority":null}"#,
    );
}

#[test]
fn tool_call_kinds_and_statuses() {
    rt(ToolCallKind::Fetch, "\"fetch\"");
    rt(ToolCallKind::Other, "\"other\"");
    rt(ToolCallStatus::Completed, "\"completed\"");
    rt(ToolCallStatus::InProgress, "\"in_progress\"");
}

// ---------- driver.rs ----------

#[test]
fn driver_ids_and_specs() {
    rt(DriverId::ClaudeCode, "\"claude_code\"");
    rt(DriverId::GeminiCli, "\"gemini_cli\"");
    rt(DriverId::CodexCli, "\"codex_cli\"");
    // A config-style TOML/JSON spec without args/env deserializes (serde defaults).
    let spec: DriverSpec =
        serde_json::from_str(r#"{"program":"/usr/local/bin/codex-acp"}"#).unwrap();
    assert_eq!(spec.program, "/usr/local/bin/codex-acp");
    assert!(spec.args.is_empty() && spec.env.is_empty());
}

// ---------- event.rs — all nine variants ----------

#[test]
fn golden_agent_status_event() {
    rt(
        FxEvent::AgentStatus {
            agent: AgentId::from_raw("a".into()),
            driver: DriverId::ClaudeCode,
            status: AgentStatus::Starting,
        },
        r#"{"type":"agent_status","agent":"a","driver":"claude_code","status":"starting"}"#,
    );
    rt(
        FxEvent::AgentStatus {
            agent: AgentId::from_raw("a".into()),
            driver: DriverId::GeminiCli,
            status: AgentStatus::Crashed { exit_code: Some(-9) },
        },
        r#"{"type":"agent_status","agent":"a","driver":"gemini_cli","status":{"crashed":{"exit_code":-9}}}"#,
    );
    rt(
        FxEvent::AgentStatus {
            agent: AgentId::from_raw("a".into()),
            driver: DriverId::CodexCli,
            status: AgentStatus::Crashed { exit_code: None },
        },
        r#"{"type":"agent_status","agent":"a","driver":"codex_cli","status":{"crashed":{"exit_code":null}}}"#,
    );
}

#[test]
fn golden_turn_lifecycle_events() {
    rt(
        FxEvent::TurnStarted { session: SessionId::from_raw("s".into()), turn: TurnId::from_raw("t1".into()) },
        r#"{"type":"turn_started","session":"s","turn":"t1"}"#,
    );
    rt(
        FxEvent::Chunk {
            session: SessionId::from_raw("s".into()),
            turn: TurnId::from_raw("t1".into()),
            role: Role::Agent,
            text: "streamed text".into(),
        },
        r#"{"type":"chunk","session":"s","turn":"t1","role":"agent","text":"streamed text"}"#,
    );
    rt(
        FxEvent::TurnFinished {
            session: SessionId::from_raw("s".into()),
            turn: TurnId::from_raw("t1".into()),
            stop_reason: StopReason::EndTurn,
        },
        r#"{"type":"turn_finished","session":"s","turn":"t1","stop_reason":"end_turn"}"#,
    );
}

#[test]
fn golden_tool_call_upsert_preserves_meta_opaquely() {
    rt(
        FxEvent::ToolCallUpsert {
            session: SessionId::from_raw("s".into()),
            tool_call: ToolCallId::from_raw("tc1".into()),
            title: "ls -la".into(),
            kind: ToolCallKind::Execute,
            status: ToolCallStatus::InProgress,
            output: None,
            _meta: Some(serde_json::json!({"vendor_key": [1, 2, {"deep": true}]})),
        },
        r#"{"type":"tool_call_upsert","session":"s","tool_call":"tc1","title":"ls -la","kind":"execute","status":"in_progress","output":null,"_meta":{"vendor_key":[1,2,{"deep":true}]}}"#,
    );
}

#[test]
fn golden_plan_updated_replaces_wholesale() {
    rt(
        FxEvent::PlanUpdated {
            session: SessionId::from_raw("s".into()),
            entries: vec![
                PlanEntry { content: "one".into(), status: PlanEntryStatus::Completed, priority: None },
                PlanEntry { content: "two".into(), status: PlanEntryStatus::Pending, priority: Some(PlanPriority::Low) },
            ],
        },
        r#"{"type":"plan_updated","session":"s","entries":[{"content":"one","status":"completed","priority":null},{"content":"two","status":"pending","priority":"low"}]}"#,
    );
}

#[test]
fn golden_permission_events() {
    rt(
        FxEvent::PermissionRequested {
            request_id: RequestId::from_raw("r1".into()),
            session: SessionId::from_raw("s".into()),
            tool_call: ToolCallSummary { tool_call: ToolCallId::from_raw("tc2".into()), title: "rm -rf /tmp/x".into() },
            options: vec![
                PermissionOption {
                    option_id: OptionId::from_raw("opt_allow".into()),
                    name: "Allow once".into(),
                    kind: PermissionOptionKind::AllowOnce,
                },
                PermissionOption {
                    option_id: OptionId::from_raw("opt_reject_always".into()),
                    name: "Reject always".into(),
                    kind: PermissionOptionKind::RejectAlways,
                },
            ],
        },
        concat!(
            r#"{"type":"permission_requested","request_id":"r1","session":"s","tool_call":{"tool_call":"tc2","title":"rm -rf /tmp/x"},"options":["#,
            r#"{"option_id":"opt_allow","name":"Allow once","kind":"allow_once"},"#,
            r#"{"option_id":"opt_reject_always","name":"Reject always","kind":"reject_always"}]}"#,
        ),
    );
    rt(
        FxEvent::PermissionResolved {
            request_id: RequestId::from_raw("r1".into()),
            chosen: Some(OptionId::from_raw("opt_allow".into())),
        },
        r#"{"type":"permission_resolved","request_id":"r1","chosen":"opt_allow"}"#,
    );
    // chosen: null == cancelled sweep — audit fact, must not be lost.
    rt(
        FxEvent::PermissionResolved { request_id: RequestId::from_raw("r2".into()), chosen: None },
        r#"{"type":"permission_resolved","request_id":"r2","chosen":null}"#,
    );
}

#[test]
fn golden_session_created() {
    rt(
        FxEvent::SessionCreated {
            session: SessionId::from_raw("s7".into()),
            agent: AgentId::from_raw("a3".into()),
            cwd: PathBuf::from("/home/dev/proj"),
            mcp_servers: vec![McpServerSpec {
                name: "git".into(),
                command: "mcp-git".into(),
                args: vec![],
                env: BTreeMap::new(),
            }],
        },
        concat!(
            r#"{"type":"session_created","session":"s7","agent":"a3","cwd":"/home/dev/proj","#,
            r#""mcp_servers":[{"name":"git","command":"mcp-git","args":[],"env":{}}]}"#,
        ),
    );
}

// ---------- command.rs — every variant ----------

#[test]
fn golden_commands() {
    rt(Command::DetectAgents, r#"{"type":"detect_agents"}"#);
    rt(
        Command::StartAgent { driver: DriverId::CodexCli },
        r#"{"type":"start_agent","driver":"codex_cli"}"#,
    );
    rt(
        Command::NewSession {
            agent: AgentId::from_raw("a".into()),
            cwd: PathBuf::from("/work"),
            mcp_servers: vec![],
        },
        r#"{"type":"new_session","agent":"a","cwd":"/work","mcp_servers":[]}"#,
    );
    rt(
        Command::Prompt {
            session: SessionId::from_raw("s".into()),
            blocks: vec![ContentBlock::Text { text: "ship it".into() }],
        },
        r#"{"type":"prompt","session":"s","blocks":[{"type":"text","text":"ship it"}]}"#,
    );
    rt(
        Command::Cancel { session: SessionId::from_raw("s".into()) },
        r#"{"type":"cancel","session":"s"}"#,
    );
    rt(
        Command::PermissionResponse {
            request_id: RequestId::from_raw("r1".into()),
            option_id: OptionId::from_raw("opt_allow".into()),
        },
        r#"{"type":"permission_response","request_id":"r1","option_id":"opt_allow"}"#,
    );
}

// ---------- reply.rs — every variant ----------

#[test]
fn golden_replies() {
    rt(
        Reply::DetectedAgents { drivers: vec![
            DetectedDriver {
                driver: DriverId::ClaudeCode,
                found: true,
                version: Some("4.5".into()),
                spec_used: DriverId::ClaudeCode.default_spec(),
            },
            DetectedDriver {
                driver: DriverId::CodexCli,
                found: false,
                version: None,
                spec_used: DriverId::CodexCli.default_spec(),
            },
        ] },
        concat!(
            r#"{"type":"detected_agents","drivers":[{"driver":"claude_code","found":true,"version":"4.5","#,
            r#""spec_used":{"program":"npx","args":["-y","@agentclientprotocol/claude-agent-acp"],"env":{}}},"#,
            r#"{"driver":"codex_cli","found":false,"version":null,"spec_used":{"program":"codex-acp","args":[],"env":{}}}]}"#,
        ),
    );
    rt(Reply::Started { agent: AgentId::from_raw("a".into()) }, r#"{"type":"started","agent":"a"}"#);
    rt(
        Reply::SessionCreated { session: SessionId::from_raw("s".into()) },
        // Same snake_case tag as FxEvent::SessionCreated — unambiguous because it
        // only ever rides inside a Response frame.
        r#"{"type":"session_created","session":"s"}"#,
    );
    rt(
        Reply::PromptAccepted { turn: TurnId::from_raw("t".into()) },
        r#"{"type":"prompt_accepted","turn":"t"}"#,
    );
    rt(Reply::Cancelled, r#"{"type":"cancelled"}"#);
    rt(Reply::PermissionRecorded, r#"{"type":"permission_recorded"}"#);
    rt(
        Reply::Error(FxError { code: FxErrorCode::TurnNotActive, message: "no active turn".into() }),
        r#"{"type":"error","code":"turn_not_active","message":"no active turn"}"#,
    );
}

// ---------- envelope.rs ----------

#[test]
fn golden_handshake_frames() {
    rt(
        Message::Hello { proto_version: PROTO_VERSION, token: "hex-token".into() },
        r#"{"type":"hello","proto_version":1,"token":"hex-token"}"#,
    );
    rt(
        Message::Welcome { server_version: "fxcode 0.1.0".into(), head_seq: Seq::new(17) },
        r#"{"type":"welcome","server_version":"fxcode 0.1.0","head_seq":17}"#,
    );
    rt(Message::Subscribe { last_seq: Seq::new(0) }, r#"{"type":"subscribe","last_seq":0}"#);
}

#[test]
fn golden_request_response_correlation() {
    rt(
        Message::Request { id: 7, command: Command::DetectAgents },
        r#"{"type":"request","id":7,"command":{"type":"detect_agents"}}"#,
    );
    rt(
        Message::Response {
            id: 7,
            reply: Reply::PromptAccepted { turn: TurnId::from_raw("t".into()) },
        },
        r#"{"type":"response","id":7,"reply":{"type":"prompt_accepted","turn":"t"}}"#,
    );
}

#[test]
fn golden_event_frame_wraps_sequenced_inner() {
    rt(
        Message::Event {
            event: Sequenced {
                seq: Seq::new(1),
                inner: FxEvent::TurnStarted {
                    session: SessionId::from_raw("s".into()),
                    turn: TurnId::from_raw("t".into()),
                },
            },
        },
        r#"{"type":"event","event":{"seq":1,"inner":{"type":"turn_started","session":"s","turn":"t"}}}"#,
    );
}

#[test]
fn golden_snapshot_serializes_byte_stably_and_round_trips() {
    let mut threads = ThreadsState::default();
    threads.threads.entry(SessionId::from_raw("s2".into())).or_default();
    let snap = fxproto::envelope::Snapshot {
        baseline_seq: Seq::new(30),
        agents: Default::default(),
        threads: threads.clone(),
        perms: PermsState::default(),
    };

    let wire = serde_json::to_string(&snap).unwrap();

    // Determinism: BTreeMap-ordered states give identical bytes across instances.
    let snap2 = fxproto::envelope::Snapshot {
        baseline_seq: snap.baseline_seq,
        agents: Default::default(),
        threads: threads.clone(),
        perms: PermsState::default(),
    };
    assert_eq!(wire, serde_json::to_string(&snap2).unwrap());

    // The thread with the pinned key must appear in key order inside the frame.
    assert!(
        wire.contains(r#""threads":{"s2":{"cwd":"","mcp_servers":[],"messages":[],"tool_calls":{},"flow":[],"plan":[],"active_turn":null,"pending_perm_tools":{}}}}"#),
        "{wire}"
    );

    // Round trip: all three states arrive intact.
    let back: fxproto::envelope::Snapshot = serde_json::from_str(&wire).unwrap();
    assert_eq!(back.baseline_seq, snap.baseline_seq);
    assert_eq!(back.agents, snap.agents);
    assert_eq!(back.threads, threads);
    assert_eq!(back.perms, snap.perms);
}
