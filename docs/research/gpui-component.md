# gpui-component: a primer for Next.js/React developers

*Researched 2026-08-26 against primary sources. This repo (`fxcode`) already uses `gpui` + `gpui-component` from git — see `Cargo.toml` and `src/main.rs`.*

## What it is

**GPUI** is the GPU-accelerated Rust UI framework behind the [Zed editor](https://zed.dev) — "a hybrid immediate and retained mode, GPU accelerated, UI framework for Rust". Each frame, GPUI calls `render()` on your window's root view; you build a tree of styled elements, and GPUI turns it into pixels (Metal/Vulkan/DirectX via platform backends). It's pre-1.0 and tracks Zed's `main`. (github.com/zed-industries/zed crates/gpui README)

**gpui-component** is Longbridge's component library *on top of* GPUI: "60+ UI components" — forms, tables, dialogs, charts, dock layouts, a code editor. The library itself draws the shadcn analogy explicitly: GPUI ≈ HTML + Tailwind, its `gpui-base` crate ≈ Base UI (unstyled behavior), `gpui-component` ≈ shadcn's styled layer. So for you: **gpui-component ≈ shadcn/ui**, batteries included. (github.com/longbridge/gpui-component README)

## Mental model translation table

| React/Next.js concept | GPUI/gpui-component equivalent | Notes |
|---|---|---|
| JSX / TSX | View builders: `div().flex().gap_2().child(...)` | Method chain instead of markup; compiled Rust |
| Component (`function Comp()`) | Struct implementing `Render`, with a `render()` method | `render(&mut self, window, cx)` re-runs each frame |
| Props | Plain struct fields, set in the constructor | No prop drilling machinery; pass what you need |
| `useState` / store (Zustand) | `Entity<T>` — state owned by the `App`, accessed via smart-pointer handle | `cx.new(\|cx\| MyState::new())`; like an `Rc` |
| Re-render on state change | **Manual**: mutate state, then call `cx.notify()` | No automatic dependency tracking |
| `useEffect` + subscription | `cx.observe(&entity, \|_, cx\| ...)` / `cx.subscribe(...)` for emitted events | Closures called when `cx.notify()` fires on the observed entity |
| `onClick={handler}` | `.on_click(\|event, window, cx\| { ... })` | Handlers are Rust closures taking context params |
| Tailwind classes | Styling methods: `.p_2()`, `.text_sm()`, `.bg(...)`, `.rounded()` | Deliberately Tailwind-like naming; rem-based so zoom works |
| `key={}` in lists | `ElementId` — every element needs one: `Button::new("ok")` | Use domain-derived ids, not list indexes |
| Context API | Global state on `App` via `cx.set_global` / `cx.global::<T>()` | Typed globals, not provider trees |
| shadcn/ui | `gpui_component` widgets (`Button`, `Dialog`, …) | Same design lineage; UI design based on shadcn/ui |

Key difference from React: **no virtual DOM, no diffing, no hooks rules**. GPUI calls your `render()` fresh each frame and rebuilds the element tree ("hybrid immediate/retained"); retained state lives in `Entity<T>`s that survive across frames. Rust ownership replaces hook rules: closures capturing state must satisfy the borrow checker, which is why handlers receive `&mut Window, &mut Context` parameters instead of closing over whatever they like.

## Stateful vs stateless components

Two patterns (longbridge.github.io/gpui-component getting-started):

- **Stateless** (`RenderOnce`): `Button`, `Checkbox`, `Tag`, `Badge`, `Icon`… configure inline during render.
- **Stateful**: `Input`, `Select`, `DataTable`, `List`, `Tree`… hold their own state as an `Entity<T>` that *you create and keep in your view struct*, then render by passing the handle:

```rust
struct MyView {
    input: Entity<InputState>, // like holding a controlled-input store
}
// in constructor:
let input = cx.new(|cx| InputState::new(window, cx).default_value("Hello"));
// in render():
Input::new(&self.input)
```

## What's in the box

Verified against the docs index (longbridge.github.io/gpui-component llms.txt):

- **Inputs/forms**: Input, Textarea, NumberInput, OtpInput, Select, Combobox, Checkbox, Switch, Radio, Slider, DatePicker, Calendar, ColorPicker, Rating, Stepper, Form
- **Display/feedback**: Button, DropdownButton, Toggle, Icon (Lucide-style SVGs), Badge, Tag, Avatar, Label, Kbd, Alert, Spinner, Skeleton, Progress, Tooltip, HoverCard, Clipboard
- **Overlays**: Dialog, AlertDialog, Sheet, Notification (toasts), Popover, Menu (context menus), FocusTrap
- **Navigation/layout**: Tabs, Sidebar, TitleBar, Breadcrumb, Pagination, Accordion, Collapsible, GroupBox, Resizable (split panes), Scrollable, StatusBar, Dock (draggable panels, serializable layout), Settings
- **Data display**: DataTable (virtual scrolling, resizable/sortable columns, huge row counts), Table (simple), VirtualList, List, Tree, DescriptionList, TextView (Markdown/HTML rendering)
- **Charts/plots**: Chart (line, bar, area, pie, radar, candlestick, sankey), Plot (low-level custom drawing)
- **Editor**: code editor with Tree-sitter highlighting, gutter, folding; LSP support shown in examples

## Theming

Think **CSS variables / design tokens, but as Rust structs plus JSON files** (longbridge.github.io/gpui-component theme docs):

- Read semantic tokens anywhere via the `ActiveTheme` trait: `cx.theme().primary`, `.background`, `.foreground`, `.border`, `.muted`.
- Full resolved tokens under `cx.theme().tokens.*` (e.g. `button_primary` with `.color` and `.background` — backgrounds can even be CSS-style `linear-gradient(...)` strings).
- 20+ built-in themes ship as JSON files in the repo's `themes/` folder; a `ThemeRegistry` can load and hot-reload them from disk (`ThemeRegistry::watch_dir`).
- There is no CSS cascade or specificity — a token lookup returns one value; per-component overrides are builder methods (`.primary()`, `.ghost()`, `.danger()`, sizes `.small()/.medium()/.large()/.xsmall()`).

## Minimal example

This is the actual quick-start (identical in the README and in this repo's `src/main.rs`):

```rust
use gpui::*;
use gpui_component::{button::*, *};

pub struct HelloWorld;
impl Render for HelloWorld {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()                       // like a <div>
            .v_flex()               // flex-direction: column
            .gap_2()
            .size_full()            // w-full h-full
            .items_center()
            .justify_center()
            .child("Hello, World!")
            .child(
                Button::new("ok")   // ElementId
                    .primary()      // variant, like className="bg-primary"
                    .label("Let's Go!")
                    .on_click(|_, _, _| println!("Clicked!")),
            )
    }
}

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx); // required first: registers themes/globals

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| HelloWorld);          // root view Entity
                cx.new(|cx| Root::new(view, window, cx))    // Root wraps every window
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
```

## Web-dev gotchas

- **No cascade**: no CSS files, selectors, inheritance games, or media queries. All styling is builder methods computed in Rust; responsive layout = flexbox constraints, not breakpoints.
- **Layout is Taffy** (a Flexbox engine, currently pinned to taffy 0.13 in Zed's tree). Grid exists but flexbox is the default mental model.
- **You must request re-renders**: forgetting `cx.notify()` after mutating state means the UI won't update — there's no automatic render-on-set.
- **Every element needs an id** where events/focus matter; unstable ids break state (like bad React keys).
- **Fonts/text shaping differ per platform**: font-kit on macOS/Linux, DirectWrite on Windows — test text rendering cross-platform; CJK works but needs font setup.
- **Platform backends are compile-time features** of `gpui_platform` (`font-kit`, `wayland`, `x11`) rather than runtime browser differences. WASM is supported (gpui_web).
- **Overlays aren't DOM portals**: dialogs/toasts go through `WindowExt` methods (`window.open_dialog`, `window.push_notification`).

## Ecosystem status & caveats

- **Maintainer**: [Longbridge](https://longbridge.com) (HK brokerage) — extracted from their shipped commercial app, Longbridge Pro. ~13.5k GitHub stars, very active (2,100+ commits).
- **License**: Apache-2.0 (gpui-component and GPUI both declare Apache-2.0).
- **Stability**: pre-1.0 and fast-moving. Both deps are pulled from git `main`, so builds track upstream and breaking changes are routine — pin commits if reproducibility matters. Current `gpui-component` ui crate version: 0.5.x.
- **Crates**: `gpui-component` (styled UI), `gpui-base` (unstyled behavior/state foundation), `gpui-component-assets` (optional bundled Lucide icons), plus `story` gallery and examples.
- **Docs**: official site at longbridge.github.io/gpui-component (the gpui-component.longbridge.com domain was unreachable when researched); AI-agent skills officially distributed via `npx skills add longbridge/gpui-component`.

## Sources consulted

- https://github.com/longbridge/gpui-component (README)
- https://longbridge.github.io/gpui-component/llms.txt (docs index)
- https://longbridge.github.io/gpui-component/docs/getting-started.md
- https://longbridge.github.io/gpui-component/docs/theme.md
- https://longbridge.github.io/gpui-component/docs/context.md
- https://gpui.rs
- https://raw.githubusercontent.com/zed-industries/zed/main/crates/gpui/README.md
- Local cargo checkouts: `~/.cargo/git/checkouts/zed-*` (crates/gpui Cargo.toml, docs/contexts.md) and `~/.cargo/git/checkouts/gpui-component-*` (crates layout, versions)
- This repo: `/home/styly/projects/personal/fxcode/Cargo.toml`, `/home/styly/projects/personal/fxcode/src/main.rs`
