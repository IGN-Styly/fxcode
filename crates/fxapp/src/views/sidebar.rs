//! Sidebar: agent list w/ status dots + session list + "New session" affordance.

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, ClickEvent, Context, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
};

use fxproto::event::AgentStatus;
use fxproto::ids::{AgentId, SessionId};
use fxproto::model::{AgentState, AgentsState, ThreadsState};

use crate::store::AppState;
use crate::views::dom_ids::{self, AgentRowIds};

// DATA SOURCE (exact paths):
//   cx.global::<AppState>().agents.agents          — BTreeMap, iteration == spawn order
//                                                     (uuid v7 lexicographic), do not sort.
//   Sessions grouped UNDER an agent come from AgentState.sessions (fold rule S2 order) —
//     NEVER from AppState.threads keys (that map says "which transcripts exist"; agents.rs
//     owns "which sessions belong to which agent"). A thread whose agent row vanished
//     (S3 garbled log) renders NOWHERE here.
//   Session label = threads.threads[&sid].cwd file_name(); missing key ⇒ "(loading)"
//   disabled row; empty cwd ⇒ full path string (still clickable).
//
// INTENTS: this view emits EVENTS; WorkspaceView is the single translation point into
// ConnectionManager::send. Selecting a session is a plain local state write — never
// protocol traffic (blueprint-locked).
//
// GAP vs blueprint (recorded; deferred to Phase 9.x): the two-phase flow for
// Stopped/Crashed agents (inline StartAgent emit → observe Ready → auto-open the
// session dialog) and the "Open Setup" screen affordance are intentionally absent in
// the minimal spine; non-ready agents simply offer no New-session control.

/// What the sidebar tells its parent, without touching projections directly.
#[derive(Clone, Debug)]
pub enum SidebarEvent {
    SessionSelected(SessionId),
    /// User confirmed a new session on a Ready agent.
    NewSessionRequested {
        agent: AgentId,
        cwd: PathBuf,
    },
}

pub struct SidebarView {
    /// Mirrors WorkspaceView's active thread; the parent stays source of truth.
    selected_session: Option<SessionId>,
    /// Which agent currently shows an inline cwd draft (one at a time, v0 UX).
    open_draft_for: Option<AgentId>,
    /// Draft working-directory input; created eagerly (InputState construction needs a
    /// Window) and reused across drafts so typed text survives reopenings.
    draft_input: gpui::Entity<InputState>,
}

impl EventEmitter<SidebarEvent> for SidebarView {}

impl SidebarView {
    pub fn new(
        selected_session: Option<SessionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // React to agent/session folds arriving over the link.
        let _global_observation = cx.observe_global::<AppState>(|_: &mut Self, cx| {
            cx.notify();
        });

        let draft_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("working directory (e.g. ~/projects/x)")
        });
        Self {
            selected_session,
            open_draft_for: None,
            draft_input,
        }
    }

    fn select_session(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.selected_session = Some(session.clone());
        cx.emit(SidebarEvent::SessionSelected(session));
        cx.notify();
    }

    fn toggle_draft(&mut self, agent: AgentId, cx: &mut Context<Self>) {
        self.open_draft_for = match &self.open_draft_for {
            Some(open) if *open == agent => None,
            _ => Some(agent),
        };
        cx.notify();
    }

    fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        self.open_draft_for = None;
        cx.notify();
    }

    fn confirm_draft(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.open_draft_for.take() else {
            return;
        };
        let typed = self.draft_input.read(cx).value().trim().to_string();
        // Empty box means "here" — the server inherits its own cwd for NewSession.
        let cwd = PathBuf::from(if typed.is_empty() {
            String::from("./")
        } else {
            typed
        });
        cx.emit(SidebarEvent::NewSessionRequested { agent, cwd });
        cx.notify();
    }
}

/// Dot color map: Starting amber · Ready green · Busy blue · Crashed red · Stopped grey.
fn status_dot(status: &AgentStatus, cx: &App) -> gpui::AnyElement {
    let theme = cx.theme();
    let color = match status {
        AgentStatus::Starting => theme.warning,
        AgentStatus::Ready => theme.success,
        AgentStatus::Busy => theme.info,
        AgentStatus::Crashed { .. } => theme.danger,
        AgentStatus::Stopped => theme.muted_foreground,
    };
    div()
        .size(px(8.0))
        .rounded_full()
        .bg(color)
        .into_any_element()
}

/// Crashed rows keep their exit code inline instead of a tooltip (v0 simplification).
fn agent_label_text(agent: &AgentState) -> SharedString {
    match &agent.status {
        AgentStatus::Crashed { exit_code } => match exit_code {
            Some(code) => SharedString::from(format!("{} (exit {code})", agent.driver.label())),
            None => SharedString::from(format!("{} (crashed)", agent.driver.label())),
        },
        _ => SharedString::from(agent.driver.label()),
    }
}

/// `(label, row-exists)` — exists=false renders the disabled "(loading)" line.
fn session_label(session_id: &SessionId, threads: &ThreadsState) -> (SharedString, bool) {
    let Some(thread) = threads.threads.get(session_id) else {
        return (SharedString::from("(loading)"), false);
    };
    if thread.cwd.as_os_str().is_empty() {
        return (SharedString::from(thread.cwd.display().to_string()), true);
    }
    let label = thread
        .cwd
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| thread.cwd.display().to_string());
    (SharedString::from(label), true)
}

impl gpui::Render for SidebarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let AppState {
            agents, threads, ..
        } = cx.global::<AppState>();
        let AgentsState { agents: agents_map } = agents;

        let mut panel = gpui_component::v_flex()
            .id(dom_ids::SIDEBAR)
            .h_full()
            .overflow_y_scroll()
            .p_2()
            .gap_2()
            .bg(theme.background)
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child("AGENTS"),
            );

        if agents_map.is_empty() {
            // STATE: no-agents — Setup screen is M3 (gap note above).
            return panel
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("No agents yet.")
                .into_any_element();
        }

        for (agent_id, agent_state) in agents_map {
            let AgentRowIds {
                row,
                new_session,
                draft_confirm,
                draft_cancel,
            } = dom_ids::agent_row(agent_id);
            let ready = matches!(agent_state.status, AgentStatus::Ready);
            let drafting = self.open_draft_for.as_ref() == Some(agent_id);

            let mut group = gpui_component::v_flex().gap_1();

            let mut agent_row = div()
                .id(row)
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(4.0))
                .rounded(theme.radius)
                .text_color(theme.foreground)
                .child(status_dot(&agent_state.status, cx))
                .child(agent_label_text(agent_state));

            if ready {
                agent_row = agent_row
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.secondary_hover))
                    .on_click(cx.listener({
                        let agent_id = agent_id.clone();
                        move |this, _: &ClickEvent, _window, cx| {
                            this.toggle_draft(agent_id.clone(), cx)
                        }
                    }));
            } else {
                agent_row = agent_row.text_color(theme.muted_foreground);
            }
            group = group.child(agent_row);

            if ready {
                group = group.child(
                    gpui_component::h_flex().pl(px(22.0)).child(
                        Button::new(new_session)
                            .ghost()
                            .small()
                            .label(if drafting { "cancel" } else { "+ New session" })
                            .on_click(cx.listener({
                                let agent_id = agent_id.clone();
                                move |this, _: &ClickEvent, _window, cx| {
                                    this.toggle_draft(agent_id.clone(), cx)
                                }
                            })),
                    ),
                );
            }

            if drafting {
                group = group.child(
                    gpui_component::v_flex()
                        .pl(px(22.0))
                        .gap_1()
                        .child(Input::new(&self.draft_input))
                        .child(
                            gpui_component::h_flex()
                                .gap_1()
                                .child(
                                    Button::new(draft_confirm)
                                        .primary()
                                        .small()
                                        .label("Open")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                            this.confirm_draft(cx)
                                        })),
                                )
                                .child(
                                    Button::new(draft_cancel)
                                        .ghost()
                                        .small()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                            this.cancel_draft(cx)
                                        })),
                                ),
                        ),
                );
            }

            for session_id in &agent_state.sessions {
                let session_dom_id = dom_ids::session_row(session_id);
                let (label, exists) = session_label(session_id, threads);
                let selected = self.selected_session.as_ref() == Some(session_id);

                let mut row = div()
                    .id(session_dom_id)
                    .flex()
                    .items_center()
                    .px_2()
                    .py(px(3.0))
                    .pl(px(24.0))
                    .rounded(theme.radius)
                    .text_size(px(13.0));

                if exists {
                    row = row
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.secondary_hover))
                        .when(selected, |r| r.bg(theme.accent))
                        .on_click(cx.listener({
                            let session_id = session_id.clone();
                            move |this, _: &ClickEvent, _window, cx| {
                                this.select_session(session_id.clone(), cx)
                            }
                        }));
                } else {
                    row = row.text_color(theme.muted_foreground);
                }
                group = group.child(row.child(label));
            }

            panel = panel.child(group);
        }

        panel.into_any_element()
    }
}
