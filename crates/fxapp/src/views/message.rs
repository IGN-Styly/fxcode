//! One transcript message bubble.

use fxproto::model::threads::Message;

// TODO:
//
// pub fn render(msg: &Message) -> impl IntoElement;
//   - role styling: User = right-ish/accent bubble; Agent = plain full-width text
//   - agent text via gpui-component TextView (markdown rendering) — verify streaming
//     perf at M3; fallback = plain styled text if TextView re-parse per chunk is hot
//   - long-text handling + selection behavior (GPUI text selection semantics — check)
