//! The app's look: one dark palette, laid over GPUI Kit's theme so its
//! components wear it too, and the few building blocks every view is made
//! of.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyView, App, ClickEvent, Div, FontWeight, Hsla, ScrollHandle,
    SharedString, Window, div, prelude::*, pulsating_between, px, rgb,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon as KitIcon, Sizable as _, Theme as KitTheme};

use crate::icons::Icon;

/// Whether the light palette is in use; the dark one otherwise.
static LIGHT: AtomicBool = AtomicBool::new(false);

/// `dark` or `light`, by the palette in use.
fn pick(dark: u32, light: u32) -> Hsla {
    rgb(if LIGHT.load(Ordering::Relaxed) {
        light
    } else {
        dark
    })
    .into()
}

pub struct Theme;

impl Theme {
    pub fn background() -> Hsla {
        pick(0x0e_10_15, 0xf5_f6_f8)
    }
    /// The sandbox list and the settings menu.
    pub fn sidebar() -> Hsla {
        pick(0x14_17_1d, 0xec_ee_f2)
    }
    /// Cards and bars.
    pub fn surface() -> Hsla {
        pick(0x16_1a_21, 0xff_ff_ff)
    }
    /// The panel under a sandbox's cover, a shade off the page.
    pub fn panel() -> Hsla {
        pick(0x13_16_1c, 0xf0_f2_f5)
    }
    /// Controls on a surface, and the hover state of rows.
    pub fn raised() -> Hsla {
        pick(0x1c_20_28, 0xe6_e9_ee)
    }
    /// The chosen row of a list and the chosen tab.
    pub fn selected() -> Hsla {
        pick(0x23_28_38, 0xdc_e3_f3)
    }
    pub fn border() -> Hsla {
        pick(0x22_26_2e, 0xdd_e1_e8)
    }
    /// Borders of dialogs, menus and panels over the page.
    pub fn line() -> Hsla {
        pick(0x2a_2f_3b, 0xd3_d8_e0)
    }
    /// Lines between the rows of a list.
    pub fn divider() -> Hsla {
        pick(0x1d_21_29, 0xe6_e9_ee)
    }
    /// A group of settings rows.
    pub fn group() -> Hsla {
        pick(0x18_1c_24, 0xff_ff_ff)
    }
    /// Sunken boxes: file lists and fields.
    pub fn well() -> Hsla {
        pick(0x0f_12_18, 0xf7_f8_fa)
    }
    pub fn dialog() -> Hsla {
        pick(0x16_1a_22, 0xff_ff_ff)
    }
    /// Menus and messages floating over the page.
    pub fn popover() -> Hsla {
        pick(0x1b_1f_28, 0xff_ff_ff)
    }
    /// A row under the pointer.
    pub fn hover() -> Hsla {
        pick(0x1a_1f_2b, 0xee_f1_f6)
    }
    /// A chosen card.
    pub fn chosen() -> Hsla {
        pick(0x1a_21_31, 0xe9_ef_fc)
    }
    /// The column beside a sandbox's screen.
    pub fn aside() -> Hsla {
        pick(0x11_13_19, 0xf3_f4_f7)
    }
    pub fn text() -> Hsla {
        pick(0xe8_ea_f0, 0x1b_1e_25)
    }
    /// Text under a heading, quieter than the text and louder than muted.
    pub fn secondary_text() -> Hsla {
        pick(0xa3_aa_b8, 0x4f_57_67)
    }
    pub fn muted() -> Hsla {
        pick(0x8b_93_a3, 0x5f_67_77)
    }
    pub fn subtle() -> Hsla {
        pick(0x6f_77_87, 0x8a_92_a2)
    }
    pub fn accent() -> Hsla {
        pick(0x7a_a2_ff, 0x3a_6e_e0)
    }
    /// Text in the accent's colour on a dim accent ground.
    pub fn accent_text() -> Hsla {
        pick(0xa9_c1_ff, 0x2f_5b_c4)
    }
    /// Text on [`Theme::accent`].
    pub fn on_accent() -> Hsla {
        pick(0x0e_10_15, 0xff_ff_ff)
    }
    pub fn good() -> Hsla {
        pick(0x4a_de_80, 0x1f_9d_55)
    }
    pub fn warning() -> Hsla {
        pick(0xf5_b9_4a, 0xb7_79_1f)
    }
    /// Warning text, a shade softer than [`Theme::warning`].
    pub fn warning_text() -> Hsla {
        pick(0xf5_c4_6e, 0x9a_64_12)
    }
    pub fn danger() -> Hsla {
        pick(0xf8_71_71, 0xd6_45_45)
    }
    /// Text of a quiet danger button, lighter than [`Theme::danger`].
    pub fn danger_text() -> Hsla {
        pick(0xfc_a5_a5, 0xc5_30_30)
    }
    /// Text on [`Theme::danger`].
    pub fn on_danger() -> Hsla {
        pick(0x1a_0b_0b, 0xff_ff_ff)
    }

    /// Picks the light or the dark palette and lays it over GPUI Kit's
    /// theme of the same kind. Switching the Kit's mode reloads its own
    /// colours, so the palette goes over them after every switch.
    pub fn apply(light: bool, cx: &mut App) {
        LIGHT.store(light, Ordering::Relaxed);
        KitTheme::change(
            if light {
                gpui_kit::component::ThemeMode::Light
            } else {
                gpui_kit::component::ThemeMode::Dark
            },
            None,
            cx,
        );
        KitTheme::update(cx, |t| {
            t.font_family = "Inter".into();
            t.font_size = px(14.);
            t.radius = px(8.);
            t.radius_lg = px(12.);
            t.shadow = false;

            t.background = Self::background();
            t.foreground = Self::text();
            t.muted = Self::raised();
            t.muted_foreground = Self::muted();
            t.border = Self::border();
            t.input = Self::border();
            t.ring = Self::accent().opacity(0.6);
            t.caret = Self::accent();
            t.selection = Self::accent().opacity(0.3);
            t.link = Self::accent();
            t.overlay = Hsla::from(rgb(0x05_06_0a)).opacity(if light { 0.35 } else { 0.7 });
            t.popover = Self::popover();
            t.popover_foreground = Self::text();
            t.accent = Self::selected();
            t.accent_foreground = Self::text();

            t.primary = Self::accent();
            t.primary_foreground = Self::on_accent();
            t.primary_hover = Self::accent().opacity(0.88);
            t.primary_active = Self::accent().opacity(0.78);
            t.button_primary = Self::accent();
            t.button_primary_foreground = Self::on_accent();
            t.button_primary_hover = Self::accent().opacity(0.88);
            t.button_primary_active = Self::accent().opacity(0.78);

            t.secondary = Self::raised();
            t.secondary_foreground = Self::text();
            t.secondary_hover = Self::selected();
            t.secondary_active = Self::selected();
            t.button = Self::raised();
            t.button_foreground = Self::text();
            t.button_hover = Self::selected();
            t.button_active = Self::selected();
            t.button_secondary = Self::raised();
            t.button_secondary_foreground = Self::text();
            t.button_secondary_hover = Self::selected();
            t.button_secondary_active = Self::selected();

            // Danger buttons are tinted, not filled: a red slab shouts over
            // everything else on the page.
            t.danger = Self::danger();
            t.danger_foreground = Self::danger_text();
            t.button_danger = Self::danger().opacity(0.15);
            t.button_danger_foreground = Self::danger_text();
            t.button_danger_hover = Self::danger().opacity(0.25);
            t.button_danger_active = Self::danger().opacity(0.32);

            t.success = Self::good();
            t.warning = Self::warning();
            t.info = Self::accent();

            t.sidebar = Self::sidebar();
            t.sidebar_foreground = Self::text();
            t.sidebar_border = Self::border();
            t.sidebar_accent = Self::selected();
            t.sidebar_accent_foreground = Self::text();
            t.title_bar = Self::background();
            t.title_bar_border = Self::background();
            t.window_border = Self::border();

            t.tab_bar = Self::panel();
            t.tab_bar_segmented = Self::background();
            t.tab = Self::panel();
            t.tab_active = Self::selected();
            t.tab_foreground = Self::muted();
            t.tab_active_foreground = Self::text();

            t.colors.list = Self::panel();
            t.list_hover = Self::raised();
            t.list_active = Self::selected();
            t.table = Self::panel();
            t.table_head = Self::panel();
            t.table_head_foreground = Self::subtle();
            t.table_hover = Self::raised();
            t.table_row_border = Self::border();

            t.switch = Self::selected();
            t.switch_thumb = gpui::white();
            t.progress_bar = Self::accent();
            t.scrollbar = Self::background().opacity(0.0);
            t.scrollbar_thumb = Self::subtle().opacity(0.6);
            t.scrollbar_thumb_hover = Self::muted();
            t.skeleton = Self::raised();
            t.group_box = Self::surface();
        });
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

fn styled(button: Button, kind: Kind) -> Button {
    match kind {
        Kind::Primary => button.primary(),
        Kind::Secondary => button,
        Kind::Danger => button.danger(),
        Kind::Ghost => button.ghost(),
    }
}

pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let id: SharedString = id.into();
    styled(Button::new(id), kind)
        .label(label.into())
        .disabled(!enabled)
        .on_click(on_click)
}

/// An [`Icon`] as GPUI Kit draws icons.
pub fn kit_icon(icon: Icon) -> KitIcon {
    KitIcon::empty().path(icon.path())
}

/// `icon` at the size of small text, painted in `color`: an SVG takes no
/// colour from the element around it.
pub fn icon(icon: Icon, color: Hsla) -> KitIcon {
    kit_icon(icon).size_4().text_color(color)
}

/// Shows `text` while the pointer rests on an element.
pub fn tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// A square button that shows `icon` and says what it does in a tooltip.
pub fn icon_button(
    id: impl Into<SharedString>,
    shown: Icon,
    tip: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let id: SharedString = id.into();
    let tip: SharedString = tip.into();
    styled(Button::new(id), kind)
        .icon(kit_icon(shown))
        .tooltip(tip)
        .disabled(!enabled)
        .on_click(on_click)
}

/// A small square icon button for rows of a table, always enabled.
pub fn small_icon_button(
    id: impl Into<SharedString>,
    shown: Icon,
    tip: &'static str,
    kind: Kind,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let id: SharedString = id.into();
    let button = Button::new(id)
        .ghost()
        .xsmall()
        .icon(kit_icon(shown))
        .tooltip(tip)
        .on_click(on_click);
    if kind == Kind::Danger {
        button.text_color(Theme::danger())
    } else {
        button
    }
}

/// The thumb of a scrolling area's scroll bar, to put over the area in a
/// relative parent; none while everything fits.
pub fn scrollbar(handle: &ScrollHandle) -> Option<Div> {
    let max = handle.max_offset().y;
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
        .font_weight(FontWeight::SEMIBOLD)
        .child(div().size_1p5().rounded_full().bg(color))
        .child(label.into())
}

/// A key of the keyboard, as a hint names it.
pub fn key(label: &'static str) -> Div {
    div()
        .px_2()
        .py_0p5()
        .rounded(px(6.))
        .bg(Theme::selected())
        .text_sm()
        .text_color(Theme::text())
        .child(label)
}

/// A quiet pill for labels that are not states.
pub fn tag(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_md()
        .bg(Theme::selected())
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
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(30.))
        .px_3()
        .rounded_md()
        .text_sm()
        .whitespace_nowrap()
        .child(label.into())
        .map(|s| {
            if selected {
                s.bg(Theme::selected())
                    .text_color(Theme::text())
                    .font_weight(FontWeight::SEMIBOLD)
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
}

/// A tinted box for something the user should notice.
pub fn notice(color: Hsla, title: impl Into<SharedString>, body: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_3()
        .rounded_xl()
        .bg(color.opacity(0.1))
        .child(div().size_2().flex_none().rounded_full().bg(color))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(Theme::text())
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

/// The most steps a timeline names each of.
const CROWDED: usize = 7;

/// Steps in a line in `color`: done ones solid, the current one pulsing,
/// the ones ahead dashed. `reached` is the current step, `None` before the
/// first; `finished` marks it done too. `id` keeps the pulse apart from
/// other lines.
pub fn timeline(
    id: &str,
    labels: &[&str],
    reached: Option<usize>,
    finished: bool,
    color: Hsla,
) -> Div {
    let done = |i: usize| reached.is_some_and(|r| i < r || (i == r && finished));
    let mut line = div().flex().items_center();
    let mut row = div().flex();
    for (i, label) in labels.iter().enumerate() {
        let current = reached == Some(i) && !finished;
        let lit = done(i) || current;
        let dot = div().flex_none().size_3().rounded_full().map(|d| {
            if lit {
                d.bg(color)
            } else {
                d.border_2().border_color(Theme::selected())
            }
        });
        line = line.child(if current {
            div()
                .flex_none()
                .size_3()
                .rounded_full()
                .bg(color)
                .with_animation(
                    SharedString::from(format!("{id}-{i}")),
                    Animation::new(Duration::from_millis(1400))
                        .repeat()
                        .with_easing(pulsating_between(0.4, 1.0)),
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
                segment.h(px(2.)).bg(color)
            } else {
                segment
                    .h(px(0.))
                    .border_t_2()
                    .border_dashed()
                    .border_color(Theme::selected())
            });
        }
        // Each label hangs off an anchor as wide as its dot, centred on it;
        // the first and the last stay inside the line's ends. A long line
        // names only the step it is at, so the names never overlap.
        let named = labels.len() <= CROWDED
            || current
            || (finished && reached == Some(i))
            || (reached.is_none() && i == 0);
        let text = div()
            .flex_none()
            .whitespace_nowrap()
            .text_sm()
            .text_color(if lit { color } else { Theme::muted() })
            .when(named, |t| t.child(label.to_string()));
        let anchor = div().flex().flex_none().w(px(12.));
        row = row.child(match i {
            0 => anchor.justify_start().child(text),
            _ if last => anchor.justify_end().child(text),
            _ => anchor.justify_center().child(text),
        });
        if !last {
            row = row.child(div().flex_1().mx_1());
        }
    }
    div().flex().flex_col().gap_2().child(line).child(row)
}
