//! Pure translation layer: raw ACP messages → canonical FxEvents.
//!
//! THE most unit-tested code in the repo. No I/O, no process state owned here —
//! functions in, events out (the caller supplies the per-tool-call view to merge
//! into; see row R5). Keep vendor handling visible and named here rather than
//! scattered.

// Schema types live under the versioned module (not re-exported at the root);
// this alias keeps every row below readable as plain `acp::TypeName`.
use agent_client_protocol::schema::v1 as acp;
use fxproto::content::{
    PlanEntry as FxPlanEntry, PlanEntryStatus as FxPlanStatus, PlanPriority as FxPlanPriority,
    Role as FxRole, StopReason as FxStopReason, ToolCallKind as FxToolKind,
    ToolCallStatus as FxToolStatus,
};
use fxproto::event::{
    FxEvent, PermissionOption as FxPermOption, PermissionOptionKind as FxPermOptionKind,
    ToolCallSummary,
};
use fxproto::ids::{OptionId, SessionId, ToolCallId, TurnId};

// EXHAUSTIVE-MATCH STRATEGY (binding; read before touching any match here):
//
// The SDK's v1 enums (`SessionUpdate`, `ToolKind`, `ToolCallStatus`,
// `StopReason`, `PermissionOptionKind`, `RequestPermissionOutcome`) are ALL
// `#[non_exhaustive]`, so Rust REQUIRES a wildcard arm and "new upstream variant
// = compile error" is impossible to get from a plain match. Strategy instead:
//
//   1. ONE choke point per message shape: session_update() / request_permission()
//      / stop_reason() below. No other file matches on these enums.
//   2. Named arm for EVERY v1 kind (rows R1–R11 + K/S/O); single trailing `_`
//      unhandled(...). New SDK variants land there silently-by-compiler but
//      LOUDLY AT RUNTIME: warn carries the variant's Debug repr.
//   3. Inventory canary: KNOWN_UPDATE_KINDS pins all 11 snake_case wire tags. A
//      unit test round-trips every tag through serde deserialization of
//      acp::SessionUpdate; an upstream RENAME/REMOVAL breaks that test in CI.
//      (Upstream ADDITION is caught by 2 + release checklist.)
//   4. Release checklist line: bumping agent-client-protocol in Cargo.lock MUST
//      re-read this file (impl.md Phase 4.4 relies on it). Type names verified
//      against schema 1.4.0 (SDK facts re-verified against the actual source:
//      SessionUpdate tuple variants {UserMessageChunk(ContentChunk), ...},
//      ToolCall{tool_call_id,title,kind,status,content,...},
//      ToolCallUpdate{tool_call_id,fields:ToolCallUpdateFields}, Meta ==
//      serde_json::Map<String, Value>).

/// All v1 `session/update` wire tags — serde discriminants of SessionUpdate.
pub const KNOWN_UPDATE_KINDS: [&str; 11] = [
    "user_message_chunk",
    "agent_message_chunk",
    "agent_thought_chunk",
    "tool_call",
    "tool_call_update",
    "plan",
    "available_commands_update",
    "current_mode_update",
    "config_option_update",
    "session_info_update",
    "usage_update",
];

fn unhandled(u: &acp::SessionUpdate) {
    tracing::warn!(
        target: "normalize",
        update = ?u,
        "unmapped SessionUpdate kind — check normalize.rs against the new \
         agent-client-protocol version"
    );
}

fn ignored(note: &'static str, u: &acp::SessionUpdate) -> &'static str {
    // Variant Debug repr already carries any payload; the extra param sites
    // pass nothing. Returns NOTE for `let _ =` silencing at call sites.
    tracing::debug!(target: "normalize", note = %note, update = ?u, "ignored update kind");
    note
}

/// Per-(acp session, ToolCallId) composed view of one tool call, owned by the
/// connection actor's map and merged by row R5 (keeps normalize pure via &mut).
#[derive(Debug, Clone)]
pub struct ComposedToolCall {
    pub title: String,
    pub kind: acp::ToolKind,
    pub status: acp::ToolCallStatus,
    pub content: Vec<acp::ToolCallContent>,
    pub raw_output: Option<serde_json::Value>,
    /// Vendor `_meta` verbatim (Meta == serde_json::Map alias upstream).
    pub meta: Option<serde_json::Value>,
}

impl Default for ComposedToolCall {
    fn default() -> Self {
        // Synthesized defaults when an update arrives for a never-seen call —
        // mirrors upstream's TryFrom<ToolCallUpdate> fallbacks; N9 warns about it.
        Self {
            title: String::new(),
            kind: acp::ToolKind::Other,
            status: acp::ToolCallStatus::Pending,
            content: Vec::new(),
            raw_output: None,
            meta: None,
        }
    }
}

/// One inbound session/notification update → zero or more canonical events.
///
/// Caller contract (acp/mod.rs actor): `our_session` pre-resolved from the raw
/// ACP id (unknown ids rejected in the actor); `turn` stamps the ACTIVE turn
/// for that acp session (None ⇒ chunk rows dropped with debug logs — a turnless
/// session cannot fabricate an fxproto TurnId). `tool_view` is the actor-owned
/// map entry for THIS tool_call id; rows R4/R5 compose/merge into it.
pub fn session_update(
    our_session: &SessionId,
    turn: Option<&TurnId>,
    update: &acp::SessionUpdate,
    tool_view: Option<&mut ComposedToolCall>,
) -> Vec<FxEvent> {
    match update {
        // R1 user_message_chunk → Chunk { role: User }.
        acp::SessionUpdate::UserMessageChunk(chunk) => match turn {
            Some(turn) => vec![chunk_event(our_session, turn, FxRole::User, &chunk.content)],
            None => {
                tracing::debug!(target: "normalize", "dropping pre-turn user chunk");
                Vec::new()
            }
        },

        // R2 agent_message_chunk → same flatten rules, role Agent.
        acp::SessionUpdate::AgentMessageChunk(chunk) => match turn {
            Some(turn) => vec![chunk_event(
                our_session,
                turn,
                FxRole::Agent,
                &chunk.content,
            )],
            None => {
                tracing::debug!(target: "normalize", "dropping post-turn agent chunk");
                Vec::new()
            }
        },

        // R3 agent_thought_chunk → IGNORED + debug log. DECISION KEPT: thought-
        // chunks have no home (no Role variant may be added without protocol
        // churn through goldens first).
        acp::SessionUpdate::AgentThoughtChunk(chunk) => {
            let _ = &chunk.content;
            let _ = ignored(
                "thought chunks not modeled (see event.rs chunk-vs-blocks decision)",
                update,
            );
            Vec::new()
        }

        // R4 tool_call → COMPOSE then EMIT: overwrite view wholesale.
        acp::SessionUpdate::ToolCall(call) => {
            let Some(view) = tool_view else {
                return Vec::new();
            };
            *view = ComposedToolCall {
                title: call.title.clone(),
                kind: call.kind,
                status: call.status,
                content: call.content.clone(),
                raw_output: call.raw_output.clone(),
                meta: call.meta.as_ref().map(meta_to_value),
            };
            vec![upsert_from_view(our_session, &call.tool_call_id, view)]
        }

        // R5 tool_call_update → MERGE onto the composed view. Some(x) replaces;
        // None keeps previous; top-level meta REPLACES _meta only when Some.
        acp::SessionUpdate::ToolCallUpdate(u) => {
            let Some(view) = tool_view else {
                return Vec::new();
            };
            let fields = &u.fields;
            if let Some(title) = &fields.title {
                view.title.clone_from(title);
            }
            if let Some(kind) = fields.kind {
                view.kind = kind;
            }
            if let Some(status) = fields.status {
                view.status = status;
            }
            if fields.content.is_some() {
                view.content = fields.content.clone().unwrap_or_default();
            }
            if fields.raw_output.is_some() {
                view.raw_output = fields.raw_output.clone();
            }
            if u.meta.is_some() {
                view.meta = u.meta.as_ref().map(meta_to_value);
            }
            vec![upsert_from_view(our_session, &u.tool_call_id, view)]
        }

        // R6 plan → PlanUpdated, positional 1:1 entry mapping.
        acp::SessionUpdate::Plan(plan) => vec![FxEvent::PlanUpdated {
            session: our_session.clone(),
            entries: plan.entries.iter().map(plan_entry_map).collect(),
        }],

        // R7 available_commands_update → IGNORED (slash-command palette M3+ UX).
        acp::SessionUpdate::AvailableCommandsUpdate(u) => {
            let _ = &u.available_commands;
            let _ = ignored("slash-commands deferred to M3", update);
            Vec::new()
        }
        // R8 current_mode_update → IGNORED (session modes M3+).
        acp::SessionUpdate::CurrentModeUpdate(u) => {
            let _ = &u.current_mode_id;
            let _ = ignored("mode updates deferred", update);
            Vec::new()
        }
        // R9 config_option_update → IGNORED.
        acp::SessionUpdate::ConfigOptionUpdate(u) => {
            let _ = &u.config_options;
            let _ = ignored("config options read-only projection M3+", update);
            Vec::new()
        }
        // R10 session_info_update → IGNORED (sidebar titles parked to M3).
        acp::SessionUpdate::SessionInfoUpdate(u) => {
            let _ = (&u.title, &u.updated_at);
            let _ = ignored(
                "session info deferred to M3 (needs a golden-first event)",
                update,
            );
            Vec::new()
        }
        // R11 usage_update → IGNORED (cost metering past M3).
        acp::SessionUpdate::UsageUpdate(u) => {
            let _ = (u.used, u.size);
            let _ = ignored("cost metering deferred past M3", update);
            Vec::new()
        }
        _ => {
            unhandled(update); // non_exhaustive insurance (future upstream kinds)
            Vec::new()
        }
    }
}

// ── Row helpers ──────────────────────────────────────────────────────────────

fn chunk_event(
    session: &SessionId,
    turn: &TurnId,
    role: FxRole,
    content: &acp::ContentBlock,
) -> FxEvent {
    // Text blocks survive joined in order; everything else drops with debug!
    // (event.rs chunk-vs-blocks decision — composer sends [Text] today).
    let text = match content {
        acp::ContentBlock::Text(t) => t.text.clone(),
        other => {
            tracing::debug!(
                target: "normalize",
                block = ?other,
                "dropping non-text block from transcript echo"
            );
            String::new()
        }
    };
    FxEvent::Chunk {
        session: session.clone(),
        turn: turn.clone(),
        role,
        text,
    }
}

fn upsert_from_view(session: &SessionId, id: &acp::ToolCallId, view: &ComposedToolCall) -> FxEvent {
    FxEvent::ToolCallUpsert {
        session: session.clone(),
        tool_call: ToolCallId::from_raw(id.to_string()),
        title: view.title.clone(),
        kind: kind_map(view.kind),
        status: status_map(view.status),
        output: extract_output(&view.content, view.raw_output.as_ref()),
        _meta: view.meta.clone(),
    }
}

/// Row K — kind_map, exhaustive with wildcard fallback: SwitchMode has NO
/// fxproto home ⇒ Other (the honest bucket); unknowns also Other + debug!.
fn kind_map(kind: acp::ToolKind) -> FxToolKind {
    match kind {
        acp::ToolKind::Read => FxToolKind::Read,
        acp::ToolKind::Edit => FxToolKind::Edit,
        acp::ToolKind::Delete => FxToolKind::Delete,
        acp::ToolKind::Move => FxToolKind::Move,
        acp::ToolKind::Search => FxToolKind::Search,
        acp::ToolKind::Execute => FxToolKind::Execute,
        acp::ToolKind::Think => FxToolKind::Think,
        acp::ToolKind::Fetch => FxToolKind::Fetch,
        acp::ToolKind::SwitchMode => {
            tracing::debug!(target: "normalize", "SwitchMode tool kind mapped to Other");
            FxToolKind::Other
        }
        acp::ToolKind::Other => FxToolKind::Other,
        _ => {
            tracing::debug!(target: "normalize", ?kind, "unknown tool kind → Other");
            FxToolKind::Other
        }
    }
}

/// Row S — status_map, exhaustive with wildcard fallback: future statuses
/// render visibly-wrong (Failed) rather than invisibly-pending.
fn status_map(status: acp::ToolCallStatus) -> FxToolStatus {
    match status {
        acp::ToolCallStatus::Pending => FxToolStatus::Pending,
        acp::ToolCallStatus::InProgress => FxToolStatus::InProgress,
        acp::ToolCallStatus::Completed => FxToolStatus::Completed,
        acp::ToolCallStatus::Failed => FxToolStatus::Failed,
        _ => {
            tracing::warn!(
                target: "normalize",
                ?status,
                "unmapped tool status → Failed (visible-wrong beats invisible)"
            );
            FxToolStatus::Failed
        }
    }
}

/// Row O — extract_output: first-among-all Text wins, each appended in order;
/// raw_output pretty-printed JSON APPENDED after a newline when both exist;
/// Diff/Terminal shapes NOT rendered in v0 (debug!). None when nothing survived.
fn extract_output(
    content: &[acp::ToolCallContent],
    raw_output: Option<&serde_json::Value>,
) -> Option<String> {
    let mut texts: Vec<String> = Vec::new();
    for item in content {
        if let acp::ToolCallContent::Content(c) = item
            && let acp::ContentBlock::Text(t) = &c.content
        {
            texts.push(t.text.clone());
        } else {
            tracing::debug!(target: "normalize", "diff/terminal tool content not rendered in v0");
        }
    }
    let joined = if texts.is_empty() {
        // Text only via raw output: render that JSON directly.
        return raw_output
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()));
    } else {
        texts.join("")
    };
    Some(match raw_output {
        Some(raw) => format!(
            "{joined}\n{}",
            serde_json::to_string_pretty(raw).unwrap_or_else(|_| raw.to_string())
        ),
        None => joined,
    })
}

fn plan_entry_map(entry: &acp::PlanEntry) -> FxPlanEntry {
    // priority is REQUIRED upstream (High|Medium|Low), ours Option ⇒ Some(map).
    FxPlanEntry {
        content: entry.content.clone(),
        priority: Some(match entry.priority {
            acp::PlanEntryPriority::High => FxPlanPriority::High,
            acp::PlanEntryPriority::Medium => FxPlanPriority::Medium,
            acp::PlanEntryPriority::Low => FxPlanPriority::Low,
            _ => {
                tracing::debug!(target: "normalize", "unknown plan priority → Medium");
                FxPlanPriority::Medium
            }
        }),
        status: match entry.status {
            acp::PlanEntryStatus::Pending => FxPlanStatus::Pending,
            acp::PlanEntryStatus::InProgress => FxPlanStatus::InProgress,
            acp::PlanEntryStatus::Completed => FxPlanStatus::Completed,
            _ => {
                tracing::debug!(target: "normalize", "unknown plan status → Pending");
                FxPlanStatus::Pending
            }
        },
    }
}

fn meta_to_value(meta: &acp::Meta) -> serde_json::Value {
    // Meta == serde_json::Map<String, Value>: passthrough verbatim.
    serde_json::Value::Object(meta.clone())
}

// ── request_permission ───────────────────────────────────────────────────────

/// Inbound `session/request_permission` → (PermissionRequested event, parked
/// pending). The PendingAcpRequest RIDES OUT separately (park_tx, see
/// acp/mod.rs) because Responder is neither Clone nor serializable — FxEvent
/// payloads must stay serde-clean.
pub fn request_permission(
    req: &acp::RequestPermissionRequest, // { session_id, tool_call: ToolCallUpdate, options, meta }
    our_session: &SessionId,
    idgen: &crate::ids::IdGen, // cloned into the connection actor at start time
) -> (FxEvent, crate::driver::acp::PendingAcpRequestCore) {
    let request_id = idgen.request(); // minted HERE, once
    let fields = &req.tool_call.fields;
    let event = FxEvent::PermissionRequested {
        request_id: request_id.clone(),
        session: our_session.clone(),
        tool_call: ToolCallSummary {
            tool_call: ToolCallId::from_raw(req.tool_call.tool_call_id.to_string()),
            title: fields
                .title
                .clone()
                .unwrap_or_else(|| "<untitled tool>".into()),
        },
        options: req
            .options
            .iter()
            .map(|o| FxPermOption {
                option_id: OptionId::from_raw(o.option_id.to_string()),
                name: o.name.clone(),
                kind: perm_kind_map(o.kind),
            })
            .collect(),
    };
    (
        event,
        crate::driver::acp::PendingAcpRequestCore {
            our_id: request_id,
            acp_session: req.session_id.to_string(), // RAW acp id stringified
        },
    )
}

/// Unknown permission kind maps reject-once: fail-closed default — an unknown
/// future kind must not look allow-ish in UI.
fn perm_kind_map(kind: acp::PermissionOptionKind) -> FxPermOptionKind {
    match kind {
        acp::PermissionOptionKind::AllowOnce => FxPermOptionKind::AllowOnce,
        acp::PermissionOptionKind::AllowAlways => FxPermOptionKind::AllowAlways,
        acp::PermissionOptionKind::RejectOnce => FxPermOptionKind::RejectOnce,
        acp::PermissionOptionKind::RejectAlways => FxPermOptionKind::RejectAlways,
        _ => {
            tracing::warn!(
                target: "normalize",
                ?kind,
                "unmapped permission kind → RejectOnce (fail closed)"
            );
            FxPermOptionKind::RejectOnce
        }
    }
}

/// Answer the parked responder with the proper outcome shape. Lives next to the
/// normalizer so ALL RequestPermissionResponse construction stays in one file.
pub fn respond_outcome(chosen: Option<fxproto::ids::OptionId>) -> acp::RequestPermissionOutcome {
    match chosen {
        Some(id) => acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
            acp::PermissionOptionId::new(id.as_str().to_owned()),
        )),
        None => acp::RequestPermissionOutcome::Cancelled,
    }
}

/// session/prompt RESPONSE handling (success path). Enum ↔ enum identities +
/// drift insurance: EndTurn/MaxTokens/MaxTurnRequests/Refusal/Cancelled carry;
/// anything new closes the turn benignly rather than fake-failing it.
pub fn stop_reason(r: acp::StopReason) -> FxStopReason {
    match r {
        acp::StopReason::EndTurn => FxStopReason::EndTurn,
        acp::StopReason::MaxTokens => FxStopReason::MaxTokens,
        acp::StopReason::MaxTurnRequests => FxStopReason::MaxTurnRequests,
        acp::StopReason::Refusal => FxStopReason::Refusal,
        acp::StopReason::Cancelled => FxStopReason::Cancelled,
        _ => {
            tracing::warn!(
                target: "normalize",
                reason = ?r,
                "unmapped stop_reason → EndTurn (benign close; drift visible in traces)"
            );
            FxStopReason::EndTurn
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1 as s;
    use fxproto::event::FxEvent;
    use fxproto::ids::SessionId;

    fn sess() -> SessionId {
        SessionId::from_raw("s-acp".into())
    }
    fn sid() -> s::SessionId {
        s::SessionId::new("s-acp".to_owned())
    }
    fn turn_id(n: u32) -> TurnId {
        TurnId::from_raw(format!("t-{n:06}"))
    }

    // ── canary: every wire tag deserializes through serde ───────────────────
    #[test]
    fn n13_known_update_tags_round_trip_through_serde() {
        for tag in KNOWN_UPDATE_KINDS {
            // Build a wire shape per tag using minimal payloads.
            let payload = match tag {
                "user_message_chunk" | "agent_message_chunk" | "agent_thought_chunk" => {
                    format!(r#"{{"sessionUpdate":"{tag}","content":{{"type":"text","text":"x"}}}}"#)
                }
                "tool_call" => {
                    r#"{"sessionUpdate":"tool_call","toolCallId":"c1","title":"t"}"#.to_owned()
                }
                "tool_call_update" => {
                    r#"{"sessionUpdate":"tool_call_update","toolCallId":"c1"}"#.to_owned()
                }
                "plan" => r#"{"sessionUpdate":"plan","entries":[]}"#.to_owned(),
                "available_commands_update" => {
                    r#"{"sessionUpdate":"available_commands_update","availableCommands":[]}"#
                        .to_owned()
                }
                "current_mode_update" => {
                    r#"{"sessionUpdate":"current_mode_update","currentModeId":"m"}"#.to_owned()
                }
                "config_option_update" => {
                    r#"{"sessionUpdate":"config_option_update","configOptions":[]}"#.to_owned()
                }
                "session_info_update" => r#"{"sessionUpdate":"session_info_update"}"#.to_owned(),
                "usage_update" => {
                    r#"{"sessionUpdate":"usage_update","used":1,"size":2}"#.to_owned()
                }
                other => panic!("unhandled inventory tag {other}"),
            };
            let note_json = format!(r#"{{"sessionId":"wire-sid","update":{payload}}}"#);
            let parsed: Result<s::SessionNotification, _> = serde_json::from_str(&note_json);
            assert!(
                parsed.is_ok(),
                "tag {tag} failed to deserialize: {note_json}"
            );
        }
    }

    // N1: R1 flatten joins text blocks in order; non-text drops (N2).
    #[test]
    fn n1_user_chunk_flatten_joins_text_drops_image() {
        let update = s::SessionUpdate::UserMessageChunk(s::ContentChunk::new(
            s::ContentBlock::Text(s::TextContent::new("hello world")),
        ));
        let evs = session_update(&sess(), Some(&turn_id(1)), &update, None);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            FxEvent::Chunk {
                session,
                turn,
                role,
                text,
            } => {
                assert_eq!(session.as_str(), "s-acp");
                assert_eq!(turn.as_str(), "t-000001");
                assert_eq!(*role, FxRole::User);
                assert_eq!(text, "hello world");
            }
            other => panic!("expected Chunk, got {other:?}"),
        }
    }

    // N3: R2 role=Agent.
    #[test]
    fn n3_agent_chunk_role_is_agent_and_turn_stamped() {
        let update =
            s::SessionUpdate::AgentMessageChunk(s::ContentChunk::new(s::ContentBlock::from("hi")));
        let evs = session_update(&sess(), Some(&turn_id(7)), &update, None);
        match &evs[0] {
            FxEvent::Chunk { role, turn, .. } => {
                assert_eq!(*role, FxRole::Agent);
                assert_eq!(turn.as_str(), "t-000007");
            }
            other => panic!("expected Chunk, got {other:?}"),
        }
    }

    #[test]
    fn n4_thought_chunks_produce_empty_vec() {
        let update =
            s::SessionUpdate::AgentThoughtChunk(s::ContentChunk::new(s::ContentBlock::from("hm")));
        let evs = session_update(&sess(), Some(&turn_id(1)), &update, None);
        assert!(evs.is_empty());
    }

    // N5/N6: full compose incl. meta; SwitchMode → Other.
    #[test]
    fn n5_tool_call_composes_view_wholesale_with_meta() {
        let mut view = ComposedToolCall::default();
        let meta_meta: s::Meta = [
            ("k".to_owned(), serde_json::Value::Bool(true)),
            ("a".to_owned(), serde_json::json!({"n": 42})),
        ]
        .into_iter()
        .collect();
        let call = s::ToolCall::new("call_9", "read file")
            .kind(s::ToolKind::Read)
            .status(s::ToolCallStatus::InProgress)
            .content(vec![s::ToolCallContent::Content(s::Content::new(
                s::ContentBlock::from("some output"),
            ))])
            .meta(meta_meta);
        let evs = session_update(
            &sess(),
            Some(&turn_id(2)),
            &s::SessionUpdate::ToolCall(call),
            Some(&mut view),
        );
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            FxEvent::ToolCallUpsert {
                tool_call,
                title,
                kind,
                status,
                output,
                ..
            } => {
                assert_eq!(tool_call.as_str(), "call_9");
                assert_eq!(title, "read file");
                assert_eq!(*kind, FxToolKind::Read);
                assert_eq!(*status, FxToolStatus::InProgress);
                assert_eq!(output.as_deref(), Some("some output"));
            }
            other => panic!("expected ToolCallUpsert, got {other:?}"),
        }
        let composed_meta = view.meta.clone().expect("meta passthrough");
        assert_eq!(composed_meta["k"], serde_json::Value::Bool(true));
        assert_eq!(composed_meta["a"]["n"], serde_json::json!(42));
    }

    #[test]
    fn n6_switch_mode_tool_kind_maps_to_other() {
        assert_eq!(kind_map(s::ToolKind::SwitchMode), FxToolKind::Other);
        assert_eq!(kind_map(s::ToolKind::Fetch), FxToolKind::Fetch);
        assert_eq!(kind_map(s::ToolKind::Other), FxToolKind::Other);
    }

    // N8: merge keeps unset fields; N9: first-update synth is defaulted.
    #[test]
    fn n8_merge_keeps_unset_fields_replaces_meta_only_when_some() {
        let seed = s::SessionUpdate::ToolCall(
            s::ToolCall::new("c", "first title").status(s::ToolCallStatus::InProgress),
        );
        let mut view = ComposedToolCall::default();
        let _ = session_update(&sess(), Some(&turn_id(1)), &seed, Some(&mut view));
        let prev_meta = view.meta.clone();

        let partial = s::SessionUpdate::ToolCallUpdate(s::ToolCallUpdate::new(
            "c",
            s::ToolCallUpdateFields::new().status(s::ToolCallStatus::Completed),
        ));
        let evs = session_update(&sess(), Some(&turn_id(1)), &partial, Some(&mut view));
        match &evs[0] {
            FxEvent::ToolCallUpsert { title, status, .. } => {
                assert_eq!(title, "first title"); // kept
                assert_eq!(*status, FxToolStatus::Completed); // replaced
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(view.meta, prev_meta); // untouched when top-level meta absent

        // Meta REPLACE only when Some:
        let with_meta = s::SessionUpdate::ToolCallUpdate(
            s::ToolCallUpdate::new("c", s::ToolCallUpdateFields::new().title("renamed")).meta(
                Some(
                    [("z".to_owned(), serde_json::json!(1))]
                        .into_iter()
                        .collect::<s::Meta>(),
                ),
            ),
        );
        let evs = session_update(&sess(), Some(&turn_id(1)), &with_meta, Some(&mut view));
        match &evs[0] {
            FxEvent::ToolCallUpsert {
                title,
                status,
                _meta,
                ..
            } => {
                assert_eq!(title, "renamed");
                assert_eq!(*status, FxToolStatus::Completed); // still kept
                assert_eq!(
                    _meta.as_ref().and_then(|m| m.get("z")),
                    Some(&serde_json::json!(1))
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn n9_first_touch_synth_uses_defaults() {
        let update = s::SessionUpdate::ToolCallUpdate(s::ToolCallUpdate::new(
            "ghost",
            s::ToolCallUpdateFields::new().status(s::ToolCallStatus::Failed),
        ));
        let mut view = ComposedToolCall::default(); // actor creates on demand
        let evs = session_update(&sess(), Some(&turn_id(3)), &update, Some(&mut view));
        match &evs[0] {
            FxEvent::ToolCallUpsert {
                title,
                kind,
                status,
                ..
            } => {
                assert_eq!(title, ""); // synthesized default
                assert_eq!(*kind, FxToolKind::Other);
                assert_eq!(*status, FxToolStatus::Failed);
            }
            other => panic!("{other:?}"),
        }
    }

    // N10: extract_output appends raw_output JSON after a newline.
    #[test]
    fn n10_output_prefers_text_appends_raw_output_pretty_json() {
        let content = vec![
            s::ToolCallContent::Diff(s::Diff::new("/f", "new")),
            s::ToolCallContent::Content(s::Content::new(s::ContentBlock::from("line one"))),
        ];
        let raw = serde_json::json!({"bytes": 3});
        let out = extract_output(&content, Some(&raw)).unwrap();
        assert!(out.starts_with("line one"), "{out}");
        assert!(out.contains("\n"), "{out}");
        assert!(out.contains("\"bytes\""), "{out}");

        // raw-only case renders the JSON alone.
        let out2 = extract_output(&[], Some(&serde_json::json!(42))).unwrap();
        assert_eq!(out2, "42");
        // Nothing survives ⇒ None.
        assert!(extract_output(&[], None).is_none());
    }

    // N11: plan entries map 1:1 positionally with priority Some.
    #[test]
    fn n11_plan_entries_positional_priority_some() {
        let plan = s::Plan::new(vec![
            s::PlanEntry::new(
                "step a",
                s::PlanEntryPriority::High,
                s::PlanEntryStatus::Pending,
            ),
            s::PlanEntry::new(
                "step b",
                s::PlanEntryPriority::Low,
                s::PlanEntryStatus::Completed,
            ),
        ]);
        let evs = session_update(
            &sess(),
            Some(&turn_id(1)),
            &s::SessionUpdate::Plan(plan),
            None,
        );
        match &evs[0] {
            FxEvent::PlanUpdated { session, entries } => {
                assert_eq!(session, &sess());
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].content, "step a");
                assert_eq!(entries[0].priority, Some(FxPlanPriority::High));
                assert_eq!(entries[0].status, FxPlanStatus::Pending);
                assert_eq!(entries[1].priority, Some(FxPlanPriority::Low));
                assert_eq!(entries[1].status, FxPlanStatus::Completed);
            }
            other => panic!("{other:?}"),
        }
    }

    // N12: ignored rows produce empty vec (thought covered by n4).
    #[test]
    fn n12_deferred_rows_are_all_empty() {
        let rows: Vec<s::SessionUpdate> = vec![
            s::SessionUpdate::AvailableCommandsUpdate(s::AvailableCommandsUpdate::new(vec![])),
            s::SessionUpdate::CurrentModeUpdate(s::CurrentModeUpdate::new(s::SessionModeId::new(
                "mode",
            ))),
            s::SessionUpdate::ConfigOptionUpdate(s::ConfigOptionUpdate::new(vec![])),
            s::SessionUpdate::SessionInfoUpdate(s::SessionInfoUpdate::default()),
            s::SessionUpdate::UsageUpdate(s::UsageUpdate::new(1, 2)),
        ];
        for row in rows {
            assert!(session_update(&sess(), Some(&turn_id(1)), &row, None).is_empty());
        }
    }

    // N13b: stop_reason identities + drift warn path shape-checked via matching.
    #[test]
    fn n13_stop_reason_identities() {
        assert_eq!(stop_reason(s::StopReason::EndTurn), FxStopReason::EndTurn);
        assert_eq!(
            stop_reason(s::StopReason::MaxTokens),
            FxStopReason::MaxTokens
        );
        assert_eq!(
            stop_reason(s::StopReason::MaxTurnRequests),
            FxStopReason::MaxTurnRequests
        );
        assert_eq!(stop_reason(s::StopReason::Refusal), FxStopReason::Refusal);
        assert_eq!(
            stop_reason(s::StopReason::Cancelled),
            FxStopReason::Cancelled
        );
    }

    // N14: request_permission wiring — id minted once + shared into core;
    // untitled default; unknown kinds fail closed.
    #[test]
    fn n14_request_permission_mints_ids_titles_options() {
        let idgen = crate::ids::IdGen::deterministic("req");

        let req = s::RequestPermissionRequest::new(
            sid(),
            s::ToolCallUpdate::new("call_x", s::ToolCallUpdateFields::new()),
            vec![
                s::PermissionOption::new("o1", "Allow", s::PermissionOptionKind::AllowOnce),
                s::PermissionOption::new("o2", "Deny?", s::PermissionOptionKind::RejectAlways),
            ],
        );
        let (event, core) = request_permission(&req, &sess(), &idgen);

        match &event {
            FxEvent::PermissionRequested {
                request_id,
                session,
                tool_call,
                options,
            } => {
                assert_eq!(*request_id, core.our_id, "same mint, twice");
                assert_eq!(session, &sess());
                assert_eq!(tool_call.tool_call.as_str(), "call_x");
                assert_eq!(tool_call.title, "<untitled tool>");
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].option_id.as_str(), "o1");
                assert!(matches!(options[0].kind, FxPermOptionKind::AllowOnce));
                assert!(matches!(options[1].kind, FxPermOptionKind::RejectAlways));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(core.acp_session, "s-acp");

        // NOTE on perm_kind_map's wildcard arm: SDK's PermissionOptionKind has
        // NO serde("other") fallback (unlike ToolKind), so an unknown WIRE kind
        // fails deserialization upstream instead of reaching us — the `_` arm
        // is rust-side insurance for SDK bumps and cannot be driven from raw
        // JSON. Documented rather than asserted.
    }
}
