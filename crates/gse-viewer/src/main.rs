//! Development viewer for the environment screen.
//!
//! Shows the newest frame a running environment published into its screen memory,
//! scaled into a window with its aspect ratio kept, and reports how many
//! frames per second the guest publishes and how many reach the window. The
//! gse app takes over this job; the viewer measures whether the path
//! keeps up.
//!
//! While the window has focus, its keyboard and mouse go to the environment: the
//! pointer as an absolute position on the environment's virtual tablet.
//! Ctrl+Alt+G captures the mouse instead: the host pointer is locked and its
//! motion goes to the environment's virtual mouse as relative movement, which
//! games that lock the pointer need. Ctrl+Alt+G again, or losing focus,
//! releases it. The combination exists on every keyboard, compact ones
//! included, and stays on the host.

mod input;

use std::fs::File;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use clap::Parser;
use gse_screen::{Frame, Reader};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    DeviceEvent, DeviceId, ElementState, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::input::Input;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The environment's screen memory, `/dev/shm/gse-<name>.screen`.
    #[arg(default_value = "/dev/shm/gse-test.screen")]
    memory: PathBuf,
    /// The environment's input socket, `/run/gse/<name>.input`.
    #[arg(long, default_value = "/run/gse/test.input")]
    input: PathBuf,
}

/// Where a frame lands in a window: its scale and top-left corner.
#[derive(Clone, Copy)]
struct Placement {
    scale: f64,
    left: f64,
    top: f64,
}

impl Placement {
    fn new(window: (u32, u32), frame: (u32, u32)) -> Self {
        let (ww, wh) = (f64::from(window.0), f64::from(window.1));
        let (fw, fh) = (f64::from(frame.0), f64::from(frame.1));
        let scale = (ww / fw).min(wh / fh);
        Self {
            scale,
            left: (ww - fw * scale) / 2.0,
            top: (wh - fh * scale) / 2.0,
        }
    }
}

type WindowSurface = Surface<Rc<Window>, Rc<Window>>;

struct Viewer {
    reader: Reader,
    input: Input,
    window: Option<Rc<Window>>,
    surface: Option<WindowSurface>,
    frame: Option<Frame>,
    spare: Vec<u32>,
    captured: bool,
    focused: bool,
    modifiers: ModifiersState,
    /// Sub-pixel remainder of relative motion not sent yet.
    residue: (f64, f64),
    shown: u32,
    first_in_second: u64,
    second: Instant,
    error: Option<anyhow::Error>,
}

impl Viewer {
    fn frame_size(&self) -> (u32, u32) {
        self.frame.as_ref().map_or((1, 1), |f| (f.width, f.height))
    }

    fn set_captured(&mut self, captured: bool) {
        let Some(window) = &self.window else { return };
        self.captured = captured;
        if captured {
            window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
                .ok();
        } else {
            window.set_cursor_grab(CursorGrabMode::None).ok();
        }
        self.residue = (0.0, 0.0);
        self.status();
    }

    fn status(&mut self) {
        let published = self.reader.published();
        let (w, h) = self.frame_size();
        let status = format!(
            "{w}x{h}: guest {} fps, shown {} fps{}",
            published.saturating_sub(self.first_in_second),
            self.shown,
            if self.captured {
                ", mouse captured (Ctrl+Alt+G releases)"
            } else {
                ""
            }
        );
        if let Some(window) = &self.window {
            window.set_title(&format!("gse screen {status}"));
        }
        eprintln!("{status}");
        self.first_in_second = published;
        self.shown = 0;
        self.second = Instant::now();
    }

    /// Scales the current frame into the window, nearest neighbour.
    fn draw(&mut self) -> Result<()> {
        let (Some(window), Some(surface), Some(frame)) =
            (&self.window, self.surface.as_mut(), &self.frame)
        else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return Ok(());
        };
        surface.resize(w, h).map_err(|e| anyhow!("resizing: {e}"))?;
        let mut buffer = surface.buffer_mut().map_err(|e| anyhow!("drawing: {e}"))?;
        let place = Placement::new((size.width, size.height), (frame.width, frame.height));
        let (ww, wh) = (size.width as usize, size.height as usize);
        let (fw, fh) = (frame.width as usize, frame.height as usize);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let source = |at: usize, start: f64, limit: usize| -> Option<usize> {
            let v = (at as f64 + 0.5 - start) / place.scale;
            (v >= 0.0 && (v as usize) < limit).then_some(v as usize)
        };
        let columns: Vec<Option<usize>> = (0..ww).map(|x| source(x, place.left, fw)).collect();
        for (y, row) in buffer.chunks_exact_mut(ww).take(wh).enumerate() {
            if let Some(sy) = source(y, place.top, fh) {
                let line = &frame.pixels[sy * fw..sy * fw + fw];
                for (pixel, column) in row.iter_mut().zip(&columns) {
                    *pixel = column.map_or(0, |sx| line[sx]);
                }
            } else {
                row.fill(0);
            }
        }
        buffer.present().map_err(|e| anyhow!("presenting: {e}"))?;
        self.shown += 1;
        Ok(())
    }

    fn handle(&mut self, event: &WindowEvent) -> Result<()> {
        match event {
            WindowEvent::Focused(focused) => {
                self.focused = *focused;
                if !focused && self.captured {
                    self.set_captured(false);
                }
            }
            WindowEvent::CursorMoved { position, .. } if self.focused && !self.captured => {
                if let Some(window) = &self.window {
                    let size = window.inner_size();
                    let (fw, fh) = self.frame_size();
                    let place = Placement::new((size.width, size.height), (fw, fh));
                    let x = (position.x - place.left) / (f64::from(fw) * place.scale);
                    let y = (position.y - place.top) / (f64::from(fh) * place.scale);
                    self.input.pointer(x, y)?;
                }
            }
            WindowEvent::MouseInput { state, button, .. } if self.focused => {
                let name = match button {
                    MouseButton::Left => "left",
                    MouseButton::Right => "right",
                    MouseButton::Middle => "middle",
                    MouseButton::Back => "side",
                    MouseButton::Forward => "extra",
                    MouseButton::Other(_) => return Ok(()),
                };
                self.input.button(name, *state == ElementState::Pressed)?;
            }
            WindowEvent::MouseWheel { delta, .. } if self.focused => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if dy != 0.0 {
                    let name = if dy > 0.0 { "wheel-up" } else { "wheel-down" };
                    self.input.button(name, true)?;
                    self.input.button(name, false)?;
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } if self.focused => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return Ok(());
                };
                let toggle = code == KeyCode::KeyG
                    && self.modifiers.control_key()
                    && self.modifiers.alt_key();
                if toggle {
                    if event.state == ElementState::Pressed && !event.repeat {
                        self.set_captured(!self.captured);
                    }
                } else if !event.repeat {
                    self.input.key(code, event.state == ElementState::Pressed)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn open(event_loop: &ActiveEventLoop) -> Result<(Rc<Window>, WindowSurface)> {
        let attributes = Window::default_attributes()
            .with_title("gse screen")
            .with_inner_size(LogicalSize::new(1280, 720));
        let window = Rc::new(event_loop.create_window(attributes)?);
        // The environment draws its own pointer into the frames.
        window.set_cursor_visible(false);
        let context = Context::new(Rc::clone(&window)).map_err(|e| anyhow!("display: {e}"))?;
        let surface =
            Surface::new(&context, Rc::clone(&window)).map_err(|e| anyhow!("surface: {e}"))?;
        Ok((window, surface))
    }
}

impl ApplicationHandler for Viewer {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        match Self::open(event_loop) {
            Ok((window, surface)) => {
                self.window = Some(window);
                self.surface = Some(surface);
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let outcome = match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                Ok(())
            }
            WindowEvent::RedrawRequested => self.draw(),
            other => self.handle(other),
        };
        if let Err(e) = outcome {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        let DeviceEvent::MouseMotion { delta } = event else {
            return;
        };
        if !self.captured {
            return;
        }
        let x = self.residue.0 + delta.0;
        let y = self.residue.1 + delta.1;
        self.residue = (x.fract(), y.fract());
        #[allow(clippy::cast_possible_truncation)]
        let (dx, dy) = (x.trunc() as i32, y.trunc() as i32);
        if (dx, dy) != (0, 0)
            && let Err(e) = self.input.motion(dx, dy)
        {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        let last = self.frame.as_ref().map_or(0, |f| f.number);
        if let Some(frame) = self.reader.read(last, std::mem::take(&mut self.spare)) {
            if let Some(old) = self.frame.replace(frame) {
                self.spare = old.pixels;
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        } else {
            // The shared memory is polled; a short nap keeps a core free
            // without costing a frame.
            std::thread::sleep(Duration::from_micros(500));
        }
        if self.second.elapsed() >= Duration::from_secs(1) {
            self.status();
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let file = File::open(&cli.memory).with_context(|| format!("{}", cli.memory.display()))?;
    let reader = Reader::new(&file)?;
    let input = Input::connect(&cli.input)?;
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let first_in_second = reader.published();
    let mut viewer = Viewer {
        reader,
        input,
        window: None,
        surface: None,
        frame: None,
        spare: Vec::new(),
        captured: false,
        focused: false,
        modifiers: ModifiersState::empty(),
        residue: (0.0, 0.0),
        shown: 0,
        first_in_second,
        second: Instant::now(),
        error: None,
    };
    event_loop.run_app(&mut viewer)?;
    viewer.error.map_or(Ok(()), Err)
}
