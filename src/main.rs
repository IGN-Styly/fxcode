use std::default;

use termion::terminal_size;

use crate::{AlignItems::Center, Position::Relative};

#[derive(Debug, Clone, Copy, Default)]
struct App {
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
    }
}
impl Container {
    fn render(&self, ctx: &mut App) {}
}
fn main() {
    let mut app = App::default();
    app.init();

    println!("x:{} y:{}", app.width, app.height);
}
