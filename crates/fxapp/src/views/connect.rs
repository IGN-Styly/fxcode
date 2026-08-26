//! Connect screen: server address + pairing token, first-run UX.

use gpui::{
    AppContext as _, ClickEvent, Context, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Sizable as _};

use crate::conn::{ConnStatus, FatalError, cursor, ws::normalize_url};
use crate::views::dom_ids;

// DATA SOURCE: local only — cursor.rs ClientState + ws.rs normalize_url. No AppState
// reads for projections here.
//
// INTENT → ACTIONS on "Connect" click (id "connect-submit"), strictly in order:
//   1. conn::ws::normalize_url(url_input) — Err ⇒ set error line from UrlError variant,
//      STOP before any dial or persistence (never remember a URL that failed validation).
//   2. persist ClientState { server_url, token, last_seq untouched } via cursor::save();
//      Err ⇒ error line "could not save client state" + stop (disk problems are user-
//      visible rather than silently dropped credentials).
//   3. EMIT ConnectSubmitted — WorkspaceView performs ConnectionManager::connect
//      (substitution note: spawn-from-the-screen is routed through the parent so there
//      is exactly ONE owner of the manager handle per process; status transitions render
//      back through WorkspaceView routing either way).
//
// STATUS / ERROR LINE MAPPING (exact strings; close-string → human text):
//   Connecting { attempt }        ⇒ "Connecting (attempt N)…" — form dims but stays
//                                    editable so a wrong token can be fixed mid-retry;
//                                    clicking Connect again restarts cleanly.
//   Disconnected { fatal: None }  ⇒ "Not connected."
//   Dial/transport failures       ⇒ surfaced by the reconnect banner in WorkspaceView
//                                    (attempts continue) — see views/mod.rs.
//   FatalError::AuthFailed        ⇒ AUTH_FAILED_TEXT          ← "auth_failed"
//   FatalError::ProtocolVersion   ⇒ PROTOCOL_VERSION_TEXT    ← "protocol_version"
//   Fatal states PARK the reconnect loop (conn/mod.rs), so the submit button stays
//   enabled and highlighted as the way forward.

pub const AUTH_FAILED_TEXT: &str =
    "Pairing token rejected. Paste the token fxserver printed on first boot.";
pub const PROTOCOL_VERSION_TEXT: &str =
    "Protocol version mismatch — update fxapp and fxserver together.";
const DEFAULT_URL_PREFILL: &str = "ws://127.0.0.1:8949";

pub fn fatal_message(fatal: FatalError) -> &'static str {
    match fatal {
        FatalError::AuthFailed => AUTH_FAILED_TEXT,
        FatalError::ProtocolVersion => PROTOCOL_VERSION_TEXT,
    }
}

#[derive(Clone, Debug)]
pub enum ConnectEvent {
    Submitted { url: String, token: String },
    Dismissed,
}

pub struct ConnectScreen {
    url_input: gpui::Entity<InputState>, // prefill: cursor.load().server_url else DEFAULT twin
    token_input: gpui::Entity<InputState>, // prefill: cursor.load().token (echo-off Input)
    error: Option<String>,
    dismissible: bool, // rendered from the status-chip path mid-session
}

impl EventEmitter<ConnectEvent> for ConnectScreen {}

impl ConnectScreen {
    pub fn new(status_snapshot: &ConnStatus, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let stored = cursor::load(&cursor::default_dir());

        let mut screen = Self {
            // Prefill remembered server url else the default-port twin of fxserver's
            // ifaddr.rs DEFAULT_PORT.
            url_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(DEFAULT_URL_PREFILL)
                    .default_value(
                        stored
                            .server_url
                            .clone()
                            .unwrap_or_else(|| DEFAULT_URL_PREFILL.into()),
                    )
            }),
            token_input: cx.new(|cx| {
                // Echo-off input for the pairing token.
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("pairing token")
                    .default_value(stored.token.clone().unwrap_or_default())
            }),
            error: None,
            dismissible: !matches!(status_snapshot, ConnStatus::Disconnected { fatal: None }),
        };
        if let ConnStatus::Disconnected { fatal: Some(fatal) } = status_snapshot {
            screen.error = Some(fatal_message(*fatal).to_string());
        }
        screen
    }

    fn submit(&mut self, _: &ClickEvent, cx: &mut Context<Self>) {
        let raw_url = self.url_input.read(cx).value().trim().to_string();
        let token = self.token_input.read(cx).value().to_string();

        // 1. validate BEFORE any dial or persistence.
        let url = match normalize_url(&raw_url) {
            Ok(url) => url.to_string(),
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };

        // 2. persist immediately (last_seq untouched — load fresh and overwrite only
        //    identity fields).
        let dir = cursor::default_dir();
        let mut stored = cursor::load(&dir);
        let dirty = stored.server_url.as_deref() != Some(url.as_str())
            || stored.token.as_deref() != Some(token.as_str());
        if dirty {
            stored.server_url = Some(url.clone());
            stored.token = Some(token.clone());
            if let Err(error) = stored.save(&dir) {
                self.error = Some("could not save client state".to_string());
                tracing::warn!(error = %error, "client state save failed on connect");
                cx.notify();
                return;
            }
        }
        self.error = None;

        // 3. hand off to the parent, which swaps the manager entity wholesale.
        cx.emit(ConnectEvent::Submitted { url, token });
        cx.notify();
    }
}

impl gpui::Focusable for ConnectScreen {
    fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.url_input.read(cx).focus_handle(cx)
    }
}

impl Render for ConnectScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let connecting = matches!(
            cx.global::<crate::store::AppState>().conn_status,
            ConnStatus::Connecting { .. }
        );

        let mut panel = gpui_component::v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .gap_3();

        panel = panel.child(
            div()
                .text_size(px(20.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child("Connect to fxserver"),
        );

        let attempt_line = match cx.global::<crate::store::AppState>().conn_status {
            ConnStatus::Connecting { attempt } => Some(format!("Connecting (attempt {attempt})…")),
            ConnStatus::Disconnected { fatal: None } => Some("Not connected.".to_string()),
            _ => None,
        };

        let mut form = gpui_component::v_flex()
            .w(px(420.0))
            .gap_2()
            .when(connecting, |form| form.opacity(0.75)); // dimmed but EDITABLE

        // Blueprint ids attach to wrapper rows around the inputs (Input itself keys
        // off its state entity).
        form = form
            .child(
                div()
                    .id(dom_ids::CONNECT_URL)
                    .child(Input::new(&self.url_input)),
            )
            .child(
                div()
                    .id(dom_ids::CONNECT_TOKEN)
                    .child(Input::new(&self.token_input)),
            );

        form = form.child(
            Button::new(dom_ids::CONNECT_SUBMIT)
                .primary()
                .label(if connecting { "Reconnect" } else { "Connect" })
                .on_click(
                    cx.listener(|this, event: &ClickEvent, _window, cx| this.submit(event, cx)),
                ),
        );

        let error_line = self.error.clone().or(attempt_line);
        if let Some(line) = error_line {
            panel = panel.child(
                div()
                    .id(dom_ids::CONNECT_ERROR)
                    .text_size(px(12.5))
                    .text_color(theme.danger)
                    .child(line),
            );
        }

        if self.dismissible {
            panel = panel.child(
                Button::new(dom_ids::CONNECT_DISMISS)
                    .ghost()
                    .label("← back to workspace")
                    .small()
                    .on_click(cx.listener(|_this, _: &ClickEvent, _window, cx| {
                        cx.emit(ConnectEvent::Dismissed);
                        cx.notify();
                    })),
            );
        }

        panel.child(form)
    }
}
