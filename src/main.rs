use crate::{AlignItems::Center, Position::Relative};
use libc::{STDOUT_FILENO, TIOCGWINSZ, ioctl, winsize};
use std::{
    default,
    fmt::Debug,
    io::{Write, stdout},
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
    gap: u8,
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
impl App {
    fn init(&mut self) {
        let result = unsafe { ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut self.winfo) };
        if result == -1 {
            eprintln!("could not get terminal size");
            return;
        }
    }
    fn render(&mut self) {
        for
    }
}

fn main() {
    let mut app = App::default();
    app.init();
    app.tree.push(Node::Container(Container::default()));
    app.render();
}
