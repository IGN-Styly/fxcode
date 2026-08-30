use libc::{STDOUT_FILENO, TIOCGWINSZ, ioctl, winsize};
use std::{
    io::{self, Write, stdout},
    num::ParseIntError,
    str::FromStr,
};
use termion::{color, cursor, raw::IntoRawMode};

#[derive(Debug, Clone)]
pub struct App {
    screen_buffer: Vec<Cell>,
    previous_buffer: Vec<Cell>,
    winfo: winsize,
    pub tree: Vec<Node>,
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
            previous_buffer: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    grapheme: String,
    width: u8,
    foreground: Option<Color>,
}
impl Default for Cell {
    fn default() -> Self {
        Self {
            grapheme: " ".to_string(),
            width: 1,
            foreground: None,
        }
    }
}
#[derive(Default, Debug, Clone)]
pub struct Style {
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub x: u16,
    pub y: u16,
    pub padding: Padding,
    pub border: Option<Border>,
    pub gap: u16,
    pub position: Position,
    pub z: i16,
    pub align_items: AlignItems,
    pub align_self: Option<AlignSelf>,
    pub flex_direction: FlexDirection,
    pub justify_content: JustifyContent,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Color(u32);
impl Color {
    pub const BLACK: Self = Self(0x000000);
    pub const WHITE: Self = Self(0xffffff);
    pub const RED: Self = Self(0xff0000);
    pub const GREEN: Self = Self(0x00ff00);
    pub const BLUE: Self = Self(0x0000ff);
    pub const YELLOW: Self = Self(0xffff00);
    pub const CYAN: Self = Self(0x00ffff);
    pub const MAGENTA: Self = Self(0xff00ff);
    pub const GRAY: Self = Self(0x808080);
    pub const ORANGE: Self = Self(0xffa500);
    pub const PURPLE: Self = Self(0x800080);
    pub const PINK: Self = Self(0xffc0cb);

    pub const fn black() -> Self {
        Self::BLACK
    }

    pub const fn white() -> Self {
        Self::WHITE
    }

    pub const fn red() -> Self {
        Self::RED
    }

    pub const fn green() -> Self {
        Self::GREEN
    }

    pub const fn blue() -> Self {
        Self::BLUE
    }

    pub const fn yellow() -> Self {
        Self::YELLOW
    }

    pub const fn cyan() -> Self {
        Self::CYAN
    }

    pub const fn magenta() -> Self {
        Self::MAGENTA
    }

    pub const fn gray() -> Self {
        Self::GRAY
    }

    pub const fn orange() -> Self {
        Self::ORANGE
    }

    pub const fn purple() -> Self {
        Self::PURPLE
    }

    pub const fn pink() -> Self {
        Self::PINK
    }

    pub fn from_hex(value: &str) -> Result<Self, ParseIntError> {
        let value = value.strip_prefix('#').unwrap_or(value);
        u32::from_str_radix(value, 16).map(Self)
    }

    const fn rgb(self) -> (u8, u8, u8) {
        (
            ((self.0 >> 16) & 0xff) as u8,
            ((self.0 >> 8) & 0xff) as u8,
            (self.0 & 0xff) as u8,
        )
    }
}
impl FromStr for Color {
    type Err = ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_hex(value)
    }
}
#[derive(Debug, Clone)]
pub struct Border {
    pub border_color: Color,
    pub border_style: BorderStyle,
    pub title: String,
    pub title_color: Color,
    pub title_alignment: TitleAlignment,
}
#[derive(Debug, Default, Clone)]
pub enum TitleAlignment {
    #[default]
    Left,
    Center,
    Right,
}
#[derive(Debug, Default, Clone)]
pub enum BorderStyle {
    #[default]
    Plain,
}
#[derive(Debug, Clone)]
pub enum Padding {
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum JustifyContent {
    #[default]
    Start,
    End,
    Center,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    #[default]
    Column,
    ColumnReverse,
    Row,
    RowReverse,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    #[default]
    Relative,
    Absolute,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignSelf {
    Start,
    End,
    Center,
    Stretch,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AlignItems {
    Start,
    End,
    Center,
    #[default]
    Stretch,
}
#[derive(Debug, Default, Clone)]
pub struct Container {
    pub style: Style,
    pub items: Vec<Node>,
}
#[derive(Debug, Clone)]
pub enum Node {
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
    pub fn init(&mut self) -> io::Result<()> {
        let result = unsafe { ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut self.winfo) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub fn render(&mut self) -> io::Result<()> {
        let viewport = Rect {
            x: 0,
            y: 0,
            w: self.winfo.ws_col,
            h: self.winfo.ws_row,
        };
        let cell_count = usize::from(viewport.w) * usize::from(viewport.h);
        self.screen_buffer.resize(cell_count, Cell::default());
        self.screen_buffer.fill(Cell::default());

        let mut nodes = Vec::new();
        for node in &self.tree {
            layout(node, root_rect(node, viewport), &mut nodes);
        }
        nodes.sort_by_key(|f| {
            let Node::Container(x) = f.0;
            x.style.z
        });
        paint(&nodes, &mut self.screen_buffer, viewport.w);

        let stdout = stdout();
        let mut output = stdout.lock();
        flush(
            &self.screen_buffer,
            &mut self.previous_buffer,
            viewport.w,
            &mut output,
        )
    }
}
fn flush<W: Write>(
    buf: &[Cell],
    previous: &mut Vec<Cell>,
    cols: u16,
    output: &mut W,
) -> io::Result<()> {
    if cols == 0 {
        previous.clear();
        previous.extend_from_slice(buf);
        return Ok(());
    }

    for (i, cell) in buf.iter().enumerate() {
        if previous.get(i) != Some(cell) {
            let x = i % usize::from(cols);
            let y = i / usize::from(cols);
            write!(output, "{}", cursor::Goto(x as u16 + 1, y as u16 + 1))?;
            match cell.foreground {
                Some(foreground) => {
                    let (red, green, blue) = foreground.rgb();
                    write!(output, "{}", color::Fg(color::Rgb(red, green, blue)))?;
                }
                None => write!(output, "{}", color::Fg(color::Reset))?,
            }
            write!(output, "{}", cell.grapheme)?;
        }
    }
    output.flush()?;
    previous.clear();
    previous.extend_from_slice(buf);
    Ok(())
}
fn node_style(node: &Node) -> &Style {
    let Node::Container(container) = node;
    &container.style
}

fn root_rect(node: &Node, viewport: Rect) -> Rect {
    let style = node_style(node);
    let x = style.x.min(viewport.w);
    let y = style.y.min(viewport.h);

    Rect {
        x: viewport.x.saturating_add(x),
        y: viewport.y.saturating_add(y),
        w: style.width.unwrap_or(viewport.w - x).min(viewport.w - x),
        h: style.height.unwrap_or(viewport.h - y).min(viewport.h - y),
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

    let relative: Vec<usize> = c
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, child)| {
            (node_style(child).position == Position::Relative).then_some(index)
        })
        .collect();
    let mut child_rects = vec![None; n];
    let main_size = match s.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => content.w,
        FlexDirection::Column | FlexDirection::ColumnReverse => content.h,
    };
    let gap_count = relative.len().saturating_sub(1) as u64;
    let gaps = u64::from(s.gap).saturating_mul(gap_count);
    let available = u64::from(main_size).saturating_sub(gaps);
    let mut sizes = vec![0_u16; n];
    let mut fixed = 0_u64;
    let mut automatic = Vec::new();

    for &index in &relative {
        let child_style = node_style(&c.items[index]);
        let size = match s.flex_direction {
            FlexDirection::Row | FlexDirection::RowReverse => child_style.width,
            FlexDirection::Column | FlexDirection::ColumnReverse => child_style.height,
        };

        if let Some(size) = size {
            sizes[index] = size.min(main_size);
            fixed = fixed.saturating_add(u64::from(sizes[index]));
        } else {
            automatic.push(index);
        }
    }

    let automatic_space = available.saturating_sub(fixed);
    if !automatic.is_empty() {
        let count = automatic.len() as u64;
        let size = automatic_space / count;
        let remainder = automatic_space % count;
        for (position, &index) in automatic.iter().enumerate() {
            sizes[index] =
                u16::try_from(size + u64::from((position as u64) < remainder)).unwrap_or(main_size);
        }
    }

    let used = relative.iter().fold(gaps, |total, &index| {
        total.saturating_add(u64::from(sizes[index]))
    });
    let free = u64::from(main_size).saturating_sub(used) as u16;
    let reversed = matches!(
        s.flex_direction,
        FlexDirection::RowReverse | FlexDirection::ColumnReverse
    );
    let mut cursor = match (s.justify_content, reversed) {
        (JustifyContent::Start, false) | (JustifyContent::End, true) => 0,
        (JustifyContent::End, false) | (JustifyContent::Start, true) => free,
        (JustifyContent::Center, _) => free / 2,
    };
    let mut visual_order = relative;
    if reversed {
        visual_order.reverse();
    }

    for index in visual_order {
        let child_style = node_style(&c.items[index]);
        let remaining = main_size.saturating_sub(cursor);
        let child_main = sizes[index].min(remaining);
        let (cross_size, cross_offset) = cross_layout(s, child_style, content);
        let rect = match s.flex_direction {
            FlexDirection::Column | FlexDirection::ColumnReverse => Rect {
                x: content.x.saturating_add(cross_offset),
                y: content.y.saturating_add(cursor),
                w: cross_size,
                h: child_main,
            },
            FlexDirection::Row | FlexDirection::RowReverse => Rect {
                x: content.x.saturating_add(cursor),
                y: content.y.saturating_add(cross_offset),
                w: child_main,
                h: cross_size,
            },
        };
        child_rects[index] = Some(offset_and_clip(rect, content, child_style.x, child_style.y));
        cursor = cursor.saturating_add(child_main).saturating_add(s.gap);
    }

    for (index, child) in c.items.iter().enumerate() {
        let child_style = node_style(child);
        if child_style.position == Position::Absolute {
            child_rects[index] = Some(root_rect(child, content));
        }
    }

    for (index, child) in c.items.iter().enumerate() {
        if let Some(rect) = child_rects[index] {
            layout(child, rect, nodes);
        }
    }
}

fn cross_layout(parent: &Style, child: &Style, content: Rect) -> (u16, u16) {
    let (available, requested) = match parent.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => (content.h, child.height),
        FlexDirection::Column | FlexDirection::ColumnReverse => (content.w, child.width),
    };
    let size = requested.unwrap_or(available).min(available);
    let free = available - size;
    let offset = match child.align_self {
        Some(AlignSelf::Start | AlignSelf::Stretch) => 0,
        Some(AlignSelf::End) => free,
        Some(AlignSelf::Center) => free / 2,
        None => match parent.align_items {
            AlignItems::Start | AlignItems::Stretch => 0,
            AlignItems::End => free,
            AlignItems::Center => free / 2,
        },
    };
    (size, offset)
}

fn offset_and_clip(mut rect: Rect, bounds: Rect, x: u16, y: u16) -> Rect {
    let x = x.min(bounds.x.saturating_add(bounds.w).saturating_sub(rect.x));
    let y = y.min(bounds.y.saturating_add(bounds.h).saturating_sub(rect.y));
    rect.x = rect.x.saturating_add(x);
    rect.y = rect.y.saturating_add(y);
    rect.w = rect
        .w
        .min(bounds.x.saturating_add(bounds.w).saturating_sub(rect.x));
    rect.h = rect
        .h
        .min(bounds.y.saturating_add(bounds.h).saturating_sub(rect.y));
    rect
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
fn put(buf: &mut [Cell], cols: u16, x: u16, y: u16, ch: &str, foreground: Color) {
    if x < cols {
        // usize here: u16 * u16 can overflow u16, and Vec indexes use usize.
        if let Some(cell) = buf.get_mut(usize::from(y) * usize::from(cols) + usize::from(x)) {
            *cell = Cell {
                grapheme: ch.to_string(),
                width: 1,
                foreground: Some(foreground),
            };
        }
    }
}

fn draw_frame(buf: &mut [Cell], cols: u16, r: Rect, b: &Border) {
    if r.w == 0 || r.h == 0 {
        return;
    }

    let (top_left, top_right, bottom_left, bottom_right, horizontal, vertical) =
        match b.border_style {
            BorderStyle::Plain => ("┌", "┐", "└", "┘", "─", "│"),
        };
    let right = r.x.saturating_add(r.w - 1);
    let bottom = r.y.saturating_add(r.h - 1);
    if r.w == 1 {
        for y in r.y..=bottom {
            put(buf, cols, r.x, y, vertical, b.border_color);
        }
        return;
    }
    if r.h == 1 {
        for x in r.x..=right {
            put(buf, cols, x, r.y, horizontal, b.border_color);
        }
        draw_title(buf, cols, r, b);
        return;
    }

    put(buf, cols, r.x, r.y, top_left, b.border_color);
    put(buf, cols, right, r.y, top_right, b.border_color);
    put(buf, cols, r.x, bottom, bottom_left, b.border_color);
    put(buf, cols, right, bottom, bottom_right, b.border_color);
    for x in (r.x + 1)..right {
        put(buf, cols, x, r.y, horizontal, b.border_color);
        put(buf, cols, x, bottom, horizontal, b.border_color);
    }
    for y in (r.y + 1)..bottom {
        put(buf, cols, r.x, y, vertical, b.border_color);
        put(buf, cols, right, y, vertical, b.border_color);
    }
    draw_title(buf, cols, r, b);
}

fn draw_title(buf: &mut [Cell], cols: u16, r: Rect, border: &Border) {
    let available = usize::from(r.w.saturating_sub(2));
    let title: Vec<char> = border.title.chars().take(available).collect();
    let free = available.saturating_sub(title.len());
    let offset = match border.title_alignment {
        TitleAlignment::Left => 0,
        TitleAlignment::Center => free / 2,
        TitleAlignment::Right => free,
    };
    let start = r.x.saturating_add(1).saturating_add(offset as u16);

    for (index, ch) in title.into_iter().enumerate() {
        put(
            buf,
            cols,
            start.saturating_add(index as u16),
            r.y,
            &ch.to_string(),
            border.title_color,
        );
    }
}

fn paint(nodes: &[(&Node, Rect)], buf: &mut [Cell], cols: u16) {
    for &(node, r) in nodes {
        let Node::Container(c) = node;
        if let Some(border) = &c.style.border {
            draw_frame(buf, cols, r, border);
        }
    }
}
fn main() -> io::Result<()> {
    stdout().into_raw_mode()?;
    let mut app = App::default();
    app.init()?;
    let mut c = Container::default();
    c.style.border = Some(Border {
        border_color: Color::white(),
        border_style: BorderStyle::Plain,
        title: "Test".into(),
        title_color: Color::white(),
        title_alignment: TitleAlignment::Left,
    });
    app.tree.push(Node::Container(c));
    app.render();
    loop {}
}
