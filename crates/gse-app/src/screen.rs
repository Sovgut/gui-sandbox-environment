//! The environment screen inside the app: the newest frame of the
//! environment's screen memory, scaled into the view with its aspect ratio
//! kept, and the view's keyboard and mouse sent to the environment.
//!
//! While the view has focus every key goes to the environment by its
//! physical position (raw evdev codes), so the host's keyboard layout does
//! not matter and no key reaches the app's own shortcuts. Ctrl+Alt+G
//! captures the mouse: the host pointer is locked and its motion goes to the
//! environment's relative mouse, which games that lock the pointer need.
//! Ctrl+Alt+G again, or the view losing focus, releases it and every key
//! still held.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use gpui::{
    App, Bounds, Context, Corners, FocusHandle, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMotionEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    RawKeyEvent, Render, RenderImage, ScrollDelta, ScrollEvent, Styled, Window, canvas, div, point,
    prelude::FluentBuilder as _, px, size,
};
use gse_protocol::ScreenAccess;
use gse_screen::Reader;
use smallvec::SmallVec;

use crate::input::Input;
use crate::ui::Theme;

/// Evdev codes the capture combination uses.
const KEY_LEFT_CTRL: u32 = 29;
const KEY_RIGHT_CTRL: u32 = 97;
const KEY_LEFT_ALT: u32 = 56;
const KEY_RIGHT_ALT: u32 = 100;
const KEY_G: u32 = 34;
const KEY_F: u32 = 33;

pub struct Screen {
    reader: Reader,
    input: Input,
    focus: FocusHandle,
    image: Option<Arc<RenderImage>>,
    /// The last frame's pixels, reused as the buffer for the next one.
    spare: Vec<u32>,
    last: u64,
    /// Where the frame was painted, for mapping the pointer onto it.
    painted: Rc<Cell<Option<Bounds<Pixels>>>>,
    captured: bool,
    /// Keys the environment has seen go down and not up.
    held: BTreeSet<u32>,
    /// Sub-pixel relative motion not sent yet.
    residue: (f64, f64),
    raw_keys: bool,
    /// The last input failure, shown over the screen.
    error: Option<String>,
}

/// An environment's screen memory and input socket, opened.
pub struct Connection {
    reader: Reader,
    input: Input,
}

impl Connection {
    pub fn open(access: &ScreenAccess) -> Result<Self> {
        Ok(Self {
            reader: Reader::new(&File::open(&access.memory)?)?,
            input: Input::connect(Path::new(&access.input))?,
        })
    }
}

impl Screen {
    pub fn new(connection: Connection, cx: &mut Context<Self>) -> Self {
        let Connection { reader, input } = connection;
        Self {
            reader,
            input,
            focus: cx.focus_handle(),
            image: None,
            spare: Vec::new(),
            last: 0,
            painted: Rc::new(Cell::new(None)),
            captured: false,
            held: BTreeSet::new(),
            residue: (0.0, 0.0),
            raw_keys: false,
            error: None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn captured(&self) -> bool {
        self.captured
    }

    fn sent(&mut self, outcome: Result<()>) {
        if let Err(e) = outcome {
            let text = format!("{e:#}");
            // Input goes out many times a second; one line per new error.
            if self.error.as_deref() != Some(&text) {
                tracing::warn!("screen input: {text}");
            }
            self.error = Some(text);
        }
    }

    /// Takes the newest frame, if there is one, as an image the window can
    /// draw, and drops the one it replaces from the window's atlas.
    fn take_frame(&mut self, window: &mut Window) {
        let Some(frame) = self.reader.read(self.last, std::mem::take(&mut self.spare)) else {
            return;
        };
        self.last = frame.number;
        // The window draws BGRA; the frame is 0x00RRGGBB, which is BGRA in
        // memory once the alpha byte is set.
        let mut bytes = Vec::with_capacity(frame.pixels.len() * 4);
        for pixel in &frame.pixels {
            bytes.extend_from_slice(&(pixel | 0xff00_0000).to_le_bytes());
        }
        self.spare = frame.pixels;
        let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, bytes) else {
            return;
        };
        let image = Arc::new(RenderImage::new(SmallVec::from_elem(
            image::Frame::new(buffer),
            1,
        )));
        if let Some(old) = self.image.replace(image) {
            window.drop_image(old).ok();
        }
    }

    /// Where the window's `position` falls on the frame, as fractions of it.
    fn on_frame(&self, position: gpui::Point<Pixels>) -> Option<(f64, f64)> {
        let painted = self.painted.get()?;
        let x = f32::from(position.x - painted.origin.x) / f32::from(painted.size.width);
        let y = f32::from(position.y - painted.origin.y) / f32::from(painted.size.height);
        Some((f64::from(x), f64::from(y)))
    }

    fn set_captured(&mut self, captured: bool, window: &mut Window) {
        tracing::info!("mouse {}", if captured { "captured" } else { "freed" });
        self.captured = captured;
        self.residue = (0.0, 0.0);
        window.set_pointer_lock(captured);
    }

    /// Lets go of everything the environment still thinks is held.
    fn release_all(&mut self, window: &mut Window) {
        for code in std::mem::take(&mut self.held) {
            let outcome = self.input.key(code, false);
            self.sent(outcome);
        }
        if self.captured {
            self.set_captured(false, window);
        }
    }

    fn raw_key(&mut self, event: RawKeyEvent, window: &mut Window) -> bool {
        let ctrl = self.held.contains(&KEY_LEFT_CTRL) || self.held.contains(&KEY_RIGHT_CTRL);
        let alt = self.held.contains(&KEY_LEFT_ALT) || self.held.contains(&KEY_RIGHT_ALT);
        if event.code == KEY_G && ctrl && alt {
            if event.pressed {
                let capture = !self.captured;
                self.set_captured(capture, window);
            }
            return true;
        }
        // Full screen hides the bar, so this is the way out of it.
        if event.code == KEY_F && ctrl && alt {
            if event.pressed {
                window.toggle_fullscreen();
            }
            return true;
        }
        if event.pressed {
            self.held.insert(event.code);
        } else {
            self.held.remove(&event.code);
        }
        let outcome = self.input.key(event.code, event.pressed);
        self.sent(outcome);
        true
    }

    fn motion(&mut self, event: &MouseMotionEvent) {
        if !self.captured {
            return;
        }
        let x = self.residue.0 + event.unaccelerated.x;
        let y = self.residue.1 + event.unaccelerated.y;
        self.residue = (x.fract(), y.fract());
        #[allow(clippy::cast_possible_truncation)]
        let (dx, dy) = (x.trunc() as i32, y.trunc() as i32);
        if (dx, dy) != (0, 0) {
            let outcome = self.input.motion(dx, dy);
            self.sent(outcome);
        }
    }

    fn button(&mut self, button: MouseButton, down: bool) {
        let name = match button {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
            MouseButton::Navigate(gpui::NavigationDirection::Back) => "side",
            MouseButton::Navigate(gpui::NavigationDirection::Forward) => "extra",
        };
        let outcome = self.input.button(name, down);
        self.sent(outcome);
    }

    /// A wheel turn, sent as one click of the wheel up or down.
    fn scrolled(&mut self, event: &ScrollEvent) {
        let dy = match event.delta {
            ScrollDelta::Lines(lines) => lines.y,
            ScrollDelta::Pixels(pixels) => f32::from(pixels.y),
        };
        if dy != 0.0 {
            let name = if dy > 0.0 { "wheel-up" } else { "wheel-down" };
            let outcome = self
                .input
                .button(name, true)
                .and_then(|()| self.input.button(name, false));
            self.sent(outcome);
        }
    }

    fn released(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.button(event.button, false);
    }

    /// Hands every key to the environment while the view has focus, and
    /// gives them back to the app when it loses it.
    fn route_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focus.is_focused(window);
        if focused && !self.raw_keys {
            let screen = cx.weak_entity();
            window.set_raw_key_handler(Some(
                move |event: &RawKeyEvent, window: &mut Window, cx: &mut gpui::App| {
                    screen
                        .update(cx, |screen, _| screen.raw_key(*event, window))
                        .unwrap_or(false)
                },
            ));
            self.raw_keys = true;
        } else if !focused && self.raw_keys {
            window
                .set_raw_key_handler(None::<fn(&RawKeyEvent, &mut Window, &mut gpui::App) -> bool>);
            self.raw_keys = false;
            self.release_all(window);
        }
    }
}

impl Render for Screen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.route_keys(window, cx);
        self.take_frame(window);
        // The screen memory is polled once per window frame.
        window.request_animation_frame();

        let image = self.image.clone();
        let painted = Rc::clone(&self.painted);
        let screen = cx.weak_entity();
        let view = canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let Some(image) = image else {
                    painted.set(None);
                    return;
                };
                let frame = image.size(0);
                // Frame sides stay far below 2^24, exact in f32.
                #[allow(clippy::cast_precision_loss)]
                let (fw, fh) = (frame.width.0 as f32, frame.height.0 as f32);
                let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let scale = (bw / fw).min(bh / fh);
                let (w, h) = (fw * scale, fh * scale);
                let area = Bounds::new(
                    point(
                        bounds.origin.x + px((bw - w) / 2.0),
                        bounds.origin.y + px((bh - h) / 2.0),
                    ),
                    size(px(w), px(h)),
                );
                painted.set(Some(area));
                window
                    .paint_image(area, area, Corners::default(), image, 0, false)
                    .ok();
                let screen = screen.clone();
                window.on_mouse_event(move |event: &MouseMotionEvent, phase, _, cx| {
                    if phase.bubble() {
                        screen.update(cx, |screen, _| screen.motion(event)).ok();
                    }
                });
            },
        )
        .size_full();

        div()
            .id("environment-screen")
            .track_focus(&self.focus)
            .size_full()
            .bg(gpui::black())
            // The guest draws its own pointer into the frame, in the shape
            // its programs ask for; the host's would be a second one.
            .when(self.image.is_some(), |view| {
                view.cursor(gpui::CursorStyle::Hidden)
            })
            .child(view)
            .on_mouse_move(cx.listener(|screen, event: &MouseMoveEvent, _, _| {
                if screen.captured {
                    return;
                }
                if let Some((x, y)) = screen.on_frame(event.position) {
                    let outcome = screen.input.pointer(x, y);
                    screen.sent(outcome);
                }
            }))
            .on_any_mouse_down(cx.listener(|screen, event: &MouseDownEvent, window, cx| {
                screen.focus(window, cx);
                screen.button(event.button, true);
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::released))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::released))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::released))
            .on_scroll(cx.listener(|screen, event: &ScrollEvent, _, _| screen.scrolled(event)))
            .when(self.image.is_none(), |view| {
                view.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(Theme::muted())
                        .child("Waiting for the screen..."),
                )
            })
            .children(self.error.clone().map(|e| {
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(Theme::surface())
                    .text_color(Theme::danger())
                    .text_sm()
                    .child(e)
            }))
    }
}
