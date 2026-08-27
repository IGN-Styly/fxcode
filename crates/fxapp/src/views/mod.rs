//! View layer. Rules:
//! - render(state) only — views never mutate projections directly
//! - user intents call ConnectionManager::send(Command)
//! - ElementIds derive from SessionId/ToolCallId/etc., NEVER list indexes
//!
//! ELEMENTID NAMESPACE REGISTRY (single place; extend per file only here):
//!   ("agent-row", AgentId) + "-new-session"/"-confirm"/"-cancel" suffixes
//!   ("session-row", SessionId) · ("msg", usize) · ("tool-call", ToolCallId)
//!   ("perm-option", OptionId) · "perm-session" · "perm-summary"
//!   "composer" · "stop-turn" · "jump-latest" · "plan-header" · "flow-list"
//!   "connect-url" / "connect-token" / "connect-submit" / "connect-error"
//! - extension (recorded): "connect-dismiss" — the chip-opened mid-session screen
//!   needs a back affordance the blueprint never sketched; likewise "send-turn".
//! - "statusbar" · "statusbar-chip" (extension: chip click target) ·
//!   "reconn-banner" · "dock-container".

pub mod connect;
pub mod message;
pub mod perms;
// pub mod setup; // M3 (Phase 9.3) — DetectAgents UX ships alongside pairing polish.
pub mod sidebar;
pub mod thread;
pub mod tool_call;

/// Centralized ElementId minting so the namespace registry above stays honest.
pub(crate) mod dom_ids {
    use fxproto::ids::{AgentId, OptionId, ToolCallId};
    use gpui::SharedString;

    pub const STATUSBAR: &str = "statusbar";
    pub const STATUSBAR_CHIP: &str = "statusbar-chip";
    pub const RECONNECT_BANNER: &str = "reconn-banner";
    pub const DOCK_CONTAINER: &str = "dock-container";
    pub const FLOW_LIST_ITEMS: &str = "flow-list";
    pub const PLAN_HEADER: &str = "plan-header";
    pub const JUMP_LATEST: &str = "jump-latest";
    pub const COMPOSER: &str = "composer";
    pub const STOP_TURN: &str = "stop-turn";
    pub const SEND_TURN: &str = "send-turn"; // discovery duplicate of Enter (recorded addition)
    pub const SIDEBAR: &str = "sidebar";
    pub const PERM_SESSION: &str = "perm-session";
    pub const PERM_SUMMARY: &str = "perm-summary";
    pub const CONNECT_URL: &str = "connect-url";
    pub const CONNECT_TOKEN: &str = "connect-token";
    pub const CONNECT_SUBMIT: &str = "connect-submit";
    pub const CONNECT_ERROR: &str = "connect-error";
    pub const CONNECT_DISMISS: &str = "connect-dismiss";
    // ── first-turn / t3-parity onboarding surface (views/thread.rs) ──
    pub const AGENT_PREV: &str = "agent-prev";
    pub const AGENT_NEXT: &str = "agent-next";
    pub const AGENT_CHIP: &str = "agent-chip";
    pub const CWD_INPUT: &str = "cwd-input";
    pub const SETUP_BANNER: &str = "setup-banner";
    // ── sidebar parity surface (t3code Sidebar.tsx) ──
    pub const SIDEBAR_SEARCH: &str = "sidebar-search";
    pub const SIDEBAR_NEW_THREAD: &str = "sidebar-new-thread";

    pub struct AgentRowIds {
        pub row: SharedString,
        pub new_session: SharedString,
        pub draft_confirm: SharedString,
        pub draft_cancel: SharedString,
    }

    pub fn agent_row(agent_id: &AgentId) -> AgentRowIds {
        let base = format!("agent-row-{agent_id}");
        AgentRowIds {
            row: SharedString::from(base.clone()),
            new_session: SharedString::from(format!("{base}-new-session")),
            draft_confirm: SharedString::from(format!("{base}-confirm")),
            draft_cancel: SharedString::from(format!("{base}-cancel")),
        }
    }

    pub fn session_row(session_id: &fxproto::ids::SessionId) -> SharedString {
        SharedString::from(format!("session-row-{session_id}"))
    }

    /// ("msg", usize) — positional index stays valid forever via append-only flow.
    pub fn msg_row(index: usize) -> SharedString {
        SharedString::from(format!("msg-{index}"))
    }

    pub fn tool_card(tool_call: &ToolCallId) -> SharedString {
        SharedString::from(format!("tool-call-{tool_call}"))
    }

    pub fn perm_option(option_id: &OptionId) -> SharedString {
        SharedString::from(format!("perm-option-{option_id}"))
    }
}

use std::path::Path;

use gpui::{
    App, AppContext as _, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use gpui_component::badge::Badge;
use gpui_component::status_bar::StatusBar;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _};

use fxproto::command::Command;
use fxproto::ids::{RequestId, SessionId};

use crate::conn::{ConnStatus, ConnectionManager};
use crate::store::AppState;
use crate::views::connect::{ConnectEvent, ConnectScreen};
use crate::views::sidebar::{SidebarEvent, SidebarView};
use crate::views::thread::ThreadView;

// ---------------------------------------------------------------------------
// Window root: WorkspaceView
// ---------------------------------------------------------------------------

/// Routes between connect.rs (no server yet / fatal / chip-opened) and the docked
/// workspace based on AppState.conn_status (normative routing table):
///
///   Ready → Dock layout: Sidebar | ThreadView | StatusBar + permission modal.
///   Connecting { attempt } → dock STILL renders (stale projections beat a blank
///       screen; they are only ever cleared by SnapshotRequired) with an amber banner
///       "reconnecting (attempt N)" (id "reconn-banner").
///   Disconnected { fatal: None } → ConnectScreen full-window.
///   Disconnected { fatal: Some(_) } → ConnectScreen WITH the mapped fatal error line
///       (connect.rs owns the string mapping); park until a human acts.
///
/// PERMISSION MODAL ORCHESTRATION (single trigger site lives here):
///   The render pass reconciles AppState.perms.pending transitions — empty→non-empty,
///   or dialog-just-closed while still non-empty — opening
///   perms::PermissionDialog { request_id: FIRST key } (BTreeMap + uuid v7 ⇒ oldest).
///   Substitution vs the sketch's cx.observe (recorded): app-level observers cannot
///   reach &mut Window, so this paint-pass reconciler is the same single-site contract.
///   Nothing else in the tree opens permission dialogs.
///
/// STATUS BAR (24px row, id "statusbar"; M0 exit artifact):
///   left  : conn chip dot+label from ConnStatus; click ⇒ ConnectScreen even when
///           connected.
///   center: active_session cwd file_name, else "no session".
///   right : pending-permission count badge (>0 amber) · ws RTT off ConnectionManager
///           rtt_ms mirror (M0 latency badge).
pub struct WorkspaceView {
    manager: gpui::Entity<ConnectionManager>,
    active_session: Option<SessionId>,
    sidebar: gpui::Entity<SidebarView>,
    thread: gpui::Entity<ThreadView>,
    connect_screen: Option<gpui::Entity<ConnectScreen>>,
    /// Status-chip override: show the connect screen even while connected until the
    /// user submits or dismisses (see connect_open_requested).
    render_connect_override: bool,
    shown_permission: Option<RequestId>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl WorkspaceView {
    pub fn new(
        manager: gpui::Entity<ConnectionManager>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = initial_active_session(cx);
        let manager_weak = manager.downgrade();

        let sidebar = cx.new(|cx| SidebarView::new(initial.clone(), window, cx));
        let thread =
            cx.new(|cx| ThreadView::new(manager_weak.clone(), initial.clone(), window, cx));

        let mut subscriptions = Vec::new();
        // store/mod.rs contract: globals mutate ⇒ observe_global fires ⇒ notify.
        // Without this the dock paints once and never reacts to the event log.
        subscriptions.push(
            cx.observe_global::<AppState>(|_this, cx: &mut gpui::Context<Self>| {
                cx.notify();
            }),
        );
        let thread_for_sub = thread.clone();
        subscriptions.push(cx.subscribe(&sidebar, move |this, _sidebar, event, cx| {
            match event {
                SidebarEvent::SessionSelected(session_id) => {
                    this.active_session = Some(session_id.clone());
                    thread_for_sub.update(cx, |view, cx| {
                        view.set_active_session(Some(session_id.clone()), cx)
                    });
                }
                SidebarEvent::NewSessionRequested { agent, cwd } => {
                    send_command(
                        &this.manager,
                        Command::NewSession {
                            agent: agent.clone(),
                            cwd: cwd.clone(),
                            mcp_servers: vec![],
                        },
                        cx,
                    );
                    // Optimistic focus: newer sessions sort last among uuid v7 keys;
                    // replay confirmation keeps the label fresh once known.
                    this.active_session = None;
                }
                SidebarEvent::NewThreadRequested => {
                    // t3 "New thread": back to the hero card; nothing on the wire —
                    // the session materializes lazily from the composer.
                    this.active_session = None;
                    thread_for_sub.update(cx, |view, cx| view.set_active_session(None, cx));
                }
            }
        }));

        Self {
            manager,
            active_session: initial,
            sidebar,
            thread,
            connect_screen: None,
            render_connect_override: false,
            shown_permission: None,
            _subscriptions: subscriptions,
        }
    }

    fn ensure_connect_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connect_screen.is_some() {
            return;
        }
        let status_snapshot = cx.global::<AppState>().conn_status.clone();
        let screen = cx.new(|cx| ConnectScreen::new(&status_snapshot, window, cx));
        let subscription = cx.subscribe(&screen, |this, screen, event, cx| {
            match event {
                ConnectEvent::Submitted { url, token } => {
                    // Replace the manager wholesale (exactly-one-manager rule).
                    this.manager = ConnectionManager::connect(cx, url.clone(), token.clone());
                    this.active_session = None;
                    this.render_connect_override = false;
                    this.close_connect_screen(&screen, cx);
                    cx.notify();
                }
                ConnectEvent::Dismissed => {
                    this.render_connect_override = false;
                    this.close_connect_screen(&screen, cx);
                }
            }
        });
        self.connect_screen = Some(screen);
        self._subscriptions.push(subscription);
    }

    fn close_connect_screen(
        &mut self,
        screen: &gpui::Entity<ConnectScreen>,
        cx: &mut Context<Self>,
    ) {
        // Dropping the entity drops its subscriptions with it (GPUI semantics).
        let _ = screen;
        self.connect_screen = None;
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Paint-pass plumbing
    // -----------------------------------------------------------------------

    fn reconcile_permissions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let oldest = cx.global::<AppState>().perms.pending.keys().next().cloned();

        match (&self.shown_permission, oldest.as_ref()) {
            (None, None) => {}
            (None, Some(next)) => {
                if !window.has_active_dialog(cx) {
                    PermissionTrigger {
                        request_id: next.clone(),
                        manager: self.manager.downgrade(),
                    }
                    .open(window, cx);
                }
                self.shown_permission = Some(next.clone());
            }
            (Some(shown), None) => {
                if window.has_active_dialog(cx) && is_gone(cx, shown) {
                    window.close_dialog(cx);
                }
                self.shown_permission = None;
            }
            (Some(_), Some(_)) => {
                // Still the head ask; rotation happens after close in the (None,..) arm.
            }
        }
    }
}

struct PermissionTrigger {
    request_id: RequestId,
    manager: gpui::WeakEntity<ConnectionManager>,
}

impl PermissionTrigger {
    fn open(self, window: &mut Window, cx: &mut App) {
        perms::PermissionDialog {
            request_id: self.request_id,
            manager: self.manager,
        }
        .open(window, cx);
    }
}

fn is_gone(cx: &App, id: &RequestId) -> bool {
    !cx.global::<AppState>().perms.pending.contains_key(id)
}

fn initial_active_session(cx: &Context<WorkspaceView>) -> Option<SessionId> {
    // Prefer the newest transcript available client-side; session ids are uuid v7 so
    // BTreeMap back() == chronological latest.
    cx.global::<AppState>()
        .threads
        .threads
        .keys()
        .next_back()
        .cloned()
}

pub(crate) fn send_command(
    manager: &gpui::Entity<ConnectionManager>,
    command: Command,
    cx: &mut App,
) {
    match manager.update(cx, |manager, cx| manager.send(command, cx)) {
        Ok(task) => task.detach(),
        Err(error) => tracing::debug!(?error, "workspace intent rejected locally"),
    }
}

fn conn_label(conn_status: &ConnStatus) -> SharedString {
    match conn_status {
        ConnStatus::Ready => "Ready".into(),
        ConnStatus::Connecting { attempt } => format!("Connecting ·{attempt}").into(),
        ConnStatus::Disconnected { fatal: Some(_) } => "Fatal".into(),
        ConnStatus::Disconnected { fatal: None } => "Offline".into(),
    }
}

fn file_label(cwd: &Path) -> String {
    cwd.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.display().to_string())
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // THE single permission-modal trigger site (see struct doc). Runs before the
        // route reads state below.
        self.reconcile_permissions(window, cx);

        let theme = cx.theme();
        let AppState {
            conn_status,
            threads,
            ..
        } = cx.global::<AppState>();

        if self.render_connect_override || matches!(conn_status, ConnStatus::Disconnected { .. }) {
            let bg = {
                let t = cx.theme();
                t.background
            }; // copy token, drop borrow
            #[allow(unused_variables)]
            let _ = &bg;
            self.ensure_connect_screen(window, cx);
            let screen = self
                .connect_screen
                .clone()
                .expect("ensure_connect_screen populated it");
            return div().size_full().bg(bg).child(screen).into_any_element();
        }

        let mut root = gpui_component::v_flex().size_full();

        if let ConnStatus::Connecting { attempt } = conn_status {
            root = root.child(
                div()
                    .id(dom_ids::RECONNECT_BANNER)
                    .w_full()
                    .px_2()
                    .py(px(3.0))
                    .bg(theme.warning.opacity(0.15))
                    .text_color(theme.warning)
                    .text_size(px(12.0))
                    .child(SharedString::from(format!(
                        "reconnecting (attempt {attempt})"
                    ))),
            );
        }

        // ---- status bar ----------------------------------------------------------
        let session_line: SharedString = self
            .active_session
            .as_ref()
            .and_then(|sid| threads.threads.get(sid))
            .map(|thread_state| SharedString::from(file_label(&thread_state.cwd)))
            .unwrap_or_else(|| "no session".into());

        let pending_count = cx.global::<AppState>().perms.pending.len();
        let rtt_ms = self.manager.read(cx).rtt_ms();
        let rtt_label: SharedString = if rtt_ms == 0 {
            "ws —".into()
        } else {
            format!("ws {rtt_ms} ms").into()
        };

        let chip = div()
            .id(dom_ids::STATUSBAR_CHIP)
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.connect_open_requested(cx);
            }))
            .child(conn_dot(conn_status, cx))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.foreground)
                    .child(conn_label(conn_status)),
            );

        let status_bar = StatusBar::new()
            .left(
                div()
                    .id(dom_ids::STATUSBAR)
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(chip),
            )
            .child(session_line)
            .right(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Badge::new()
                            .count(pending_count)
                            .small()
                            .color(theme.warning),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.muted_foreground)
                            .child(rtt_label),
                    ),
            )
            .h(px(24.0));

        // ---- docked body: fixed 2-pane split, sidebar resizable ------------------
        let body = gpui_component::h_resizable(dom_ids::DOCK_CONTAINER)
            .child(
                gpui_component::resizable_panel()
                    .size(px(240.0))
                    .size_range(px(160.0)..px(400.0))
                    .child(self.sidebar.clone()),
            )
            .child(gpui_component::resizable_panel().child(self.thread.clone()));

        root = root
            .child(div().flex_1().min_h_0().child(body))
            .child(status_bar);

        root.into_any_element()
    }
}

fn conn_dot(conn_status: &ConnStatus, cx: &App) -> gpui::AnyElement {
    let theme = cx.theme();
    let color = match conn_status {
        ConnStatus::Ready => theme.success,
        ConnStatus::Connecting { .. } => theme.warning,
        ConnStatus::Disconnected { .. } => theme.danger,
    };
    div()
        .size(px(7.0))
        .rounded_full()
        .bg(color)
        .into_any_element()
}

impl WorkspaceView {
    fn connect_open_requested(&mut self, cx: &mut Context<Self>) {
        // Chip-opened screen override; dropping the LIVE manager would be rude — show
        // the form until Submitted/Dismissed flips the flag back off.
        self.render_connect_override = true;
        cx.notify();
    }
}
