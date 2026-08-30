# fxcode

Terminal UI library with flex layout, true-color cells, a managed runtime, and Kitty images.

```rust
use fxcode::{App, Border, Color, Container, ControlFlow, Event, Runtime, Style};
use termion::event::Key;

let view = Container::new().style(
    Style::new()
        .border(Border::plain(Color::white()).title("fxcode"))
        .background(Color::black()),
);

Runtime::new()?.run(App::root(view), |_, event| match event {
    Event::Key(Key::Char('q') | Key::Esc) => ControlFlow::Exit,
    Event::Init | Event::Resize(_) => ControlFlow::Render,
    Event::Key(_) | Event::Mouse(_) => ControlFlow::Continue,
})?;
```

Use `Renderer<W>` directly when the program already has an input loop or needs custom output.

## Images

```rust
use fxcode::{Image, ImageFit, Style};

let image = Image::from_path("logo.png")?
    .fit(ImageFit::Contain)
    .style(Style::new().width(30).height(10));
```

`KittyMode::Auto` enables images in Kitty, WezTerm, and Ghostty. Use `KittyMode::Enabled`
when support is known through another terminal or multiplexer. PNG and RGBA sources are uploaded
once, placed again after resize, and removed during shutdown.

## Video

```rust
use fxcode::{ImageFit, Style, Video};

let video = Video::from_path("demo.mp4")?
    .fit(ImageFit::Contain)
    .style(Style::new().width(60).height(20));

video.toggle_pause();
video.seek_forward(); // Five seconds
video.set_volume(80);

let border = Border::plain(Color::white())
    .title("Video")
    .media_controls(video.controls());
```

Media controls use Unicode icons in the bottom border. Click the play/pause icon to toggle
playback, click or drag the progress bar to seek, or click the centered play icon while paused.
Mouse events are also sent to the app as `Event::Mouse`.

Video uses libmpv to decode into a reused frame buffer. Fxcode sends the frames through the
Kitty graphics protocol, so all terminal writes stay on the render thread. Libmpv must be installed
and the terminal must support Kitty graphics. The player starts when the node first renders and
stops when the node leaves the tree.

`Runtime` polls for new frames. When using `Renderer` directly, call
`Renderer::draw_video_frames` from the program's event loop.
