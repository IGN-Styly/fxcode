use crate::{ImageFit, Node, TerminalSize, Video, VideoControls, VideoId, layout::PositionedNode};
use std::{
    alloc::{Layout, alloc, dealloc, handle_alloc_error},
    collections::HashMap,
    ffi::{CStr, CString, c_char, c_double, c_int, c_void},
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;
const MPV_RENDER_UPDATE_FRAME: u64 = 1;
const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_DOUBLE: c_int = 5;

#[repr(C)]
struct MpvHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct MpvRenderContext {
    _private: [u8; 0],
}

#[repr(C)]
struct MpvRenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
}

#[link(name = "mpv")]
unsafe extern "C" {
    fn mpv_create() -> *mut MpvHandle;
    fn mpv_initialize(handle: *mut MpvHandle) -> c_int;
    fn mpv_set_option_string(
        handle: *mut MpvHandle,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;
    fn mpv_command(handle: *mut MpvHandle, args: *const *const c_char) -> c_int;
    fn mpv_set_property_string(
        handle: *mut MpvHandle,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;
    fn mpv_set_property(
        handle: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    fn mpv_get_property(
        handle: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    fn mpv_wait_event(handle: *mut MpvHandle, timeout: c_double) -> *mut MpvEvent;
    fn mpv_wakeup(handle: *mut MpvHandle);
    fn mpv_terminate_destroy(handle: *mut MpvHandle);
    fn mpv_error_string(error: c_int) -> *const c_char;
    fn mpv_render_context_create(
        result: *mut *mut MpvRenderContext,
        handle: *mut MpvHandle,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_set_update_callback(
        context: *mut MpvRenderContext,
        callback: Option<unsafe extern "C" fn(*mut c_void)>,
        callback_context: *mut c_void,
    );
    fn mpv_render_context_update(context: *mut MpvRenderContext) -> u64;
    fn mpv_render_context_render(
        context: *mut MpvRenderContext,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_free(context: *mut MpvRenderContext);
}

pub(crate) struct MpvRenderer {
    enabled: bool,
    players: HashMap<VideoId, Player>,
    removed: Vec<VideoId>,
    generation: u64,
}

impl MpvRenderer {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            players: HashMap::new(),
            removed: Vec::new(),
            generation: 0,
        }
    }

    pub fn reconcile<W: Write>(
        &mut self,
        output: &mut W,
        nodes: &[PositionedNode<'_>],
        terminal: TerminalSize,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;

        for item in nodes {
            let Node::Video(video) = item.node else {
                continue;
            };
            let frame = Frame::new(item.rect, terminal, video.style.z)?;
            match self.players.get_mut(&video.id) {
                Some(player) => {
                    player.seen = generation;
                    player.resize(frame)?;
                    player.set_fit(video.fit)?;
                }
                None => {
                    self.players
                        .insert(video.id, Player::new(video, frame, generation)?);
                }
            }
        }

        self.removed.clear();
        self.removed.extend(
            self.players
                .iter()
                .filter_map(|(&id, player)| (player.seen != generation).then_some(id)),
        );
        for id in self.removed.drain(..) {
            delete_image(output, graphic_id(id))?;
            self.players.remove(&id);
        }
        self.draw_ready(output)
    }

    pub fn draw_ready<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut wrote = false;
        for (&id, player) in &mut self.players {
            if player.render()? {
                transmit(output, graphic_id(id), player)?;
                wrote = true;
            }
        }
        if wrote {
            output.flush()?;
        }
        Ok(())
    }

    pub fn clear<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        for &id in self.players.keys() {
            delete_image(output, graphic_id(id))?;
        }
        self.players.clear();
        output.flush()
    }

    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    pub fn redraw(&mut self) {
        for player in self.players.values_mut() {
            player.force_render = true;
            player.ready.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Frame {
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    width: usize,
    height: usize,
    stride: usize,
    z: i16,
}

impl Frame {
    fn new(rect: crate::layout::Rect, terminal: TerminalSize, z: i16) -> io::Result<Self> {
        let cell_width = if terminal.pixel_width > 0 && terminal.cols > 0 {
            usize::from(terminal.pixel_width) / usize::from(terminal.cols)
        } else {
            8
        };
        let cell_height = if terminal.pixel_height > 0 && terminal.rows > 0 {
            usize::from(terminal.pixel_height) / usize::from(terminal.rows)
        } else {
            16
        };
        let width = usize::from(rect.w)
            .checked_mul(cell_width.max(1))
            .ok_or_else(frame_too_large)?;
        let height = usize::from(rect.h)
            .checked_mul(cell_height.max(1))
            .ok_or_else(frame_too_large)?;
        let row_bytes = width.checked_mul(4).ok_or_else(frame_too_large)?;
        let stride = row_bytes.checked_add(63).ok_or_else(frame_too_large)? & !63;
        let buffer_size = stride.checked_mul(height).ok_or_else(frame_too_large)?;
        if width > c_int::MAX as usize || height > c_int::MAX as usize {
            return Err(frame_too_large());
        }
        Layout::from_size_align(buffer_size, 64).map_err(|_| frame_too_large())?;
        Ok(Self {
            x: rect.x,
            y: rect.y,
            cols: rect.w,
            rows: rect.h,
            width,
            height,
            stride,
            z,
        })
    }
}

fn frame_too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "video frame is too large")
}

struct Player {
    handle: NonNull<MpvHandle>,
    render: NonNull<MpvRenderContext>,
    ready: Box<AtomicBool>,
    pixels: AlignedBuffer,
    encoded: Vec<u8>,
    frame: Frame,
    fit: ImageFit,
    force_render: bool,
    requested_fit: Arc<AtomicU8>,
    event_error: Arc<AtomicI32>,
    event_shutdown: Arc<AtomicBool>,
    event_thread: Option<JoinHandle<()>>,
    controls: VideoControls,
    seen: u64,
}

impl Player {
    fn new(video: &Video, frame: Frame, seen: u64) -> io::Result<Self> {
        let path = CString::new(video.path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "video path contains a null byte",
            )
        })?;
        let encoded_capacity = encoded_capacity(frame)?;
        let handle = NonNull::new(unsafe { mpv_create() })
            .ok_or_else(|| io::Error::other("mpv_create failed"))?;
        if let Err(error) = mpv_result(unsafe {
            mpv_set_option_string(handle.as_ptr(), c"vo".as_ptr(), c"libmpv".as_ptr())
        }) {
            unsafe { mpv_terminate_destroy(handle.as_ptr()) };
            return Err(error);
        }
        if let Err(error) = mpv_result(unsafe {
            mpv_set_option_string(handle.as_ptr(), c"profile".as_ptr(), c"sw-fast".as_ptr())
        }) {
            unsafe { mpv_terminate_destroy(handle.as_ptr()) };
            return Err(error);
        }
        if let Err(error) = mpv_result(unsafe { mpv_initialize(handle.as_ptr()) }) {
            unsafe { mpv_terminate_destroy(handle.as_ptr()) };
            return Err(error);
        }
        if let Err(error) = set_fit(handle, video.fit) {
            unsafe { mpv_terminate_destroy(handle.as_ptr()) };
            return Err(error);
        }

        let api = c"sw";
        let mut params = [
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_API_TYPE,
                data: api.as_ptr().cast_mut().cast(),
            },
            MpvRenderParam {
                kind: 0,
                data: ptr::null_mut(),
            },
        ];
        let mut render = ptr::null_mut();
        if let Err(error) = mpv_result(unsafe {
            mpv_render_context_create(&mut render, handle.as_ptr(), params.as_mut_ptr())
        }) {
            unsafe { mpv_terminate_destroy(handle.as_ptr()) };
            return Err(error);
        }
        let render = NonNull::new(render)
            .ok_or_else(|| io::Error::other("mpv returned an empty render context"))?;
        let ready = Box::new(AtomicBool::new(true));
        unsafe {
            mpv_render_context_set_update_callback(
                render.as_ptr(),
                Some(mark_ready),
                (&*ready as *const AtomicBool).cast_mut().cast(),
            )
        };
        let args = [c"loadfile".as_ptr(), path.as_ptr(), ptr::null()];
        if let Err(error) = mpv_result(unsafe { mpv_command(handle.as_ptr(), args.as_ptr()) }) {
            unsafe {
                mpv_render_context_set_update_callback(render.as_ptr(), None, ptr::null_mut());
                mpv_render_context_free(render.as_ptr());
                mpv_terminate_destroy(handle.as_ptr());
            }
            return Err(error);
        }

        let requested_fit = Arc::new(AtomicU8::new(fit_value(video.fit)));
        let thread_fit = Arc::clone(&requested_fit);
        let event_error = Arc::new(AtomicI32::new(0));
        let thread_error = Arc::clone(&event_error);
        let event_shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&event_shutdown);
        let thread_handle = SendHandle(handle);
        let thread_control = Arc::clone(&video.control.inner);
        let event_thread = match thread::Builder::new()
            .name(format!("fxcode-mpv-{}", video.id.get()))
            .spawn(move || {
                let mut applied_fit = thread_fit.load(Ordering::Acquire);
                let mut paused = false;
                let mut volume = 100;
                let mut last_status = Instant::now() - Duration::from_secs(1);
                while !thread_shutdown.load(Ordering::Acquire) {
                    let requested = thread_fit.load(Ordering::Acquire);
                    if requested != applied_fit {
                        match thread_handle.set_fit(fit_from_value(requested)) {
                            Ok(()) => applied_fit = requested,
                            Err(error) => thread_error.store(error, Ordering::Release),
                        }
                    }
                    let requested_pause = thread_control.paused.load(Ordering::Acquire);
                    if requested_pause != paused {
                        match thread_handle.set_paused(requested_pause) {
                            Ok(()) => paused = requested_pause,
                            Err(error) => thread_error.store(error, Ordering::Release),
                        }
                    }
                    let requested_volume = thread_control.volume.load(Ordering::Acquire);
                    if requested_volume != volume {
                        match thread_handle.set_volume(requested_volume) {
                            Ok(()) => volume = requested_volume,
                            Err(error) => thread_error.store(error, Ordering::Release),
                        }
                    }
                    let seek_steps = thread_control.seek_steps.swap(0, Ordering::AcqRel);
                    if seek_steps != 0
                        && let Err(error) = thread_handle.seek(seek_steps)
                    {
                        thread_error.store(error, Ordering::Release);
                    }
                    let seek_target = f64::from_bits(
                        thread_control
                            .seek_target
                            .swap((-1.0_f64).to_bits(), Ordering::AcqRel),
                    );
                    if seek_target >= 0.0
                        && let Err(error) = thread_handle.set_position(seek_target)
                    {
                        thread_error.store(error, Ordering::Release);
                    }
                    if last_status.elapsed() >= Duration::from_millis(100) {
                        if let Ok(position) = thread_handle.get_double(c"time-pos") {
                            thread_control
                                .position
                                .store(position.max(0.0).to_bits(), Ordering::Release);
                        }
                        if let Ok(duration) = thread_handle.get_double(c"duration") {
                            thread_control
                                .duration
                                .store(duration.max(0.0).to_bits(), Ordering::Release);
                        }
                        last_status = Instant::now();
                    }
                    thread_handle.drain_events(Duration::from_millis(10), &thread_error);
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                unsafe {
                    mpv_render_context_set_update_callback(render.as_ptr(), None, ptr::null_mut());
                    mpv_render_context_free(render.as_ptr());
                    mpv_terminate_destroy(handle.as_ptr());
                }
                return Err(error);
            }
        };

        let pixels = AlignedBuffer::new(frame.stride * frame.height);
        Ok(Self {
            handle,
            render,
            ready,
            pixels,
            encoded: Vec::with_capacity(encoded_capacity),
            frame,
            fit: video.fit,
            force_render: false,
            requested_fit,
            event_error,
            event_shutdown,
            event_thread: Some(event_thread),
            controls: video.controls(),
            seen,
        })
    }

    fn resize(&mut self, frame: Frame) -> io::Result<()> {
        if self.frame == frame {
            return Ok(());
        }
        let size = frame
            .stride
            .checked_mul(frame.height)
            .ok_or_else(frame_too_large)?;
        if size != self.pixels.len() {
            self.pixels = AlignedBuffer::new(size);
        }
        let capacity = encoded_capacity(frame)?;
        if self.encoded.capacity() < capacity {
            self.encoded.reserve(capacity - self.encoded.len());
        }
        self.frame = frame;
        self.force_render = true;
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    fn render(&mut self) -> io::Result<bool> {
        let error = self.event_error.load(Ordering::Acquire);
        if error < 0 {
            return mpv_result(error).map(|()| false);
        }
        if !self.ready.swap(false, Ordering::AcqRel) {
            return Ok(false);
        }
        let flags = unsafe { mpv_render_context_update(self.render.as_ptr()) };
        if (flags & MPV_RENDER_UPDATE_FRAME == 0 && !self.force_render)
            || self.frame.width == 0
            || self.frame.height == 0
        {
            return Ok(false);
        }
        self.force_render = false;

        let mut size = [self.frame.width as c_int, self.frame.height as c_int];
        let format = c"rgb0";
        let mut stride = self.frame.stride;
        let mut params = [
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr().cast(),
            },
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_SW_FORMAT,
                data: format.as_ptr().cast_mut().cast(),
            },
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_SW_STRIDE,
                data: (&mut stride as *mut usize).cast(),
            },
            MpvRenderParam {
                kind: MPV_RENDER_PARAM_SW_POINTER,
                data: self.pixels.as_mut_ptr().cast(),
            },
            MpvRenderParam {
                kind: 0,
                data: ptr::null_mut(),
            },
        ];
        mpv_result(unsafe {
            mpv_render_context_render(self.render.as_ptr(), params.as_mut_ptr())
        })?;
        if self.controls.is_paused() {
            draw_play_icon(self.pixels.as_mut_slice(), self.frame);
        }
        self.encode();
        Ok(true)
    }

    fn set_fit(&mut self, fit: ImageFit) -> io::Result<()> {
        if self.fit != fit {
            self.requested_fit.store(fit_value(fit), Ordering::Release);
            unsafe { mpv_wakeup(self.handle.as_ptr()) };
            self.fit = fit;
            self.force_render = true;
            self.ready.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn encode(&mut self) {
        self.encoded.clear();
        base64_rgb0_rows(&mut self.encoded, self.pixels.as_mut_slice(), self.frame);
    }
}

fn encoded_capacity(frame: Frame) -> io::Result<usize> {
    frame
        .width
        .checked_mul(frame.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(frame_too_large)
}

impl Drop for Player {
    fn drop(&mut self) {
        self.event_shutdown.store(true, Ordering::Release);
        unsafe { mpv_wakeup(self.handle.as_ptr()) };
        if let Some(thread) = self.event_thread.take() {
            let _ = thread.join();
        }
        unsafe {
            mpv_render_context_set_update_callback(self.render.as_ptr(), None, ptr::null_mut());
            mpv_render_context_free(self.render.as_ptr());
            mpv_terminate_destroy(self.handle.as_ptr());
        }
    }
}

unsafe extern "C" fn mark_ready(context: *mut c_void) {
    let ready = unsafe { &*context.cast::<AtomicBool>() };
    ready.store(true, Ordering::Release);
}

#[derive(Clone, Copy)]
struct SendHandle(NonNull<MpvHandle>);

// Libmpv allows different client API threads to use one initialized handle.
unsafe impl Send for SendHandle {}

impl SendHandle {
    fn set_fit(self, fit: ImageFit) -> Result<(), c_int> {
        set_fit_code(self.0, fit)
    }

    fn set_paused(self, paused: bool) -> Result<(), c_int> {
        let mut paused = c_int::from(paused);
        mpv_code(unsafe {
            mpv_set_property(
                self.0.as_ptr(),
                c"pause".as_ptr(),
                MPV_FORMAT_FLAG,
                (&mut paused as *mut c_int).cast(),
            )
        })
    }

    fn set_volume(self, volume: u8) -> Result<(), c_int> {
        let mut volume = f64::from(volume);
        mpv_code(unsafe {
            mpv_set_property(
                self.0.as_ptr(),
                c"volume".as_ptr(),
                MPV_FORMAT_DOUBLE,
                (&mut volume as *mut f64).cast(),
            )
        })
    }

    fn seek(self, steps: i32) -> Result<(), c_int> {
        let amount = if steps < 0 { c"-5" } else { c"5" };
        let args = [
            c"seek".as_ptr(),
            amount.as_ptr(),
            c"relative".as_ptr(),
            ptr::null(),
        ];
        for _ in 0..steps.unsigned_abs() {
            mpv_code(unsafe { mpv_command(self.0.as_ptr(), args.as_ptr()) })?;
        }
        Ok(())
    }

    fn get_double(self, name: &CStr) -> Result<f64, c_int> {
        let mut value = 0.0;
        mpv_code(unsafe {
            mpv_get_property(
                self.0.as_ptr(),
                name.as_ptr(),
                MPV_FORMAT_DOUBLE,
                (&mut value as *mut f64).cast(),
            )
        })?;
        Ok(value)
    }

    fn set_position(self, position: f64) -> Result<(), c_int> {
        let mut position = position;
        mpv_code(unsafe {
            mpv_set_property(
                self.0.as_ptr(),
                c"time-pos".as_ptr(),
                MPV_FORMAT_DOUBLE,
                (&mut position as *mut f64).cast(),
            )
        })
    }

    fn drain_events(self, timeout: Duration, error: &AtomicI32) {
        let mut timeout = timeout.as_secs_f64();
        loop {
            let event = unsafe { mpv_wait_event(self.0.as_ptr(), timeout) };
            if event.is_null() || unsafe { (*event).event_id } == 0 {
                break;
            }
            let event = unsafe { &*event };
            if event.error < 0 {
                error.store(event.error, Ordering::Release);
            }
            if event.event_id == 7 && !event.data.is_null() {
                let end = unsafe { &*event.data.cast::<MpvEventEndFile>() };
                if end.reason == 4 && end.error < 0 {
                    error.store(end.error, Ordering::Release);
                }
            }
            timeout = 0.0;
        }
    }
}

fn mpv_code(code: c_int) -> Result<(), c_int> {
    if code < 0 { Err(code) } else { Ok(()) }
}

fn set_fit(handle: NonNull<MpvHandle>, fit: ImageFit) -> io::Result<()> {
    set_fit_code(handle, fit)
        .map_err(|code| mpv_result(code).expect_err("a negative mpv result must produce an error"))
}

fn set_fit_code(handle: NonNull<MpvHandle>, fit: ImageFit) -> Result<(), c_int> {
    let (unscaled, keep_aspect, panscan) = match fit {
        ImageFit::Fill => (c"no", c"no", c"0"),
        ImageFit::Contain => (c"no", c"yes", c"0"),
        ImageFit::Cover => (c"no", c"yes", c"1"),
        ImageFit::Original => (c"yes", c"yes", c"0"),
    };
    for (name, value) in [
        (c"video-unscaled", unscaled),
        (c"keepaspect", keep_aspect),
        (c"panscan", panscan),
    ] {
        let result =
            unsafe { mpv_set_property_string(handle.as_ptr(), name.as_ptr(), value.as_ptr()) };
        if result < 0 {
            return Err(result);
        }
    }
    Ok(())
}

fn fit_value(fit: ImageFit) -> u8 {
    match fit {
        ImageFit::Fill => 0,
        ImageFit::Contain => 1,
        ImageFit::Cover => 2,
        ImageFit::Original => 3,
    }
}

fn fit_from_value(value: u8) -> ImageFit {
    match value {
        0 => ImageFit::Fill,
        2 => ImageFit::Cover,
        3 => ImageFit::Original,
        _ => ImageFit::Contain,
    }
}

fn mpv_result(code: c_int) -> io::Result<()> {
    if code >= 0 {
        return Ok(());
    }
    let message = unsafe { CStr::from_ptr(mpv_error_string(code)) }.to_string_lossy();
    Err(io::Error::other(format!("mpv: {message}")))
}

fn graphic_id(id: VideoId) -> u32 {
    id.get() | (1 << 31)
}

fn transmit<W: Write>(output: &mut W, id: u32, player: &Player) -> io::Result<()> {
    let frame = player.frame;
    for (index, chunk) in player.encoded.chunks(4096).enumerate() {
        let more = u8::from((index + 1) * 4096 < player.encoded.len());
        if index == 0 {
            write!(
                output,
                "\x1b_Ga=t,f=24,t=d,i={id},s={},v={},m={more},q=2;",
                frame.width, frame.height
            )?;
        } else {
            write!(output, "\x1b_Gm={more};")?;
        }
        output.write_all(chunk)?;
        output.write_all(b"\x1b\\")?;
    }
    write!(
        output,
        "\x1b7\x1b[{};{}H\x1b_Ga=p,i={id},p=1,c={},r={},z={},q=2;\x1b\\\x1b8",
        frame.y + 1,
        frame.x + 1,
        frame.cols,
        frame.rows,
        frame.z
    )
}

fn delete_image<W: Write>(output: &mut W, id: u32) -> io::Result<()> {
    write!(output, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")
}

fn base64_rgb0_rows(output: &mut Vec<u8>, pixels: &[u8], frame: Frame) {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for row in 0..frame.height {
        let start = row * frame.stride;
        let end = start + frame.width * 4;
        let (row, _) = pixels[start..end].as_chunks::<4>();
        for pixel in row {
            let value =
                (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2]);
            output.push(TABLE[((value >> 18) & 63) as usize]);
            output.push(TABLE[((value >> 12) & 63) as usize]);
            output.push(TABLE[((value >> 6) & 63) as usize]);
            output.push(TABLE[(value & 63) as usize]);
        }
    }
}

fn draw_play_icon(pixels: &mut [u8], frame: Frame) {
    if frame.width < 8 || frame.height < 8 {
        return;
    }
    let size = (frame.width.min(frame.height) / 8).clamp(4, 64) as isize;
    let center_x = frame.width as isize / 2;
    let center_y = frame.height as isize / 2;
    for y in -size..=size {
        let right = size - 2 * y.abs();
        for x in -size..=right {
            let pixel_x = (center_x + x) as usize;
            let pixel_y = (center_y + y) as usize;
            let offset = pixel_y * frame.stride + pixel_x * 4;
            pixels[offset..offset + 3].fill(u8::MAX);
        }
    }
}

struct AlignedBuffer {
    pointer: NonNull<u8>,
    length: usize,
}

impl AlignedBuffer {
    fn new(length: usize) -> Self {
        if length == 0 {
            return Self {
                pointer: NonNull::dangling(),
                length,
            };
        }
        let layout = Layout::from_size_align(length, 64).expect("valid video buffer layout");
        let pointer =
            NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout));
        Self { pointer, length }
    }

    fn len(&self) -> usize {
        self.length
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.pointer.as_ptr()
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.length) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        if self.length != 0 {
            let layout =
                Layout::from_size_align(self.length, 64).expect("valid video buffer layout");
            unsafe { dealloc(self.pointer.as_ptr(), layout) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Rect;

    #[test]
    fn frame_stride_and_pointer_are_aligned() {
        let frame = Frame::new(
            Rect {
                x: 0,
                y: 0,
                w: 7,
                h: 3,
            },
            TerminalSize::default(),
            0,
        )
        .unwrap();
        let buffer = AlignedBuffer::new(frame.stride * frame.height);
        assert_eq!(frame.stride % 64, 0);
        assert_eq!(buffer.pointer.as_ptr() as usize % 64, 0);
    }

    #[test]
    fn base64_rgb0_skips_alpha_and_stride_padding() {
        let frame = Frame {
            x: 0,
            y: 0,
            cols: 1,
            rows: 2,
            width: 1,
            height: 2,
            stride: 64,
            z: 0,
        };
        let mut pixels = [0_u8; 128];
        pixels[..4].copy_from_slice(&[b'a', b'b', b'c', 42]);
        pixels[64..68].copy_from_slice(&[b'd', b'e', b'f', 42]);
        let mut output = Vec::with_capacity(8);
        base64_rgb0_rows(&mut output, &pixels, frame);
        assert_eq!(output, b"YWJjZGVm");
    }

    #[test]
    fn paused_play_icon_is_drawn_in_the_frame_center() {
        let frame = Frame {
            x: 0,
            y: 0,
            cols: 2,
            rows: 1,
            width: 16,
            height: 16,
            stride: 64,
            z: 0,
        };
        let mut pixels = vec![0_u8; frame.stride * frame.height];
        draw_play_icon(&mut pixels, frame);
        let center = 8 * frame.stride + 8 * 4;
        assert_eq!(&pixels[center..center + 3], &[255, 255, 255]);
        assert_eq!(&pixels[..3], &[0, 0, 0]);
    }

    #[test]
    #[ignore = "requires FXCODE_TEST_VIDEO"]
    fn libmpv_renders_a_frame() {
        let path = std::env::var_os("FXCODE_TEST_VIDEO").expect("set FXCODE_TEST_VIDEO");
        let video = Video::from_path(path).unwrap();
        let frame = Frame::new(
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 4,
            },
            TerminalSize::default(),
            0,
        )
        .unwrap();
        let mut player = Player::new(&video, frame, 1).unwrap();
        for _ in 0..200 {
            if player.render().unwrap() {
                assert!(!player.encoded.is_empty());
                assert_eq!(player.encoded.len(), frame.width * frame.height * 4);
                for _ in 0..100 {
                    if video.controls().duration() > 0.0 {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                panic!("libmpv did not report the duration");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("libmpv did not produce a frame");
    }

    #[test]
    #[ignore = "requires FXCODE_TEST_BAD_VIDEO"]
    fn libmpv_reports_bad_media() {
        let path = std::env::var_os("FXCODE_TEST_BAD_VIDEO").expect("set FXCODE_TEST_BAD_VIDEO");
        let video = Video::from_path(path).unwrap();
        let frame = Frame::new(
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 4,
            },
            TerminalSize::default(),
            0,
        )
        .unwrap();
        let mut player = Player::new(&video, frame, 1).unwrap();
        for _ in 0..200 {
            if player.render().is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("libmpv did not report the bad media");
    }
}
