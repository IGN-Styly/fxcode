//! Tool call card — upsert target keyed by ToolCallId.

use fxproto::model::threads::ToolCall;

// TODO:
//
// pub fn render(call: &ToolCall, expanded: bool, toggle: impl Fn() + 'static)
//   -> impl IntoElement;
//   Data source: &ToolCall handed over by thread.rs from ThreadState.tool_calls — this
//   view NEVER reaches into AppState itself (pass-through args only).
//   Expansion state lives in ThreadView.expanded_tools (view-local UI), which is why
//   `expanded` + `toggle` are parameters here: render stays a pure projection.
//
//   ElementId (whole card, for interaction targets): ("tool-call", call's ToolCallId).
//
//   - header row (click ⇒ toggle): kind icon (lucide via gpui-component Icon) + title
//     (+ Spinner while Pending/InProgress, see below)
//   - status affordance by ToolCallStatus:
//       Pending | InProgress → Spinner
//       Completed            → Badge(success)
//       Failed               → Badge(danger)
//   - PERM BADGE (tri-state PermOutcome stamped onto the card by fold rule W6; tri-state,
//     NOT Option<OptionId> — None must render DIFFERENTLY from Cancelled):
//       None                    → render nothing at all ("never asked" ≠ "cancelled")
//       Some(Cancelled)         → Badge(muted) "cancelled"
//       Some(Chosen(option_id)) → Badge(neutral) with the option id text. The option NAME
//         is intentionally unavailable here: options ride only on the transient modal
//         payload (perms.rs) and are gone once resolved — do NOT invent a lookup back
//         into PermsState.recent (it stores chosen id only). Tooltip: "permission
//         resolved". If M3 UX needs human names, extend the WIRE type via fxproto first.
//   - body when output present AND expanded: monospace, scrollable clamp; collapsed by
//     default (height const COLLAPSED_HEADER_H shared with thread.rs sizing math).
//   - _meta passthrough ignored for v0 rendering
