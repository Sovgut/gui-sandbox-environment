//! The app's look: one dark theme and the few building blocks every view
//! is made of.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyView, App, ClickEvent, Context, Div, FontWeight, Hsla,
    ScrollHandle, SharedString, Stateful, Svg, Window, div, prelude::*, pulsating_between, px, rgb,
    svg,
};

use crate::icons::Icon;

pub struct Theme;

impl Theme {
    pub fn background() -> Hsla {
        rgb(0x10_12_17).into()
    }
    /// Cards and bars.
    pub fn surface() -> Hsla {
        rgb(0x18_1b_22).into()
    }
    /// Controls on a surface, and the hover state of rows.
    pub fn raised() -> Hsla {
        rgb(0x23_27_31).into()
    }
    pub fn border() -> Hsla {
        rgb(0x2c_31_3d).into()
    }
    pub fn text() -> Hsla {
        rgb(0xe6_e9_ef).into()
    }
    pub fn muted() -> Hsla {
        rgb(0x9a_a2_b3).into()
    }
    pub fn subtle() -> Hsla {
        rgb(0x6b_72_80).into()
    }
    pub fn accent() -> Hsla {
        rgb(0x6c_a6_ff).into()
    }
    /// Text on [`Theme::accent`].
    pub fn on_accent() -> Hsla {
        rgb(0x0b_14_24).into()
    }
    pub fn good() -> Hsla {
        rgb(0x4a_de_80).into()
    }
    pub fn warning() -> Hsla {
        rgb(0xf5_b9_4a).into()
    }
    pub fn danger() -> Hsla {
        rgb(0xf8_71_71).into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The main action of a view.
    Primary,
    Secondary,
    /// An action that throws something away.
    Danger,
    /// A quiet action next to others.
    Ghost,
}

pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (background, text, border, hover) = match kind {
        Kind::Primary => (
            Theme::accent(),
            Theme::on_accent(),
            Theme::accent(),
            Theme::accent().opacity(0.85),
        ),
        Kind::Secondary => (
            Theme::raised(),
            Theme::text(),
            Theme::border(),
            Theme::border(),
        ),
        Kind::Danger => (
            Theme::danger().opacity(0.0),
            Theme::danger(),
            Theme::danger().opacity(0.5),
            Theme::danger().opacity(0.15),
        ),
        Kind::Ghost => (
            Theme::raised().opacity(0.0),
            Theme::muted(),
            Theme::raised().opacity(0.0),
            Theme::raised(),
        ),
    };
    let id: SharedString = id.into();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(30.))
        .px_3()
        .rounded_md()
        .border_1()
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .child(label.into())
        .map(|b| {
            if enabled {
                b.bg(background)
                    .border_color(border)
                    .text_color(text)
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .active(|s| s.opacity(0.8))
                    .on_click(on_click)
            } else {
                b.bg(Theme::raised().opacity(0.5))
                    .border_color(Theme::border().opacity(0.5))
                    .text_color(Theme::subtle())
            }
        })
}

/// `icon` at the size of small text, painted in `color`: an SVG takes no
/// colour from the element around it.
pub fn icon(icon: Icon, color: Hsla) -> Svg {
    svg()
        .path(icon.path())
        .flex_none()
        .size_4()
        .text_color(color)
}

/// The colour of a button's label.
fn label_color(kind: Kind, enabled: bool) -> Hsla {
    if !enabled {
        return Theme::subtle();
    }
    match kind {
        Kind::Primary => Theme::on_accent(),
        Kind::Secondary => Theme::text(),
        Kind::Danger => Theme::danger(),
        Kind::Ghost => Theme::muted(),
    }
}

/// The words a [`tooltip`] shows.
pub struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(Theme::raised())
            .border_1()
            .border_color(Theme::border())
            .text_xs()
            .text_color(Theme::text())
            .child(self.0.clone())
    }
}

/// Shows `text` while the pointer rests on an element.
pub fn tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |_, cx| cx.new(|_| Tooltip(text.clone())).into()
}

/// A square button that shows `icon` and says what it does in a tooltip.
pub fn icon_button(
    id: impl Into<SharedString>,
    shown: Icon,
    tip: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    button(id, "", kind, enabled, on_click)
        .w(px(30.))
        .px_0()
        .child(icon(shown, label_color(kind, enabled)))
        .tooltip(tooltip(tip.into()))
}

/// A small square icon button for rows of a table, always enabled.
pub fn small_icon_button(
    id: impl Into<SharedString>,
    shown: Icon,
    tip: &'static str,
    kind: Kind,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded_md()
        .cursor_pointer()
        .hover(|b| b.bg(Theme::raised()))
        .active(|b| b.opacity(0.8))
        .child(
            svg()
                .path(shown.path())
                .size_3p5()
                .text_color(label_color(kind, true)),
        )
        .tooltip(tooltip(tip.into()))
        .on_click(on_click)
}

/// The thumb of a scrolling area's scroll bar, to put over the area in a
/// relative parent; none while everything fits.
pub fn scrollbar(handle: &ScrollHandle) -> Option<Div> {
    let max = handle.max_offset().height;
    let view = handle.bounds().size.height;
    if max <= px(0.) || view <= px(0.) {
        return None;
    }
    let thumb = (view * (view / (view + max))).max(px(24.));
    let scrolled = (-handle.offset().y).clamp(px(0.), max);
    let top = (view - thumb) * (scrolled / max);
    Some(
        div()
            .absolute()
            .top(top)
            .right(px(1.))
            .w(px(4.))
            .h(thumb)
            .rounded_full()
            .bg(Theme::subtle().opacity(0.7)),
    )
}

/// A coloured pill with a dot, for states.
pub fn badge(label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(color.opacity(0.14))
        .text_color(color)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(div().size_1p5().rounded_full().bg(color))
        .child(label.into())
}

/// A quiet pill for labels that are not states.
pub fn tag(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_md()
        .bg(Theme::raised())
        .text_color(Theme::muted())
        .text_xs()
        .child(label.into())
}

/// One option of a segmented control; put several in [`segments`].
pub fn segment(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    selected: bool,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(28.))
        .px_3()
        .rounded_md()
        .text_sm()
        .whitespace_nowrap()
        .child(label.into())
        .map(|s| {
            if selected {
                s.bg(Theme::accent().opacity(0.18))
                    .text_color(Theme::accent())
                    .font_weight(FontWeight::MEDIUM)
            } else {
                s.text_color(if enabled {
                    Theme::muted()
                } else {
                    Theme::subtle()
                })
            }
        })
        .when(enabled && !selected, |s| {
            s.cursor_pointer()
                .hover(|s| s.bg(Theme::raised()).text_color(Theme::text()))
                .on_click(on_click)
        })
}

/// The frame of a segmented control.
pub fn segments() -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .p_1()
        .rounded_lg()
        .bg(Theme::background())
        .border_1()
        .border_color(Theme::border())
}

/// A card with a title and an optional line under it.
pub fn card(title: impl Into<SharedString>, description: Option<&'static str>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(Theme::surface())
        .border_1()
        .border_color(Theme::border())
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_color(Theme::text())
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .children(description.map(|d| div().text_sm().text_color(Theme::muted()).child(d))),
        )
}

/// A tinted box for something the user should notice.
pub fn notice(color: Hsla, title: impl Into<SharedString>, body: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_start()
        .gap_3()
        .p_3()
        .rounded_lg()
        .bg(color.opacity(0.1))
        .border_1()
        .border_color(color.opacity(0.35))
        .child(div().mt_1p5().size_2().flex_none().rounded_full().bg(color))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(Theme::muted())
                        .child(body.into()),
                ),
        )
}

/// Steps in a line: done ones solid, the current one pulsing, the ones
/// ahead dashed. `reached` is the current step, `None` before the first;
/// `finished` marks it done too. `id` keeps the pulse apart from other
/// lines.
pub fn timeline(id: &str, labels: &[&str], reached: Option<usize>, finished: bool) -> Div {
    let done = |i: usize| reached.is_some_and(|r| i < r || (i == r && finished));
    let mut line = div().flex().items_center();
    let mut row = div().flex();
    for (i, label) in labels.iter().enumerate() {
        let current = reached == Some(i) && !finished;
        let color = if done(i) || current {
            Theme::accent()
        } else {
            Theme::subtle()
        };
        let dot = div().flex_none().size_2p5().rounded_full().map(|d| {
            if done(i) {
                d.bg(Theme::accent())
            } else {
                d.border_2().border_color(color)
            }
        });
        line = line.child(if current {
            dot.bg(Theme::accent())
                .with_animation(
                    SharedString::from(format!("{id}-{i}")),
                    Animation::new(Duration::from_millis(1400))
                        .repeat()
                        .with_easing(pulsating_between(0.3, 1.0)),
                    Styled::opacity,
                )
                .into_any_element()
        } else {
            dot.into_any_element()
        });
        let last = i + 1 == labels.len();
        if !last {
            let segment = div().flex_1().mx_1();
            line = line.child(if done(i) {
                segment.h(px(2.)).bg(Theme::accent())
            } else {
                segment
                    .h(px(0.))
                    .border_t_2()
                    .border_dashed()
                    .border_color(Theme::border())
            });
        }
        // Each label hangs off an anchor as wide as its dot, centred on it;
        // the first and the last stay inside the line's ends.
        let text = div()
            .flex_none()
            .whitespace_nowrap()
            .text_xs()
            .text_color(color)
            .child(label.to_string());
        let anchor = div().flex().flex_none().w(px(10.));
        row = row.child(match i {
            0 => anchor.justify_start().child(text),
            _ if last => anchor.justify_end().child(text),
            _ => anchor.justify_center().child(text),
        });
        if !last {
            row = row.child(div().flex_1().mx_1());
        }
    }
    div().flex().flex_col().gap_1p5().child(line).child(row)
}
