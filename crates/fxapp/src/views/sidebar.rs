//! Sidebar: agent list w/ status dots + session list + "New session" affordance.

use std::path::PathBuf;

use gpui::{
    App, AppContext as _, ClickEvent, Context, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
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
    /// t3 "New thread": clear the active thread so the hero composer shows.
    /// No protocol traffic — the session materializes from the composer.
    NewThreadRequested,
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
    /// t3 header Search box (Sidebar.tsx aria "Search threads").
    search_input: gpui::Entity<InputState>,
    /// Lowercased live filter query; empty = show all.
    query: String,
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
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let _search_sub = cx.subscribe_in(
            &search_input,
            window,
            |this, state, event: &InputEvent, _window, cx| {
                if let InputEvent::Change = event {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            },
        );
        Self {
            selected_session,
            open_draft_for: None,
            draft_input,
            search_input,
            query: String::new(),
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
        let query_lc = self.query.to_lowercase();

        // ── header: Search + New-thread compose button (t3 Sidebar.tsx) ──
        let header = gpui_component::h_flex()
            .w_full()
            .gap_1()
            .px_1()
            .child(
                div()
                    .id(dom_ids::SIDEBAR_SEARCH)
                    .flex_1()
                    .child(Input::new(&self.search_input)),
            )
            .child(
                Button::new(dom_ids::SIDEBAR_NEW_THREAD)
                    .ghost()
                    .small()
                    .label("＋")
                    .tooltip("New thread")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.selected_session = None;
                        cx.emit(SidebarEvent::NewThreadRequested);
                        cx.notify();
                    })),
            );

        let mut panel = gpui_component::v_flex()
            .id(dom_ids::SIDEBAR)
            .h_full()
            .overflow_y_scroll()
            .p_2()
            .gap_2()
            .bg(theme.background)
            .child(header);

        // ── PROJECTS section (t3 groups threads by project; project == cwd) ──
        panel = panel.child(
            gpui_component::h_flex()
                .gap_1()
                .px_1()
                .items_center()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::Folder).xsmall())
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("All projects"),
                ),
        );

        let mut matched_any = false;
        for (session_id, thread) in &threads.threads {
            let (title, exists) = session_label(session_id, threads);
            if !exists {
                continue; // "(loading)" placeholder rows add noise to the rail
            }
            let path_text = thread.cwd.display().to_string();
            if !query_lc.is_empty()
                && !title.to_lowercase().contains(&query_lc)
                && !path_text.to_lowercase().contains(&query_lc)
            {
                continue;
            }
            matched_any = true;
            let selected = self.selected_session.as_ref() == Some(session_id);
            let mut row = div()
                .id(dom_ids::session_row(session_id))
                .flex()
                .flex_col()
                .px_2()
                .py(px(5.0))
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|st| st.bg(theme.secondary_hover))
                .when(selected, |r| r.bg(theme.accent))
                .on_click(cx.listener({
                    let session_id = session_id.clone();
                    move |this, _: &ClickEvent, _window, cx| {
                        this.select_session(session_id.clone(), cx)
                    }
                }))
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .child(path_text),
                );
            let _ = &mut row;
            panel = panel.child(row);
        }
        if threads.threads.is_empty() {
            panel = panel.child(
                div()
                    .px_2()
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child("No threads yet — start typing in the composer."),
            );
        } else if !matched_any && !query_lc.is_empty() {
            panel = panel.child(
                div()
                    .px_2()
                    .text_size(px(12.0))
                    .text_color(theme.muted_foreground)
                    .child("No matches."),
            );
        }

        // ── AGENTS section: processes + their per-agent session affordances ──
        panel = panel.child(
            div()
                .pt_2()
                .px_1()
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child("AGENTS"),
        );

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

            panel = panel.child(group);
        }

        panel.into_any_element()
    }
}
