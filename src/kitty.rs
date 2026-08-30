use crate::{Color, Image, ImageFit, ImageId, TerminalSize, layout::Rect, tree::ImageSource};
use std::{
    collections::HashSet,
    env,
    io::{self, Write},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum KittyMode {
    #[default]
    Auto,
    Enabled,
    Disabled,
}

pub(crate) struct KittyRenderer {
    enabled: bool,
    uploaded: HashSet<ImageId>,
    media_actions: HashSet<u32>,
}

const MEDIA_ACTION_IMAGE_BASE: u32 = 1 << 30;

impl KittyRenderer {
    pub fn new(mode: KittyMode) -> Self {
        let enabled = match mode {
            KittyMode::Enabled => true,
            KittyMode::Disabled => false,
            KittyMode::Auto => detect_from_environment(),
        };
        Self {
            enabled,
            uploaded: HashSet::new(),
            media_actions: HashSet::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn draw<W: Write>(
        &mut self,
        output: &mut W,
        images: &[(&Image, Rect)],
        terminal: TerminalSize,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let active: HashSet<ImageId> = images.iter().map(|(image, _)| image.id).collect();
        let removed: Vec<ImageId> = self.uploaded.difference(&active).copied().collect();
        for id in removed {
            delete_image(output, id)?;
            self.uploaded.remove(&id);
        }

        let mut placements = std::collections::HashMap::new();
        for &(image, rect) in images {
            if self.uploaded.insert(image.id) {
                transmit(output, image)?
            }
            let placement = placements.entry(image.id).or_insert(0_u32);
            if *placement == 0 {
                delete_placements(output, image.id)?
            }
            *placement += 1;
            place(output, image, *placement, fit(image, rect, terminal))?;
        }
        output.flush()
    }

    pub fn clear<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        if self.enabled && (!self.uploaded.is_empty() || !self.media_actions.is_empty()) {
            write!(output, "\x1b_Ga=d,d=A,q=2;\x1b\\")?;
            output.flush()?;
            self.uploaded.clear();
            self.media_actions.clear();
        }
        Ok(())
    }

    pub fn draw_media_action<W: Write>(
        &mut self,
        output: &mut W,
        index: usize,
        rect: Rect,
        color: Color,
        play: bool,
        terminal: TerminalSize,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let id = media_action_id(index)?;
        let (width, height) = media_action_dimensions(terminal);
        let pixels = media_action_pixels(color, play, width, height);
        transmit_rgba(output, id, width, height, &pixels)?;
        let x = rect.x.saturating_add(2);
        let y = rect.y.saturating_add(rect.h.saturating_sub(1));
        write!(
            output,
            "\x1b7\x1b[{};{}H\x1b_Ga=p,i={id},p=1,c=2,r=1,z=32767,q=2;\x1b\\\x1b8",
            y + 1,
            x + 1
        )?;
        self.media_actions.insert(id);
        Ok(())
    }

    pub fn retain_media_actions<W: Write>(
        &mut self,
        output: &mut W,
        active_indices: &[usize],
    ) -> io::Result<()> {
        let active: HashSet<u32> = active_indices
            .iter()
            .filter_map(|&index| media_action_id(index).ok())
            .collect();
        let removed: Vec<u32> = self
            .media_actions
            .iter()
            .copied()
            .filter(|id| !active.contains(id))
            .collect();
        for id in removed {
            write!(output, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
            self.media_actions.remove(&id);
        }
        Ok(())
    }
}

fn media_action_id(index: usize) -> io::Result<u32> {
    let index = u32::try_from(index).map_err(|_| io::Error::other("too many media controls"))?;
    if index >= MEDIA_ACTION_IMAGE_BASE {
        return Err(io::Error::other("too many media controls"));
    }
    Ok(MEDIA_ACTION_IMAGE_BASE + index)
}

fn media_action_dimensions(terminal: TerminalSize) -> (usize, usize) {
    let cell_width = if terminal.pixel_width > 0 && terminal.cols > 0 {
        (usize::from(terminal.pixel_width) + usize::from(terminal.cols) / 2)
            / usize::from(terminal.cols)
    } else {
        8
    };
    let cell_height = if terminal.pixel_height > 0 && terminal.rows > 0 {
        (usize::from(terminal.pixel_height) + usize::from(terminal.rows) / 2)
            / usize::from(terminal.rows)
    } else {
        16
    };
    (cell_width.max(1) * 2, cell_height.max(1))
}

fn media_action_pixels(color: Color, play: bool, width: usize, height: usize) -> Vec<u8> {
    let mut pixels = vec![0; width * height * 4];
    let rgba = color.rgba();

    if play {
        const SAMPLES: usize = 4;
        let left = width as f64 * 0.22;
        let tip = width as f64 * 0.82;
        let top = height as f64 * 0.12;
        let bottom = height as f64 * 0.88;
        let center = (top + bottom) / 2.0;
        let half_height = (bottom - top) / 2.0;
        for y in 0..height {
            for x in 0..width {
                let mut covered = 0;
                for sample_y in 0..SAMPLES {
                    let sample_y = y as f64 + (sample_y as f64 + 0.5) / SAMPLES as f64;
                    if !(top..=bottom).contains(&sample_y) {
                        continue;
                    }
                    let edge = tip - (sample_y - center).abs() * (tip - left) / half_height;
                    for sample_x in 0..SAMPLES {
                        let sample_x = x as f64 + (sample_x as f64 + 0.5) / SAMPLES as f64;
                        covered += usize::from(sample_x >= left && sample_x <= edge);
                    }
                }
                if covered > 0 {
                    let alpha = (usize::from(rgba.3) * covered + SAMPLES * SAMPLES / 2)
                        / (SAMPLES * SAMPLES);
                    let offset = (y * width + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[
                        rgba.0,
                        rgba.1,
                        rgba.2,
                        alpha as u8,
                    ]);
                }
            }
        }
    } else {
        let top = height * 3 / 24;
        let bottom = height * 21 / 24;
        for y in top..bottom {
            for (start, end) in [
                (width * 5 / 24, width * 9 / 24),
                (width * 15 / 24, width * 19 / 24),
            ] {
                for x in start..end {
                    let offset = (y * width + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[rgba.0, rgba.1, rgba.2, rgba.3]);
                }
            }
        }
    }
    pixels
}

fn transmit_rgba<W: Write>(
    output: &mut W,
    id: u32,
    width: usize,
    height: usize,
    pixels: &[u8],
) -> io::Result<()> {
    let encoded = base64(pixels);
    for (index, chunk) in encoded.as_bytes().chunks(4096).enumerate() {
        let chunk = std::str::from_utf8(chunk).expect("base64 is ASCII");
        let more = u8::from((index + 1) * 4096 < encoded.len());
        if index == 0 {
            write!(
                output,
                "\x1b_Ga=t,f=32,t=d,i={id},s={width},v={height},m={more},q=2;{chunk}\x1b\\"
            )?;
        } else {
            write!(output, "\x1b_Gm={more};{chunk}\x1b\\")?;
        }
    }
    Ok(())
}

fn detect_from_environment() -> bool {
    let term = env::var("TERM").unwrap_or_default().to_ascii_lowercase();
    let program = env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    term.contains("kitty") || program.contains("wezterm") || program.contains("ghostty")
}

fn transmit<W: Write>(output: &mut W, image: &Image) -> io::Result<()> {
    let (format, dimensions, bytes): (u8, String, &[u8]) = match &image.source {
        ImageSource::Png(bytes) => (100, String::new(), bytes),
        ImageSource::Rgba {
            width,
            height,
            pixels,
        } => (32, format!(",s={width},v={height}"), pixels),
    };
    let encoded = base64(bytes);
    if encoded.is_empty() {
        return write!(
            output,
            "\x1b_Ga=t,f={format},t=d,i={}{};\x1b\\",
            image.id.get(),
            dimensions
        );
    }
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(4096)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
        .collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        if index == 0 {
            write!(
                output,
                "\x1b_Ga=t,f={format},t=d,i={}{}{},q=2;{}\x1b\\",
                image.id.get(),
                dimensions,
                format_args!(",m={more}"),
                chunk
            )?;
        } else {
            write!(output, "\x1b_Gm={more};{chunk}\x1b\\")?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Placement {
    rect: Rect,
    crop: Option<(u32, u32, u32, u32)>,
}

fn place<W: Write>(
    output: &mut W,
    image: &Image,
    placement_id: u32,
    placement: Placement,
) -> io::Result<()> {
    let rect = placement.rect;
    if rect.w == 0 || rect.h == 0 {
        return Ok(());
    }
    write!(output, "\x1b7\x1b[{};{}H", rect.y + 1, rect.x + 1)?;
    write!(
        output,
        "\x1b_Ga=p,i={},p={},c={},r={},z={},q=2",
        image.id.get(),
        placement_id,
        rect.w,
        rect.h,
        image.style.z
    )?;
    if let Some((x, y, w, h)) = placement.crop {
        write!(output, ",x={x},y={y},w={w},h={h}")?;
    }
    write!(output, ";\x1b\\\x1b8")
}

fn delete_image<W: Write>(output: &mut W, id: ImageId) -> io::Result<()> {
    write!(output, "\x1b_Ga=d,d=I,i={},q=2;\x1b\\", id.get())
}

fn delete_placements<W: Write>(output: &mut W, id: ImageId) -> io::Result<()> {
    write!(output, "\x1b_Ga=d,d=i,i={},q=2;\x1b\\", id.get())
}

fn fit(image: &Image, rect: Rect, terminal: TerminalSize) -> Placement {
    let Some((source_w, source_h)) = dimensions(image) else {
        return Placement { rect, crop: None };
    };
    let cell_w = if terminal.pixel_width > 0 && terminal.cols > 0 {
        terminal.pixel_width as f64 / terminal.cols as f64
    } else {
        8.0
    };
    let cell_h = if terminal.pixel_height > 0 && terminal.rows > 0 {
        terminal.pixel_height as f64 / terminal.rows as f64
    } else {
        16.0
    };
    let target_w = rect.w as f64 * cell_w;
    let target_h = rect.h as f64 * cell_h;

    match image.fit {
        ImageFit::Fill => Placement { rect, crop: None },
        ImageFit::Contain | ImageFit::Original => {
            let scale = if image.fit == ImageFit::Original {
                1.0
            } else {
                (target_w / source_w as f64).min(target_h / source_h as f64)
            };
            let cols = ((source_w as f64 * scale) / cell_w).ceil() as u16;
            let rows = ((source_h as f64 * scale) / cell_h).ceil() as u16;
            let cols = cols.min(rect.w);
            let rows = rows.min(rect.h);
            Placement {
                rect: Rect {
                    x: rect.x + (rect.w - cols) / 2,
                    y: rect.y + (rect.h - rows) / 2,
                    w: cols,
                    h: rows,
                },
                crop: None,
            }
        }
        ImageFit::Cover => {
            let source_aspect = source_w as f64 / source_h as f64;
            let target_aspect = target_w / target_h.max(1.0);
            let crop = if source_aspect > target_aspect {
                let width = (source_h as f64 * target_aspect) as u32;
                ((source_w - width) / 2, 0, width, source_h)
            } else {
                let height = (source_w as f64 / target_aspect) as u32;
                (0, (source_h - height) / 2, source_w, height)
            };
            Placement {
                rect,
                crop: Some(crop),
            }
        }
    }
}

fn dimensions(image: &Image) -> Option<(u32, u32)> {
    image.dimensions()
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((value >> 18) & 63) as usize] as char);
        output.push(TABLE[((value >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_actions_are_colored_transparent_two_cell_images() {
        let terminal = TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 800,
            pixel_height: 480,
        };
        let (width, height) = media_action_dimensions(terminal);
        assert_eq!((width, height), (20, 20));
        let play = media_action_pixels(Color::RED, true, width, height);
        assert_eq!(&play[..4], &[0, 0, 0, 0]);
        assert!(play.chunks_exact(4).any(|pixel| pixel == [255, 0, 0, 255]));
        assert!(
            play.chunks_exact(4)
                .filter(|pixel| pixel[3] != 0)
                .all(|pixel| pixel[..3] == [255, 0, 0])
        );
        assert!(
            play.chunks_exact(4)
                .any(|pixel| pixel[3] > 0 && pixel[3] < 255)
        );
        let visible_in_row = |y: usize| {
            play[y * width * 4..(y + 1) * width * 4]
                .chunks_exact(4)
                .filter(|pixel| pixel[3] != 0)
                .count()
        };
        let visible_rows: Vec<_> = (0..height)
            .map(visible_in_row)
            .filter(|&width| width > 0)
            .collect();
        assert!(visible_rows.first().is_some_and(|&width| width <= 2));
        assert!(visible_rows.last().is_some_and(|&width| width <= 2));
        assert!(base64(&play).len() <= 4096);

        let mut renderer = KittyRenderer::new(KittyMode::Enabled);
        let mut output = Vec::new();
        renderer
            .draw_media_action(
                &mut output,
                0,
                Rect {
                    x: 4,
                    y: 3,
                    w: 20,
                    h: 5,
                },
                Color::RED,
                true,
                terminal,
            )
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("a=t,f=32"));
        assert!(output.contains("s=20,v=20"));
        assert!(output.contains("c=2,r=1"));
        assert!(!output.contains("\x1b]66;"));
    }
}
