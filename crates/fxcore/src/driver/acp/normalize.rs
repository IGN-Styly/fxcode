//! Pure translation layer: raw ACP messages → canonical FxEvents.
//!
//! THE most unit-tested code in the repo. No I/O, no process state owned here —
//! functions in, events out (the caller supplies the per-tool-call view to merge
//! into; see ownership note under tool_call/tool_call_update). Keep vendor
//! handling visible and named here rather than scattered.

// Imports to restore as you implement:
// use agent_client_protocol as acp;    // pinned v1 workspace dep; adjust paths on bump
// use tracing::{debug, warn};
//
// use fxproto::content::{
//     ContentBlock, PlanEntry, PlanPriority, Role, StopReason, ToolCallKind,
//     ToolCallStatus,
// };
// use fxproto::event::{
//     FxEvent, PermissionOption, PermissionOptionKind as FxPermOptionKind,
//     ToolCallSummary,
// };
// use fxproto::ids::{OptionId, RequestId, SessionId, ToolCallId};
//
// use crate::driver::acp::PendingAcpRequest;
// use crate::ids::IdGen;

// EXHAUSTIVE-MATCH STRATEGY (read before touching any match in this file):
//
// The SDK's v1 enums (`SessionUpdate`, `ToolKind`, `ToolCallStatus`,
// `StopReason`, `PermissionOptionKind`, `RequestPermissionOutcome`) are ALL
// `#[non_exhaustive]`, so Rust REQUIRES a wildcard arm and "new upstream variant
// = compile error" is impossible to get from a plain match. Strategy instead:
//
//   1. ONE choke point per message shape: session_update() / request_permission()
//      / stop_reason() below. No other file matches on these enums.
//   2. Named arm for EVERY v1 kind (table below); single trailing `_ =>`
//      unhandled(...). New SDK variants land there silently-by-compiler but
//      LOUDLY AT RUNTIME: unhandled logs at WARN (not debug) carrying the
//      variant's Debug repr — drift is observable in real traffic.
//   3. Inventory canary: KNOWN_UPDATE_KINDS below pins all 11 snake_case wire
//      tags. A unit test round-trips every tag through serde deserialization of
//      acp::SessionUpdate; an upstream RENAME/REMOVAL breaks that test in CI.
//      (Upstream ADDITION is caught by 2 + release checklist.)
//   4. Release checklist line: bumping agent-client-protocol in Cargo.lock MUST
//      re-read this file (impl.md Phase 4.4 relies on it). Type names verified
//      against schema 1.4.x when this scaffold was written.
//
// pub const KNOWN_UPDATE_KINDS: [&str; 11] = [
//     "user_message_chunk", "agent_message_chunk", "agent_thought_chunk",
//     "tool_call", "tool_call_update", "plan", "available_commands_update",
//     "current_mode_update", "config_option_update", "session_info_update",
//     "usage_update",
// ];
//
// fn unhandled(u: &acp::SessionUpdate);
//     warn!(target: "normalize", update = ?u, "unmapped SessionUpdate kind — \
//           check normalize.rs against the new agent-client-protocol version");

// TODO: COMPLETE mapping table — every ACP v1 inbound shape → exact output.
// Entry point receives OUR session already resolved by the actor (see
// acp/mod.rs register_session); callers pre-reject unknown sessions.
//
// WIRE SHAPE REALITY (verified schema 1.4.0): acp::SessionUpdate is an
// #[non_exhaustive] ENUM OF TUPLE VARIANTS (UserMessageChunk(ContentChunk),
// ToolCall(ToolCall), …) with `#[serde(tag = "sessionUpdate",
// rename_all = "snake_case")]`. The snake_case names below are the WIRE TAGS;
// in Rust they are CamelCase tuple variants carrying their payload struct —
// match on e.g. SessionUpdate::ToolCall(t), read t.tool_call_id etc.
//
// pub fn session_update(
//     our_session: &SessionId,
//     note: &acp::SessionNotification,          // { session_id, update, meta }
//     tool_view: Option<&mut ComposedToolCall>, // actor-owned per-(sid, ToolCallId)
//                                               // map entry; see rows T3/T4
// ) -> Vec<FxEvent>;
//
// Row table (ACP `update.sessionUpdate` discriminant → output):
//
//   R1 user_message_chunk(acp::ContentChunk{ content, message_id, meta })
//        → vec![Chunk { session: *our_session, turn: <caller's>, role: User, text }]
//        text = flatten_content(&chunk.content): only Text blocks survive;
//        Image/others ARE dropped here from ECHOES with debug! (fxproto event.rs
//        chunk-vs-blocks decision — composer sends [Text] today). Multiple text
//        blocks inside one chunk join in order. turn comes from the call context
//        (prompt caller), never from ACP (ACP has no turn ids).
//
//   R2 agent_message_chunk(ContentChunk) → Chunk { role: Agent, .. } — same
//        flatten rules as R1. message_id deliberately ignored: fxproto folds merge
//        consecutive same-role chunks by flow position (threads.rs W2), ACP's
//        messageId adds nothing we store in v0.
//
//   R3 agent_thought_chunk(ContentChunk) → IGNORED + debug! log.
//        DECISION KEPT (do not reopen without M1 evidence, fxproto event.rs):
//        thought-chunks have no home — no Role variant may be added (event.rs),
//        and a dedicated FxEvent would be protocol churn. Log counts so traffic
//        volume stays visible during bring-up.
//
//   R4 tool_call(acp::ToolCall{ tool_call_id, title, kind, status, content,
//        locations, raw_input, raw_output, meta })
//        → COMPOSE then EMIT: overwrite tool_view with THIS payload wholesale;
//          emit exactly one ToolCallUpsert built from the composed view:
//            session: *our_session, tool_call: ToolCallId::from_raw(id.to_string()),
//            title: title.clone(),
//            kind:   kind_map(kind)            — row K below
//            status: status_map(status)        — row S below
//            output: extract_output(content, raw_output) — row O below
//            _meta:  meta.map(Meta→serde_json::Value) verbatim (Meta IS a
//                    serde_json::Map alias upstream), else None.
//
//   R5 tool_call_update(acp::ToolCallUpdate{ tool_call_id, fields:
//        ToolCallUpdateFields{ kind?, status?, title?, content?, raw_input?,
//        raw_output? }, meta })
//        MERGE semantics onto the composed view (view persists across updates —
//        ACP deltas are partial; fxproto fold W3 replaces WHOLESALE, so merging
//        must happen HERE, keeping normalize pure by taking &mut ComposedToolCall):
//          Some(x) field replaces the view's field; None keeps previous;
//          top-level update.meta REPLACES _meta only when Some (latest-wins).
//        Then emit one ToolCallUpsert from the merged view (same shape as R4).
//        Ownership: the VIEW MAP lives in the connection actor (BTreeMap member);
//        this function owns the MERGE RULES; empty view on first update ever =>
//        synthesize defaults (title "", kind Other, status Pending) and warn!
//        (agents normally send tool_call first — treat as vendor quirk, not bug).
//
//   K kind_map(exhaustive over acp::ToolKind, `_` fallback required):
//        Read→Read Edit→Edit Delete→Delete Move→Move Search→Search Execute→Execute
//        Think→Think Fetch→Fetch SwitchMode→Other(!! SDK-only variant: fxproto has
//        no SwitchMode; Other is the honest bucket) `_`→Other + debug!.
//
//   S status_map(exhaustive over acp::ToolCallStatus):
//        Pending→Pending InProgress→InProgress Completed→Completed Failed→Failed
//        `_`→Failed + warn! (future statuses render visibly-wrong rather than
//        invisibly-pending; failed cards invite investigation).
//
//   O extract_output(content: &[acp::ToolCallContent], raw_output) -> Option<String>
//        first Text block among content items wins, joined in order if several;
//        raw_output pretty-printed JSON APPENDED after a newline when both exist;
//        Diff/Terminal content shapes are NOT rendered in v0 (debug!). None when
//        nothing survived.
//
//   R6 plan(acp::Plan{ entries }) → vec![PlanUpdated { session, entries }]
//        entry map is 1:1 positional: content→content; priority: SDK's
//        PlanEntryPriority is REQUIRED (High|Medium|Low) while ours is Option =>
//        priority: Some(map) identity; status: PlanEntryStatus identity
//        (pending|in_progress|completed). Whole-vector REPLACE semantics come
//        from fold W4; ACP docs confirm agents always send complete plans.
//
//   R7 available_commands_update(_) → IGNORED + debug!. Slash-command palette is
//        M3+ UX with NO fxproto home yet; modeling now would be speculative
//        protocol surface (add via golden-first rule when needed).
//
//   R8 current_mode_update(CurrentModeUpdate{ current_mode_id }) → IGNORED +
//        debug!. Same reasoning as R7; revisit together with config options.
//
//   R9 config_option_update(ConfigOptionUpdate{ config_options }) → IGNORED +
//        debug!. Read-only projection M3+ at the earliest.
//
//   R10 session_info_update(SessionInfoUpdate{ title?, timestamps?, meta? }) →
//        IGNORED + debug!. Sidebar titles could consume `title` — parked to M3;
//        flag explicitly THERE when implemented (needs new fxproto event or
//        SessionCreated extension; golden tests first).
//
//   R11 usage_update(UsageUpdate{ cost: Cost{ amount, currency }, ... }) →
//        IGNORED + debug!. Cost metering UI deferred past M3; note unstable
//        features behind it are cfg-gated off in the SDK anyway.
//
// pub fn request_permission(
//     req: acp::RequestPermissionRequest, // { session_id, tool_call: ToolCallUpdate,
//                                         //   options: Vec<PermissionOption>, meta }
//     our_session: &SessionId,
//     gen: &crate::ids::IdGen,            // IdGen wiring: cloned into us by Orchestrator
//     responder: acp::Responder<acp::RequestPermissionResponse>,
// ) -> (FxEvent, PendingAcpRequest);
//     out.0 = PermissionRequested {
//         request_id: gen.request(),                       // minted HERE, once
//         session:    *our_session,
//         tool_call:  ToolCallSummary {
//             tool_call: ToolCallId::from_raw(req.tool_call.tool_call_id.to_string()),
//             title: req.tool_call.fields.title.clone()
//                    .unwrap_or_else(|| "<untitled tool>".into()),
//         },
//         options: req.options.iter().map(|o| PermissionOption {
//             option_id: OptionId::from_raw(o.option_id.to_string()),
//             name: o.name.clone(),
//             kind: perm_kind_map(o.kind),                 // AllowOnce|AllowAlways|
//                                                          // RejectOnce|RejectAlways
//             // exhaustive identity + `_` → RejectOnce + warn! (fail-closed
//             // default: an unknown future kind must not look allow-ish in UI)
//         }).collect(),
//     };
//     out.1 = PendingAcpRequest {
//         our_id: <same minted id>,
//         acp_session: req.session_id.to_string(),         // RAW acp id (SDK type)
//         responder,                                       // moved whole
//     };
//
// /// session/prompt RESPONSE handling (success path): PromptResponse.stop_reason
// pub fn stop_reason(r: acp::StopReason) -> StopReason;   // enum ↔ enum
//     EndTurn→EndTurn MaxTokens→MaxTokens MaxTurnRequests→MaxTurnRequests
//     Refusal→Refusal Cancelled→Cancelled
//     `_`(non_exhaustive insurance)→EndTurn + warn!: absent better data close the
//     turn benignly rather than fake-failing it; drift still visible in traces.
//
// /// ERROR FRAMES: JSON-RPC error responses TO OUR requests arrive as failures
// /// of the awaited request future (acp::Error { code: ErrorCode, message }),
// /// not as notifications. Contract:
// ///   - prompt future Err            → handled by TURN TASK in cmd/session.rs:
// ///                                    TurnFinished { stop_reason: Cancelled }
// ///                                    (+ process-liveness judged by the ACTOR,
// ///                                    which alone emits AgentStatus::Crashed).
// ///   - initialize/session/new Err   → propagates as crate::Error::Acp(detail):
// ///                                    StartAgent/NewSession convert to their
// ///                                    FxError replies (cmd/session.rs tables).
// ///   - acp::Error codes preserved VERBATIM in the log record (code + data) —
// ///     no re-mapping table in v0; add one here if agents prove chatty enough
// ///     to need triage buckets (M5 hardening candidate).
//
// Rules (restate, binding):
// - All SDK-enum matches listed above live ONLY here; wildcard arms route through
//   the shared helpers so runtime WARNs stay greppable ("unmapped").
// - _meta passthrough preserved verbatim (Meta == serde_json::Map alias).
// - Every ignored row (R3, R7–R11) logs at debug INCLUDING its Debug repr, so
//   bring-up sessions answer "what did the agent send?" without code changes.
//
// Unit-test checklist (one fn per row — impl.md 4.4 "unit tests per mapping row"):
//   N1  R1 flatten: [Text("a"),Text("b")] → "ab"; N2 R1 image dropped + logged.
//   N3  R2 role=Agent.                        N4  R3 produces EMPTY vec.
//   N5  R4 full compose fields incl. meta.    N6  K  SwitchMode→Other.
//   N7  S  unknown-status wildcard → Failed.  N8  R5 merge keeps unset fields.
//   N9  R5 first-update-synthesize + warn.    N10 O raw_output append order.
//   N11 R6 1:1 entries + priority Some.       N12 R7..R11 each ignore + empty.
//   N13 stop_reason five identities + drift warn. N14 request_permission: id
//       minted equals PendingAcpRequest.our_id; Untitled default title;
//       unknown permission kind maps reject-once.
