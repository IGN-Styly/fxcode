use crate::{AlignItems::Center, Position::Relative};
use libc::{STDOUT_FILENO, TIOCGWINSZ, ioctl, winsize};
use std::{
    default,
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
}
impl Default for App {
    fn default() -> Self {
        Self {
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
#[derive(Default, Debug)]
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
#[derive(Debug, Default)]
struct Color(u32);
#[derive(Debug)]
struct Border {
    border_color: Color,
    border_style: BorderStyle,
    title: String,
    title_color: Color,
    title_alignment: TitleAlignment,
}
#[derive(Debug)]
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
#[derive(Debug)]
enum BorderStyle {}
#[derive(Debug)]
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
#[derive(Debug)]
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
#[derive(Debug)]
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
#[derive(Debug)]
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
#[derive(Debug)]
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
#[derive(Debug, Default)]
struct Box {
    style: Style,
}
trait Node {
    fn render(&self, ctx: &mut App);
}
impl App {
    fn init(&mut self) {
        let result = unsafe { ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut self.winfo) };
        if result == -1 {
            eprintln!("could not get terminal size");
            return;
        }
    }
    fn render(&mut self) {}
}

fn main() {
    let mut app = App::default();
    app.init();
}
