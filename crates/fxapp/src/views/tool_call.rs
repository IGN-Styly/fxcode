//! Tool call card — upsert target keyed by ToolCallId.

use fxproto::content::{ToolCallKind, ToolCallStatus};
use fxproto::ids::ToolCallId;
use fxproto::model::threads::{PermOutcome, ToolCall};
use gpui::{
    App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::spinner::Spinner;
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _};

// Data source: (&ToolCallId, &ToolCall) handed over by thread.rs from
// ThreadState.{flow → tool_calls} — this view NEVER reaches into AppState itself
// (pass-through args only). Expansion state lives in ThreadView.expanded_tools
// (view-local UI), which is why `expanded` + the toggle callback are parameters here:
// render stays a pure projection.
//
// ADAPTATION notes (recorded):
// 1. The blueprint sketch was `render(call: &ToolCall, …)`; a `&ToolCallId` parameter
//    was added because the ElementId MUST derive from the domain id (crates.md) and
//    ToolCall does not carry its own key.
// 2. `toggle: impl Fn()` became `on_toggle: Rc<dyn Fn()>` so one closure can move into
//    a `'static` on_click handler per row (thread.rs builds it).
// 3. Blueprint's Badge(success)/Badge(danger): this gpui-component version renders TEXT
//    badges through `Tag` (Badge itself is count/dot/icon only) — same visual language,
//    correct primitive. The dotted count Badge form is used for the status-bar pending-
//    permission counter instead.
// 4. Kind icons (lucide set shipped by gpui-component): closest stock matches; kinds
//    without an obvious glyph fall back to Info rather than inventing semantics:
//      Read→File  Delete→Delete  Move→ArrowRight  Search→Search
//      Execute→SquareTerminal  Think→Bot  Fetch→Globe  Edit/Other→Info(fallback)

/// Whole-card interaction id, ("tool-call", ToolCallId)-shaped string derived from
/// the domain id (crates.md rule: never a flow index).
pub fn card_id(tool_call: &ToolCallId) -> SharedString {
    SharedString::from(format!("tool-call-{tool_call}"))
}

/// Expanded-card body height clamp (thread.rs sizing math shares this constant's
/// spirit; collapsed height is just the header row height).
pub const EXPANDED_OUTPUT_MAX_H: f32 = 240.0;

pub fn render(
    tool_call_id: &ToolCallId,
    call: &ToolCall,
    expanded: bool,
    on_toggle: impl 'static + Fn(&ClickEvent, &mut gpui::Window, &mut App),
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();

    div()
        .id(card_id(tool_call_id))
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .bg(theme.secondary)
        .child(header_row(call, expanded, on_toggle, cx))
        .when(expanded, |card| match &call.output {
            Some(output) if !output.is_empty() => card.child(output_body(output, cx)),
            _ => card,
        })
}

fn header_row(
    call: &ToolCall,
    expanded: bool,
    on_toggle: impl 'static + Fn(&ClickEvent, &mut gpui::Window, &mut App),
    cx: &App,
) -> Stateful<gpui::Div> {
    let theme = cx.theme();

    div()
        .id("tool-call-header")
        .flex()
        .w_full()
        .items_center()
        .gap_2()
        .px(px(8.0))
        .py(px(6.0))
        .cursor_pointer()
        .on_click(on_toggle)
        .child(kind_icon(&call.kind, &call.status, cx))
        .child(
            div()
                .flex_1()
                .text_size(px(13.0))
                .text_color(theme.foreground)
                .child(call.title.clone()),
        )
        .children(perm_badge(&call.perm, cx))
        .child(status_affordance(&call.status, cx))
        .child(if expanded {
            Icon::new(IconName::ChevronDown).small().into_any_element()
        } else {
            Icon::new(IconName::ChevronRight).small().into_any_element()
        })
}

fn kind_icon(kind: &ToolCallKind, status: &ToolCallStatus, cx: &App) -> gpui::AnyElement {
    let name = match kind {
        ToolCallKind::Read => IconName::File,
        ToolCallKind::Delete => IconName::Delete,
        ToolCallKind::Move => IconName::ArrowRight,
        ToolCallKind::Search => IconName::Search,
        ToolCallKind::Execute => IconName::SquareTerminal,
        ToolCallKind::Think => IconName::Bot,
        ToolCallKind::Fetch => IconName::Globe,
        ToolCallKind::Edit | ToolCallKind::Other => IconName::Info,
    };
    let mut icon = Icon::new(name).small();
    // A live spinner already advertises activity; keep static glyphs quiet-colored.
    if matches!(status, ToolCallStatus::Pending | ToolCallStatus::InProgress) {
        icon = icon.text_color(cx.theme().muted_foreground);
    }
    icon.into_any_element()
}

/// Status affordance by ToolCallStatus:
///   Pending | InProgress → Spinner
///   Completed            → success tag
///   Failed               → danger tag
fn status_affordance(status: &ToolCallStatus, cx: &App) -> gpui::AnyElement {
    match status {
        ToolCallStatus::Pending | ToolCallStatus::InProgress => Spinner::new()
            .small()
            .color(cx.theme().info)
            .into_any_element(),
        ToolCallStatus::Completed => Tag::success().small().child("done").into_any_element(),
        ToolCallStatus::Failed => Tag::danger().small().child("failed").into_any_element(),
    }
}

/// PERM BADGE — tri-state PermOutcome stamped onto the card by fold rule W6; tri-state,
/// NOT Option<OptionId>: None must render DIFFERENTLY from Cancelled.
///   None                    → nothing at all ("never asked" ≠ "cancelled")
///   Some(Cancelled)         → muted "cancelled"
///   Some(Chosen(option_id)) → neutral tag carrying the option id text. The option NAME
///     is intentionally unavailable here: options ride only on the transient modal
///     payload (perms.rs) and are gone once resolved — do NOT invent a lookup back into
///     PermsState.recent (it stores chosen id only).
fn perm_badge(perm: &Option<PermOutcome>, cx: &App) -> Option<gpui::AnyElement> {
    let tag = match perm {
        None => return None,
        Some(PermOutcome::Cancelled) => Tag::secondary().small().child("cancelled"),
        Some(PermOutcome::Chosen(option_id)) => Tag::secondary()
            .small()
            .child(SharedString::from(format!("{option_id}"))),
    };
    let _ = cx;
    Some(tag.into_any_element())
}

/// Body when output present AND expanded: monospace, clamped + scrollable.
fn output_body(output: &str, cx: &App) -> Stateful<gpui::Div> {
    div()
        .flex()
        .w_full()
        .max_h(px(EXPANDED_OUTPUT_MAX_H))
        .id("tool-call-output")
        .overflow_scroll()
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .px(px(8.0))
        .py(px(6.0))
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(12.0))
        .text_color(cx.theme().foreground)
        .child(output.to_string())
}
