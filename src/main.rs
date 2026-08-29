use std::{
    default,
    io::{stdout, Write},
    sync::{atomic::{AtomicBool, Ordering}, Arc},
    time::Duration,
};
use termion::event::Key;
use termion::input::TermRead;
use termion::{async_stdin, clear, cursor, raw::IntoRawMode, terminal_size};

use crate::{AlignItems::Center, Position::Relative};

#[derive(Debug, Clone, Default)]
struct App {
    screen_buffer: Vec<char>,
    width: u16,
    height: u16,
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
struct Container {
    style: Style,
}

impl App {
    fn init(&mut self) {
        let (x, y) = terminal_size().unwrap();
        self.width = x;
        self.height = y;
        self.screen_buffer.clear();
        self.screen_buffer.resize((x as usize) * (y as usize), 'e');
    }
}
impl Container {
    fn render(&self, ctx: &mut App) {
        // render top

        // render bottom
        // render left
        // render right
        // write buffer to screen
        for (i, cell) in ctx.screen_buffer.iter().enumerate() {
            print!("{}", cell);
            if (i as i32 + 1) % ctx.width as i32 == 0 {
                print!("\r\n");
            };
        }
    }
}
fn main() {
    let resized = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resized)).unwrap();

    let mut stdout = stdout().into_raw_mode().unwrap();
    write!(stdout, "{}{}", cursor::Hide, clear::All).unwrap();

    let mut app = App::default();
    app.init();
    let c = Container::default();
    c.render(&mut app);
    stdout.flush().unwrap();

    let mut keys = async_stdin().keys();
    loop {
        if let Some(Ok(key)) = keys.next() {
            if let Key::Char('q') = key {
                break;
            }
        }
        if resized.swap(false, Ordering::Relaxed) {
            app.init();
            write!(stdout, "{}{}", cursor::Goto(1, 1), clear::All).unwrap();
            c.render(&mut app);
        }
        stdout.flush().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    write!(stdout, "{}", cursor::Show).unwrap();
}

