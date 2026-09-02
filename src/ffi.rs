use crate::{
    AlignItems, AlignSelf, App, Border, Color, Container, Event, FlexDirection, Image, ImageFit,
    JustifyContent, KittyMode, Node, Padding, Position, Runtime, RuntimeConfig, Style,
    TerminalSize, TitleAlignment, Video,
};
use std::{
    any::Any,
    ffi::{CString, c_char},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, slice, str,
    time::Duration,
};
use termion::event::{Key, MouseButton, MouseEvent};

const FX_OK: i32 = 0;
const FX_ERROR: i32 = 1;

#[repr(C)]
pub struct FxResult {
    code: i32,
    message: *mut c_char,
}

impl FxResult {
    fn ok() -> Self {
        Self {
            code: FX_OK,
            message: ptr::null_mut(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        let message = message.into().replace('\0', "\\0");
        let message = CString::new(message).expect("nul bytes were replaced");
        Self {
            code: FX_ERROR,
            message: message.into_raw(),
        }
    }
}

fn ffi_result(action: impl FnOnce() -> Result<(), String>) -> FxResult {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(Ok(())) => FxResult::ok(),
        Ok(Err(error)) => FxResult::error(error),
        Err(panic) => FxResult::error(format!("Rust panic: {}", panic_message(&panic))),
    }
}

fn panic_message(panic: &Box<dyn Any + Send>) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

fn output<'a, T>(pointer: *mut *mut T) -> Result<&'a mut *mut T, String> {
    if pointer.is_null() {
        return Err("output pointer is null".into());
    }
    Ok(unsafe { &mut *pointer })
}

fn required<'a, T>(pointer: *const T, name: &str) -> Result<&'a T, String> {
    if pointer.is_null() {
        return Err(format!("{name} is null"));
    }
    Ok(unsafe { &*pointer })
}

fn required_mut<'a, T>(pointer: *mut T, name: &str) -> Result<&'a mut T, String> {
    if pointer.is_null() {
        return Err(format!("{name} is null"));
    }
    Ok(unsafe { &mut *pointer })
}

fn bytes<'a>(pointer: *const u8, length: usize, name: &str) -> Result<&'a [u8], String> {
    if length == 0 {
        return Ok(&[]);
    }
    if pointer.is_null() {
        return Err(format!("{name} is null"));
    }
    Ok(unsafe { slice::from_raw_parts(pointer, length) })
}

fn text<'a>(pointer: *const u8, length: usize, name: &str) -> Result<&'a str, String> {
    str::from_utf8(bytes(pointer, length, name)?).map_err(|error| error.to_string())
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FxRuntimeConfig {
    kitty_mode: u32,
    event_wait_ms: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FxTerminalSize {
    cols: u16,
    rows: u16,
    pixel_width: u16,
    pixel_height: u16,
}

impl From<TerminalSize> for FxTerminalSize {
    fn from(size: TerminalSize) -> Self {
        Self {
            cols: size.cols,
            rows: size.rows,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FxEvent {
    kind: u32,
    key: u32,
    value: u32,
    mouse_kind: u32,
    mouse_button: u32,
    x: u16,
    y: u16,
    size: FxTerminalSize,
}

impl Default for FxEvent {
    fn default() -> Self {
        Self {
            kind: 0,
            key: 0,
            value: 0,
            mouse_kind: 0,
            mouse_button: 0,
            x: 0,
            y: 0,
            size: TerminalSize::default().into(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FxStyle {
    has_width: u8,
    has_height: u8,
    width: u16,
    height: u16,
    x: u16,
    y: u16,
    padding_kind: u32,
    padding: u8,
    has_border: u8,
    has_background: u8,
    has_align_self: u8,
    border_color: u32,
    title_color: u32,
    background_color: u32,
    title: *const u8,
    title_len: usize,
    title_alignment: u32,
    media_video: *const FxVideo,
    gap: u16,
    position: u32,
    z: i16,
    align_items: u32,
    align_self: u32,
    flex_direction: u32,
    justify_content: u32,
}

pub struct FxRuntime {
    inner: Runtime,
}

pub struct FxNode {
    inner: Node,
}

pub struct FxImage {
    inner: Image,
}

pub struct FxVideo {
    inner: Video,
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_result_free(result: FxResult) {
    if !result.message.is_null() {
        unsafe { drop(CString::from_raw(result.message)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_runtime_new(config: FxRuntimeConfig, out: *mut *mut FxRuntime) -> FxResult {
    ffi_result(|| {
        let kitty = kitty_mode(config.kitty_mode)?;
        let runtime = Runtime::with_config(RuntimeConfig {
            kitty,
            event_wait: Duration::from_millis(u64::from(config.event_wait_ms)),
        })
        .map_err(|error| error.to_string())?;
        *output(out)? = Box::into_raw(Box::new(FxRuntime { inner: runtime }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_runtime_free(runtime: *mut FxRuntime) {
    if !runtime.is_null() {
        unsafe { drop(Box::from_raw(runtime)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_runtime_size(runtime: *const FxRuntime, out: *mut FxTerminalSize) -> FxResult {
    ffi_result(|| {
        let runtime = required(runtime, "runtime")?;
        if out.is_null() {
            return Err("output pointer is null".into());
        }
        unsafe { *out = runtime.inner.size().into() };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_runtime_next_event(runtime: *mut FxRuntime, out: *mut FxEvent) -> FxResult {
    ffi_result(|| {
        let runtime = required_mut(runtime, "runtime")?;
        if out.is_null() {
            return Err("output pointer is null".into());
        }
        let event = runtime
            .inner
            .next_event()
            .map_err(|error| error.to_string())?;
        unsafe { *out = event.map(event_to_ffi).unwrap_or_else(end_event) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_runtime_draw(runtime: *const FxRuntime, root: *const FxNode) -> FxResult {
    ffi_result(|| {
        let runtime = required(runtime, "runtime")?;
        let root = required(root, "root")?;
        runtime
            .inner
            .draw(&App::root(root.inner.clone()))
            .map_err(|error| error.to_string())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_container_new(style: FxStyle, out: *mut *mut FxNode) -> FxResult {
    ffi_result(|| {
        let container = Container::new().style(style_from_ffi(style)?);
        *output(out)? = Box::into_raw(Box::new(FxNode {
            inner: container.into(),
        }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_container_add(parent: *mut FxNode, child: *const FxNode) -> FxResult {
    ffi_result(|| {
        let parent = required_mut(parent, "parent")?;
        let child = required(child, "child")?;
        let Node::Container(container) = &mut parent.inner else {
            return Err("parent is not a container".into());
        };
        container.items.push(child.inner.clone());
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_image_from_path(
    path: *const u8,
    path_len: usize,
    out: *mut *mut FxImage,
) -> FxResult {
    ffi_result(|| {
        let image =
            Image::from_path(text(path, path_len, "path")?).map_err(|error| error.to_string())?;
        *output(out)? = Box::into_raw(Box::new(FxImage { inner: image }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_image_from_rgba(
    width: u32,
    height: u32,
    pixels: *const u8,
    pixels_len: usize,
    out: *mut *mut FxImage,
) -> FxResult {
    ffi_result(|| {
        let image = Image::from_rgba(width, height, bytes(pixels, pixels_len, "pixels")?.to_vec())
            .map_err(|error| error.to_string())?;
        *output(out)? = Box::into_raw(Box::new(FxImage { inner: image }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_image_from_png(
    data: *const u8,
    data_len: usize,
    out: *mut *mut FxImage,
) -> FxResult {
    ffi_result(|| {
        let image = Image::from_png(bytes(data, data_len, "PNG data")?.to_vec());
        if image.dimensions().is_none() {
            return Err("invalid PNG data".into());
        }
        *output(out)? = Box::into_raw(Box::new(FxImage { inner: image }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_image_node(
    image: *const FxImage,
    style: FxStyle,
    fit: u32,
    out: *mut *mut FxNode,
) -> FxResult {
    ffi_result(|| {
        let image = required(image, "image")?;
        let node = image
            .inner
            .clone()
            .style(style_from_ffi(style)?)
            .fit(image_fit(fit)?);
        *output(out)? = Box::into_raw(Box::new(FxNode { inner: node.into() }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_image_free(image: *mut FxImage) {
    if !image.is_null() {
        unsafe { drop(Box::from_raw(image)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_from_path(
    path: *const u8,
    path_len: usize,
    out: *mut *mut FxVideo,
) -> FxResult {
    ffi_result(|| {
        let video =
            Video::from_path(text(path, path_len, "path")?).map_err(|error| error.to_string())?;
        *output(out)? = Box::into_raw(Box::new(FxVideo { inner: video }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_node(
    video: *const FxVideo,
    style: FxStyle,
    fit: u32,
    out: *mut *mut FxNode,
) -> FxResult {
    ffi_result(|| {
        let video = required(video, "video")?;
        let node = video
            .inner
            .clone()
            .style(style_from_ffi(style)?)
            .fit(image_fit(fit)?);
        *output(out)? = Box::into_raw(Box::new(FxNode { inner: node.into() }));
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_free(video: *mut FxVideo) {
    if !video.is_null() {
        unsafe { drop(Box::from_raw(video)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_play(video: *const FxVideo) -> FxResult {
    video_action(video, Video::play)
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_pause(video: *const FxVideo) -> FxResult {
    video_action(video, Video::pause)
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_toggle_pause(video: *const FxVideo) -> FxResult {
    video_action(video, Video::toggle_pause)
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_seek_forward(video: *const FxVideo) -> FxResult {
    video_action(video, Video::seek_forward)
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_seek_backward(video: *const FxVideo) -> FxResult {
    video_action(video, Video::seek_backward)
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_set_volume(video: *const FxVideo, volume: u8) -> FxResult {
    ffi_result(|| {
        required(video, "video")?.inner.set_volume(volume);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_seek_to(video: *const FxVideo, position: f64) -> FxResult {
    ffi_result(|| {
        required(video, "video")?.inner.controls().seek_to(position);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_video_state(
    video: *const FxVideo,
    paused: *mut u8,
    volume: *mut u8,
    position: *mut f64,
    duration: *mut f64,
) -> FxResult {
    ffi_result(|| {
        let video = required(video, "video")?;
        if paused.is_null() || volume.is_null() || position.is_null() || duration.is_null() {
            return Err("state output pointer is null".into());
        }
        let controls = video.inner.controls();
        unsafe {
            *paused = u8::from(video.inner.is_paused());
            *volume = video.inner.volume();
            *position = controls.position();
            *duration = controls.duration();
        }
        Ok(())
    })
}

fn video_action(video: *const FxVideo, action: fn(&Video)) -> FxResult {
    ffi_result(|| {
        action(&required(video, "video")?.inner);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fx_node_free(node: *mut FxNode) {
    if !node.is_null() {
        unsafe { drop(Box::from_raw(node)) };
    }
}

fn style_from_ffi(value: FxStyle) -> Result<Style, String> {
    let mut style = Style::new()
        .offset(value.x, value.y)
        .padding(padding(value.padding_kind, value.padding)?)
        .gap(value.gap)
        .position(position(value.position)?)
        .z(value.z)
        .align_items(align_items(value.align_items)?)
        .direction(flex_direction(value.flex_direction)?)
        .justify(justify_content(value.justify_content)?);
    if value.has_width != 0 {
        style = style.width(value.width);
    }
    if value.has_height != 0 {
        style = style.height(value.height);
    }
    if value.has_background != 0 {
        style = style.background(color(value.background_color));
    }
    if value.has_align_self != 0 {
        style = style.align_self(align_self(value.align_self)?);
    }
    if value.has_border != 0 {
        let mut border = Border::plain(color(value.border_color))
            .title(text(value.title, value.title_len, "title")?)
            .title_color(color(value.title_color))
            .title_alignment(title_alignment(value.title_alignment)?);
        if !value.media_video.is_null() {
            border = border.media_controls(unsafe { &*value.media_video }.inner.controls());
        }
        style = style.border(border);
    }
    Ok(style)
}

fn color(value: u32) -> Color {
    Color::from_rgba(
        (value >> 24) as u8,
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    )
}

fn kitty_mode(value: u32) -> Result<KittyMode, String> {
    match value {
        0 => Ok(KittyMode::Auto),
        1 => Ok(KittyMode::Enabled),
        2 => Ok(KittyMode::Disabled),
        _ => Err("invalid Kitty mode".into()),
    }
}

fn image_fit(value: u32) -> Result<ImageFit, String> {
    match value {
        0 => Ok(ImageFit::Fill),
        1 => Ok(ImageFit::Contain),
        2 => Ok(ImageFit::Cover),
        3 => Ok(ImageFit::Original),
        _ => Err("invalid image fit".into()),
    }
}

fn padding(kind: u32, value: u8) -> Result<Padding, String> {
    match kind {
        0 => Ok(Padding::All(value)),
        1 => Ok(Padding::Top(value)),
        2 => Ok(Padding::Bottom(value)),
        3 => Ok(Padding::Right(value)),
        4 => Ok(Padding::Left(value)),
        5 => Ok(Padding::Horizontal(value)),
        6 => Ok(Padding::Vertical(value)),
        _ => Err("invalid padding".into()),
    }
}

fn position(value: u32) -> Result<Position, String> {
    match value {
        0 => Ok(Position::Relative),
        1 => Ok(Position::Absolute),
        _ => Err("invalid position".into()),
    }
}

fn align_items(value: u32) -> Result<AlignItems, String> {
    match value {
        0 => Ok(AlignItems::Start),
        1 => Ok(AlignItems::End),
        2 => Ok(AlignItems::Center),
        3 => Ok(AlignItems::Stretch),
        _ => Err("invalid item alignment".into()),
    }
}

fn align_self(value: u32) -> Result<AlignSelf, String> {
    match value {
        0 => Ok(AlignSelf::Start),
        1 => Ok(AlignSelf::End),
        2 => Ok(AlignSelf::Center),
        3 => Ok(AlignSelf::Stretch),
        _ => Err("invalid self alignment".into()),
    }
}

fn flex_direction(value: u32) -> Result<FlexDirection, String> {
    match value {
        0 => Ok(FlexDirection::Column),
        1 => Ok(FlexDirection::ColumnReverse),
        2 => Ok(FlexDirection::Row),
        3 => Ok(FlexDirection::RowReverse),
        _ => Err("invalid flex direction".into()),
    }
}

fn justify_content(value: u32) -> Result<JustifyContent, String> {
    match value {
        0 => Ok(JustifyContent::Start),
        1 => Ok(JustifyContent::End),
        2 => Ok(JustifyContent::Center),
        _ => Err("invalid justification".into()),
    }
}

fn title_alignment(value: u32) -> Result<TitleAlignment, String> {
    match value {
        0 => Ok(TitleAlignment::Left),
        1 => Ok(TitleAlignment::Center),
        2 => Ok(TitleAlignment::Right),
        _ => Err("invalid title alignment".into()),
    }
}

fn end_event() -> FxEvent {
    FxEvent {
        kind: 4,
        ..FxEvent::default()
    }
}

fn event_to_ffi(event: Event) -> FxEvent {
    match event {
        Event::Init => FxEvent {
            kind: 0,
            ..FxEvent::default()
        },
        Event::Key(key) => key_event(key),
        Event::Mouse(mouse) => mouse_event(mouse),
        Event::Resize(size) => FxEvent {
            kind: 3,
            size: size.into(),
            ..FxEvent::default()
        },
    }
}

fn key_event(key: Key) -> FxEvent {
    let (key, value) = match key {
        Key::Backspace => (1, 0),
        Key::Left => (2, 0),
        Key::ShiftLeft => (3, 0),
        Key::AltLeft => (4, 0),
        Key::CtrlLeft => (5, 0),
        Key::Right => (6, 0),
        Key::ShiftRight => (7, 0),
        Key::AltRight => (8, 0),
        Key::CtrlRight => (9, 0),
        Key::Up => (10, 0),
        Key::ShiftUp => (11, 0),
        Key::AltUp => (12, 0),
        Key::CtrlUp => (13, 0),
        Key::Down => (14, 0),
        Key::ShiftDown => (15, 0),
        Key::AltDown => (16, 0),
        Key::CtrlDown => (17, 0),
        Key::Home => (18, 0),
        Key::CtrlHome => (19, 0),
        Key::End => (20, 0),
        Key::CtrlEnd => (21, 0),
        Key::PageUp => (22, 0),
        Key::PageDown => (23, 0),
        Key::BackTab => (24, 0),
        Key::Delete => (25, 0),
        Key::Insert => (26, 0),
        Key::F(number) => (27, u32::from(number)),
        Key::Char(character) => (28, character as u32),
        Key::Alt(character) => (29, character as u32),
        Key::Ctrl(character) => (30, character as u32),
        Key::Null => (31, 0),
        Key::Esc => (32, 0),
        Key::__IsNotComplete => (0, 0),
    };
    FxEvent {
        kind: 1,
        key,
        value,
        ..FxEvent::default()
    }
}

fn mouse_event(event: MouseEvent) -> FxEvent {
    let (mouse_kind, mouse_button, x, y) = match event {
        MouseEvent::Press(button, x, y) => (1, mouse_button(button), x, y),
        MouseEvent::Release(x, y) => (2, 0, x, y),
        MouseEvent::Hold(x, y) => (3, 0, x, y),
    };
    FxEvent {
        kind: 2,
        mouse_kind,
        mouse_button,
        x,
        y,
        ..FxEvent::default()
    }
}

fn mouse_button(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 3,
        MouseButton::WheelUp => 4,
        MouseButton::WheelDown => 5,
        MouseButton::WheelLeft => 6,
        MouseButton::WheelRight => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_errors_include_invalid_values() {
        assert_eq!(image_fit(99).unwrap_err(), "invalid image fit");
        assert_eq!(kitty_mode(99).unwrap_err(), "invalid Kitty mode");
    }

    #[test]
    fn ffi_events_keep_key_and_mouse_data() {
        let key = event_to_ffi(Event::Key(Key::Alt('x')));
        assert_eq!((key.kind, key.key, key.value), (1, 29, 'x' as u32));

        let mouse = event_to_ffi(Event::Mouse(MouseEvent::Press(MouseButton::Left, 4, 8)));
        assert_eq!(
            (
                mouse.kind,
                mouse.mouse_kind,
                mouse.mouse_button,
                mouse.x,
                mouse.y
            ),
            (2, 1, 1, 4, 8)
        );
    }
}
