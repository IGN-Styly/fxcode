use crate::{Style, TerminalSize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
};

static NEXT_IMAGE_ID: AtomicU32 = AtomicU32::new(1);
static NEXT_VIDEO_ID: AtomicU32 = AtomicU32::new(1);

#[derive(Debug, Default, Clone)]
pub struct App {
    pub tree: Vec<Node>,
}

impl App {
    pub fn new(tree: impl IntoIterator<Item = Node>) -> Self {
        Self {
            tree: tree.into_iter().collect(),
        }
    }

    pub fn root(node: impl Into<Node>) -> Self {
        Self {
            tree: vec![node.into()],
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Container {
    pub style: Style,
    pub items: Vec<Node>,
}

impl Container {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn child(mut self, child: impl Into<Node>) -> Self {
        self.items.push(child.into());
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = Node>) -> Self {
        self.items.extend(children);
        self
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Container(Container),
    Image(Image),
    Video(Video),
}

impl Node {
    pub fn style(&self) -> &Style {
        match self {
            Self::Container(container) => &container.style,
            Self::Image(image) => &image.style,
            Self::Video(video) => &video.style,
        }
    }
}

impl From<Container> for Node {
    fn from(value: Container) -> Self {
        Self::Container(value)
    }
}

impl From<Image> for Node {
    fn from(value: Image) -> Self {
        Self::Image(value)
    }
}

impl From<Video> for Node {
    fn from(value: Video) -> Self {
        Self::Video(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageId(u32);

impl ImageId {
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ImageFit {
    #[default]
    Fill,
    Contain,
    Cover,
    Original,
}

#[derive(Debug, Clone)]
pub struct Image {
    pub id: ImageId,
    pub style: Style,
    pub fit: ImageFit,
    pub(crate) source: ImageSource,
}

#[derive(Debug, Clone)]
pub(crate) enum ImageSource {
    Png(Arc<[u8]>),
    Rgba {
        width: u32,
        height: u32,
        pixels: Arc<[u8]>,
    },
}

impl Image {
    pub fn from_png(bytes: impl Into<Vec<u8>>) -> Self {
        Self::new(ImageSource::Png(bytes.into().into()))
    }

    pub fn from_png_path(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self::from_png(fs::read(path)?))
    }

    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Ok(Self::from_png(bytes));
        }

        let decoded = image::load_from_memory(&bytes).map_err(io::Error::other)?;
        let rgba = decoded.to_rgba8();
        Self::from_rgba(rgba.width(), rgba.height(), rgba.into_raw())
    }

    pub fn from_rgba(width: u32, height: u32, pixels: impl Into<Vec<u8>>) -> io::Result<Self> {
        let pixels = pixels.into();
        let expected = width as u64 * height as u64 * 4;
        if pixels.len() as u64 != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("expected {expected} RGBA bytes, got {}", pixels.len()),
            ));
        }
        Ok(Self::new(ImageSource::Rgba {
            width,
            height,
            pixels: pixels.into(),
        }))
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match &self.source {
            ImageSource::Rgba { width, height, .. } => Some((*width, *height)),
            ImageSource::Png(bytes) if bytes.len() >= 24 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" => {
                Some((
                    u32::from_be_bytes(bytes[16..20].try_into().ok()?),
                    u32::from_be_bytes(bytes[20..24].try_into().ok()?),
                ))
            }
            ImageSource::Png(_) => None,
        }
    }

    pub fn cell_size_within(
        &self,
        max_width: u16,
        max_height: u16,
        terminal: TerminalSize,
    ) -> Option<(u16, u16)> {
        let (width, height) = self.dimensions()?;
        if width == 0 || height == 0 || max_width == 0 || max_height == 0 {
            return Some((0, 0));
        }

        let cell_width = if terminal.pixel_width > 0 && terminal.cols > 0 {
            f64::from(terminal.pixel_width) / f64::from(terminal.cols)
        } else {
            8.0
        };
        let cell_height = if terminal.pixel_height > 0 && terminal.rows > 0 {
            f64::from(terminal.pixel_height) / f64::from(terminal.rows)
        } else {
            16.0
        };
        let scale = (f64::from(max_width) * cell_width / f64::from(width))
            .min(f64::from(max_height) * cell_height / f64::from(height));

        Some((
            ((f64::from(width) * scale / cell_width).ceil() as u16).min(max_width),
            ((f64::from(height) * scale / cell_height).ceil() as u16).min(max_height),
        ))
    }

    fn new(source: ImageSource) -> Self {
        let id = NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed).max(1);
        Self {
            id: ImageId(id),
            style: Style::default(),
            fit: ImageFit::Fill,
            source,
        }
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn fit(mut self, fit: ImageFit) -> Self {
        self.fit = fit;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoId(u32);

impl VideoId {
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct Video {
    pub id: VideoId,
    pub style: Style,
    pub fit: ImageFit,
    pub(crate) path: Arc<PathBuf>,
    pub(crate) control: VideoControls,
}

#[derive(Debug)]
pub(crate) struct VideoControl {
    pub paused: AtomicBool,
    pub volume: AtomicU8,
    pub seek_steps: AtomicI32,
    pub position: AtomicU64,
    pub duration: AtomicU64,
    pub seek_target: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct VideoControls {
    pub(crate) inner: Arc<VideoControl>,
}

impl VideoControls {
    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Acquire)
    }

    pub fn position(&self) -> f64 {
        f64::from_bits(self.inner.position.load(Ordering::Acquire))
    }

    pub fn duration(&self) -> f64 {
        f64::from_bits(self.inner.duration.load(Ordering::Acquire))
    }

    pub fn toggle_pause(&self) {
        self.inner.paused.fetch_xor(true, Ordering::AcqRel);
    }

    pub fn seek_to(&self, position: f64) {
        let position = position.max(0.0);
        self.inner
            .position
            .store(position.to_bits(), Ordering::Release);
        self.inner
            .seek_target
            .store(position.to_bits(), Ordering::Release);
    }
}

impl Video {
    pub fn from_path(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if !fs::metadata(&path)?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "video path is not a file",
            ));
        }
        let id = NEXT_VIDEO_ID.fetch_add(1, Ordering::Relaxed).max(1);
        Ok(Self {
            id: VideoId(id),
            style: Style::default(),
            fit: ImageFit::Contain,
            path: Arc::new(path),
            control: VideoControls {
                inner: Arc::new(VideoControl {
                    paused: AtomicBool::new(false),
                    volume: AtomicU8::new(100),
                    seek_steps: AtomicI32::new(0),
                    position: AtomicU64::new(0.0_f64.to_bits()),
                    duration: AtomicU64::new(0.0_f64.to_bits()),
                    seek_target: AtomicU64::new((-1.0_f64).to_bits()),
                }),
            },
        })
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn fit(mut self, fit: ImageFit) -> Self {
        self.fit = fit;
        self
    }

    pub fn play(&self) {
        self.control.inner.paused.store(false, Ordering::Release);
    }

    pub fn pause(&self) {
        self.control.inner.paused.store(true, Ordering::Release);
    }

    pub fn toggle_pause(&self) {
        self.control.toggle_pause();
    }

    pub fn is_paused(&self) -> bool {
        self.control.is_paused()
    }

    pub fn seek_forward(&self) {
        self.control.inner.seek_steps.fetch_add(1, Ordering::AcqRel);
    }

    pub fn seek_backward(&self) {
        self.control.inner.seek_steps.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn volume(&self) -> u8 {
        self.control.inner.volume.load(Ordering::Acquire)
    }

    pub fn set_volume(&self, volume: u8) {
        self.control
            .inner
            .volume
            .store(volume.min(100), Ordering::Release);
    }

    pub fn controls(&self) -> VideoControls {
        self.control.clone()
    }
}
