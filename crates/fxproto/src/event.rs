//! Canonical events — the only way state changes anywhere in the system.
//!
//! These are NORMALIZED: fxcore's driver layer translates raw ACP `session/update`
//! notifications into these shapes (fxcore/src/driver/acp/normalize.rs). The client
//! never sees vendor quirks. Persisted to SQLite and broadcast to clients with a Seq.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::{McpServerSpec, PlanEntry, Role, StopReason, ToolCallKind, ToolCallStatus};
use crate::driver::DriverId;
use crate::ids::{AgentId, OptionId, RequestId, Seq, SessionId, ToolCallId, TurnId};

/// Event + its global order stamp. Semantics pinned here (store + bus rely on them):
/// - `seq` is assigned by EventStore::append at persist time (fxcore/src/store), never
///   by drivers or clients. First event gets 1; strictly increasing, no gaps, no reuse.
/// - 0 is reserved as "nothing yet": head_seq of an empty log and a fresh client cursor.
/// - replay(after) returns events strictly AFTER `after`, ascending; bus fanout happens
///   post-persist, so every subscriber sees strictly increasing seq per subscription
///   (fxcore/src/bus.rs). Clients track the max seq seen as their resume cursor
///   (~/.fxcode/client-state.json) and send it back via Subscribe { last_seq }.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sequenced<T> {
    pub seq: Seq,
    pub inner: T,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FxEvent {
    /// Agent process lifecycle. Carries `driver` so the agents fold can construct a
    /// brand-new AgentState entry (first sight of this agent) without guessing.
    AgentStatus {
        agent: AgentId,
        driver: DriverId,
        status: AgentStatus,
    },

    TurnStarted {
        session: SessionId,
        turn: TurnId,
    },
    /// Streaming text for the transcript. role=User chunks echo what was sent.
    /// See "chunk vs blocks" decision at the bottom of this file for how ContentBlocks
    /// flatten into `text`.
    Chunk {
        session: SessionId,
        turn: TurnId,
        role: Role,
        text: String,
    },
    /// Upsert keyed by tool_call — UI replaces in place, never appends duplicates.
    ToolCallUpsert {
        session: SessionId,
        tool_call: ToolCallId,
        title: String,
        kind: ToolCallKind,
        status: ToolCallStatus,
        output: Option<String>,
        /// vendor extras preserved opaquely. Value is PartialEq but NOT Eq — which is
        /// why FxEvent itself derives only PartialEq-free Debug/Clone/SD.
        _meta: Option<Value>,
    },
    PlanUpdated {
        session: SessionId,
        entries: Vec<PlanEntry>,
    },

    /// Agent asked permission. Server parks the ACP request under `request_id`.
    PermissionRequested {
        request_id: RequestId,
        session: SessionId,
        tool_call: ToolCallSummary,
        options: Vec<PermissionOption>,
    },
    /// Recorded so late-joining clients see the resolution too.
    PermissionResolved {
        request_id: RequestId,
        chosen: Option<OptionId>,
    }, // None = cancelled

    TurnFinished {
        session: SessionId,
        turn: TurnId,
        stop_reason: StopReason,
    },
    /// Emitted when NewSession command succeeds. THE record that a session exists —
    /// without it, replays can't rebuild the agent→sessions list. Carries everything
    /// session/new established (replaces an earlier "McpAttached" idea — one event,
    /// one fact; do NOT reintroduce a separate MCP event).
    SessionCreated {
        session: SessionId,
        agent: AgentId,
        cwd: PathBuf,
        mcp_servers: Vec<McpServerSpec>,
    },
}

/// Process status of one agent. Reused VERBATIM by model::agents::AgentState (status
/// field) and rendered directly by the sidebar status dots — no duplicate view type.
/// Transition rules (asserted in fold tests, see model/agents.rs):
///   Starting → Ready | Crashed ; Ready ⇄ Busy ; any → Stopped (server shutdown).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Ready,
    Busy,
    Crashed { exit_code: Option<i32> },
    Stopped,
}

/// Minimal tool identity attached to a permission ask — enough for the modal header,
/// not the whole card. The full record lives in the thread's tool_calls map.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ToolCallSummary {
    pub tool_call: ToolCallId,
    pub title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PermissionOption {
    pub option_id: OptionId,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}
//     → mirrors ACP option kinds; keep aligned.

// TODO chunk-vs-blocks decision (settled — do not reopen without M1 traffic evidence):
// ACP delivers message chunks as ContentBlock arrays; Command::Prompt accepts arbitrary
// blocks. Encoding rule: consecutive Text blocks flatten into Chunk.text (joined as the
// agent delivered them). Image/non-text blocks ARE forwarded to the agent in Prompt, but
// v0 does NOT echo them into the transcript — normalize drops them from the echo with a
// tracing::debug! line (the composer only sends [Text] today, so nothing real is lost).
// If M1 traffic proves otherwise, extend Chunk with `_meta` — never add a Role variant
// (thought-chunks are likewise deferred entirely: logged, not modeled).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequenced_is_transparent_pair() {
        let s = Sequenced {
            seq: Seq::new(7),
            inner: Role::Agent,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"seq":7,"inner":"agent"}"#);
        let back: Sequenced<Role> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.inner, Role::Agent);
    }

    #[test]
    fn all_nine_variants_use_snake_case_type_tags() {
        // One representative per variant; asserts the internal tag key/values.
        let evs: Vec<FxEvent> = vec![
            FxEvent::AgentStatus {
                agent: AgentId::from_raw("a".into()),
                driver: DriverId::CodexCli,
                status: AgentStatus::Crashed {
                    exit_code: Some(-9),
                },
            },
            FxEvent::TurnStarted {
                session: SessionId::from_raw("s".into()),
                turn: TurnId::from_raw("t".into()),
            },
            FxEvent::Chunk {
                session: SessionId::from_raw("s".into()),
                turn: TurnId::from_raw("t".into()),
                role: Role::User,
                text: "hello".into(),
            },
            FxEvent::ToolCallUpsert {
                session: SessionId::from_raw("s".into()),
                tool_call: ToolCallId::from_raw("tc".into()),
                title: "ls".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Completed,
                output: None,
                _meta: None,
            },
            FxEvent::PlanUpdated {
                session: SessionId::from_raw("s".into()),
                entries: vec![],
            },
            FxEvent::PermissionRequested {
                request_id: RequestId::from_raw("r".into()),
                session: SessionId::from_raw("s".into()),
                tool_call: ToolCallSummary {
                    tool_call: ToolCallId::from_raw("tc".into()),
                    title: "ls".into(),
                },
                options: vec![PermissionOption {
                    option_id: OptionId::from_raw("o".into()),
                    name: "Allow once".into(),
                    kind: PermissionOptionKind::AllowOnce,
                }],
            },
            FxEvent::PermissionResolved {
                request_id: RequestId::from_raw("r".into()),
                chosen: None,
            },
            FxEvent::TurnFinished {
                session: SessionId::from_raw("s".into()),
                turn: TurnId::from_raw("t".into()),
                stop_reason: StopReason::EndTurn,
            },
            FxEvent::SessionCreated {
                session: SessionId::from_raw("s".into()),
                agent: AgentId::from_raw("a".into()),
                cwd: "/tmp".into(),
                mcp_servers: vec![],
            },
        ];
        for ev in &evs {
            let json = serde_json::to_value(ev).unwrap();
            assert!(
                json.get("type").and_then(|t| t.as_str()).is_some(),
                "internally tagged, got: {json}"
            );
            // Every FxEvent must round-trip losslessly.
            let back = serde_json::from_value::<FxEvent>(json.clone()).unwrap();
            let rt = serde_json::to_string(&back).unwrap();
            assert_eq!(rt, serde_json::to_string(ev).unwrap(), "variant {json}");
        }
    }

    #[test]
    fn crashed_status_round_trips_with_exit_code() {
        // AgentStatus rides INSIDE FxEvent's struct fields, so it uses serde's
        // default EXTERNAL tagging: {"crashed": {...}} / bare "ready" for units.
        let json = r#"{"crashed":{"exit_code":-9}}"#;
        let st: AgentStatus = serde_json::from_str(json).unwrap();
        assert_eq!(
            st,
            AgentStatus::Crashed {
                exit_code: Some(-9)
            }
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Ready).unwrap(),
            "\"ready\""
        );
    }
}
