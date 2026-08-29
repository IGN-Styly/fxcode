use libc::{STDOUT_FILENO, TIOCGWINSZ, ioctl, winsize};
use std::io;

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
#[derive(Debug, Default, Clone)]
enum TitleAlignment {
    #[default]
    Left,
    Center,
    Right,
}
#[derive(Debug, Default, Clone)]
enum BorderStyle {
    #[default]
    Plain,
}
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
#[derive(Debug, Default, Clone)]
enum JustifyContent {
    Start,
    End,
    #[default]
    Center,
}
#[derive(Debug, Default, Clone)]
enum FlexDirection {
    #[default]
    Column,
    ColumnReverse,
    Row,
    RowReverse,
}
#[derive(Debug, Default, Clone)]
enum Position {
    #[default]
    Relative,
    Absolute,
}
enum AlignSelf {
    Start,
    End,
    Center,
    Stretch,
}
#[derive(Debug, Default, Clone)]
enum AlignItems {
    Start,
    End,
    #[default]
    Center,
    Stretch,
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
    fn init(&mut self) -> io::Result<()> {
        let result = unsafe { ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut self.winfo) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn render(&self) {
        let viewport = Rect {
            x: 0,
            y: 0,
            w: self.winfo.ws_col,
            h: self.winfo.ws_row,
        };
        let mut nodes = Vec::new();
        for node in &self.tree {
            layout(node, viewport, &mut nodes);
        }
    }
}
fn layout<'a>(node: &'a Node, viewport: Rect, nodes: &mut Vec<(&'a Node, Rect)>) {
    nodes.push((node, viewport));
    let Node::Container(c) = node;
    let s = &c.style;
    let b = if s.border.is_some() { 1 } else { 0 };
    let content = calc_area(viewport, b, &s.padding);
    let n = c.items.len();
    if n == 0 {
        return;
    }

    let main_size = match s.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => content.w,
        FlexDirection::Column | FlexDirection::ColumnReverse => content.h,
    };
    let gap_count = u64::try_from(n - 1).unwrap_or(u64::MAX);
    let gaps = u64::from(s.gap).saturating_mul(gap_count);
    let available = u64::from(main_size).saturating_sub(gaps);
    let child_count = u64::try_from(n).unwrap_or(u64::MAX);
    let base_slot = available / child_count;
    let remainder = available % child_count;

    for (index, child_node) in c.items.iter().enumerate() {
        let visual_index = match s.flex_direction {
            FlexDirection::RowReverse | FlexDirection::ColumnReverse => n - index - 1,
            FlexDirection::Row | FlexDirection::Column => index,
        };
        let visual_index = u64::try_from(visual_index).unwrap_or(u64::MAX);
        let slot = base_slot + u64::from(visual_index < remainder);
        let offset = visual_index
            .saturating_mul(base_slot)
            .saturating_add(visual_index.min(remainder))
            .saturating_add(visual_index.saturating_mul(u64::from(s.gap)))
            .min(u64::from(main_size));
        let slot = u16::try_from(slot).unwrap_or(main_size);
        let offset = u16::try_from(offset).unwrap_or(main_size);

        let r = match s.flex_direction {
            FlexDirection::Column | FlexDirection::ColumnReverse => Rect {
                x: content.x,
                y: content.y.saturating_add(offset),
                w: content.w,
                h: slot,
            },
            FlexDirection::Row | FlexDirection::RowReverse => Rect {
                x: content.x.saturating_add(offset),
                y: content.y,
                w: slot,
                h: content.h,
            },
        };
        layout(child_node, r, nodes);
    }
}
fn calc_area(viewport: Rect, border: u16, padding: &Padding) -> Rect {
    let (top, right, bottom, left) = match padding {
        Padding::All(value) => (*value, *value, *value, *value),
        Padding::Top(value) => (*value, 0, 0, 0),
        Padding::Bottom(value) => (0, 0, *value, 0),
        Padding::Right(value) => (0, *value, 0, 0),
        Padding::Left(value) => (0, 0, 0, *value),
        Padding::Horizontal(value) => (0, *value, 0, *value),
        Padding::Vertical(value) => (*value, 0, *value, 0),
    };

    inset(
        viewport,
        border.saturating_add(u16::from(top)),
        border.saturating_add(u16::from(right)),
        border.saturating_add(u16::from(bottom)),
        border.saturating_add(u16::from(left)),
    )
}

fn inset(viewport: Rect, top: u16, right: u16, bottom: u16, left: u16) -> Rect {
    let left = left.min(viewport.w);
    let width = viewport.w - left;
    let right = right.min(width);
    let top = top.min(viewport.h);
    let height = viewport.h - top;
    let bottom = bottom.min(height);

    Rect {
        x: viewport.x.saturating_add(left),
        y: viewport.y.saturating_add(top),
        w: width - right,
        h: height - bottom,
    }
}

fn main() -> io::Result<()> {
    let mut app = App::default();
    app.init()?;
    app.tree.push(Node::Container(Container::default()));
    app.render();
    Ok(())
}
