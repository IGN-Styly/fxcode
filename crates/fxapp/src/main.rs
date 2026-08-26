//! GPUI client. Thin by construction:
//!   events → fxproto::model folds → stores → cx.notify() → views re-render.

use gpui::*;
use gpui_component::{button::*, *};

// NOTE: modules are comment-scaffolds right now; uncomment each `mod` as it gains code.
// mod conn;
// mod store;
// mod theme;
// mod views;

fn main() {
    // Existing hello-world kept runnable until M0/M1 replaces it. Boot order to build:
    //
    // 1. gpui_platform::application().run(...)
    // 2. gpui_component::init(cx)                    // required before any component
    // 3. theme::init(cx)                             // register chosen themes
    // 4. cx.set_global(AppState::new())              // stores live here
    // 5. ConnectionManager::spawn(cx, server_url, token)   // starts reconnect loop;
    //    its events drive AppState folds via store/mod.rs dispatch
    // 6. open window → Root::new(views::WorkspaceView::new(...))
    //    → WorkspaceView routes between connect.rs screen and the docked workspace
    //      based on AppState.connection_status.
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

// TODO: delete HelloWorld once WorkspaceView exists (M0 exit criteria: latency badge
// replaces this screen).
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
