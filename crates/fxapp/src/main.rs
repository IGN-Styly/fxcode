//! GPUI client. Thin by construction:
//!   events → fxproto::model folds → stores → cx.notify() → views re-render.

use gpui::*;
use gpui_component::{button::*, *};

// NOTE (module status — accurate as of 2026-08-26, the old blanket claim was wrong):
//   these files exist ON DISK and some carry real top-level code already:
//     conn/mod.rs, conn/ws.rs, conn/cursor.rs, store/mod.rs, views/*   → live `pub mod`
//       decls + use-lists; declaring them here COMPILES today.
//     theme.rs                                                         → pure TODO
//       comments; declare it only when init()/set_theme gain bodies.
//   Uncomment each `mod` as it becomes compilable; never declare a file that only holds
//   TODO comments (zero-warning rule).
// mod conn;
// mod store;
// mod theme;
// mod views;

fn main() {
    // Existing hello-world kept runnable until Phase 6.4 replaces it. Boot order to build:
    //
    // 1. gpui_platform::application().run(...)
    // 2. gpui_component::init(cx)                    // required before any component
    // 3. theme::init(cx)                             // register chosen themes
    // 4. cx.set_global(AppState::default())          // stores live here; empty projections —
    //                                                //   real content arrives via events or
    //                                                //   SnapshotRequired, never a boot load
    // 5. ConnectionManager::spawn(cx, url, token)    // reads cursor::load() itself for url/
    //                                                //   token/last_seq seeds when args are
    //                                                //   None (fresh boot w/ remembered state);
    //                                                //   starts handshake + reconnect loop
    // 6. open window → Root::new(views::WorkspaceView::new(...))
    //    → WorkspaceView routes between connect.rs screen and the docked workspace
    //      based on AppState.conn_status (views/mod.rs routing table is normative).
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| HelloWorld);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}

// DELETE HelloWorld when ALL of: WorkspaceView renders in this window AND the status bar
// shows conn status + ws RTT (M0 exit check, impl.md Phase 6.4). Delete the struct, its
// Render impl, AND the `use gpui_component::{button::*…}` import it uniquely needs — no
// dead imports may survive it. It must not outlive Phase 6.
struct HelloWorld;
impl Render for HelloWorld {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .child("Hello, World!")
            .child(
                Button::new("ok")
                    .primary()
                    .label("Let's Go!")
                    .on_click(|_, _, _| println!("Clicked!")),
            )
    }
}
