# fxcode

Terminal UI

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

## Go

Build the native library before using the cgo package:

```sh
cargo build --lib
go run ./go/examples/basic
```

The Go package is `github.com/IGN-Styly/fxcode/go/fxcode`. Call `Close` on runtimes,
images, and videos when they are no longer needed.

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
