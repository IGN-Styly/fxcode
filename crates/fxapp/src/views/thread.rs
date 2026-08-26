//! Thread view: one session's transcript + composer. The main screen.

use fxproto::ids::SessionId;

// TODO:
//
// pub struct ThreadView {
//     active_session: Option<SessionId>,
//     composer: Entity<InputState>,        // gpui-component Input (stateful)
//     scroll: gpui-component list scroll handle,
//     stick: bool,                         // sticky-bottom latch; ALGORITHM below
//     expanded_tools: HashSet<ToolCallId>, // view-local UI state; NOT projection state
// }
//
// DATA SOURCE (exact paths, all read-only):
//   cx.global::<AppState>()
//     .threads.threads.get(&active_session) → &ThreadState … else EMPTY-STATE below.
//     .flow drives ONE flat VirtualList:
//       FlowItem::Message(i) → views::message::render(&state.messages[i])
//                              (i = append-only index, stable forever — threads.rs
//                               invariant; safe as an id component even though it is a
//                               positional index)
//       FlowItem::Tool(id)   → views::tool_call::render(&state.tool_calls[&id],
//                                                       expanded_tools.contains(&id))
//     missing Tool entry for a Tool item = impossible by fold construction (W3/W6);
//     render a collapsed placeholder anyway + tracing::error! (defense in depth).
//   Plan section: state.plan non-empty ⇒ collapsible header ("Plan · N items") pinned
//     ABOVE the flow list; collapse flag lives in this view, default open.
//
// SCROLL-STICKINESS (decided algorithm — implement exactly):
//   consts: STICK_MARGIN_PX: f32 = 24.0.
//   stick starts TRUE on session switch and on every reconnect re-render.
//   BEFORE each render that appends flow items (compare rendered_len vs state.flow.len()):
//     measure current offset_from_bottom = content_h − scroll_top − viewport_h;
//     stick := offset_from_bottom <= STICK_MARGIN_PX.
//   AFTER the fold applied and in the same frame's post-layout pass:
//     if stick && grew { scroll_to_bottom() }.
//   User scrolling up past the margin flips stick=false via the same measurement rule
//     on the next scroll event (single source of truth — no second "user intent" flag).
//   When !stick && grew since last paint: show a "Jump to latest ↓" pill overlaying the
//     composer top edge (ElementId "jump-latest"); click ⇒ stick=true + scroll_to_bottom.
//
// VIRTUAL LIST ITEM SIZING:
//   Per-item CACHED heights keyed (ToolCallId|usize index, text byte length):
//     Message(i): ESTIMATE = ceil(text.len() / CHARS_PER_LINE_WORST ≈ 48) * LINE_HEIGHT
//                 + BUBBLE_PADDING; replaced by measured height at first visibility;
//                 text.len() changes ONLY via tail merges of the last message, so cache
//                 hit rate for history ≈ 100%.
//     Tool(id): COLLAPSED_HEADER_H constant when !expanded_tools.contains(id); expanded ⇒
//                 header + clamped(output.len()-derived estimate).
//   Overscan: 10 items above + 10 below the viewport.
//
// COMPOSER (intents → Commands):
//   ElementIds: "composer" (input) · "stop-turn" (button).
//   Enter (non-empty draft) ⇒ send(Command::Prompt { session, blocks: [Text(draft)] })
//     then CLEAR local draft optimistically. Do NOT self-append the echo into any store:
//     Prompt only yields Reply::PromptAccepted (no transcript data) — the user-visible
//     message arrives as Chunk { role: User } echoed by the server, and double-appending
//     locally would duplicate it (locked decision).
//     Disable while active_turn.is_some() (server would answer TurnNotActive anyway;
//     disabling just prevents lying to users). Stop button visible iff turn active ⇒
//     Command::Cancel { session }; leave draft untouched on Cancel.
//
// STATES ENUMERATED:
//   empty      (active_session None or not in threads map): centered placeholder
//              "No session selected — pick one in the sidebar".
//   loading    (session present but flow empty ∧ active_turn None): dim "waiting for
//              events…" row — distinguishable from empty AFTER first event arrives.
//   replaying  (reconnect refill in progress): no special chrome; stickiness rule keeps
//              the tail pinned while history streams in above.
