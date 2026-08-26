//! Pure translation layer: raw ACP messages → canonical FxEvents.
//!
//! THE most unit-tested code in the repo. No I/O, no state — functions in, events out.
//! Keep vendor-specific handling visible and named here rather than scattered.

// Imports to restore as you define the types:
// use agent_client_protocol as acp;    // adjust to the SDK's real module paths
// use fxproto::event::FxEvent;

// TODO: map each ACP inbound shape (from the agent-client-protocol crate's types):
//
// pub fn session_update(acp_session: &str, update: SessionUpdate) -> Vec<FxEvent>;
//     user_message_chunk   → Chunk { role: User, .. }        (echo of what we sent)
//     agent_message_chunk  → Chunk { role: Agent, .. }
//     agent_thought_chunk  → decide: own variant or Chunk w/ role? (lean: defer, log)
//     tool_call            → ToolCallUpsert (insert semantics)
//     tool_call_update     → ToolCallUpsert (merge semantics — same ToolCallId)
//     plan                 → PlanUpdated
//     available_commands_update / current_mode_update / config_option_update /
//     session_info_update / usage_update → ignore for now, tracing::debug! each
//
// pub fn request_permission(req: RequestPermissionRequest) -> (FxEvent /* PermissionRequested */,
//                                                            PendingAcpRequest);
//     generate our RequestId here so correlation lives in one place.
//
// pub fn stop_reason(r: acp::StopReason) -> StopReason;   // enum ↔ enum, exhaustive match
//
// Rules:
// - Exhaustive matches over ACP enums; adding an ACP variant must be a compile error here.
// - _meta passthrough preserved verbatim.
// - Every ignored update kind gets a debug log line (discoverability during bring-up).
