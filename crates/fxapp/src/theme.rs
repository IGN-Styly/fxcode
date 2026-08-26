//! Theme selection + registration (gpui-component ThemeRegistry).
//!
//! gpui-component ships 20+ JSON themes; tokens read via ActiveTheme trait
//!

// TODO:
//
// pub fn init(cx: &mut App);
//   - register built-in themes from the crate's themes/ dir (see gpui-component docs)
//   - pick default (dark) + persist user choice later (~/.fxcode/client-state.json)
//
// pub fn set_theme(cx, name: &str);   // hot-swap via ThemeRegistry, notify all windows
//
// NOTE: no CSS cascade exists — component variants (.primary(), .ghost()) are builder
// methods over these tokens. Keep custom colors OUT of views; add tokens here if the
// stock palette misses something.
