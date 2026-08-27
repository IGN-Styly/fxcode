//! Thread view: one session's transcript + composer. The main screen.

use std::{collections::HashSet, rc::Rc};

use gpui::{
    AppContext, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, ScrollStrategy, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _, VirtualListScrollHandle};

use fxproto::command::Command;
use fxproto::content::ContentBlock;
use fxproto::driver::DriverId;
use fxproto::ids::{AgentId, SessionId, ToolCallId};
use fxproto::model::{FlowItem, ThreadState};
use fxproto::reply::Reply;

use crate::conn::{ConnectionManager, SendError};
use crate::store::{AppState, DriverRow};
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
    // ---- first-turn (t3 parity) surface ------------------------------------
    /// Composer-side agent selection (t3: ModelPickerSidebar, no spawn on pick).
    chosen_driver: Option<DriverId>,
    cwd_input: gpui::Entity<InputState>,
    /// Inline ProviderStatusBanner analogue; cleared on next attempt.
    setup_error: Option<String>,
    /// Draft preserved across a failed pipeline so Retry restores the composer.
    last_failed_draft: Option<String>,
    /// t3's isSendBusy: freeze the sender while the create-session pipeline runs.
    busy: bool,
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

        // t3 "project" ≈ our cwd: it IS the working directory. Default mirrors the
        // compose-in-empty-state behavior — server home via "~", user-editable.
        let initial_cwd = cx
            .global::<AppState>()
            .last_cwd
            .clone()
            .unwrap_or_else(|| "~".to_string());
        let cwd_input = cx.new(|cx| {
            let mut st =
                InputState::new(window, cx).placeholder("Working directory  (~ = server home)");
            st.set_value(initial_cwd, window, cx);
            st
        });

        let _subscription = cx.subscribe_in(
            &composer,
            window,
            |this, _composer, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send_prompt(window, cx);
                }
            },
        );
        // Re-render on every fold/detect mutation (see store/mod.rs contract).
        let _global_observation = cx.observe_global::<AppState>(|_: &mut Self, cx| {
            cx.notify();
        });

        let mut this = Self {
            active_session: None,
            manager,
            composer,
            cwd_input,
            chosen_driver: None,
            setup_error: None,
            last_failed_draft: None,
            busy: false,
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
        if draft.is_empty() || self.busy {
            return;
        }
        let Some(session) = self.active_session.clone() else {
            // t3 flow: the composer IS the session creator. Optimistic clear here
            // (window is in scope); failures restore text via the Retry button.
            let cwd_text = self.cwd_input.read(cx).value().trim().to_string();
            let driver = self.chosen_driver;
            self.composer
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.start_first_turn(draft, driver, cwd_text, cx);
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

    // -----------------------------------------------------------------------
    // First-turn pipeline (t3code parity: typing = create everything lazily)
    //
    // FXPROTO CHOREOGRAPHY (5-command surface): DetectAgents informs the picker;
    // if a running process of the chosen driver exists we reuse it, else
    // StartAgent → NewSession{agent, cwd} → Prompt. Each Reply is awaited in
    // order inside ONE spawned task so users never see intermediate screens —
    // exactly t3's "send starts the thread" behavior with our server contract.
    // -----------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    fn start_first_turn(
        &mut self,
        draft: String,
        driver: Option<DriverId>,
        cwd_text: String,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        self.setup_error = None;
        self.last_failed_draft = Some(draft.clone());
        let manager = self.manager.clone();
        cx.spawn(async move |this, cx| {
            async fn step(
                manager: gpui::WeakEntity<ConnectionManager>,
                command: Command,
                cx: &mut gpui::AsyncApp,
            ) -> Result<Reply, String> {
                let task = manager
                    .update(cx, |m, cx| m.send(command, cx))
                    .map_err(|_| "connection lost".to_string())?
                    .map_err(|e| match e {
                        SendError::NotReady => "not connected yet".to_string(),
                        SendError::Transport => "send failed".to_string(),
                        SendError::ConnectionLost => "connection lost".to_string(),
                    })?;
                task.await
                    .map_err(|_| "connection lost".to_string())
                    .and_then(|reply| match reply {
                        Reply::Error(err) => Err(err.to_string()),
                        other => Ok(other),
                    })
            }

            let fail = |this: &gpui::WeakEntity<Self>, msg: String, cx: &mut gpui::AsyncApp| {
                this.update(cx, |m, _cx| {
                    m.busy = false;
                    m.setup_error = Some(msg);
                })
                .ok();
            };

            // 0) DetectAgents-driven picker state.
            let rows = cx.update(|cx| cx.global::<AppState>().found_drivers());
            let found: Vec<DriverRow> = rows.iter().filter(|r| r.found).cloned().collect();
            if found.is_empty() {
                fail(
                    &this,
                    "No installable agents detected on the server".into(),
                    cx,
                );
                return;
            }
            let driver = driver
                .filter(|d| found.iter().any(|r| r.driver == *d))
                .or_else(|| found.first().map(|r| r.driver))
                .expect("non-empty found list");

            // 1) reuse a running process of that driver or spawn one.
            let agent_id: AgentId = if let Some(id) =
                cx.update(|cx| cx.global::<AppState>().running_agent_for(driver))
            {
                id
            } else {
                match step(manager.clone(), Command::StartAgent { driver }, cx).await {
                    Ok(Reply::Started { agent }) => agent,
                    Err(msg) => {
                        fail(
                            &this,
                            format!("{} failed to start — {msg}", driver.label()),
                            cx,
                        );
                        return;
                    }
                    Ok(other) => {
                        fail(&this, format!("unexpected StartAgent reply {other:?}"), cx);
                        return;
                    }
                }
            };

            // 2) session anchored at cwd (our "project").
            let cwd = expand_tilde(&cwd_text);
            let new_session_reply = step(
                manager.clone(),
                Command::NewSession {
                    agent: agent_id.clone(),
                    cwd,
                    mcp_servers: vec![],
                },
                cx,
            )
            .await;
            let session: SessionId = match new_session_reply {
                Ok(Reply::SessionCreated { session }) => session,
                Err(msg) => {
                    fail(&this, format!("could not open a session — {msg}"), cx);
                    return;
                }
                Ok(other) => {
                    fail(&this, format!("unexpected NewSession reply {other:?}"), cx);
                    return;
                }
            };

            // 3) select + fire the turn; transcript streams via events.
            if this
                .update(cx, |m, _| m.set_active_session_raw(session.clone()))
                .is_err()
            {
                return;
            }
            let prompt_result = step(
                manager.clone(),
                Command::Prompt {
                    session: session.clone(),
                    blocks: vec![ContentBlock::Text {
                        text: draft.clone(),
                    }],
                },
                cx,
            )
            .await;

            this.update(cx, |m, cx| {
                m.busy = false;
                // Persist the project for next launch (t3 remembers projects too).
                m.remember_cwd(&cwd_text, cx);
                if let Err(msg) = prompt_result {
                    m.setup_error = Some(format!("turn did not start — {msg}"));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Render-time path into set_active_session without a Window argument.
    fn set_active_session_raw(&mut self, session: SessionId) -> bool {
        if self.active_session.as_ref() == Some(&session) {
            return false;
        }
        self.active_session = Some(session);
        self.stick = true;
        true
    }

    fn remember_cwd(&self, cwd: &str, cx: &mut Context<Self>) {
        cx.global_mut::<AppState>().last_cwd = Some(cwd.to_string());
    }

    fn cycle_driver(&mut self, direction: i32, cx: &mut Context<Self>) {
        let found: Vec<DriverRow> = cx
            .global::<AppState>()
            .found_drivers()
            .into_iter()
            .filter(|r| r.found)
            .collect();
        if found.is_empty() {
            // Nothing installed: nudge detection again through the manager.
            dispatch_detect(&self.manager, cx);
            return;
        }
        let current = self
            .chosen_driver
            .or_else(|| found.first().map(|r| r.driver));
        let idx = current
            .and_then(|c| found.iter().position(|r| r.driver == c))
            .unwrap_or(0);
        let len = found.len() as i32;
        let next = ((idx as i32) + direction).rem_euclid(len) as usize;
        self.chosen_driver = Some(found[next].driver);
        cx.notify();
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

/// Resolve "~" / "~/…" against the CLIENT machine's $HOME — the string travels
/// to the server verbatim as NewSession.cwd, so "~" is only meaningful when the
/// server and client share a home layout (documented in the picker tooltip).
fn expand_tilde(input: &str) -> std::path::PathBuf {
    if input == "~" {
        std::env::var("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
    } else if let Some(rest) = input.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        std::path::PathBuf::from(home).join(rest)
    } else {
        std::path::PathBuf::from(input)
    }
}

fn dispatch_detect(manager: &gpui::WeakEntity<ConnectionManager>, cx: &mut Context<ThreadView>) {
    // WeakEntity::update yields a Result; flatten before detaching the task.
    if let Ok(Ok(task)) = manager.update(cx, |m, cx| m.send(Command::DetectAgents, cx)) {
        task.detach();
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

impl ThreadView {
    /// t3code parity surface (`IndexDraftLanding` + composer agent rail):
    /// "What should we build?" — type anything; agent+folder sit AT the
    /// composer; sending lazily spawns agent → session → turn. found:false rows
    /// stay visible as install hints (data, not errors).
    fn render_first_turn(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::h_flex;

        let theme = cx.theme();
        let rows = cx.global::<AppState>().found_drivers();
        let selected_row: Option<DriverRow> = self
            .chosen_driver
            .as_ref()
            .and_then(|d| rows.iter().find(|r| r.driver == *d))
            .or_else(|| rows.iter().find(|r| r.found))
            .cloned();
        if self.chosen_driver.is_none() {
            self.chosen_driver = selected_row.as_ref().map(|r| r.driver);
        }
        let any_found = rows.iter().any(|r| r.found);

        let (chip_label, dot_color): (String, gpui::Hsla) = match (&selected_row, any_found) {
            (Some(row), _) => (
                row.label(),
                if row.found {
                    theme.success
                } else {
                    theme.muted_foreground
                },
            ),
            (None, true) => ("…".to_string(), theme.muted_foreground),
            (None, false) => ("Detecting agents…".to_string(), theme.muted_foreground),
        };

        // ── inline error banner (ProviderStatusBanner analogue) + Retry/Dismiss ──
        let banner = self.setup_error.clone().map(|msg| {
            h_flex()
                .id(dom_ids::SETUP_BANNER)
                .w_full()
                .px_2()
                .py(px(4.0))
                .gap_2()
                .bg(theme.danger.opacity(0.12))
                .text_color(theme.danger)
                .text_size(px(12.0))
                .child(div().flex_1().overflow_hidden().child(format!(
                    "{msg}  —  your message was kept; press Retry to send again."
                )))
                .child(
                    Button::new("setup-retry")
                        .label("Retry")
                        .small()
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            let draft = this.last_failed_draft.take().unwrap_or_default();
                            this.setup_error = None;
                            this.composer
                                .update(cx, |state, cx| state.set_value(draft, window, cx));
                            this.send_prompt(window, cx);
                        })),
                )
                .child(
                    Button::new("setup-dismiss")
                        .label("✕")
                        .small()
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.setup_error = None;
                            this.last_failed_draft = None;
                            cx.notify();
                        })),
                )
        });

        // ── agent rail + folder field ABOVE the composer (t3 composer annexes) ──
        let driver_bar = h_flex()
            .w_full()
            .px_2()
            .py(px(4.0))
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .id(dom_ids::CWD_INPUT)
                    .flex_1()
                    .max_w(px(360.0))
                    .child(Input::new(&self.cwd_input)),
            )
            .child(
                Button::new(dom_ids::AGENT_PREV)
                    .label("◂")
                    .small()
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _w, cx| this.cycle_driver(-1, cx)),
                    ),
            )
            .child(
                div()
                    .id(dom_ids::AGENT_CHIP)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .py(px(3.0))
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .text_size(px(12.0))
                    .text_color(theme.foreground)
                    .cursor_pointer()
                    .hover(|st| st.bg(theme.secondary_hover))
                    .child(div().size(px(7.0)).rounded_full().bg(dot_color))
                    .child(chip_label)
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        // Chip click re-probes when nothing is installed yet.
                        if !cx
                            .global::<AppState>()
                            .found_drivers()
                            .iter()
                            .any(|r| r.found)
                        {
                            dispatch_detect(&this.manager, cx);
                        }
                    })),
            )
            .child(
                Button::new(dom_ids::AGENT_NEXT)
                    .label("▸")
                    .small()
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.cycle_driver(1, cx))),
            );

        let mut column = h_flex().size_full().flex_col().bg(theme.background);

        // Hero copy (DraftHeroHeadline analogue).
        column = column.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .max_w(px(520.0))
                        .text_center()
                        .child(
                            div()
                                .text_size(px(20.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child("What should we build?"),
                        )
                        .child(
                            div()
                                .mt_2()
                                .text_size(px(13.0))
                                .text_color(theme.muted_foreground)
                                .child("Pick an agent and a working directory, then start typing."),
                        ),
                ),
        );

        if let Some(banner) = banner {
            column = column.child(banner);
        }
        column = column.child(driver_bar);

        // Composer row: Send doubles as the creator button while busy-latched.
        let send_label = if self.busy { "Starting…" } else { "Send" };
        let mut composer_row = h_flex()
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
        if !self.busy {
            composer_row =
                composer_row.child(
                    Button::new(dom_ids::SEND_TURN)
                        .primary()
                        .label(send_label)
                        .small()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.send_prompt(window, cx)
                        })),
                );
        } else {
            composer_row = composer_row.child(
                Button::new(dom_ids::SEND_TURN)
                    .primary()
                    .disabled(true)
                    .label(send_label)
                    .small(),
            );
        }
        column = column.child(composer_row);

        column.into_any_element()
    }
}

impl gpui::Render for ThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        let Some(session) = self.active_session.clone() else {
            return self.render_first_turn(cx);
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
