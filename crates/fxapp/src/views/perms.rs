//! Permission modal: renders AppState.perms.pending entries.

use fxproto::command::Command;
use fxproto::event::PermissionOptionKind;
use fxproto::ids::RequestId;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::Dialog;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _};

use crate::conn::ConnectionManager;
use crate::store::AppState;
use crate::views::dom_ids;

// ORCHESTRATION (WorkspaceView owns the trigger; this file owns ONE dialog):
//   The WorkspaceView render pass reconciles AppState.perms.pending transitions —
//   empty → non-empty (or "previous dialog answered/closed but still non-empty") with
//   no active dialog ⇒ opens THIS modal for the FIRST key, which is the OLDEST ask
//   (BTreeMap + uuid v7 ⇒ chronological). Substitution note (recorded): the sketch's
//   `cx.observe` trigger cannot reach a `&mut Window` from an app-level observer, so
//   the same single-site contract runs through WorkspaceView's paint pass instead.
//
// RENDER reads the pending entry LIVE every construction:
//     session id · summary.title (ToolCallSummary) · option buttons grouped:
//       AllowOnce | AllowAlways under "Allow"   ·   RejectOnce | RejectAlways under "Deny".
//   Button click ⇒ send(Command::PermissionResponse { request_id, option_id }) THEN
//   close the dialog. The fold chain clears `pending` when PermissionResolved lands;
//   this view never optimistically mutates PermsState (single-writer rule: folds only).
//
// EDGE CASES fall out of the fold + reconciler with zero imperative code:
//   - multiple pendings queued ⇒ sequential showings (next oldest after each close).
//   - PermissionResolved arriving EXTERNALLY (turn cancelled server-side, watchdog)
//     removes the map entry behind our back ⇒ get() returns None ⇒ one empty body
//     frame and the reconciler closes it — auto-dismiss IS the "cancelled" UX; there
//     is deliberately no error state because disappearing permissions are
//     protocol-normal, not failures.

pub struct PermissionDialog {
    pub request_id: RequestId,
    /// Responses ride the same manager every other intent uses.
    pub manager: gpui::WeakEntity<ConnectionManager>,
}

impl PermissionDialog {
    pub fn open(self, window: &mut Window, cx: &mut App) {
        let Self {
            request_id,
            manager,
        } = self;
        let request_id = request_id.clone();

        window.open_dialog(cx, move |_dialog_builder, _window, cx| {
            build_body(&request_id, manager.clone(), cx)
        });
    }
}

fn build_body(
    request_id: &RequestId,
    manager: gpui::WeakEntity<ConnectionManager>,
    cx: &mut App,
) -> Dialog {
    // Copy out tokens before any &mut use of cx.
    let muted_foreground = cx.theme().muted_foreground;
    // LIVE read: if the entry vanished between trigger and paint this renders the
    // empty body and the reconciler closes on the next pass.
    let pending = cx
        .global::<AppState>()
        .perms
        .pending
        .get(request_id)
        .cloned();

    let base = Dialog::new(cx).w(px(420.0));

    let Some(pending) = pending else {
        // STATE: entry-vanished — one-frame empty body, never an error screen.
        return base.child(div().child(""));
    };

    let option_button = |manager: &gpui::WeakEntity<ConnectionManager>,
                         request_id: &RequestId,
                         option: &fxproto::event::PermissionOption| {
        let request_id = request_id.clone();
        let dom_id = dom_ids::perm_option(&option.option_id);
        let manager = manager.clone();
        let option_id = option.option_id.clone();
        Button::new(dom_id)
            .secondary()
            .label(SharedString::from(option.name.clone()))
            .small()
            .on_click(move |_event, window, cx| {
                if let Ok(Ok(task)) = manager.update(cx, |manager, cx| {
                    manager.send(
                        Command::PermissionResponse {
                            request_id: request_id.clone(),
                            option_id: option_id.clone(),
                        },
                        cx,
                    )
                }) {
                    task.detach();
                } else {
                    tracing::warn!("permission response could not be sent");
                }
                // Close AFTER handing off (blueprint's exact ordering).
                window.close_dialog(cx);
            })
    };

    let group_for = |label: &'static str, items: Vec<gpui::AnyElement>| {
        gpui_component::v_flex()
            .flex_1()
            .gap_1()
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(muted_foreground)
                    .child(label),
            )
            .children(items)
    };

    let mut allow_buttons = Vec::new();
    let mut deny_buttons = Vec::new();
    for option in &pending.options {
        match option.kind {
            PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways => {
                allow_buttons.push(option_button(&manager, request_id, option).into_any_element());
            }
            PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways => {
                deny_buttons.push(option_button(&manager, request_id, option).into_any_element());
            }
        }
    }

    base.child(
        gpui_component::v_flex()
            .gap_2()
            .child(
                div()
                    .id(dom_ids::PERM_SESSION)
                    .child(SharedString::from(format!("session {}", pending.session))),
            )
            .child(
                div()
                    .id(dom_ids::PERM_SUMMARY)
                    .text_size(px(14.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(pending.summary.title.clone()),
            )
            .when(!pending.options.is_empty(), |body| {
                body.child(
                    gpui_component::h_flex()
                        .items_start()
                        .gap_3()
                        .child(group_for("Allow", allow_buttons))
                        .child(group_for("Deny", deny_buttons)),
                )
            }),
    )
}
