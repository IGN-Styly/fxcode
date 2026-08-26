//! Thread view: one session's transcript + composer. The main screen.

use std::{collections::HashSet, rc::Rc};

use gpui::{
    AppContext, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, ScrollStrategy, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _, VirtualListScrollHandle};

use fxproto::command::Command;
use fxproto::content::ContentBlock;
use fxproto::ids::{SessionId, ToolCallId};
use fxproto::model::{FlowItem, ThreadState};

use crate::conn::ConnectionManager;
use crate::store::AppState;
use crate::views::{dom_ids, message, tool_call};

// DATA SOURCE (exact paths, all read-only):
//   AppState.threads.threads[&active_session] → ThreadState …else EMPTY-STATE below.
//     .flow drives ONE flat VirtualList:
//       FlowItem::Message(i) → views::message::render(&state.messages[i], cx)
//         (i = append-only index, stable forever — threads.rs invariant; safe as an id
//          component even though it is a positional index)
//       FlowItem::Tool(id)   → views::tool_call::render(&id, &state.tool_calls[&id],
//                                                       expanded, on_toggle, cx)
//     missing Tool entry for a Tool item is impossible by fold construction (W3/W6);
//     a collapsed placeholder renders anyway + tracing::error! (defense in depth).
//
// SCROLL-STICKINESS (single source of truth): stick flips FALSE only when the measured
// offset-from-bottom clearly exceeds the margin at paint time (user scrolled up), TRUE
// again when back near the bottom before growth; programmatic scroll_to_bottom fires on
// growth frames while latched. The "Jump to latest ↓" pill shows while not stuck but the
// flow grew since the last paint.
const STICK_MARGIN_PX: f32 = 24.0;

/// Estimated row heights feed VirtualList sizing. Blueprint's per-item cache keyed
/// (index, text byte length) collapses to pure recomputation here because text length
/// changes ONLY via tail merges — history rows are byte-stable, so estimates hit every
/// paint identically. Revisit if profiling disagrees (impl.md 9.1).
const CHARS_PER_LINE_WORST: f32 = 48.0;
const LINE_HEIGHT_PX: f32 = 20.0;
const BUBBLE_PADDING_PX: f32 = 12.0;
/// Collapsed AND expanded card headers share this height constant with sizing math.
const TOOL_HEADER_PX: f32 = 34.0;
const TOOL_OUTPUT_LINE_HEIGHT_PX: f32 = 16.0;

pub struct ThreadView {
    active_session: Option<SessionId>,
    manager: gpui::WeakEntity<ConnectionManager>,
    composer: gpui::Entity<InputState>,
    scroll_handle: VirtualListScrollHandle,
    /// View-local UI state — NOT projection state.
    expanded_tools: HashSet<ToolCallId>,
    /// Sticky-bottom latch; see SCROLL-STICKINESS above.
    stick: bool,
    plan_collapsed: bool,
    last_seen_flow_len: usize,
    _subscription: gpui::Subscription,
}

impl ThreadView {
    pub fn new(
        manager: gpui::WeakEntity<ConnectionManager>,
        initial_session: Option<SessionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx
            .new(|cx| InputState::new(window, cx).placeholder("Message the agent…  (Enter sends)"));

        let _subscription = cx.subscribe_in(
            &composer,
            window,
            |this, _composer, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send_prompt(window, cx);
                }
            },
        );

        let mut this = Self {
            active_session: None,
            manager,
            composer,
            scroll_handle: VirtualListScrollHandle::new(),
            expanded_tools: HashSet::new(),
            stick: true,
            plan_collapsed: false,
            last_seen_flow_len: 0,
            _subscription,
        };
        this.set_active_session(initial_session, cx);
        this
    }

    pub fn set_active_session(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        if self.active_session == session {
            return;
        }
        self.active_session = session;
        self.stick = true; // sticky-bottom ON for switches and reconnect re-renders
        self.plan_collapsed = false;
        self.expanded_tools.clear();
        self.last_seen_flow_len = self.current_flow_len(cx);
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn current_flow_len(&self, cx: &Context<Self>) -> usize {
        self.flow_state(cx)
            .map_or(0, |thread_state| thread_state.flow.len())
    }

    fn flow_state<'a>(&self, cx: &'a Context<Self>) -> Option<&'a ThreadState> {
        self.active_session
            .as_ref()
            .and_then(|session_id| cx.global::<AppState>().threads.threads.get(session_id))
    }

    // -----------------------------------------------------------------------
    // Intents → Commands
    // -----------------------------------------------------------------------

    fn send_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().trim().to_string();
        if draft.is_empty() {
            return;
        }
        let Some(session) = self.active_session.clone() else {
            return;
        };
        // Clear optimistically — Prompt yields NO local transcript echo; the user's
        // words arrive as Chunk { role: User } events, and self-appending would
        // duplicate them (locked decision).
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));

        dispatch(
            &self.manager,
            Command::Prompt {
                session,
                blocks: vec![ContentBlock::Text { text: draft }],
            },
            cx,
        );
    }

    fn send_cancel(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.active_session.clone() else {
            return;
        };
        // Draft untouched on Cancel (locked decision).
        dispatch(&self.manager, Command::Cancel { session }, cx);
    }

    fn toggle_tool(&mut self, id: ToolCallId, cx: &mut Context<Self>) {
        if !self.expanded_tools.remove(&id) {
            self.expanded_tools.insert(id);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Height estimation (VirtualList item_sizes)
    // -----------------------------------------------------------------------

    fn estimate_sizes(&self, cx: &Context<Self>) -> Rc<Vec<Size<Pixels>>> {
        const WIDTH: Pixels = px(0.); // cross-axis inferred by the list itself

        let Some(thread_state) = self.flow_state(cx) else {
            return Rc::new(Vec::new());
        };

        let heights = thread_state
            .flow
            .iter()
            .map(|item| match item {
                FlowItem::Message(index) => {
                    let text_len = thread_state
                        .messages
                        .get(*index)
                        .map(|m| m.text.len())
                        .unwrap_or_default();
                    let lines = ((text_len as f32 / CHARS_PER_LINE_WORST).ceil() as u32).max(1);
                    px(lines as f32 * LINE_HEIGHT_PX + BUBBLE_PADDING_PX)
                }
                FlowItem::Tool(id) => {
                    if !self.expanded_tools.contains(id) {
                        return px(TOOL_HEADER_PX); // COLLAPSED_HEADER_H constant
                    }
                    let output_len = thread_state
                        .tool_calls
                        .get(id)
                        .and_then(|card| card.output.as_deref())
                        .map(str::len)
                        .unwrap_or(0);
                    let lines =
                        ((output_len as f32 / CHARS_PER_LINE_WORST).ceil() as u32).max(1) + 2; // header + breathing room
                    px((lines as f32 * TOOL_OUTPUT_LINE_HEIGHT_PX)
                        .min(tool_call::EXPANDED_OUTPUT_MAX_H)
                        + TOOL_HEADER_PX)
                }
            })
            .map(|height| Size {
                width: WIDTH,
                height,
            })
            .collect::<Vec<_>>();

        Rc::new(heights)
    }
}

fn dispatch(
    manager: &gpui::WeakEntity<ConnectionManager>,
    command: Command,
    cx: &mut Context<ThreadView>,
) {
    match manager
        .update(cx, |manager, cx| manager.send(command, cx))
        .ok()
    {
        Some(Ok(task)) => task.detach(),
        Some(Err(error)) => tracing::debug!(?error, "command rejected locally"),
        None => tracing::debug!("connection manager released"),
    }
}

impl gpui::Render for ThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let Some(session) = self.active_session.clone() else {
            // STATE: empty — nothing selected yet.
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.background)
                .text_color(theme.muted_foreground)
                .child("No session selected — pick one in the sidebar")
                .into_any_element();
        };

        let Some(thread_state) = cx
            .global::<AppState>()
            .threads
            .threads
            .get(&session)
            .cloned()
        else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.background)
                .text_color(theme.muted_foreground)
                .child("Waiting for session state…")
                .into_any_element();
        };

        let turn_active = thread_state.active_turn.is_some();
        let flow_len = thread_state.flow.len();
        let grew_since_last_paint = flow_len > self.last_seen_flow_len;

        // ---- stickiness measurement (single source of truth) ------------------
        let offset_from_bottom = self.scroll_handle.max_offset().y - self.scroll_handle.offset().y;
        if offset_from_bottom <= px(STICK_MARGIN_PX) {
            self.stick = true;
        } else if offset_from_bottom > px(STICK_MARGIN_PX * 3.0) {
            // Only CLEARLY-upward scrolling unlatches; ×3 guard keeps transient
            // layout jitter from fighting the tail.
            self.stick = false;
        }
        if self.stick && grew_since_last_paint {
            self.scroll_handle.scroll_to_bottom(); // deferred into next prepaint
        }

        let item_sizes = self.estimate_sizes(cx);

        let mut column = gpui_component::v_flex().size_full().bg(theme.background);

        // ---- plan section (collapsible header pinned ABOVE the flow list) -----
        if !thread_state.plan.is_empty() {
            let entries = thread_state.plan.len();
            column = column.child(
                div()
                    .id(dom_ids::PLAN_HEADER)
                    .px_3()
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.secondary_hover))
                    .text_color(theme.foreground)
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.plan_collapsed = !this.plan_collapsed;
                        cx.notify();
                    }))
                    .child(if self.plan_collapsed {
                        SharedString::from(format!("Plan · {entries} items ▸"))
                    } else {
                        SharedString::from(format!("Plan · {entries} items ▾"))
                    }),
            );
            if !self.plan_collapsed {
                column = column.children(thread_state.plan.iter().map(|entry| {
                    div()
                        .pl(px(18.0))
                        .text_size(px(12.5))
                        .text_color(theme.muted_foreground)
                        .child(entry.content.clone())
                }));
            }
        }

        // ---- jump-latest pill ---------------------------------------------------
        if !self.stick && grew_since_last_paint {
            column = column.child(
                div()
                    .mx_auto()
                    .id(dom_ids::JUMP_LATEST)
                    .cursor_pointer()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .px(px(10.0))
                    .py(px(2.0))
                    .mt_1()
                    .text_size(px(11.5))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.stick = true;
                        this.scroll_handle.scroll_to_bottom();
                        cx.notify();
                    }))
                    .child("Jump to latest ↓"),
            );
        }

        // ---- flat flow list -------------------------------------------------------
        column = column.child(
            div().flex_1().min_h_0().overflow_hidden().child(
                gpui_component::v_virtual_list::<gpui::AnyElement, ThreadView>(
                    cx.entity(),
                    dom_ids::FLOW_LIST_ITEMS,
                    item_sizes,
                    move |this, visible_range, _window, cx| {
                        render_flow_slice(this, &visible_range, cx)
                    },
                )
                .track_scroll(&self.scroll_handle)
                .px_2()
                .py_1(),
            ),
        );

        // ---- composer row ----------------------------------------------------------
        let mut composer_row = gpui_component::h_flex()
            .p_2()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .items_end()
            .child(
                div()
                    .id(dom_ids::COMPOSER)
                    .flex_1()
                    .child(Input::new(&self.composer)),
            );

        if turn_active {
            composer_row = composer_row.child(
                Button::new(dom_ids::STOP_TURN)
                    .danger()
                    .label("Stop")
                    .small()
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.send_cancel(cx))),
            );
        } else {
            composer_row =
                composer_row.child(
                    Button::new(dom_ids::SEND_TURN)
                        .primary()
                        .label("Send")
                        .small()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.send_prompt(window, cx)
                        })),
                );
        }
        column = column.child(composer_row);

        // Bookkeeping AFTER painting decisions so the next frame starts clean.
        self.last_seen_flow_len = flow_len;

        column.into_any_element()
    }
}

fn render_flow_slice(
    this: &mut ThreadView,
    visible_range: &std::ops::Range<usize>,
    cx: &mut Context<ThreadView>,
) -> Vec<gpui::AnyElement> {
    let Some(session) = this.active_session.clone() else {
        return vec![];
    };
    let Some(thread_state) = cx.global::<AppState>().threads.threads.get(&session) else {
        return vec![];
    };

    let theme_danger = cx.theme().danger;

    visible_range
        .clone()
        .filter_map(|item_index| {
            let Some(item) = thread_state.flow.get(item_index) else {
                return None; // stale slice overlap; the list refetches next paint
            };
            Some(match item {
                FlowItem::Message(message_index) => {
                    let Some(message_value) = thread_state.messages.get(*message_index) else {
                        tracing::error!(index = message_index, "dangling message index");
                        return Some(
                            div()
                                .id(dom_ids::msg_row(item_index))
                                .w_full()
                                .text_color(theme_danger)
                                .child("(unavailable message)")
                                .into_any_element(),
                        );
                    };
                    div()
                        .id(dom_ids::msg_row(item_index))
                        .w_full()
                        .child(message::render(message_value, cx))
                        .into_any_element()
                }
                FlowItem::Tool(tool_call_id) => {
                    let Some(card) = thread_state.tool_calls.get(tool_call_id) else {
                        tracing::error!(tool_call = %tool_call_id,
                            "flow references missing card (defense-in-depth placeholder)");
                        return Some(
                            div()
                                .id(dom_ids::tool_card(tool_call_id))
                                .w_full()
                                .text_color(theme_danger)
                                .child("(unavailable tool call)")
                                .into_any_element(),
                        );
                    };
                    let expanded = this.expanded_tools.contains(tool_call_id);
                    let toggle_listener = cx.listener({
                        let tool_call_id = tool_call_id.clone();
                        move |this: &mut ThreadView, _: &ClickEvent, _window, cx| {
                            this.toggle_tool(tool_call_id.clone(), cx)
                        }
                    });
                    div()
                        .id(dom_ids::tool_card(tool_call_id))
                        .w_full()
                        .child(tool_call::render(
                            tool_call_id,
                            card,
                            expanded,
                            toggle_listener,
                            cx,
                        ))
                        .into_any_element()
                }
            })
        })
        .collect()
}
