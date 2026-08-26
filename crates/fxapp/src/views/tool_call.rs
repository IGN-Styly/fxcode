//! Tool call card — upsert target keyed by ToolCallId.

use fxproto::model::threads::ToolCall;

// TODO:
//
// pub fn render(call: &ToolCall) -> impl IntoElement;
//   - header: kind icon (lucide via gpui-component Icon) + title
//   - status affordance: Pending/InProgress → Spinner; Completed → Badge(ok);
//     Failed → Badge(danger)
//   - body: output (monospace, collapsed by default, expandable) when present
//   - _meta passthrough ignored for v0 rendering
