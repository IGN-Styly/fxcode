//! GPUI client. Thin by construction:
//!   events → fxproto::model folds → stores → cx.notify() → views re-render.
//!
//! BOOT ORDER (locked; mirrors main.rs TODO blueprint):
//!
//! 1. gpui_platform::application().run(…)
//! 2. gpui_component::init(cx) — required before any component
//! 3. theme::init(cx) — dark-first registration
//! 4. cx.set_global(AppState…) — stores live here; empty projections. Real content
//!    arrives via events or SnapshotRequired, NEVER a boot load.
//! 5. ConnectionManager::spawn(cx, url, token) — reads cursor::load() itself for
//!    url/token/last_seq seeds when args are None (fresh boot w/ remembered state),
//!    starting the handshake + reconnect loop.
//! 6. open window → Root::new(WorkspaceView…)) — routing table in views/mod.rs swaps
//!    between connect screen and dock by conn_status.

use gpui::*;

mod conn;
mod store;
mod theme;
mod views;

fn main() {
    // Optional CLI overrides for smoke-testing without editing client-state.json:
    // `fxapp [url [token]]`. Absent args fall through to remembered state (step 5).
    let arg_url = std::env::args().nth(1);
    let arg_token = std::env::args().nth(2);

    gpui_platform::application().run(move |cx| {
        // 2 — component library first: it registers ThemeRegistry + all component
        // actions keybindings underneath us.
        gpui_component::init(cx);

        // 3 — dark-first theme choice on top of the stock registry set.
        theme::init(cx);

        // 4 — empty projections + idle conn status; the manager mirrors status into
        // this global from now on.
        cx.set_global(store::AppState::default());

        // 5 — connection lifecycle starts before any window exists so the FIRST frame
        // already reflects Connecting / fatal / remembered-state realities.
        let manager = conn::ConnectionManager::spawn(cx, arg_url, arg_token);

        // 6 — window root routes between ConnectScreen and the docked workspace.
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
            ..Default::default()
        };
        cx.spawn(async move |cx| {
            cx.open_window(window_options, |window, cx| {
                let workspace = cx.new(|cx| views::WorkspaceView::new(manager.clone(), window, cx));
                cx.new(|cx| gpui_component::Root::new(workspace, window, cx))
            })
            .expect("failed to open fxapp window");
        })
        .detach();
    });
}
