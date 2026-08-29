use termion::terminal_size;

#[derive(Debug, Clone, Copy, Default)]
struct App {
    width: u16,
    height: u16,
}

struct Style{
    align_content:AlignContent,
    align_items:AlignItems,
    align_self:
    flex:
    flex_basis:
    flex_direction:
    flex_flow:
    flex_grow:
    flex_line_count:
    flex_shrink:
    flex_wrap:
    justify_content:
}
enum AlignContent{
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Stretch,
}
enum AlignItems{
    Start,
    End,
    Center,
    Stretch,
}
struct Container {
    style:Style,
}

impl App {
    fn init(&mut self) {
        let (x, y) = terminal_size().unwrap();
        self.width = x;
        self.height = y;
    }
}

fn main() {
    let mut app = App::default();
    app.init();
    println!("x:{} y:{}", app.width, app.height);
}
