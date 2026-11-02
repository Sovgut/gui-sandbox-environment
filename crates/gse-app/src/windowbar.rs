//! The window's own title bar: it moves the window, maximizes it on a
//! double click, opens the window menu on a right click, and holds the
//! window's buttons drawn and ordered the way KDE's Breeze has them. In
//! full screen, where KDE shows no title bar at all, it keeps only leaving
//! full screen and close.

use std::sync::OnceLock;

use gpui::{
    AnyElement, Context, Div, MouseButton, SharedString, Stateful, Window, div, prelude::*, px,
};

use crate::Home;
use crate::icons::Icon;
use crate::shell::BAR_HEIGHT;
use crate::ui::{self, Theme};

/// A button of the window, as KWin's button layout letters name them, and
/// the one that leaves full screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Minimize,
    Maximize,
    LeaveFullScreen,
    Close,
}

/// The buttons at the right of the bar, in KWin's order: `ButtonsOnRight`
/// of the Breeze decoration in `~/.config/kwinrc`, minimize, maximize and
/// close by default.
fn controls() -> &'static [Control] {
    static CONTROLS: OnceLock<Vec<Control>> = OnceLock::new();
    CONTROLS.get_or_init(|| {
        if crate::preview::active() {
            return vec![Control::Minimize, Control::Maximize, Control::Close];
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })
            .map(|dir| dir.join("kwinrc"));
        let letters = config
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| {
                let mut in_section = false;
                text.lines().find_map(|line| {
                    let line = line.trim();
                    if line.starts_with('[') {
                        in_section = line == "[org.kde.kdecoration2]";
                        return None;
                    }
                    line.strip_prefix("ButtonsOnRight=")
                        .filter(|_| in_section)
                        .map(str::to_owned)
                })
            })
            .unwrap_or_else(|| "IAX".to_owned());
        parse(&letters)
    })
}

/// The buttons `letters` name, by KWin's letters: I minimize, A maximize,
/// X close; the rest are not offered.
fn parse(letters: &str) -> Vec<Control> {
    letters
        .chars()
        .filter_map(|c| match c {
            'I' => Some(Control::Minimize),
            'A' => Some(Control::Maximize),
            'X' => Some(Control::Close),
            _ => None,
        })
        .collect()
}

/// The buttons of a window in full screen, out of its usual `controls`:
/// minimize and maximize mean nothing there, so leaving full screen takes
/// maximize's place, or comes first in a layout without maximize, and
/// close stays where it is.
fn in_full_screen(controls: &[Control]) -> Vec<Control> {
    let mut shown: Vec<Control> = controls
        .iter()
        .filter_map(|control| match control {
            Control::Minimize | Control::LeaveFullScreen => None,
            Control::Maximize => Some(Control::LeaveFullScreen),
            Control::Close => Some(Control::Close),
        })
        .collect();
    if !shown.contains(&Control::LeaveFullScreen) {
        shown.insert(0, Control::LeaveFullScreen);
    }
    shown
}

/// A row for controls inside the title bar: pressing one of them does not
/// start moving the window, so their clicks arrive.
pub fn not_dragging() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

impl Home {
    fn control(control: Control, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let (id, icon) = match control {
            Control::Minimize => ("window-minimize", Icon::WindowMinimize),
            Control::Maximize if window.is_maximized() => ("window-restore", Icon::WindowRestore),
            Control::Maximize => ("window-maximize", Icon::WindowMaximize),
            Control::LeaveFullScreen => ("window-leave-full-screen", Icon::ExitFullscreen),
            Control::Close => ("window-close", Icon::WindowClose),
        };
        let close = control == Control::Close;
        div()
            .id(SharedString::from(id))
            .group(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded_full()
            .cursor_pointer()
            .map(|b| {
                if close {
                    b.hover(|b| b.bg(Theme::danger()))
                } else {
                    b.hover(|b| b.bg(Theme::raised()))
                }
            })
            .child(
                div()
                    .flex()
                    .text_color(Theme::secondary_text())
                    .group_hover(id, move |s| {
                        s.text_color(if close { gpui::white() } else { Theme::text() })
                    })
                    .child(ui::kit_icon(icon).size(px(16.))),
            )
            .when(control == Control::LeaveFullScreen, |b| {
                b.tooltip(ui::tooltip("Leave full screen".into()))
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |home, _, window, cx| match control {
                Control::Minimize => window.minimize_window(),
                Control::Maximize => window.zoom_window(),
                Control::LeaveFullScreen => window.toggle_fullscreen(),
                Control::Close => {
                    if home.should_close(cx) {
                        cx.quit();
                    }
                }
            }))
    }

    /// The title bar with `content` at its left.
    pub(crate) fn window_bar(
        content: Option<AnyElement>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let shown = if window.is_fullscreen() {
            in_full_screen(controls())
        } else {
            controls().to_vec()
        };
        let mut buttons = div().flex().flex_none().items_center().gap_1p5().pr_3();
        for control in shown {
            buttons = buttons.child(Self::control(control, window, cx));
        }
        div()
            .id("window-bar")
            .flex()
            .flex_none()
            .items_center()
            .h(px(BAR_HEIGHT))
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            })
            .on_mouse_down(MouseButton::Right, |event, window, _| {
                window.show_window_menu(event.position);
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .children(content),
            )
            .child(buttons)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kwin_letters_name_the_buttons_in_their_order() {
        assert_eq!(
            parse("HIAX"),
            [Control::Minimize, Control::Maximize, Control::Close]
        );
        assert_eq!(parse("XA"), [Control::Close, Control::Maximize]);
        assert_eq!(parse(""), Vec::<Control>::new());
    }

    #[test]
    fn full_screen_keeps_leaving_it_and_close() {
        assert_eq!(
            in_full_screen(&parse("IAX")),
            [Control::LeaveFullScreen, Control::Close]
        );
        assert_eq!(
            in_full_screen(&parse("XA")),
            [Control::Close, Control::LeaveFullScreen]
        );
        assert_eq!(
            in_full_screen(&parse("IX")),
            [Control::LeaveFullScreen, Control::Close]
        );
        assert_eq!(in_full_screen(&[]), [Control::LeaveFullScreen]);
    }
}
