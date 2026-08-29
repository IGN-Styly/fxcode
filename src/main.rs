use crate::{AlignItems::Center, Position::Relative};
use libc::{STDOUT_FILENO, TIOCGWINSZ, ioctl, winsize};
use std::{
    default,
    fmt::Debug,
    io::{Write, stdout},
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use termion::event::Key;
use termion::input::TermRead;
use termion::{async_stdin, clear, cursor, raw::IntoRawMode, terminal_size};

#[derive(Debug, Clone)]
struct App {
    screen_buffer: Vec<Cell>,
    winfo: winsize,
    tree: Vec<Node>,
}
impl Default for App {
    fn default() -> Self {
        Self {
            tree: Vec::new(),
            winfo: {
                winsize {
                    ws_row: 0,
                    ws_col: 0,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                }
            },
            screen_buffer: Vec::new(),
        }
    }
}
#[derive(Debug, Default, Clone)]
struct Cell {
    grapheme: String,
    width: u8,
}
#[derive(Default, Debug, Clone)]
struct Style {
    padding: Padding,
    border: Option<Border>,
    gap: u16,
    position: Position,
    z: i16,
    align_items: AlignItems,
    flex_direction: FlexDirection,
    justify_content: JustifyContent,
}
#[derive(Debug, Default, Clone)]
struct Color(u32);
#[derive(Debug, Clone)]
struct Border {
    border_color: Color,
    border_style: BorderStyle,
    title: String,
    title_color: Color,
    title_alignment: TitleAlignment,
}
#[derive(Debug, Clone)]
enum TitleAlignment {
    Left,
    Center,
    Right,
}
impl Default for TitleAlignment {
    fn default() -> Self {
        TitleAlignment::Left
    }
}
#[derive(Debug, Clone)]
enum BorderStyle {}
#[derive(Debug, Clone)]
enum Padding {
    All(u8),
    Top(u8),
    Bottom(u8),
    Right(u8),
    Left(u8),
    Horizontal(u8),
    Vertical(u8),
}
impl Default for Padding {
    fn default() -> Self {
        Padding::All(0)
    }
}
#[derive(Debug, Clone)]
enum JustifyContent {
    Start,
    End,
    Center,
}
impl Default for JustifyContent {
    fn default() -> Self {
        JustifyContent::Center
    }
}
#[derive(Debug, Clone)]
enum FlexDirection {
    Column,
    ColumnReverse,
    Row,
    RowReverse,
}
impl Default for FlexDirection {
    fn default() -> Self {
        FlexDirection::Column
    }
}
#[derive(Debug, Clone)]
enum Position {
    Relative,
    Absolute,
}
impl Default for Position {
    fn default() -> Self {
        Position::Relative
    }
}
enum AlignSelf {
    Start,
    End,
    Center,
    Stretch,
}
#[derive(Debug, Clone)]
enum AlignItems {
    Start,
    End,
    Center,
    Stretch,
}
impl Default for AlignItems {
    fn default() -> Self {
        Center
    }
}
#[derive(Debug, Default, Clone)]
struct Container {
    style: Style,
    items: Vec<Node>,
}
#[derive(Debug, Clone)]
enum Node {
    Container(Container),
}
// in cols/rows
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}
impl App {
    fn init(&mut self) {
        let result = unsafe { ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut self.winfo) };
        if result == -1 {
            eprintln!("could not get terminal size");
            return;
        }
    }
    fn render(&mut self) {
        let viewport = Rect {
            x: 0,
            y: 0,
            w: self.winfo.ws_col as u16,
            h: self.winfo.ws_row as u16,
        };
        let mut nodes: Vec<(Node, Rect)> = Vec::new();
        for node in &self.tree {
            layout(node, viewport, &mut nodes);
        }
    }
}
fn layout(node: &Node, viewport: Rect, nodes: &mut Vec<(Node, Rect)>) {
    nodes.push((node.clone(), viewport));
    let Node::Container(c) = node;
    let s = &c.style;
    let b = if s.border.is_some() { 1 } else { 0 };
    let content = calc_area(viewport, b, &s.padding);
    let n = c.items.len() as u16;
    if n == 0 {
        return;
    }

    let gaps = s.gap as u16 * (n - 1);
    let slot = match s.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => content.w.saturating_sub(gaps) / n,
        FlexDirection::Column | FlexDirection::ColumnReverse => content.h.saturating_sub(gaps) / n,
    };
    let (mut cx, mut cy) = (content.x, content.y);
    for childNode in &c.items {
        let r = match s.flex_direction {
            FlexDirection::Column | FlexDirection::ColumnReverse => Rect {
                x: cx,
                y: content.y,
                w: slot,
                h: content.h,
            },
            FlexDirection::Row | FlexDirection::RowReverse => Rect {
                x: content.x,
                y: cy,
                w: content.w,
                h: slot,
            },
        };
        layout(childNode, r, nodes);
        match s.flex_direction {
            FlexDirection::Column | FlexDirection::ColumnReverse => {
                cx += slot + s.gap as u16;
            }
            FlexDirection::Row | FlexDirection::RowReverse => {
                cy += slot + s.gap as u16;
            }
        }
    }
}
fn calc_area(viewport: Rect, border: u16, padding: &Padding) -> Rect {
    let mut content = viewport;
    match padding {
        Padding::All(i) => {
            content.h -= *i as u16;
            content.w -= *i as u16;
            content.x += *i as u16;
            content.y += *i as u16;
        }
        Padding::Bottom(i) => {
            content.h -= *i as u16;
        }
        Padding::Top(i) => {
            content.y += *i as u16;
        }
        Padding::Left(i) => {
            content.w -= *i as u16;
        }
        Padding::Right(i) => {
            content.x += *i as u16;
        }
        Padding::Horizontal(i) => {
            content.w -= *i as u16;
            content.x += *i as u16;
        }
        Padding::Vertical(i) => {
            content.h -= *i as u16;
            content.y += *i as u16;
        }
    }
    return content;
}

fn main() {
    let mut app = App::default();
    app.init();
    app.tree.push(Node::Container(Container::default()));
    app.render();
}
