//! The app's look: one dark palette, laid over GPUI Kit's theme so its
//! components wear it too, and the few building blocks every view is made
//! of.
//!
//! Sizes are the design's pixels. The rem is 16 px, so Tailwind's units
//! (`p_4`, `gap_3`, `text_xs`) are the CSS values the boards are drawn in;
//! the window's own text is 14 px, with lines as tall as a browser's
//! `normal` for Inter (see [`window_text`]). Sizes Tailwind has no unit for
//! are written in pixels: `text_size(px(13.))`.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, AnyView, App, BoxShadow, ClickEvent, Div, ElementId,
    FontWeight, Hsla, Refineable as _, ScrollHandle, SharedString, StyleRefinement, Window, div,
    prelude::*, pulsating_between, px, relative, rgb,
};
use gpui_kit::base::{
    Button as BaseButton, Switch as BaseSwitch, SwitchThumb, SwitchTrack, spring,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon as KitIcon, IconName, Theme as KitTheme};

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
    /// The fill of a secondary button.
    pub fn control() -> Hsla {
        pick(0x1f_24_30, 0xe4_e8_ef)
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
    /// The steps of a line still ahead.
    pub fn pending() -> Hsla {
        pick(0x3a_40_50, 0xc8_ce_d8)
    }
    /// The ring of a step of a list not begun yet, and the dots of the
    /// tour's steps ahead.
    pub fn ahead() -> Hsla {
        pick(0x34_3a_46, 0xcd_d2_db)
    }
    /// The empty part of a progress bar.
    pub fn track() -> Hsla {
        pick(0x26_2b_36, 0xe1_e5_ec)
    }
    /// The card picked among a few side by side on a page.
    pub fn picked() -> Hsla {
        pick(0x16_1c_2a, 0xed_f2_fd)
    }
    /// The border of a card that can be picked, and of a picture on a page.
    pub fn outline() -> Hsla {
        pick(0x26_2a_33, 0xd6_db_e3)
    }
    /// A box over the dimmed window that points at what to press.
    pub fn callout() -> Hsla {
        pick(0x1b_20_30, 0xff_ff_ff)
    }
    /// The border of a [`Theme::callout`].
    pub fn callout_line() -> Hsla {
        pick(0x2f_36_47, 0xd3_d8_e0)
    }
    /// A key of the keyboard named in a [`Theme::callout`]'s text.
    pub fn callout_key() -> Hsla {
        pick(0x2a_30_42, 0xe6_e9_ef)
    }
    pub fn text() -> Hsla {
        pick(0xe8_ea_f0, 0x1b_1e_25)
    }
    /// Text a step quieter than [`Theme::text`]: secondary buttons, chips,
    /// and values beside a label.
    pub fn soft_text() -> Hsla {
        pick(0xc4_ca_d6, 0x3d_44_52)
    }
    /// The running text of a dialog.
    pub fn body_text() -> Hsla {
        pick(0xb4_bb_c8, 0x47_4e_5d)
    }
    /// Text under a heading, quieter than the text and louder than muted.
    pub fn secondary_text() -> Hsla {
        pick(0xa3_aa_b8, 0x4f_57_67)
    }
    /// The entries of the settings menu that are not chosen, and its way
    /// back.
    pub fn nav_text() -> Hsla {
        pick(0xb7_bd_c9, 0x45_4c_5a)
    }
    pub fn muted() -> Hsla {
        pick(0x8b_93_a3, 0x5f_67_77)
    }
    /// The label of a button that cannot be pressed.
    pub fn disabled_text() -> Hsla {
        pick(0x7c_84_94, 0x9a_a1_ae)
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
    /// Links, and steps passed on an accent line.
    pub fn link() -> Hsla {
        pick(0x8f_b2_ff, 0x2f_5b_c4)
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
    /// Text on [`Theme::warning`].
    pub fn on_warning() -> Hsla {
        pick(0x1a_14_06, 0xff_ff_ff)
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
            // The rem: Tailwind's units come out as the CSS pixels the design
            // is drawn in, and the Kit's components at the sizes they are
            // made for. The window's text is 14 px all the same.
            t.font_size = px(16.);
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

            t.secondary = Self::control();
            t.secondary_foreground = Self::text();
            t.secondary_hover = Self::selected();
            t.secondary_active = Self::selected();
            t.button = Self::control();
            t.button_foreground = Self::text();
            t.button_hover = Self::selected();
            t.button_active = Self::selected();
            t.button_secondary = Self::control();
            t.button_secondary_foreground = Self::text();
            t.button_secondary_hover = Self::selected();
            t.button_secondary_active = Self::selected();

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
            t.switch_thumb = Self::on_accent();
            t.progress_bar = Self::accent();
            t.scrollbar = Self::background().opacity(0.0);
            t.scrollbar_thumb = Self::subtle().opacity(0.6);
            t.scrollbar_thumb_hover = Self::muted();
            t.skeleton = Self::raised();
            t.group_box = Self::surface();
        });
    }
}

/// The text of the window, on its root: 14 px, each line as tall as
/// Chromium's `normal` for Inter, so the boards' padding around a line of
/// text gives the boards' heights. Multi-line text the design spaces out
/// sets its own line height.
pub fn window_text(root: Div) -> Div {
    root.text_size(px(14.)).line_height(relative(1.21))
}

/// The height Chromium gives a line of Inter at `size` px when its line
/// height is `normal`: the font's ascent and descent, each rounded to a
/// pixel. It is within a pixel of [`window_text`]'s, and where a board
/// stacks lines of several sizes it gives the board's heights exactly.
pub fn normal_line(size: f32) -> gpui::Pixels {
    px((size * 0.968_75).round() + (size * 0.242).round())
}

/// What a banner or a line of steps is about, and the colours that go
/// with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Warning,
    Danger,
}

impl Tone {
    /// The colour itself: dots, lines and icons.
    pub fn color(self) -> Hsla {
        match self {
            Self::Accent => Theme::accent(),
            Self::Warning => Theme::warning(),
            Self::Danger => Theme::danger(),
        }
    }

    /// Text in the tone, a shade softer than its colour.
    pub fn text(self) -> Hsla {
        match self {
            Self::Accent => Theme::link(),
            Self::Warning => Theme::warning_text(),
            Self::Danger => Theme::danger_text(),
        }
    }

    /// The solid ground of a one-line [`banner`], and of a mark or a card
    /// tinted with the tone.
    pub fn ground(self) -> Hsla {
        match self {
            Self::Accent => pick(0x1c_23_33, 0xe6_ee_fc),
            Self::Warning => pick(0x2a_24_14, 0xfb_f0_d9),
            Self::Danger => pick(0x2b_1a_1d, 0xfb_e3_e3),
        }
    }
}

/// How a button looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The main action of a view: filled with the accent, bold.
    Primary,
    /// A plain action: the control fill and full-strength text.
    Secondary,
    /// A dismissal beside the main action, and a tool in a bar: the
    /// control fill with softer text.
    Quiet,
    /// An action that adds or builds, tinted with the accent.
    Accent,
    /// An action that throws something away, tinted: the first of two
    /// clicks, or a quiet one among others.
    Danger,
    /// The click that throws something away for good, filled red: the last
    /// step of a confirmation.
    DangerFilled,
    /// Throwing away everything, outlined in red: on a page, where a fill
    /// would shout over it.
    DangerOutline,
    /// The action of a warning banner, filled with its amber.
    Warning,
    /// No fill: a quiet action next to others.
    Ghost,
}

/// How large a button is, by where it sits: its height, and the text,
/// padding, corners and icon that go with it. An icon button is a square
/// as wide as it is tall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Size {
    /// 24 tall, 12 px: an icon among the cells of a table row.
    Tiny,
    /// 28 tall, 12 px: an action or a menu inside a list row.
    Row,
    /// 28 tall, 13 px: the action of a one-line banner.
    Banner,
    /// 30 tall, 13 px: at the end of a set's or a settings row.
    Small,
    /// 32 tall, 13 px: in bars, panels and banners with two lines.
    #[default]
    Medium,
    /// 34 tall, 13 px: beside a search field, and in settings rows.
    Toolbar,
    /// 36 tall, 14 px: a page's own action.
    Large,
    /// 40 tall, 14 px: the answers of a dialog, and saving an editor.
    Dialog,
    /// 46 tall, 15 px: the one action a step of the tour asks for.
    Hero,
}

/// What a [`Size`] measures, in pixels.
struct Metrics {
    height: f32,
    text: f32,
    padding: f32,
    radius: f32,
    icon: f32,
    gap: f32,
}

impl Size {
    /// How tall a button of this size is.
    pub const fn height(self) -> f32 {
        self.metrics().height
    }

    const fn metrics(self) -> Metrics {
        let (height, text, padding, radius, icon, gap) = match self {
            Self::Tiny => (24., 12., 6., 6., 14., 4.),
            Self::Row => (28., 12., 10., 7., 14., 6.),
            Self::Banner => (28., 13., 12., 7., 14., 6.),
            Self::Small => (30., 13., 12., 8., 15., 6.),
            Self::Medium => (32., 13., 12., 8., 16., 6.),
            Self::Toolbar => (34., 13., 14., 8., 16., 6.),
            Self::Large => (36., 14., 14., 9., 16., 6.),
            Self::Dialog => (40., 14., 18., 10., 16., 8.),
            Self::Hero => (46., 15., 26., 10., 18., 8.),
        };
        Metrics {
            height,
            text,
            padding,
            radius,
            icon,
            gap,
        }
    }
}

/// The colours of a button of one [`Kind`] in each of its states.
struct Look {
    fill: Hsla,
    text: Hsla,
    weight: FontWeight,
    border: Option<Hsla>,
    hover: Hsla,
    hover_text: Hsla,
    press: Hsla,
    disabled_fill: Hsla,
    disabled_text: Hsla,
    disabled_border: Option<Hsla>,
}

/// `fill` a shade lighter in the dark palette, darker in the light one:
/// the ground of a button under the pointer (`step` 1) or pressed (2).
fn shade(fill: Hsla, step: f32) -> Hsla {
    fill.blend(Theme::text().opacity(0.06 * step))
}

impl Kind {
    /// The look of this kind; `fill` takes the place of its ground, and
    /// `icon_only` gives a borderless icon a muted colour.
    fn look(self, fill: Option<Hsla>, icon_only: bool) -> Look {
        let none = gpui::transparent_black();
        // A filled ground goes a shade lighter under the pointer; a solid
        // colour fades a little; a tint deepens.
        let solid = |color: Hsla, text: Hsla| {
            let fill = fill.unwrap_or(color);
            Look {
                fill,
                text,
                weight: FontWeight::BOLD,
                border: None,
                hover: fill.opacity(0.88),
                hover_text: text,
                press: fill.opacity(0.78),
                disabled_fill: Theme::control(),
                disabled_text: Theme::disabled_text(),
                disabled_border: None,
            }
        };
        let tint = |color: Hsla, text: Hsla| Look {
            fill: fill.unwrap_or_else(|| color.opacity(0.15)),
            text,
            weight: FontWeight::SEMIBOLD,
            border: None,
            hover: color.opacity(0.25),
            hover_text: text,
            press: color.opacity(0.32),
            disabled_fill: Theme::control(),
            disabled_text: Theme::disabled_text(),
            disabled_border: None,
        };
        let control = |text: Hsla| {
            let fill = fill.unwrap_or_else(Theme::control);
            Look {
                fill,
                text,
                weight: FontWeight::NORMAL,
                border: None,
                hover: shade(fill, 1.),
                hover_text: text,
                press: shade(fill, 2.),
                disabled_fill: Theme::control(),
                disabled_text: Theme::disabled_text(),
                disabled_border: None,
            }
        };
        match self {
            Self::Primary => solid(Theme::accent(), Theme::on_accent()),
            Self::Warning => solid(Theme::warning(), Theme::on_warning()),
            Self::DangerFilled => Look {
                disabled_fill: Theme::danger().opacity(0.4),
                disabled_text: Theme::on_danger(),
                ..solid(Theme::danger(), Theme::on_danger())
            },
            Self::Secondary => control(Theme::text()),
            Self::Quiet => control(Theme::soft_text()),
            Self::Accent => tint(Theme::accent(), Theme::accent_text()),
            // A red action that waits stays red, only fainter.
            Self::Danger => Look {
                disabled_fill: Theme::danger().opacity(0.08),
                disabled_text: Theme::danger_text().opacity(0.4),
                ..tint(Theme::danger(), Theme::danger_text())
            },
            Self::DangerOutline => Look {
                fill: fill.unwrap_or(none),
                text: Theme::danger_text(),
                weight: FontWeight::SEMIBOLD,
                border: Some(Theme::danger().opacity(0.4)),
                hover: Theme::danger().opacity(0.1),
                hover_text: Theme::danger_text(),
                press: Theme::danger().opacity(0.18),
                disabled_fill: none,
                disabled_text: Theme::danger_text().opacity(0.5),
                disabled_border: Some(Theme::danger().opacity(0.2)),
            },
            Self::Ghost => {
                let text = if icon_only {
                    Theme::muted()
                } else {
                    Theme::soft_text()
                };
                let fill = fill.unwrap_or(none);
                Look {
                    fill,
                    text,
                    weight: FontWeight::NORMAL,
                    border: None,
                    hover: shade(fill, 1.),
                    hover_text: Theme::text(),
                    press: shade(fill, 2.),
                    disabled_fill: fill,
                    disabled_text: Theme::disabled_text(),
                    disabled_border: None,
                }
            }
        }
    }
}

/// A button in the design's look: its [`Kind`] gives the colours, its
/// [`Size`] the height, text, padding and corners. It takes focus and
/// answers Enter and Space like any button; what the caller styles on it
/// goes over what its kind and size give it.
#[derive(IntoElement)]
pub struct Button {
    base: BaseButton,
    label: Option<SharedString>,
    icon: Option<KitIcon>,
    icon_size: Option<f32>,
    dropdown: bool,
    kind: Kind,
    size: Size,
    fill: Option<Hsla>,
    enabled: bool,
}

impl Button {
    fn new(
        id: impl Into<ElementId>,
        kind: Kind,
        enabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            base: BaseButton::new(id).disabled(!enabled).on_click(on_click),
            label: None,
            icon: None,
            icon_size: None,
            dropdown: false,
            kind,
            size: Size::default(),
            fill: None,
            enabled,
        }
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    /// An icon before the label.
    pub fn icon(mut self, icon: impl Into<KitIcon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// The icon `size` pixels square, in place of the one its size gives.
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = Some(size);
        self
    }

    /// A chevron after the label: the button opens a menu.
    pub fn dropdown(mut self) -> Self {
        self.dropdown = true;
        self
    }

    /// The button's ground in `color` in place of its kind's, a shade off
    /// it under the pointer: a secondary button on a surface of its own
    /// colour.
    pub fn fill(mut self, color: Hsla) -> Self {
        self.fill = Some(color);
        self
    }

    /// Says what the button does while the pointer rests on it.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.base = self.base.tooltip(tooltip(text.into()));
        self
    }

    /// Keeps the button from being pressed, on top of what made it.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled &= !disabled;
        self.base = self.base.disabled(!self.enabled);
        self
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self {
            mut base,
            label,
            icon,
            icon_size,
            dropdown,
            kind,
            size,
            fill,
            enabled,
        } = self;
        let metrics = size.metrics();
        let look = kind.look(fill, label.is_none());
        // What the caller styled goes over what the kind and size give.
        let caller = std::mem::take(base.style());
        let mut base = base
            .flex_shrink_0()
            .h(px(metrics.height))
            .gap(px(metrics.gap))
            .rounded(px(metrics.radius))
            .text_size(px(metrics.text))
            .font_weight(look.weight)
            .whitespace_nowrap()
            .map(|b| {
                if label.is_none() {
                    b.w(px(metrics.height))
                } else {
                    b.px(px(metrics.padding))
                }
            })
            .map(|b| {
                if enabled {
                    b.bg(look.fill)
                        .text_color(look.text)
                        .when_some(look.border, |b, color| b.border_1().border_color(color))
                        .hover(move |s| s.bg(look.hover).text_color(look.hover_text))
                        .active(move |s| s.bg(look.press))
                } else {
                    b.bg(look.disabled_fill)
                        .text_color(look.disabled_text)
                        .when_some(look.disabled_border, |b, color| {
                            b.border_1().border_color(color)
                        })
                }
            })
            .focus_visible(|s| {
                s.shadow(vec![
                    BoxShadow::new(px(0.), px(0.), Theme::accent().opacity(0.6))
                        .spread_radius(px(2.)),
                ])
            });
        base.style().refine(&caller);
        let icon_size = px(icon_size.unwrap_or(metrics.icon));
        base.children(icon.map(|icon| icon.size(icon_size)))
            .children(label.map(|label| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(label)
            }))
            .when(dropdown, |b| {
                let text = if enabled {
                    look.text
                } else {
                    look.disabled_text
                };
                b.justify_between().child(
                    KitIcon::new(IconName::ChevronDown)
                        .size(px(14.))
                        .text_color(text.opacity(0.75)),
                )
            })
    }
}

/// A button labelled `label`, in the look of `kind`, 32 tall unless sized.
pub fn button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let id: SharedString = id.into();
    let label: SharedString = label.into();
    let mut button = Button::new(id, kind, enabled, on_click);
    button.base = button.base.accessibility_label(label.clone());
    button.label = Some(label);
    button
}

/// An [`Icon`] as GPUI Kit draws icons.
pub fn kit_icon(icon: Icon) -> KitIcon {
    KitIcon::empty().path(icon.path())
}

impl From<Icon> for KitIcon {
    fn from(icon: Icon) -> Self {
        kit_icon(icon)
    }
}

/// `icon` 16 px square, painted in `color`: an SVG takes no colour from the
/// element around it.
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
    shown: impl Into<KitIcon>,
    tip: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    let id: SharedString = id.into();
    let tip: SharedString = tip.into();
    let mut button = Button::new(id, kind, enabled, on_click).icon(shown);
    button.base = button.base.accessibility_label(tip.clone());
    button.tooltip(tip)
}

/// A 24 px square icon button among the cells of a table row, with no
/// ground; a danger one shows its icon in red.
pub fn small_icon_button(
    id: impl Into<SharedString>,
    shown: Icon,
    tip: &'static str,
    kind: Kind,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    icon_button(id, shown, tip, Kind::Ghost, true, on_click)
        .size(Size::Tiny)
        .when(kind == Kind::Danger, |b| b.text_color(Theme::danger()))
}

/// How large a [`Switch`] is, by where it sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchSize {
    /// 40 by 22: on a page.
    Page,
    /// 38 by 22: at the end of a settings row.
    Row,
    /// 28 by 16: in a bar.
    Bar,
}

/// A switch in the design's look: the accent with a dark thumb when on, a
/// muted thumb on a quiet track when off. It takes focus and answers Space
/// like the Kit's switch; a label goes after it, inside the part that
/// toggles.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    enabled: bool,
    size: SwitchSize,
    label: Option<SharedString>,
    on_change: Option<ChangeHandler>,
    style: StyleRefinement,
}

/// What a [`Switch`] calls with the value a click asks for.
type ChangeHandler = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

/// A switch that is `checked`, `size` by where it sits.
pub fn switch(id: impl Into<ElementId>, checked: bool, size: SwitchSize) -> Switch {
    Switch {
        id: id.into(),
        checked,
        enabled: true,
        size,
        label: None,
        on_change: None,
        style: StyleRefinement::default(),
    }
}

impl Switch {
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self
    }

    /// Words after the switch, which toggle it too.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Called with the value a click asks for; the caller shows it back.
    pub fn on_change(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl Styled for Switch {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (width, height, thumb) = match self.size {
            SwitchSize::Page => (40., 22., 16.),
            SwitchSize::Row => (38., 22., 16.),
            SwitchSize::Bar => (28., 16., 12.),
        };
        let inset = (height - thumb) / 2.;
        let checked = self.checked;
        let travel = spring(
            (self.id.clone(), "thumb"),
            if checked {
                px(width - thumb - inset * 2.)
            } else {
                px(0.)
            },
            cx.theme().motion_tokens().spring_move,
            window,
            cx,
        );
        let ground = if checked {
            Theme::accent()
        } else {
            Theme::selected()
        };
        // A disabled switch fades its track alone: GPUI fades each part on
        // its own, so a faded thumb would let the track show through it.
        let track = SwitchTrack::new((self.id.clone(), "track"))
            .checked(checked)
            .disabled(!self.enabled)
            .flex()
            .flex_none()
            .items_center()
            .w(px(width))
            .h(px(height))
            .p(px(inset))
            .rounded_full()
            .bg(if self.enabled {
                ground
            } else {
                ground.opacity(0.5)
            })
            .child(
                SwitchThumb::new(checked)
                    .size(px(thumb))
                    .rounded_full()
                    .left(travel)
                    .bg(if checked {
                        Theme::on_accent()
                    } else {
                        Theme::muted()
                    }),
            );
        let label = self.label.clone();
        let mut base = BaseSwitch::new(self.id)
            .checked(checked)
            .disabled(!self.enabled)
            .flex()
            .items_center()
            .gap(px(6.))
            .when_some(label.clone(), BaseSwitch::accessibility_label)
            .when_some(self.on_change, |s, on_change| {
                s.on_change(move |next, _, window, cx| on_change(&next, window, cx))
            })
            .focus_visible(|s| {
                s.shadow(vec![
                    BoxShadow::new(px(0.), px(0.), Theme::accent().opacity(0.6))
                        .spread_radius(px(2.)),
                ])
            });
        base.style().refine(&self.style);
        base.child(track).children(label)
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
        .px(px(9.))
        .py(px(3.))
        .rounded_full()
        .bg(color.opacity(0.12))
        .text_color(color)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .child(div().size_1p5().rounded_full().bg(color))
        .child(label.into())
}

/// A key of the keyboard, as a dialog names it.
pub fn key(label: &'static str) -> Div {
    div()
        .flex_none()
        .px(px(9.))
        .py_1()
        .rounded(px(6.))
        .bg(Theme::selected())
        .text_size(px(13.))
        .text_color(Theme::text())
        .whitespace_nowrap()
        .child(label)
}

/// A key named inside a line of text, in the line's colour.
pub fn inline_key(label: &'static str) -> Div {
    div()
        .flex_none()
        .px_1p5()
        .py(px(1.))
        .rounded(px(5.))
        .bg(Theme::selected())
        .text_xs()
        .whitespace_nowrap()
        .child(label)
}

/// Words in a line of text that do something when clicked, 13 px in the
/// link colour and underlined; the caller says what a click does.
pub fn link(id: impl Into<SharedString>, label: impl Into<SharedString>) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    div()
        .id(id)
        .flex_none()
        .text_size(px(13.))
        .text_color(Theme::link())
        .underline()
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|l| l.text_color(Theme::accent_text()))
        .child(label.into())
}

/// A quiet pill for labels that are not states, such as the apps of a set.
pub fn tag(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(9.))
        .py(px(3.))
        .rounded(px(6.))
        .bg(Theme::selected())
        .text_color(Theme::soft_text())
        .text_xs()
        .child(label.into())
}

/// One entry of a drop-down menu: a check before the current one, its
/// label, and a quieter note after it.
pub fn menu_item(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    note: Option<&'static str>,
    current: bool,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_2p5()
        .py_1p5()
        .rounded(px(6.))
        .text_size(px(13.))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|o| o.bg(Theme::raised()))
        .child(
            div()
                .flex()
                .flex_none()
                .w(px(14.))
                .children(current.then(|| {
                    KitIcon::new(IconName::Check)
                        .size(px(14.))
                        .text_color(Theme::accent())
                })),
        )
        .child(
            div()
                .when(current, |l| l.font_weight(FontWeight::SEMIBOLD))
                .child(label.into()),
        )
        .children(note.map(|n| div().text_xs().text_color(Theme::muted()).child(n)))
}

/// One tab of a row of tabs over a panel: 34 tall, its label 13 px.
pub fn tab(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(34.))
        .px_3p5()
        .rounded(px(8.))
        .text_size(px(13.))
        .whitespace_nowrap()
        .child(label.into())
        .map(|t| {
            if selected {
                t.bg(Theme::selected())
                    .text_color(Theme::text())
                    .font_weight(FontWeight::SEMIBOLD)
            } else {
                t.text_color(Theme::muted())
                    .cursor_pointer()
                    .hover(|t| t.bg(Theme::raised()).text_color(Theme::text()))
                    .on_click(on_click)
            }
        })
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
        .rounded(px(7.))
        .text_size(px(13.))
        .whitespace_nowrap()
        .child(label.into())
        .map(|s| {
            if selected {
                s.bg(Theme::selected()).text_color(Theme::text())
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
        .p(px(3.))
        .rounded(px(9.))
        .bg(Theme::background())
}

/// A banner of one line: an icon, what to notice, and the buttons the
/// caller adds after it.
pub fn banner(tone: Tone, shown: Icon, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_3p5()
        .py_2p5()
        .rounded(px(10.))
        .bg(tone.ground())
        .text_size(px(13.))
        .child(icon(shown, tone.color()))
        .child(div().flex_1().min_w_0().child(text.into()))
}

/// A tinted box for something the user should notice: a dot, a title and
/// what it means, and the buttons the caller adds after them.
pub fn notice(tone: Tone, title: impl Into<SharedString>, body: impl Into<SharedString>) -> Div {
    let color = tone.color();
    div()
        .flex()
        .items_center()
        .gap_3p5()
        .px_4()
        .py_3()
        .rounded(px(12.))
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
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(Theme::text())
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(Theme::muted())
                        .child(body.into()),
                ),
        )
}

/// A line 2 px thick across its parent in `color`, dashed the way a
/// browser draws a 2 px dashed border: dashes 6 px long, 4 px apart. GPUI's
/// own dashed borders have shorter ones.
fn dashes(color: Hsla) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let mut x = bounds.left();
            while x < bounds.right() {
                let end = (x + px(6.)).min(bounds.right());
                window.paint_quad(gpui::fill(
                    gpui::Bounds::from_corners(
                        gpui::point(x, bounds.top()),
                        gpui::point(end, bounds.bottom()),
                    ),
                    color,
                ));
                x += px(10.);
            }
        },
    )
    .size_full()
}

/// The most steps a timeline names each of.
const CROWDED: usize = 7;

/// Where a step of a [`timeline`] stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Done,
    Current,
    Ahead,
}

/// The dot of a step in `color`: solid once done, haloed and pulsing while
/// current, a ring ahead. `key` keeps the pulse apart from other dots.
fn step_dot(key: String, color: Hsla, reach: Reach) -> AnyElement {
    let dot = div().flex_none().size_3().rounded_full();
    match reach {
        Reach::Done => return dot.bg(color).into_any_element(),
        Reach::Ahead => {
            return dot
                .border_2()
                .border_color(Theme::pending())
                .into_any_element();
        }
        Reach::Current => {}
    }
    // The halo is a disc of its own behind the dot: a shadow's spread keeps
    // the dot's corner radius and comes out square.
    let haloed = dot
        .relative()
        .child(
            div()
                .absolute()
                .top(px(-5.))
                .left(px(-5.))
                .size(px(22.))
                .rounded_full()
                .bg(color.opacity(0.25)),
        )
        .child(div().absolute().inset_0().rounded_full().bg(color));
    // A preview holds still, so two shots of it are the same.
    if crate::preview::active() {
        return haloed.into_any_element();
    }
    haloed
        .with_animation(
            SharedString::from(key),
            Animation::new(Duration::from_millis(1400))
                .repeat()
                .with_easing(pulsating_between(0.4, 1.0)),
            Styled::opacity,
        )
        .into_any_element()
}

/// The line from one step to the next in `color`: solid once the first is
/// done, dashed ahead.
fn stretch(color: Hsla, done: bool) -> Div {
    let stretch = div().flex_1().h(px(2.));
    if done {
        stretch.bg(color)
    } else {
        stretch.child(dashes(Theme::pending()))
    }
}

/// Steps in a line in the colour of `tone`: done ones solid, the current
/// one haloed and pulsing, the ones ahead dashed. Each step's name sits
/// centred under its dot; a line of more than [`CROWDED`] steps spreads its
/// dots over the whole width and names only the step it is at, so the
/// names never run together. `reached` is the current step, `None` before
/// the first; `finished` marks it done too. `id` keeps the pulse apart from
/// other lines.
pub fn timeline(
    id: &str,
    labels: &[&str],
    reached: Option<usize>,
    finished: bool,
    tone: Tone,
) -> Div {
    let color = tone.color();
    let reach = |i: usize| match reached {
        Some(r) if i < r || (i == r && finished) => Reach::Done,
        Some(r) if i == r => Reach::Current,
        _ => Reach::Ahead,
    };
    let done = |i: usize| reach(i) == Reach::Done;
    let dot = |i: usize| step_dot(format!("{id}-{i}"), color, reach(i));
    let count = labels.len();
    if count > CROWDED {
        let mut line = div().flex().items_center();
        let mut row = div().flex().text_sm().text_color(color);
        for (i, label) in labels.iter().enumerate() {
            let last = i + 1 == count;
            line = line.child(dot(i));
            if !last {
                line = line.child(stretch(color, done(i)).mx_1());
            }
            let named = reach(i) == Reach::Current
                || (finished && reached == Some(i))
                || (reached.is_none() && i == 0);
            // The name hangs off an anchor as wide as its dot, centred on
            // it; the first and the last stay inside the line's ends.
            let text = div()
                .flex_none()
                .whitespace_nowrap()
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
        return div().flex().flex_col().gap_2().child(line).child(row);
    }
    // Each step is a column as wide as its name, its dot centred over it
    // between halves of the line; the rest of the line runs between the
    // columns, level with the dots.
    let mut line = div().flex().items_start().text_size(px(13.));
    for (i, label) in labels.iter().enumerate() {
        let last = i + 1 == count;
        let lit = reach(i) != Reach::Ahead;
        let step = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap_3p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(if i == 0 {
                        div().flex_1()
                    } else {
                        stretch(color, done(i - 1)).mr_1()
                    })
                    .child(dot(i))
                    .child(if last {
                        div().flex_1()
                    } else {
                        stretch(color, done(i)).ml_1()
                    }),
            )
            .child(
                div()
                    .flex()
                    .justify_center()
                    .whitespace_nowrap()
                    .text_color(if lit { tone.text() } else { Theme::muted() })
                    .child(label.to_string()),
            );
        line = line.child(step);
        if !last {
            line = line.child(stretch(color, done(i)).mt(px(5.)));
        }
    }
    line
}

/// How a [`step_line`] draws its steps.
#[derive(Debug, Clone, Copy)]
pub struct LineLook {
    /// How wide a dot is.
    pub dot: f32,
    /// How far the current dot's halo reaches past it.
    pub halo: f32,
    /// The colour of the steps ahead: their rings and the dashes between.
    pub pending: Hsla,
    /// Whether the step the line is at is named under its dot.
    pub named: bool,
}

/// The dot of a [`step_line`]'s step, as [`step_dot`] draws one but at the
/// size, halo and pending colour of `look`.
fn line_dot(key: String, color: Hsla, reach: Reach, look: LineLook) -> AnyElement {
    let dot = div().flex_none().size(px(look.dot)).rounded_full();
    match reach {
        Reach::Done => return dot.bg(color).into_any_element(),
        Reach::Ahead => {
            return dot.border_2().border_color(look.pending).into_any_element();
        }
        Reach::Current => {}
    }
    let haloed = dot
        .relative()
        .child(
            div()
                .absolute()
                .top(px(-look.halo))
                .left(px(-look.halo))
                .size(px(look.dot + 2. * look.halo))
                .rounded_full()
                .bg(color.opacity(0.25)),
        )
        .child(div().absolute().inset_0().rounded_full().bg(color));
    if crate::preview::active() {
        return haloed.into_any_element();
    }
    haloed
        .with_animation(
            SharedString::from(key),
            Animation::new(Duration::from_millis(1400))
                .repeat()
                .with_easing(pulsating_between(0.4, 1.0)),
            Styled::opacity,
        )
        .into_any_element()
}

/// Steps in a line over its whole width in the colour of `tone`: done ones
/// solid, the current one haloed and pulsing, the ones ahead rings joined
/// by dashes, all as `look` draws them. Beside words that already say which
/// step it is at, such as a set's row, it names none; otherwise it names
/// the step it is at under its dot. `reached` is the current step, `None`
/// before the first; `finished` marks it done too. `id` keeps the pulse
/// apart from other lines.
pub fn step_line(
    id: &str,
    labels: &[&str],
    reached: Option<usize>,
    finished: bool,
    tone: Tone,
    look: LineLook,
) -> Div {
    let color = tone.color();
    let reach = |i: usize| match reached {
        Some(r) if i < r || (i == r && finished) => Reach::Done,
        Some(r) if i == r => Reach::Current,
        _ => Reach::Ahead,
    };
    let count = labels.len();
    let mut line = div().flex().items_center();
    let mut captions = div().flex().h(px(18.)).text_sm().text_color(color);
    for (i, label) in labels.iter().enumerate() {
        let last = i + 1 == count;
        line = line.child(line_dot(format!("{id}-{i}"), color, reach(i), look));
        if !last {
            let between = div().flex_1().mx_1().h(px(2.));
            line = line.child(if reach(i) == Reach::Done {
                between.bg(color)
            } else {
                between.child(dashes(look.pending))
            });
        }
        let named = reach(i) == Reach::Current
            || (finished && reached == Some(i))
            || (reached.is_none() && i == 0);
        // The name hangs off an anchor as wide as its dot, centred on it;
        // the first and the last stay inside the line's ends.
        let text = div()
            .flex_none()
            .whitespace_nowrap()
            .when(named, |t| t.child(label.to_string()));
        let anchor = div().flex().flex_none().w(px(look.dot));
        captions = captions.child(match i {
            0 => anchor.justify_start().child(text),
            _ if last => anchor.justify_end().child(text),
            _ => anchor.justify_center().child(text),
        });
        if !last {
            captions = captions.child(div().flex_1().mx_1());
        }
    }
    if !look.named {
        return line;
    }
    div().flex().flex_col().gap_2().child(line).child(captions)
}

/// A checkbox as the design draws it, the way a browser does: a white box
/// with a grey edge and a browser's margins around it, filled with the
/// accent and checked when on; faded while it cannot change. `label` goes
/// after it, inside the part that toggles. It takes focus and answers
/// Space; `on_change` gets the value a click asks for, which the caller
/// shows back.
pub fn checkbox(
    id: impl Into<ElementId>,
    checked: bool,
    enabled: bool,
    label: impl IntoElement,
    on_change: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> gpui_kit::base::Checkbox {
    let mark = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .mt(px(3.))
        .mb(px(3.))
        .ml(px(4.))
        .mr(px(3.))
        .rounded(px(3.))
        .border_1()
        .map(|mark| {
            if checked {
                mark.bg(Theme::accent()).border_color(Theme::accent())
            } else {
                mark.bg(rgb(0xff_ff_ff)).border_color(rgb(0x76_76_76))
            }
        })
        .when(!enabled, |mark| mark.opacity(0.5))
        .children(checked.then(|| {
            KitIcon::new(IconName::Check)
                .size(px(14.))
                .text_color(Theme::on_accent())
        }));
    gpui_kit::base::Checkbox::new(id)
        .checked(checked)
        .disabled(!enabled)
        .on_change(move |state, _, window, cx| {
            on_change(
                &(state == gpui_kit::base::CheckboxState::Checked),
                window,
                cx,
            );
        })
        .flex()
        .items_center()
        .gap(px(10.))
        .when(enabled, Styled::cursor_pointer)
        .focus_visible(|s| {
            s.shadow(vec![
                BoxShadow::new(px(0.), px(0.), Theme::accent().opacity(0.6)).spread_radius(px(2.)),
            ])
        })
        .child(mark)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_sizes_grow_with_their_place() {
        let heights: Vec<f32> = [
            Size::Tiny,
            Size::Row,
            Size::Banner,
            Size::Small,
            Size::Medium,
            Size::Toolbar,
            Size::Large,
            Size::Dialog,
            Size::Hero,
        ]
        .iter()
        .map(|s| s.metrics().height)
        .collect();
        assert_eq!(heights, [24., 28., 28., 30., 32., 34., 36., 40., 46.]);
        assert_eq!(Size::default(), Size::Medium);
    }
}
