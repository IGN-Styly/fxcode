use crate::VideoControls;
use std::{num::ParseIntError, str::FromStr};

#[derive(Default, Debug, Clone)]
pub struct Style {
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub x: u16,
    pub y: u16,
    pub padding: Padding,
    pub border: Option<Border>,
    pub background_color: Option<Color>,
    pub gap: u16,
    pub position: Position,
    pub z: i16,
    pub align_items: AlignItems,
    pub align_self: Option<AlignSelf>,
    pub flex_direction: FlexDirection,
    pub justify_content: JustifyContent,
}

impl Style {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn width(mut self, width: u16) -> Self {
        self.width = Some(width);
        self
    }

    pub fn height(mut self, height: u16) -> Self {
        self.height = Some(height);
        self
    }

    pub fn position(mut self, position: Position) -> Self {
        self.position = position;
        self
    }

    pub fn offset(mut self, x: u16, y: u16) -> Self {
        self.x = x;
        self.y = y;
        self
    }

    pub fn padding(mut self, padding: Padding) -> Self {
        self.padding = padding;
        self
    }

    pub fn border(mut self, border: Border) -> Self {
        self.border = Some(border);
        self
    }

    pub fn background(mut self, color: Color) -> Self {
        self.background_color = Some(color);
        self
    }

    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    pub fn z(mut self, z: i16) -> Self {
        self.z = z;
        self
    }

    pub fn align_items(mut self, alignment: AlignItems) -> Self {
        self.align_items = alignment;
        self
    }

    pub fn align_self(mut self, alignment: AlignSelf) -> Self {
        self.align_self = Some(alignment);
        self
    }

    pub fn direction(mut self, direction: FlexDirection) -> Self {
        self.flex_direction = direction;
        self
    }

    pub fn justify(mut self, justification: JustifyContent) -> Self {
        self.justify_content = justification;
        self
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color(u32);

impl Color {
    pub const BLACK: Self = Self(0x000000ff);
    pub const WHITE: Self = Self(0xffffffff);
    pub const RED: Self = Self(0xff0000ff);
    pub const GREEN: Self = Self(0x00ff00ff);
    pub const BLUE: Self = Self(0x0000ffff);
    pub const YELLOW: Self = Self(0xffff00ff);
    pub const CYAN: Self = Self(0x00ffffff);
    pub const MAGENTA: Self = Self(0xff00ffff);
    pub const GRAY: Self = Self(0x808080ff);
    pub const ORANGE: Self = Self(0xffa500ff);
    pub const PURPLE: Self = Self(0x800080ff);
    pub const PINK: Self = Self(0xffc0cbff);

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
        u32::from_str_radix(value, 16).map(|color| {
            if value.len() == 6 {
                Self((color << 8) | 0xff)
            } else {
                Self(color)
            }
        })
    }

    pub const fn rgba(self) -> (u8, u8, u8, u8) {
        (
            ((self.0 >> 24) & 0xff) as u8,
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
    pub color: Color,
    pub style: BorderStyle,
    pub title: String,
    pub title_color: Color,
    pub title_alignment: TitleAlignment,
    pub media_controls: Option<VideoControls>,
}

impl Border {
    pub fn plain(color: Color) -> Self {
        Self {
            color,
            style: BorderStyle::Plain,
            title: String::new(),
            title_color: color,
            title_alignment: TitleAlignment::Left,
            media_controls: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn title_color(mut self, color: Color) -> Self {
        self.title_color = color;
        self
    }

    pub fn title_alignment(mut self, alignment: TitleAlignment) -> Self {
        self.title_alignment = alignment;
        self
    }

    pub fn media_controls(mut self, controls: VideoControls) -> Self {
        self.media_controls = Some(controls);
        self
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub enum TitleAlignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Default, Clone, Copy)]
pub enum BorderStyle {
    #[default]
    Plain,
}

#[derive(Debug, Clone, Copy)]
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
        Self::All(0)
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
