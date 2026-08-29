use termion::terminal_size;

#[derive(Debug, Clone, Copy, Default)]
struct App {
    width: u16,
    height: u16,
}

struct Style {
    padding: Padding,
    border: Option<Border>,
    gap: u8,
    position: Position,
    z: u8,
    align_items: AlignItems,
    flex_direction: FlexDirection,
    justify_content: JustifyContent,
}

struct Color(u32);
struct Border {
    border_color: Color,
    border_style: BorderStyle,
    title: String,
    title_color: Color,
    title_alignment: TitleAlignment,
}
enum TitleAlignment {
    Left,
    Center,
    Right,
}
enum BorderStyle {}
enum Padding {
    All(u8),
    Top(u8),
    Bottom(u8),
    Right(u8),
    Left(u8),
    Horizontal(u8),
    Vertical(u8),
}
enum JustifyContent {
    Start,
    End,
    Center,
}
enum FlexDirection {
    Column,
    ColumnReverse,
    Row,
    RowReverse,
}
enum Position {
    Relative,
    Absolute,
}
enum AlignSelf {
    Start,
    End,
    Center,
    Stretch,
}
enum AlignItems {
    Start,
    End,
    Center,
    Stretch,
}
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

fn main() {
    let mut app = App::default();
    app.init();
    println!("x:{} y:{}", app.width, app.height);
}
