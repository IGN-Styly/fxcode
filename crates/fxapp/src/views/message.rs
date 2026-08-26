//! One transcript message bubble.

use fxproto::model::threads::Message;

// TODO:
//
// pub fn render(msg: &Message) -> impl IntoElement;
//   Data source: &Message handed over by thread.rs (from ThreadState.messages[i]) —
//   positional index `i` stays OUT of this fn: bubble is id-agnostic; thread.rs supplies
//   ElementId ("msg", i) for the wrapping row (index is stable forever by the
//   append-only invariant, so it is a legal id component despite being positional).
//
//   role styling (Role from the message itself):
//     User  → accent-tinted rounded bubble, right-aligned, max-width ~70%.
//     Agent → plain full-width text block, no bubble chrome.
//
//   Agent text via gpui-component TextView (markdown rendering). Streaming caveat:
//   every tail Chunk MERGES into messages.last() (fold W2), which would re-parse the
//   whole markdown per chunk at ~chunk rates; measure at M3 (impl.md 9.1). Fallback if
//   hot: plain styled text while active_turn.is_some(), swap to markdown on TurnFinished.
//
//   Long-text handling: no custom truncation — bubbles scroll inside the VirtualList row
//   sizing math (thread.rs); selection via GPUI's text selection semantics — verify once
//   against gpui-component's TextView, else fall back with it documented here.
