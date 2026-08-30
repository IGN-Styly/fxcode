use crate::{App, KittyMode, Node, Renderer, TerminalSize};
use std::{
    io::{self, Write, stdout},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use termion::{
    clear, cursor,
    event::{Event as InputEvent, Key, MouseEvent},
    input::{MouseTerminal, TermRead},
    raw::IntoRawMode,
    screen::IntoAlternateScreen,
};

#[derive(Debug, Clone, Copy)]
pub struct RuntimeConfig {
    pub kitty: KittyMode,
    pub event_wait: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            kitty: KittyMode::Auto,
            event_wait: Duration::from_millis(16),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Init,
    Key(Key),
    Mouse(MouseEvent),
    Resize(TerminalSize),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ControlFlow {
    #[default]
    Continue,
    Render,
    Exit,
}

enum RenderCommand {
    Draw(Vec<Node>),
    Resize(TerminalSize),
    Mouse(MouseEvent),
    Shutdown,
}

pub struct Runtime {
    config: RuntimeConfig,
    render: Sender<RenderCommand>,
    input: mpsc::Receiver<io::Result<InputEvent>>,
    render_errors: mpsc::Receiver<io::Error>,
    resized: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    signal_id: signal_hook::SigId,
    input_thread: Option<JoinHandle<()>>,
    render_thread: Option<JoinHandle<()>>,
    size: TerminalSize,
    kitty_supported: bool,
}

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Self::with_config(RuntimeConfig::default())
    }

    pub fn with_config(config: RuntimeConfig) -> io::Result<Self> {
        let (render_tx, render_rx) = mpsc::channel();
        let (render_error_tx, render_errors) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let kitty_mode = config.kitty;
        let render_thread = thread::spawn(move || {
            let setup = (|| -> io::Result<_> {
                let size = TerminalSize::current()?;
                let mut output =
                    MouseTerminal::from(stdout().into_raw_mode()?.into_alternate_screen()?);
                write!(output, "{}{}", cursor::Hide, clear::All)?;
                output.flush()?;
                let renderer = Renderer::with_kitty(output, size, kitty_mode);
                Ok((renderer, size))
            })();
            let (mut renderer, size) = match setup {
                Ok(value) => value,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let _ = ready_tx.send(Ok((size, renderer.kitty_supported())));
            'render: loop {
                let command = match render_rx.recv_timeout(Duration::from_millis(4)) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => {
                        if let Err(error) = renderer.draw_video_frames() {
                            let _ = render_error_tx.send(error);
                            break;
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let result = match command {
                    RenderCommand::Draw(mut tree) => {
                        while let Ok(command) = render_rx.try_recv() {
                            match command {
                                RenderCommand::Draw(newer) => tree = newer,
                                RenderCommand::Resize(size) => {
                                    if let Err(error) = renderer.resize(size) {
                                        let _ = render_error_tx.send(error);
                                        break 'render;
                                    }
                                }
                                RenderCommand::Mouse(mouse) => {
                                    if let Err(error) = renderer.handle_mouse(mouse) {
                                        let _ = render_error_tx.send(error);
                                        break 'render;
                                    }
                                }
                                RenderCommand::Shutdown => break 'render,
                            }
                        }
                        renderer.draw(&tree)
                    }
                    RenderCommand::Resize(size) => renderer.resize(size),
                    RenderCommand::Mouse(mouse) => renderer.handle_mouse(mouse).map(|_| ()),
                    RenderCommand::Shutdown => break,
                };
                if let Err(error) = result {
                    let _ = render_error_tx.send(error);
                    break;
                }
            }
            let _ = renderer.finish();
        });
        let (size, kitty_supported) = ready_rx.recv().map_err(|_| {
            io::Error::new(io::ErrorKind::BrokenPipe, "renderer stopped during startup")
        })??;

        let resized = Arc::new(AtomicBool::new(false));
        let signal_id =
            signal_hook::flag::register(signal_hook::consts::SIGWINCH, Arc::clone(&resized))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let (input_tx, input) = mpsc::channel();
        let input_shutdown = Arc::clone(&shutdown);
        let input_thread = thread::spawn(move || read_input(input_tx, input_shutdown));

        Ok(Self {
            config,
            render: render_tx,
            input,
            render_errors,
            resized,
            shutdown,
            signal_id,
            input_thread: Some(input_thread),
            render_thread: Some(render_thread),
            size,
            kitty_supported,
        })
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    pub fn kitty_supported(&self) -> bool {
        self.kitty_supported
    }

    pub fn run<F>(mut self, mut app: App, mut update: F) -> io::Result<()>
    where
        F: FnMut(&mut App, Event) -> ControlFlow,
    {
        match update(&mut app, Event::Init) {
            ControlFlow::Exit => return Ok(()),
            ControlFlow::Continue | ControlFlow::Render => self.draw(&app)?,
        }

        loop {
            if let Ok(error) = self.render_errors.try_recv() {
                return Err(error);
            }

            let mut flow = match self.input.recv_timeout(self.config.event_wait) {
                Ok(Ok(InputEvent::Key(key))) => update(&mut app, Event::Key(key)),
                Ok(Ok(InputEvent::Mouse(mouse))) => {
                    self.send(RenderCommand::Mouse(mouse))?;
                    update(&mut app, Event::Mouse(mouse))
                }
                Ok(Ok(InputEvent::Unsupported(_))) => ControlFlow::Continue,
                Ok(Err(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => ControlFlow::Continue,
                Err(RecvTimeoutError::Disconnected) => ControlFlow::Exit,
            };

            if self.resized.swap(false, Ordering::Relaxed) {
                self.size = TerminalSize::current()?;
                self.send(RenderCommand::Resize(self.size))?;
                let resize_flow = update(&mut app, Event::Resize(self.size));
                if resize_flow == ControlFlow::Exit {
                    flow = ControlFlow::Exit
                } else {
                    flow = ControlFlow::Render
                }
            }

            match flow {
                ControlFlow::Continue => {}
                ControlFlow::Render => self.draw(&app)?,
                ControlFlow::Exit => break,
            }
        }
        Ok(())
    }

    fn draw(&self, app: &App) -> io::Result<()> {
        self.send(RenderCommand::Draw(app.tree.clone()))
    }

    fn send(&self, command: RenderCommand) -> io::Result<()> {
        self.render
            .send(command)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "renderer stopped"))
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        signal_hook::low_level::unregister(self.signal_id);
        let _ = self.render.send(RenderCommand::Shutdown);
        if let Some(thread) = self.input_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.render_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop()
    }
}

fn read_input(sender: Sender<io::Result<InputEvent>>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 50) };
        if ready < 0 {
            let _ = sender.send(Err(io::Error::last_os_error()));
            return;
        }
        if ready == 0 || descriptor.revents & libc::POLLIN == 0 {
            continue;
        }
        if let Some(event) = io::stdin().events().next()
            && sender.send(event).is_err()
        {
            return;
        }
    }
}
