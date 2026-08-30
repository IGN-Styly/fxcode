use fxcode::{
    AlignItems, App, Border, Color, Container, ControlFlow, Event, ImageFit, JustifyContent,
    Runtime, Style, TerminalSize, Video,
};
use std::io;
use termion::event::Key;

fn main() -> io::Result<()> {
    let runtime = Runtime::new()?;
    let video = Video::from_path("docs/cars.mp4")?;
    let app = App::root(view(video.clone(), runtime.size()));

    runtime.run(app, move |app, event| match event {
        Event::Key(Key::Char('q') | Key::Esc) => ControlFlow::Exit,
        Event::Resize(size) => {
            app.tree = vec![view(video.clone(), size).into()];
            ControlFlow::Render
        }
        Event::Init => ControlFlow::Render,
        Event::Mouse(_) => ControlFlow::Continue,
        Event::Key(_) => ControlFlow::Continue,
    })
}

fn view(video: Video, terminal: TerminalSize) -> Container {
    let (video_width, video_height) = video_size(terminal);
    let controls = video.controls();
    let video = video.fit(ImageFit::Contain).style(Style::new().z(11));

    let modal = Container::new()
        .style(
            Style::new()
                .width(video_width.saturating_add(2))
                .height(video_height.saturating_add(2))
                .border(
                    Border::plain(Color::white())
                        .title("Cars")
                        .media_controls(controls),
                )
                .background(Color::black())
                .z(10),
        )
        .child(video);

    Container::new()
        .style(
            Style::new()
                .border(Border::plain(Color::white()).title("fxcode"))
                .background(Color::black())
                .align_items(AlignItems::Center)
                .justify(JustifyContent::Center),
        )
        .child(modal)
}

fn video_size(terminal: TerminalSize) -> (u16, u16) {
    const VIDEO_WIDTH: f64 = 1920.0;
    const VIDEO_HEIGHT: f64 = 800.0;

    let max_width = terminal.cols.saturating_sub(4).min(64);
    let max_height = terminal.rows.saturating_sub(4);
    if max_width == 0 || max_height == 0 {
        return (0, 0);
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
    let scale = (f64::from(max_width) * cell_width / VIDEO_WIDTH)
        .min(f64::from(max_height) * cell_height / VIDEO_HEIGHT);
    (
        ((VIDEO_WIDTH * scale / cell_width).round() as u16)
            .max(1)
            .min(max_width),
        ((VIDEO_HEIGHT * scale / cell_height).round() as u16)
            .max(1)
            .min(max_height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fxcode::Node;

    #[test]
    fn cars_preview_keeps_its_pixel_aspect_ratio() {
        let terminal = TerminalSize {
            cols: 100,
            rows: 50,
            pixel_width: 800,
            pixel_height: 800,
        };
        let root = view(Video::from_path("docs/cars.mp4").unwrap(), terminal);
        let Node::Container(modal) = &root.items[0] else {
            panic!("preview must be a container");
        };
        let width = f64::from(modal.style.width.unwrap() - 2) * 8.0;
        let height = f64::from(modal.style.height.unwrap() - 2) * 16.0;
        assert!((width / height - 2.4).abs() < 0.1);
    }
}
