use crate::{
    Border, BorderStyle, Color, Node, TitleAlignment, VideoControls,
    kitty::{KittyMode, KittyRenderer},
    layout::{self, PositionedNode, Rect},
    mpv::MpvRenderer,
};
use libc::{STDOUT_FILENO, TIOCGWINSZ, ioctl, winsize};
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};
use termion::{
    clear, color, cursor,
    event::{MouseButton, MouseEvent},
};

const MEDIA_ACTION_OFFSET: usize = 1;
const MEDIA_ACTION_WIDTH: usize = 2;
const MEDIA_ACTION_AREA_WIDTH: usize = MEDIA_ACTION_OFFSET + MEDIA_ACTION_WIDTH + 1;
const MEDIA_TIME_WIDTH: usize = 12;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl TerminalSize {
    pub fn current() -> io::Result<Self> {
        let mut value = winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe { ioctl(STDOUT_FILENO, TIOCGWINSZ, &mut value) };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            cols: value.ws_col,
            rows: value.ws_row,
            pixel_width: value.ws_xpixel,
            pixel_height: value.ws_ypixel,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cell {
    grapheme: String,
    foreground: Option<Color>,
    background: Option<Color>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            grapheme: " ".into(),
            foreground: None,
            background: None,
        }
    }
}

pub struct Renderer<W: Write> {
    output: W,
    size: TerminalSize,
    screen: Vec<Cell>,
    previous: Vec<Cell>,
    kitty: KittyRenderer,
    mpv: MpvRenderer,
    media_borders: Vec<MediaBorder>,
    media_drag: Option<MediaDrag>,
    last_media_draw: Instant,
}

struct MediaBorder {
    rect: Rect,
    color: Color,
    controls: VideoControls,
}

struct MediaDrag {
    rect: Rect,
    controls: VideoControls,
}

impl<W: Write> Renderer<W> {
    pub fn new(output: W, size: TerminalSize) -> Self {
        Self::with_kitty(output, size, KittyMode::Auto)
    }

    pub fn with_kitty(output: W, size: TerminalSize, mode: KittyMode) -> Self {
        let kitty = KittyRenderer::new(mode);
        let mpv = MpvRenderer::new(kitty.is_enabled());
        Self {
            output,
            size,
            screen: Vec::new(),
            previous: Vec::new(),
            kitty,
            mpv,
            media_borders: Vec::new(),
            media_drag: None,
            last_media_draw: Instant::now(),
        }
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn kitty_supported(&self) -> bool {
        self.kitty.is_enabled()
    }

    pub fn resize(&mut self, size: TerminalSize) -> io::Result<()> {
        self.size = size;
        self.previous.clear();
        self.media_borders.clear();
        self.media_drag = None;
        self.kitty.clear(&mut self.output)?;
        self.mpv.redraw();
        write!(self.output, "{}", clear::All)?;
        self.output.flush()
    }

    pub fn draw(&mut self, tree: &[Node]) -> io::Result<()> {
        let viewport = Rect {
            x: 0,
            y: 0,
            w: self.size.cols,
            h: self.size.rows,
        };
        let count = usize::from(self.size.cols) * usize::from(self.size.rows);
        self.screen.resize(count, Cell::default());
        self.screen.fill(Cell::default());

        let mut positioned = layout::calculate(tree, viewport);
        positioned.sort_by_key(|item| item.node.style().z);
        for border in &self.media_borders {
            let y = border
                .rect
                .y
                .saturating_add(border.rect.h.saturating_sub(1));
            for x in border.rect.x..border.rect.x.saturating_add(border.rect.w) {
                if let Some(cell) = self
                    .previous
                    .get_mut(usize::from(y) * usize::from(self.size.cols) + usize::from(x))
                {
                    cell.grapheme.clear();
                }
            }
        }
        self.media_borders.clear();
        for item in &positioned {
            let Node::Container(container) = item.node else {
                continue;
            };
            let Some(border) = &container.style.border else {
                continue;
            };
            if let Some(controls) = &border.media_controls {
                self.media_borders.push(MediaBorder {
                    rect: item.rect,
                    color: border.color,
                    controls: controls.clone(),
                });
            }
        }
        paint_cells(&positioned, &mut self.screen, self.size.cols);
        flush_cells(
            &mut self.output,
            &self.screen,
            &mut self.previous,
            self.size.cols,
        )?;

        let images: Vec<_> = positioned
            .iter()
            .filter_map(|item| match item.node {
                Node::Image(image) => Some((image, item.rect)),
                Node::Container(_) | Node::Video(_) => None,
            })
            .collect();
        self.kitty.draw(&mut self.output, &images, self.size)?;
        self.mpv
            .reconcile(&mut self.output, &positioned, self.size)?;
        self.draw_media_controls(true)
    }

    pub fn draw_video_frames(&mut self) -> io::Result<()> {
        if self.mpv.is_empty() {
            return Ok(());
        }
        self.mpv.draw_ready(&mut self.output)?;
        self.draw_media_controls(false)
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> io::Result<bool> {
        match event {
            MouseEvent::Hold(x, _) => {
                let Some(drag) = &self.media_drag else {
                    return Ok(false);
                };
                seek_from_mouse(&drag.controls, drag.rect, x);
                self.draw_media_controls(true)?;
                return Ok(true);
            }
            MouseEvent::Release(x, _) => {
                let Some(drag) = self.media_drag.take() else {
                    return Ok(false);
                };
                seek_from_mouse(&drag.controls, drag.rect, x);
                self.draw_media_controls(true)?;
                return Ok(true);
            }
            MouseEvent::Press(MouseButton::Left, _, _) => {}
            MouseEvent::Press(_, _, _) => return Ok(false),
        }
        let MouseEvent::Press(MouseButton::Left, x, y) = event else {
            unreachable!();
        };
        for index in (0..self.media_borders.len()).rev() {
            let rect = self.media_borders[index].rect;
            let controls = self.media_borders[index].controls.clone();
            let footer_y = rect.y.saturating_add(rect.h);
            let content_start = rect.x.saturating_add(2);
            if y == footer_y && x >= content_start {
                let offset = usize::from(x - content_start);
                if (MEDIA_ACTION_OFFSET..MEDIA_ACTION_OFFSET + MEDIA_ACTION_WIDTH).contains(&offset)
                {
                    controls.toggle_pause();
                    self.mpv.redraw();
                    self.draw_media_controls(true)?;
                    return Ok(true);
                }
                let progress_start = MEDIA_ACTION_AREA_WIDTH + MEDIA_TIME_WIDTH;
                let progress_width =
                    usize::from(rect.w.saturating_sub(2)).saturating_sub(progress_start);
                if offset >= progress_start && progress_width > 1 {
                    seek_from_mouse(&controls, rect, x);
                    self.media_drag = Some(MediaDrag {
                        rect,
                        controls: controls.clone(),
                    });
                    self.draw_media_controls(true)?;
                    return Ok(true);
                }
            }

            let center_x = rect.x.saturating_add(rect.w / 2).saturating_add(1);
            let center_y = rect.y.saturating_add(rect.h / 2).saturating_add(1);
            if controls.is_paused() && x.abs_diff(center_x) <= 2 && y.abs_diff(center_y) <= 1 {
                controls.toggle_pause();
                self.mpv.redraw();
                self.draw_media_controls(true)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn clear(&mut self) -> io::Result<()> {
        self.kitty.clear(&mut self.output)?;
        self.mpv.clear(&mut self.output)?;
        self.media_borders.clear();
        self.media_drag = None;
        self.previous.clear();
        write!(
            self.output,
            "{}{}{}",
            color::Fg(color::Reset),
            color::Bg(color::Reset),
            clear::All
        )?;
        self.output.flush()
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.clear()?;
        write!(self.output, "{}", cursor::Show)?;
        self.output.flush()
    }
}

fn seek_from_mouse(controls: &VideoControls, rect: Rect, x: u16) {
    let inner = usize::from(rect.w.saturating_sub(2));
    let progress_start = MEDIA_ACTION_AREA_WIDTH + MEDIA_TIME_WIDTH;
    let progress_width = inner.saturating_sub(progress_start);
    if progress_width <= 1 {
        return;
    }
    let first = rect
        .x
        .saturating_add(2)
        .saturating_add(progress_start as u16);
    let offset = usize::from(x.saturating_sub(first)).min(progress_width - 1);
    let progress = offset as f64 / (progress_width - 1) as f64;
    controls.seek_to(controls.duration() * progress);
}

impl<W: Write> Renderer<W> {
    fn draw_media_controls(&mut self, force: bool) -> io::Result<()> {
        if !force && self.last_media_draw.elapsed() < Duration::from_millis(100) {
            return Ok(());
        }
        let mut active_actions = Vec::with_capacity(self.media_borders.len());
        for (index, border) in self.media_borders.iter().enumerate() {
            if draw_media_border(&mut self.output, border)? {
                self.kitty.draw_media_action(
                    &mut self.output,
                    index,
                    border.rect,
                    border.color,
                    border.controls.is_paused(),
                    self.size,
                )?;
                active_actions.push(index);
            }
        }
        self.kitty
            .retain_media_actions(&mut self.output, &active_actions)?;
        if !self.media_borders.is_empty() {
            write!(
                self.output,
                "{}{}",
                cursor::Goto(self.size.cols.max(1), self.size.rows.max(1)),
                cursor::Hide
            )?;
            self.output.flush()?;
        }
        self.last_media_draw = Instant::now();
        Ok(())
    }
}

fn draw_media_border<W: Write>(output: &mut W, border: &MediaBorder) -> io::Result<bool> {
    let rect = border.rect;
    if rect.w < 3 || rect.h == 0 {
        return Ok(false);
    }
    let inner = usize::from(rect.w - 2);
    let position = border.controls.position().max(0.0) as u64;
    let duration = border.controls.duration().max(0.0) as u64;
    let position = position.min(99 * 60 + 59);
    let duration = duration.min(99 * 60 + 59);
    let used = MEDIA_ACTION_AREA_WIDTH + MEDIA_TIME_WIDTH;

    write!(
        output,
        "{}{}",
        cursor::Goto(rect.x + 2, rect.y + rect.h),
        color::Fg(color::Rgb(
            border.color.rgba().0,
            border.color.rgba().1,
            border.color.rgba().2
        ))
    )?;
    if inner < used {
        for _ in 0..inner {
            output.write_all("─".as_bytes())?;
        }
        return Ok(false);
    }
    for _ in 0..MEDIA_ACTION_AREA_WIDTH {
        output.write_all(b" ")?;
    }
    write!(
        output,
        "{}{:02}:{:02}/{:02}:{:02} ",
        cursor::Goto(rect.x + 2 + MEDIA_ACTION_AREA_WIDTH as u16, rect.y + rect.h),
        position / 60,
        position % 60,
        duration / 60,
        duration % 60
    )?;
    let available = inner - used;
    if available == 0 {
        for _ in 0..available {
            output.write_all("─".as_bytes())?;
        }
        return Ok(true);
    }

    let bar_width = available;
    let marker = (duration > 0).then(|| {
        ((position.min(duration) as f64 / duration as f64) * (bar_width - 1) as f64).round()
            as usize
    });
    for index in 0..bar_width {
        output.write_all(match marker {
            Some(marker) if index < marker => "━".as_bytes(),
            Some(marker) if index == marker => "●".as_bytes(),
            _ => "─".as_bytes(),
        })?;
    }
    Ok(true)
}

impl<W: Write> Drop for Renderer<W> {
    fn drop(&mut self) {
        let _ = self.mpv.clear(&mut self.output);
        let _ = self.kitty.clear(&mut self.output);
        let _ = write!(
            self.output,
            "{}{}",
            color::Fg(color::Reset),
            color::Bg(color::Reset)
        );
        let _ = self.output.flush();
    }
}

fn flush_cells<W: Write>(
    output: &mut W,
    screen: &[Cell],
    previous: &mut Vec<Cell>,
    cols: u16,
) -> io::Result<()> {
    if cols == 0 {
        previous.clear();
        previous.extend_from_slice(screen);
        return Ok(());
    }
    for (index, cell) in screen.iter().enumerate() {
        if previous.get(index) == Some(cell) {
            continue;
        }
        let bottom_right = index + 1 == screen.len();
        if bottom_right {
            write!(output, "\x1b[?7l")?;
        }
        let x = index % usize::from(cols);
        let y = index / usize::from(cols);
        write!(output, "{}", cursor::Goto(x as u16 + 1, y as u16 + 1))?;
        write_foreground(output, cell.foreground)?;
        write_background(output, cell.background)?;
        write!(output, "{}", cell.grapheme)?;
        if bottom_right {
            write!(output, "\x1b[?7h")?;
        }
    }
    output.flush()?;
    previous.clear();
    previous.extend_from_slice(screen);
    Ok(())
}

fn write_foreground<W: Write>(output: &mut W, value: Option<Color>) -> io::Result<()> {
    match value {
        Some(value) => {
            let (r, g, b, _) = value.rgba();
            write!(output, "{}", color::Fg(color::Rgb(r, g, b)))
        }
        None => write!(output, "{}", color::Fg(color::Reset)),
    }
}

fn write_background<W: Write>(output: &mut W, value: Option<Color>) -> io::Result<()> {
    match value {
        Some(value) => {
            let (r, g, b, _) = value.rgba();
            write!(output, "{}", color::Bg(color::Rgb(r, g, b)))
        }
        None => write!(output, "{}", color::Bg(color::Reset)),
    }
}

fn paint_cells(nodes: &[PositionedNode<'_>], screen: &mut [Cell], cols: u16) {
    for item in nodes {
        let Node::Container(container) = item.node else {
            continue;
        };
        if let Some(background) = container.style.background_color {
            fill_background(screen, cols, item.rect, background);
        }
        if let Some(border) = &container.style.border {
            draw_frame(screen, cols, item.rect, border);
        }
    }
}

fn set(screen: &mut [Cell], cols: u16, x: u16, y: u16, text: &str, foreground: Color) {
    if x >= cols {
        return;
    }
    if let Some(cell) = screen.get_mut(usize::from(y) * usize::from(cols) + usize::from(x)) {
        cell.grapheme = text.into();
        cell.foreground = Some(foreground);
    }
}

fn fill_background(screen: &mut [Cell], cols: u16, rect: Rect, background: Color) {
    for y in rect.y..rect.y.saturating_add(rect.h) {
        for x in rect.x..rect.x.saturating_add(rect.w).min(cols) {
            if let Some(cell) = screen.get_mut(usize::from(y) * usize::from(cols) + usize::from(x))
            {
                *cell = Cell {
                    background: Some(background),
                    ..Cell::default()
                };
            }
        }
    }
}

fn draw_frame(screen: &mut [Cell], cols: u16, rect: Rect, border: &Border) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    let (tl, tr, bl, br, horizontal, vertical) = match border.style {
        BorderStyle::Plain => ("┌", "┐", "└", "┘", "─", "│"),
    };
    let right = rect.x + rect.w - 1;
    let bottom = rect.y + rect.h - 1;
    if rect.w == 1 {
        for y in rect.y..=bottom {
            set(screen, cols, rect.x, y, vertical, border.color)
        }
        return;
    }
    if rect.h == 1 {
        for x in rect.x..=right {
            set(screen, cols, x, rect.y, horizontal, border.color)
        }
        draw_title(screen, cols, rect, border);
        return;
    }
    for (x, y, text) in [
        (rect.x, rect.y, tl),
        (right, rect.y, tr),
        (rect.x, bottom, bl),
        (right, bottom, br),
    ] {
        set(screen, cols, x, y, text, border.color);
    }
    for x in rect.x + 1..right {
        set(screen, cols, x, rect.y, horizontal, border.color);
        set(screen, cols, x, bottom, horizontal, border.color);
    }
    for y in rect.y + 1..bottom {
        set(screen, cols, rect.x, y, vertical, border.color);
        set(screen, cols, right, y, vertical, border.color);
    }
    draw_title(screen, cols, rect, border);
}

fn draw_title(screen: &mut [Cell], cols: u16, rect: Rect, border: &Border) {
    let available = usize::from(rect.w.saturating_sub(2));
    let title: Vec<char> = border.title.chars().take(available).collect();
    let free = available - title.len();
    let offset = match border.title_alignment {
        TitleAlignment::Left => 0,
        TitleAlignment::Center => free / 2,
        TitleAlignment::Right => free,
    };
    let start = rect.x + 1 + offset as u16;
    for (index, character) in title.into_iter().enumerate() {
        set(
            screen,
            cols,
            start + index as u16,
            rect.y,
            &character.to_string(),
            border.title_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Video;
    use std::sync::atomic::Ordering;

    #[test]
    fn media_border_shows_action_time_and_progress() {
        let video = Video::from_path("docs/cars.mp4").unwrap();
        let controls = video.controls();
        controls
            .inner
            .position
            .store(12.0_f64.to_bits(), Ordering::Release);
        controls
            .inner
            .duration
            .store(152.0_f64.to_bits(), Ordering::Release);
        let border = MediaBorder {
            rect: Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 10,
            },
            color: Color::white(),
            controls,
        };
        let mut output = Vec::new();
        draw_media_border(&mut output, &border).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("\x1b]66;"));
        assert!(!output.contains('\u{fe0f}'));
        assert!(output.contains("00:12/02:32 "));
        assert!(output.contains('●'));

        border.controls.toggle_pause();
        let mut output = Vec::new();
        draw_media_border(&mut output, &border).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("\x1b]66;"));
        assert!(!output.contains('＞'));
        assert!(!output.contains('\u{fe0f}'));
        assert!(output.contains("00:12/02:32 "));
    }

    #[test]
    fn media_border_handles_play_and_seek_clicks() {
        let video = Video::from_path("docs/cars.mp4").unwrap();
        let controls = video.controls();
        controls
            .inner
            .duration
            .store(100.0_f64.to_bits(), Ordering::Release);
        let mut renderer = Renderer::with_kitty(
            Vec::new(),
            TerminalSize {
                cols: 80,
                rows: 24,
                ..TerminalSize::default()
            },
            KittyMode::Disabled,
        );
        renderer.media_borders.push(MediaBorder {
            rect: Rect {
                x: 0,
                y: 0,
                w: 64,
                h: 10,
            },
            color: Color::white(),
            controls: controls.clone(),
        });

        assert!(
            renderer
                .handle_mouse(MouseEvent::Press(MouseButton::Left, 3, 10))
                .unwrap()
        );
        assert!(controls.is_paused());

        assert!(
            renderer
                .handle_mouse(MouseEvent::Press(MouseButton::Left, 4, 10))
                .unwrap()
        );
        assert!(!controls.is_paused());

        assert!(
            renderer
                .handle_mouse(MouseEvent::Press(MouseButton::Left, 39, 10))
                .unwrap()
        );
        let target = f64::from_bits(controls.inner.seek_target.load(Ordering::Acquire));
        assert!((40.0..=60.0).contains(&target));

        assert!(renderer.handle_mouse(MouseEvent::Hold(55, 10)).unwrap());
        assert!(controls.position() > 75.0);
        assert!(renderer.handle_mouse(MouseEvent::Release(16, 10)).unwrap());
        assert_eq!(controls.position(), 0.0);
        assert!(renderer.media_drag.is_none());
        let parked_cursor = format!("{}{}", cursor::Goto(80, 24), cursor::Hide);
        assert!(
            String::from_utf8_lossy(&renderer.output).contains(&parked_cursor),
            "media controls must leave the cursor hidden outside the player"
        );
    }
}
