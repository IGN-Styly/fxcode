//! One transcript message bubble.

use fxproto::content::Role;
use fxproto::model::threads::Message;
use gpui::{App, IntoElement, ParentElement as _, Styled as _, div, px, relative};
use gpui_component::{ActiveTheme as _, text::TextView};

// Data source: &Message handed over by thread.rs (from ThreadState.messages[i]) —
// positional index `i` stays OUT of this fn: bubble is id-agnostic; thread.rs supplies
// ElementId ("msg", i) for the wrapping row (index is stable forever by the
// append-only invariant, so it is a legal id component despite being positional).
//
// ADAPTATION note (recorded): the sketch had `render(msg)` with no context argument,
// but role styling reads theme tokens (`cx.theme()`), which requires `&App`. Only the
// parameter list grew — the return contract (impl IntoElement, pure projection) is
// unchanged.
//
// Role styling:
//   User  → accent-tinted rounded bubble, right-aligned, max-width ~70%.
//   Agent → plain full-width text block, no bubble chrome, rendered through
//           gpui-component's TextView markdown path.
//
// Streaming caveat (measure at M3 / impl.md 9.1): every tail Chunk MERGES into
// messages.last() (fold W2), re-parsing whole markdown per chunk at streaming rates.
// Fallback if hot: plain styled text while active_turn.is_some(), swap to markdown on
// TurnFinished — fold rules make either rendering lossless.
const USER_MAX_WIDTH: f32 = 0.7;

pub fn render(msg: &Message, cx: &App) -> impl IntoElement {
    match msg.role {
        Role::User => {
            let theme = cx.theme();
            div()
                .w_full()
                .py_0p5()
                .flex()
                .justify_end()
                .child(
                    div()
                        .max_w(relative(USER_MAX_WIDTH))
                        .px(px(12.0))
                        .py(px(6.0))
                        .rounded(theme.radius + px(4.0))
                        .bg(theme.primary)
                        .text_color(theme.primary_foreground)
                        .text_size(px(14.0))
                        .child(msg.text.clone()),
                )
                .into_any_element()
        }
        // Long-text handling: no custom truncation — bubbles scroll inside the
        // VirtualList row sizing math (thread.rs); selection goes through
        // gpui-component's TextView semantics.
        Role::Agent => div()
            .w_full()
            .py_0p5()
            .text_size(px(14.0))
            .child(
                // Markdown rendering for agent prose (blueprint's TextView pick).
                // A fixed inner id is legal: sibling rows own distinct outer ids
                // (("msg", i)), and ids must be unique among siblings only.
                TextView::markdown("agent-text", msg.text.clone()),
            )
            .into_any_element(),
    }
}
