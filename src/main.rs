use termion::terminal_size;

#[derive(Debug, Clone, Copy, Default)]
struct App {
    width: u16,
    height: u16,
}

enum Properties {}
struct Component {
    inner: Vec<Component>,
    properties: Vec<Properties>,
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
