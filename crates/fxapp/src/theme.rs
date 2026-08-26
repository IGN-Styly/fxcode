//! Theme selection + registration (gpui-component ThemeRegistry).
//!
//! gpui-component ships its built-in theme set registered as part of
//! `gpui_component::init(cx)` (which must run BEFORE this module's [`init`]);
//! tokens are read via the `ActiveTheme` trait (`cx.theme().primary`, …) so
//! views never hardcode colors.
//!
//! NOTE: no CSS cascade exists — component variants (.primary(), .ghost()) are
//! builder methods over these tokens. Keep custom colors OUT of views; add
//! tokens here if the stock palette misses something.

use std::rc::Rc;

use gpui::App;
use gpui_component::{ActiveTheme as _, Theme, ThemeConfig, ThemeMode, ThemeRegistry};

/// Dark-first boot choice for fxapp. The registry still holds every default
/// theme, so any light theme remains reachable through [`set_theme`].
pub const DEFAULT_THEME_MODE: ThemeMode = ThemeMode::Dark;

/// Register/pick themes at boot. Called once after gpui_component::init (see
/// main.rs boot order) — before the window opens, so the first frame is dark.
///
/// Persistence of the user choice lands with Phase 9.2 (~/.fxcode/
/// client-state.json gains a `theme` key owned by this module; cursor.rs
/// reserves the JSON file today).
pub fn init(cx: &mut App) {
    // The stock palette + "Default Light/Dark" configs are already in the
    // registry at this point; picking the mode wires the global Theme.
    Theme::change(DEFAULT_THEME_MODE, None, cx);
}

/// Current active theme name (registry key), for persistence later (Phase 9.2
/// owns the JSON key). Unused until then — kept as the single accessor site.
#[allow(dead_code)]
pub fn current_theme_name(cx: &App) -> String {
    cx.theme().theme_name().to_string()
}

/// Hot-swap to a named theme from the registry and notify all windows. Safe to
/// call repeatedly; unknown names warn instead of crashing the caller.
#[allow(dead_code)] // hot-swap entrypoint lands with the 9.2 settings UI
pub fn set_theme(cx: &mut App, name: &str) {
    let Some(config): Option<Rc<ThemeConfig>> =
        ThemeRegistry::global(cx).themes().get(name).cloned()
    else {
        tracing::warn!(theme = %name, "requested theme is not registered");
        return;
    };
    apply_config(config, cx);
    cx.refresh_windows();
}

#[allow(dead_code)]
fn apply_config(config: Rc<ThemeConfig>, cx: &mut App) {
    let mode = config.mode;
    // apply_config re-points the light/dark slot of the chosen config, so the
    // selection survives later system-appearance follow-ups too.
    Theme::global_mut(cx).apply_config(&config);
    Theme::change(mode, None, cx);
}
