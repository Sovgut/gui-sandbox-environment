//! The frame of the window: the sandbox list on the left, full or as a rail
//! of covers, the title bar with the window's buttons, and the page of the
//! selected sandbox: its cover, its facts and the Activity, Programs and
//! Files tabs under them.

use gpui::{
    Background, Context, Div, FontWeight, Hsla, ScrollHandle, SharedString, Stateful, Window, div,
    linear_color_stop, linear_gradient, prelude::*, px, rgb,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, IconName, Sizable as _};
use gse_protocol::{Environment, Request, ScreenMode, Stage, SwitchMode};

use crate::icons::Icon;
use crate::settings::gpu_title;
use crate::ui::{self, Kind, Theme, button, icon_button};
use crate::{Home, Modal, Page, stage_text};

pub const SIDEBAR_WIDTH: f32 = 272.;
pub const RAIL_WIDTH: f32 = 64.;
/// A window shorter than this shows the cover as a strip.
const COMPACT_BELOW: f32 = 760.;
/// The tabs never get less than this; a shorter window scrolls the page.
const PANEL_MIN_HEIGHT: f32 = 300.;
/// The height of the title bar, which the sidebar's header lines up with.
pub const BAR_HEIGHT: f32 = 52.;

/// The tabs under a sandbox's cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelTab {
    Activity,
    Programs,
    Files,
}

/// The steps of a start, in order, with the names the cover shows.
const STEPS: [(Stage, &str); 4] = [
    (Stage::Preparing, "Prepare"),
    (Stage::SwitchingGpu, "Hand over the graphics card"),
    (Stage::Booting, "Boot"),
    (Stage::Ready, "Desktop"),
];

/// A sandbox's cover: two colours picked from its name, so each keeps its
/// own look in the list and on its page.
pub fn cover(name: &str) -> Background {
    const PAIRS: [(u32, u32); 6] = [
        (0x2b_3a_5e, 0x4a_2f_55),
        (0x3d_2e_22, 0x24_32_3a),
        (0x23_3a_33, 0x2b_3a_5e),
        (0x3a_2d_2a, 0x4a_2f_55),
        (0x24_31_4f, 0x20_32_3c),
        (0x2a_25_50, 0x1f_3a_44),
    ];
    let hash = name.bytes().fold(0_usize, |h, b| {
        h.wrapping_mul(31).wrapping_add(usize::from(b))
    });
    let (from, to) = PAIRS[hash % PAIRS.len()];
    linear_gradient(
        135.,
        linear_color_stop(rgb(from), 0.),
        linear_color_stop(rgb(to), 1.),
    )
}

/// When a sandbox last played, as the list says it at `now`.
fn played(last: Option<u64>, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::TimeZone as _;
    let Some(when) = last
        .and_then(|t| i64::try_from(t).ok())
        .and_then(|t| chrono::Local.timestamp_opt(t, 0).single())
    else {
        return "Never played".to_owned();
    };
    let ago = now - when;
    let days = (now.date_naive() - when.date_naive()).num_days();
    if ago < chrono::Duration::minutes(1) {
        "Played just now".to_owned()
    } else if ago < chrono::Duration::hours(1) {
        format!("Played {} min ago", ago.num_minutes())
    } else if days == 0 {
        format!("Played {} h ago", ago.num_hours())
    } else if days == 1 {
        "Played yesterday".to_owned()
    } else if days < 7 {
        format!("Played {days} days ago")
    } else {
        format!("Played {}", when.format("%-d %b"))
    }
}

/// The shade laid over a cover under its words: dark whatever the palette,
/// as the covers are.
fn cover_shade() -> Hsla {
    rgb(0x0e_10_15).into()
}

/// The app's mark: a monitor on an accent square.
pub(crate) fn mark(side: f32) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(side))
        .rounded(px(side * 0.27))
        .bg(Theme::accent())
        .child(
            ui::kit_icon(Icon::Monitor)
                .size(px(side * 0.6))
                .text_color(Theme::on_accent()),
        )
}

/// Leaves full screen, drawn like the app's [`mark`] it stands in for at the
/// top of the sidebar while the window is full screen.
fn leave_full_screen(id: &'static str, side: f32) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(side))
        .rounded(px(side * 0.27))
        .bg(Theme::accent())
        .cursor_pointer()
        .hover(|b| b.opacity(0.85))
        .tooltip(ui::tooltip("Leave full screen (Ctrl Alt F)".into()))
        .on_click(|_, window, _| window.toggle_fullscreen())
        .child(
            ui::kit_icon(Icon::ExitFullscreen)
                .size(px(side * 0.55))
                .text_color(Theme::on_accent()),
        )
}

/// How a [`pill`] looks: the accent, or see-through white over a cover.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PillKind {
    Accent,
    Glass,
}

/// A round button of the cover, `height` tall: Play's size on the full
/// cover, smaller on the strip.
fn pill(
    id: &'static str,
    label: impl Into<SharedString>,
    icon: Option<Icon>,
    kind: PillKind,
    height: f32,
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let (bg, fg) = match kind {
        PillKind::Accent => (Theme::accent(), Theme::on_accent()),
        PillKind::Glass => (gpui::white().opacity(0.12), Theme::text()),
    };
    let large = height >= 48.;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(height))
        .px(px(if large { 24. } else { 16. }))
        .rounded(px(height / 2.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(if large { 16. } else { 14. }))
        .font_weight(if large {
            FontWeight::BOLD
        } else {
            FontWeight::SEMIBOLD
        })
        .children(icon.map(|i| {
            ui::kit_icon(i)
                .size(px(if large { 18. } else { 15. }))
                .text_color(fg)
        }))
        .child(label.into())
        .map(|b| {
            if enabled {
                b.cursor_pointer()
                    .hover(move |b| b.bg(bg.opacity(bg.a * 0.85 + 0.05)))
                    .on_click(on_click)
            } else {
                b.opacity(0.5)
            }
        })
}

/// What a sandbox shows on a monitor it takes over: the monitor's preferred
/// mode, when the service could read it.
pub(crate) fn monitor_text(gpu: &gse_protocol::Gpu) -> String {
    let mode = gpu.monitor_mode.as_deref().and_then(|mode| {
        let (size, rate) = mode.split_once('@')?;
        let (width, height) = size.split_once('x')?;
        Some(format!("{width} x {height} @ {rate} FPS"))
    });
    mode.unwrap_or_else(|| "The monitor's own".to_owned())
}

/// The screen mode as the facts row says it.
fn mode_text(mode: &str) -> String {
    mode.parse::<ScreenMode>().map_or_else(
        |_| mode.to_owned(),
        |m| format!("{} x {} @ {} FPS", m.width, m.height, m.rate),
    )
}

/// What the cover of a sandbox shows, decided once per render.
enum CoverState {
    /// Stopped and ready to play.
    Stopped,
    /// Its preset's image is building; `steps` and the one reached.
    Waiting(Vec<&'static str>, usize),
    /// Its preset's image is not built and nothing builds it.
    NotBuilt,
    /// Starting or stopping, at `stage` when the service said so.
    Changing(Option<Stage>),
    /// Up, with its screen in the app or on a monitor of its own.
    Running { screen: bool },
}

impl Home {
    /// The screen a sandbox on `gpu` gets: the screen mode in the window, or
    /// the monitor's own when that GPU drives the monitor and the sandbox
    /// takes it over.
    pub(crate) fn screen_text(&self, gpu: &str, mode: &str) -> String {
        let single = self.settings.as_ref().ok().and_then(|(_, gpus)| {
            gpus.iter()
                .find(|g| g.address == gpu && g.mode == gse_protocol::SwitchMode::Single)
        });
        match single {
            Some(gpu) => monitor_text(gpu),
            None => mode_text(mode),
        }
    }

    /// The width the sidebar takes now.
    fn rail(&self) -> bool {
        self.sidebar_collapsed
    }

    /// A sandbox's state under its name in the list.
    fn row_status(&self, environment: &Environment) -> (String, Hsla) {
        if let Some(running) = &environment.running {
            return match running.stage {
                Stage::Ready => ("Running".into(), Theme::good()),
                stage => (stage_text(stage).into(), Theme::warning()),
            };
        }
        if let Some(doing) = self.busy.get(&environment.name) {
            return ((*doing).into(), Theme::accent());
        }
        if self.building(&environment.image) {
            return match self.build_percent(&environment.image) {
                Some(percent) => (format!("Preparing, {percent}%"), Theme::warning()),
                None => ("Preparing".into(), Theme::warning()),
            };
        }
        if !environment.image_ready {
            return ("Its apps are not built".into(), Theme::subtle());
        }
        (
            played(environment.last_started, chrono::Local::now()),
            Theme::muted(),
        )
    }

    fn sidebar_row(&self, environment: &Environment, cx: &mut Context<Self>) -> impl IntoElement {
        let name = environment.name.clone();
        let selected =
            self.page == Page::Sandbox && self.selected.as_deref() == Some(name.as_str());
        let (status, color) = self.row_status(environment);
        div()
            .id(SharedString::from(format!("row-{name}")))
            .flex()
            .items_center()
            .gap_3()
            .p_2()
            .rounded(px(10.))
            .cursor_pointer()
            .map(|row| {
                if selected {
                    row.bg(Theme::selected())
                } else {
                    row.hover(|s| s.bg(Theme::raised()))
                }
            })
            .on_click(cx.listener(move |home, _, window, cx| home.select(&name, window, cx)))
            .child(if self.building(&environment.image) {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(px(64.))
                    .h(px(36.))
                    .rounded(px(6.))
                    .bg(Theme::raised())
                    .child(
                        gpui_kit::component::Icon::new(IconName::LoaderCircle)
                            .size(px(16.))
                            .text_color(Theme::warning()),
                    )
            } else {
                div()
                    .flex_none()
                    .w(px(64.))
                    .h(px(36.))
                    .rounded(px(6.))
                    .bg(cover(&environment.name))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .child(environment.name.clone()),
                    )
                    .child(div().truncate().text_xs().text_color(color).child(status)),
            )
    }

    fn sidebar_full(&self, full_screen: bool, cx: &mut Context<Self>) -> Div {
        let sort = self.sort;
        let mut list = div()
            .id("sandboxes")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_0p5()
            .px_2()
            .overflow_y_scroll();
        let quiet =
            |text: &'static str| div().p_3().text_sm().text_color(Theme::muted()).child(text);
        let listed = self.listed(cx);
        if self.connected.is_none() {
            list = list.child(quiet("Loading..."));
        } else if self.environments.is_empty() && self.connected == Some(true) {
            list = list.child(quiet("No sandboxes yet."));
        } else if listed.is_empty() && !self.environments.is_empty() {
            list = list.child(quiet("No sandbox matches."));
        }
        for environment in listed {
            list = list.child(self.sidebar_row(environment, cx));
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(Theme::sidebar())
            .rounded_tl(self.corners.top_left)
            .rounded_bl(self.corners.bottom_left)
            .border_r_1()
            .border_color(Theme::border())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2p5()
                    .h(px(BAR_HEIGHT))
                    .pl_4()
                    .pr_2()
                    .map(|bar| {
                        if full_screen {
                            bar.child(leave_full_screen("sidebar-leave-full", 26.))
                        } else {
                            bar.child(mark(26.))
                        }
                    })
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("GSE"),
                    )
                    .child(
                        Button::new("collapse-sidebar")
                            .ghost()
                            .small()
                            .icon(IconName::PanelLeftClose)
                            .tooltip("Collapse the sidebar")
                            .on_click(cx.listener(|home, _, _, cx| {
                                home.sidebar_collapsed = true;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_2()
                    .px_3()
                    .pt_1()
                    .pb_2p5()
                    .child(div().flex_1().min_w_0().child(self.sandbox_search.clone()))
                    .child(icon_button(
                        "sort",
                        sort.icon(),
                        format!("Sorted {}; click for {}", sort.text(), sort.next().text()),
                        Kind::Secondary,
                        true,
                        cx.listener(move |home, _, _, cx| {
                            home.sort = sort.next();
                            cx.notify();
                        }),
                    )),
            )
            .child(list)
            .child(self.sidebar_footer(cx))
    }

    /// New sandbox and the way into settings, under the list.
    fn sidebar_footer(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_none()
            .gap_2()
            .p_3()
            .border_t_1()
            .border_color(Theme::border())
            .child(
                button(
                    "add-sandbox",
                    "New sandbox",
                    Kind::Primary,
                    self.connected == Some(true),
                    cx.listener(|home, _, _, cx| home.open_add(cx)),
                )
                .icon(IconName::Plus)
                .flex_1(),
            )
            .child(icon_button(
                "app-settings",
                Icon::Settings,
                "Settings",
                if self.page == Page::AppSettings {
                    Kind::Primary
                } else {
                    Kind::Secondary
                },
                true,
                cx.listener(|home, _, window, cx| home.open_app_settings(window, cx)),
            ))
    }

    /// The collapsed sidebar; in full screen its top leaves full screen in
    /// place of the app's mark.
    fn sidebar_rail(&self, full_screen: bool, cx: &mut Context<Self>) -> Div {
        let top = if full_screen {
            div()
                .mb_2()
                .child(leave_full_screen("rail-leave-full", 44.))
        } else {
            div().mb_2().child(mark(30.))
        };
        let mut rail = div()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap_2()
            .w(px(RAIL_WIDTH))
            .h_full()
            .py_3()
            .bg(Theme::sidebar())
            .rounded_tl(self.corners.top_left)
            .rounded_bl(self.corners.bottom_left)
            .border_r_1()
            .border_color(Theme::border())
            .child(top);
        let mut covers = div()
            .id("rail")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .gap_2()
            .overflow_y_scroll();
        for environment in self.listed(cx) {
            let name = environment.name.clone();
            let selected =
                self.page == Page::Sandbox && self.selected.as_deref() == Some(name.as_str());
            let running = environment.running.is_some();
            covers = covers.child(
                div()
                    .id(SharedString::from(format!("rail-{name}")))
                    .relative()
                    .flex_none()
                    .size(px(44.))
                    .rounded(px(10.))
                    .bg(cover(&name))
                    .cursor_pointer()
                    .when(selected, |c| c.border_2().border_color(Theme::accent()))
                    .tooltip(ui::tooltip(name.clone().into()))
                    .on_click(cx.listener(move |home, _, window, cx| {
                        home.select(&name, window, cx);
                    }))
                    .when(running, |c| {
                        c.child(
                            div()
                                .absolute()
                                .right(px(-3.))
                                .bottom(px(-3.))
                                .size(px(12.))
                                .rounded_full()
                                .bg(Theme::good())
                                .border_2()
                                .border_color(Theme::sidebar()),
                        )
                    }),
            );
        }
        rail = rail.child(covers);
        rail.child(
            Button::new("expand-sidebar")
                .ghost()
                .icon(IconName::PanelLeftOpen)
                .tooltip("Expand the sidebar")
                .on_click(cx.listener(|home, _, _, cx| {
                    home.sidebar_collapsed = false;
                    cx.notify();
                })),
        )
    }

    pub(crate) fn sidebar(&self, full_screen: bool, cx: &mut Context<Self>) -> Div {
        if self.rail() {
            self.sidebar_rail(full_screen, cx)
        } else {
            self.sidebar_full(full_screen, cx)
        }
    }

    /// The selected sandbox, when there is one.
    pub(crate) fn chosen(&self) -> Option<&Environment> {
        let name = self.selected.as_ref()?;
        self.environments.iter().find(|e| e.name == *name)
    }

    fn cover_state(&self, environment: &Environment) -> CoverState {
        let doing = self.busy.get(&environment.name).copied();
        if let Some(running) = &environment.running {
            return match running.stage {
                Stage::Ready => CoverState::Running {
                    screen: running.screen.is_some(),
                },
                stage => CoverState::Changing(Some(stage)),
            };
        }
        if doing == Some("Starting") {
            return CoverState::Changing(None);
        }
        // Nothing starts while its set builds, even when an older build of
        // the set is still there.
        if let Some((steps, reached)) = self.build_steps(&environment.image) {
            return CoverState::Waiting(steps, reached);
        }
        if !environment.image_ready {
            return CoverState::NotBuilt;
        }
        CoverState::Stopped
    }

    /// The names of `preset`'s first apps, as the cover lists them.
    fn apps_summary(&self, preset: &str) -> Option<String> {
        let apps = self.preset_apps(preset)?;
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        Some(match names.len() {
            0 => "No apps".to_owned(),
            1..=3 => names.join(", "),
            n => format!("{} and {} more", names[..3].join(", "), n - 3),
        })
    }

    /// The round Play button of the cover.
    fn play_button(
        &self,
        environment: &Environment,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = environment.name.clone();
        div()
            .id("play")
            .flex()
            .flex_none()
            .items_center()
            .gap_2p5()
            .h(px(52.))
            .px_7()
            .rounded_full()
            .text_lg()
            .font_weight(FontWeight::BOLD)
            .child(
                gpui_kit::component::Icon::new(IconName::Play)
                    .size(px(18.))
                    .text_color(if enabled {
                        Theme::on_accent()
                    } else {
                        Theme::subtle()
                    }),
            )
            .child("Play")
            .map(|b| {
                if enabled && self.tour_points_at_play() {
                    b.border_4().border_color(gpui::white().opacity(0.35))
                } else {
                    b
                }
            })
            .map(|b| {
                if enabled {
                    b.bg(Theme::accent())
                        .text_color(Theme::on_accent())
                        .cursor_pointer()
                        .hover(|b| b.bg(Theme::accent().opacity(0.88)))
                        .on_click(cx.listener(move |home, _, _, cx| home.play(&name, cx)))
                } else {
                    b.bg(Theme::selected()).text_color(Theme::subtle())
                }
            })
    }

    /// The name of a sandbox with its set over it, `on_image` white over a
    /// cover, in the page's colours over a plain one.
    fn cover_title(&self, environment: &Environment, size: f32, on_image: bool) -> Div {
        div()
            .text_color(if on_image {
                gpui::white()
            } else {
                Theme::text()
            })
            .flex()
            .flex_col()
            .gap_1p5()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(0xc4_ca_d6))
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded(px(6.))
                            .bg(gpui::white().opacity(0.12))
                            .child(environment.image.clone()),
                    )
                    .children(self.apps_summary(&environment.image)),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(size))
                    .font_weight(FontWeight::BOLD)
                    .child(environment.name.clone()),
            )
    }

    /// The cover of the selected sandbox, as large as the page allows.
    fn cover_view(&self, environment: &Environment, cx: &mut Context<Self>) -> Div {
        let frame = div()
            .relative()
            .flex_none()
            .h(px(330.))
            .rounded(px(16.))
            .overflow_hidden();
        match self.cover_state(environment) {
            CoverState::Stopped => self.cover_still(frame, environment, None, cx),
            CoverState::Running { screen } => {
                self.cover_still(frame, environment, Some(screen), cx)
            }
            CoverState::Changing(stage) => Self::cover_changing(frame, environment, stage, cx),
            CoverState::Waiting(steps, reached) => {
                self.cover_waiting(frame, environment, &steps, reached, cx)
            }
            CoverState::NotBuilt => self.cover_not_built(frame, environment, cx),
        }
    }

    /// The cover of a sandbox that is stopped, or running with `screen` in
    /// the app or not.
    fn cover_still(
        &self,
        frame: Div,
        environment: &Environment,
        screen: Option<bool>,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = environment.name.clone();
        let actions = if let Some(screen) = screen {
            let stop = format!("{name}/stop");
            let stopping = self.busy.contains_key(&stop);
            let show = name.clone();
            div()
                .flex()
                .gap_2()
                .when(screen, |a| {
                    a.child(pill(
                        "show-screen",
                        "Show screen",
                        Some(Icon::Monitor),
                        PillKind::Accent,
                        52.,
                        true,
                        cx.listener(move |home, _, window, cx| {
                            home.open_screen(&show, window, cx);
                        }),
                    ))
                })
                .when(!screen, |a| {
                    a.child(ui::badge("On its own monitor", Theme::good()))
                })
                .child(pill(
                    "stop",
                    if stopping { "Stopping..." } else { "Stop" },
                    None,
                    PillKind::Glass,
                    52.,
                    !stopping,
                    cx.listener(move |home, _, _, cx| {
                        home.request(stop.clone(), "Stopping", Request::Stop, cx);
                    }),
                ))
                .into_any_element()
        } else {
            let free = !self.busy.contains_key(&name) && self.presets.build.is_none();
            self.play_button(environment, free, cx).into_any_element()
        };
        frame.bg(cover(&name)).child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_end()
                .justify_between()
                .gap_4()
                .p_7()
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(cover_shade().opacity(0.), 0.),
                    linear_color_stop(cover_shade().opacity(0.9), 1.),
                ))
                .child(self.cover_title(environment, 34., true))
                .child(actions),
        )
    }

    /// The cover of a sandbox starting or stopping, at `stage`.
    fn cover_changing(
        frame: Div,
        environment: &Environment,
        stage: Option<Stage>,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = environment.name.clone();
        let stopping = stage == Some(Stage::Stopping);
        let reached = match stage {
            None | Some(Stage::Preparing) => 0,
            Some(Stage::SwitchingGpu) => 1,
            Some(Stage::Booting) => 2,
            Some(Stage::Ready | Stage::Stopping) => 3,
        };
        let labels: Vec<&str> = STEPS.iter().map(|(_, l)| *l).collect();
        let stop = format!("{name}/stop");
        frame.bg(cover(&name)).child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .justify_between()
                .px_10()
                .py_9()
                .bg(cover_shade().opacity(0.7))
                .text_color(gpui::white())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(34.))
                                .font_weight(FontWeight::BOLD)
                                .child(name.clone()),
                        )
                        .when(!stopping, |row| {
                            row.child(pill(
                                "cancel-start",
                                "Cancel",
                                None,
                                PillKind::Glass,
                                40.,
                                true,
                                cx.listener(move |home, _, _, cx| {
                                    home.open_when_ready = None;
                                    home.request(stop.clone(), "Stopping", Request::Stop, cx);
                                }),
                            ))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3p5()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                            if stopping {
                                "Stopping".to_owned()
                            } else {
                                match stage {
                                    Some(Stage::SwitchingGpu) => {
                                        "Handing over the graphics card".to_owned()
                                    }
                                    Some(Stage::Booting) => {
                                        "Booting the sandbox's system".to_owned()
                                    }
                                    _ => "Preparing".to_owned(),
                                }
                            },
                        ))
                        .when(!stopping, |col| {
                            col.child(ui::timeline(
                                &format!("start-{name}"),
                                &labels,
                                Some(reached),
                                false,
                                Theme::accent(),
                            ))
                            .child(
                                div().text_sm().text_color(Theme::muted()).child(
                                    "Usually under a minute. The screen opens here by \
                                 itself when the desktop is up.",
                                ),
                            )
                        }),
                ),
        )
    }

    /// The cover of a sandbox whose set of apps is building.
    fn cover_waiting(
        &self,
        frame: Div,
        environment: &Environment,
        steps: &[&'static str],
        reached: usize,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = environment.name.clone();
        frame
            .bg(Theme::surface())
            .flex()
            .flex_col()
            .justify_between()
            .px_10()
            .py_9()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(self.cover_title(environment, 34., false))
                    .child(self.play_button(environment, false, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3p5()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!(
                                "Installing its apps: {}",
                                steps.get(reached).copied().unwrap_or_default()
                            )),
                    )
                    .child(ui::timeline(
                        &format!("wait-{name}"),
                        steps,
                        Some(reached),
                        false,
                        Theme::warning(),
                    ))
                    .child(div().text_sm().text_color(Theme::muted()).child(format!(
                        "The set \"{}\" is being built; Play turns on when it is ready. \
                         You can close the app, the build goes on.",
                        environment.image
                    ))),
            )
    }

    /// The cover of a sandbox whose set of apps is not built.
    fn cover_not_built(
        &self,
        frame: Div,
        environment: &Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let preset = environment.image.clone();
        let can_build = !self.any_running() && self.presets.build.is_none();
        frame
            .bg(Theme::surface())
            .flex()
            .flex_col()
            .justify_between()
            .px_10()
            .py_9()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(self.cover_title(environment, 34., false))
                    .child(self.play_button(environment, false, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Its apps are not built yet"),
                            )
                            .child(div().text_sm().text_color(Theme::muted()).child(format!(
                                "Build the set \"{preset}\" once; every sandbox \
                                     made from it can play then."
                            ))),
                    )
                    .child(button(
                        "sandbox-build",
                        "Build its apps",
                        Kind::Primary,
                        can_build,
                        cx.listener(move |home, _, _, cx| home.build_image(&preset, cx)),
                    )),
            )
    }

    /// The cover cut down to a strip, while a tab below needs the room.
    fn cover_strip(&self, environment: &Environment, cx: &mut Context<Self>) -> Div {
        let (status, color) = self.row_status(environment);
        let name = environment.name.clone();
        let up = environment
            .running
            .as_ref()
            .filter(|r| r.stage == Stage::Ready);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3p5()
            .h(px(76.))
            .pl_6()
            .pr_4()
            .rounded(px(16.))
            .bg(cover(&name))
            .text_color(gpui::white())
            .child(
                div()
                    .text_size(px(22.))
                    .font_weight(FontWeight::BOLD)
                    .child(name.clone()),
            )
            .child(ui::badge(status, color))
            .child(div().flex_1())
            .when_some(up, |strip, running| {
                let stop = format!("{name}/stop");
                let show = name.clone();
                strip
                    .when(running.screen.is_some(), |s| {
                        s.child(pill(
                            "strip-show",
                            "Show screen",
                            None,
                            PillKind::Accent,
                            40.,
                            true,
                            cx.listener(move |home, _, window, cx| {
                                home.open_screen(&show, window, cx);
                            }),
                        ))
                    })
                    .child(pill(
                        "strip-stop",
                        "Stop",
                        None,
                        PillKind::Glass,
                        40.,
                        true,
                        cx.listener(move |home, _, _, cx| {
                            home.request(stop.clone(), "Stopping", Request::Stop, cx);
                        }),
                    ))
            })
    }

    /// Internet, graphics, screen and the way into the sandbox's settings.
    fn facts(&self, environment: &Environment, cx: &mut Context<Self>) -> Div {
        let fact = |label: &'static str, value: String| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w_0()
                .child(div().text_xs().text_color(Theme::muted()).child(label))
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(value),
                )
        };
        let tile = || {
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_between()
                .gap_2()
                .px_3p5()
                .py_3()
                .rounded(px(12.))
                .bg(Theme::surface())
        };
        let gpu = match &self.settings {
            Ok((_, gpus)) => gpus
                .iter()
                .find(|g| g.address == environment.settings.gpu)
                .map_or_else(|| "None".to_owned(), gpu_title),
            Err(_) => "...".to_owned(),
        };
        let on = environment.settings.internet;
        let name = environment.name.clone();
        let settings_of = environment.name.clone();
        div()
            .flex()
            .flex_none()
            .gap_3()
            .child(
                tile()
                    .child(fact(
                        "Internet",
                        if on { "Allowed" } else { "Cut off" }.into(),
                    ))
                    .child(
                        Switch::new("internet-switch")
                            .checked(on)
                            .disabled(self.saving)
                            .on_change(cx.listener(move |home, on: &bool, _, cx| {
                                home.set_internet(&name, *on, cx);
                            })),
                    ),
            )
            .child(tile().child(fact("Graphics", gpu)))
            .child(tile().child(fact(
                "Screen",
                self.screen_text(&environment.settings.gpu, &environment.settings.screen_mode),
            )))
            .child(
                tile()
                    .id("open-sandbox-settings")
                    .cursor_pointer()
                    .hover(|t| t.bg(Theme::raised()))
                    .on_click(cx.listener(move |home, _, _, cx| {
                        home.open_sandbox_settings(&settings_of, cx);
                    }))
                    .child(fact("Sandbox", "Settings".into()))
                    .child(
                        gpui_kit::component::Icon::new(IconName::ChevronRight)
                            .size_4()
                            .text_color(Theme::muted()),
                    ),
            )
    }

    /// The tabs and what the chosen one shows.
    fn panel(&self, environment: &Environment, cx: &mut Context<Self>) -> Div {
        let up = environment
            .running
            .as_ref()
            .is_some_and(|r| r.stage == Stage::Ready);
        let programs = match self.processes.as_ref() {
            Some((n, Ok(apps))) if *n == environment.name && up => {
                format!("Programs · {}", apps.len())
            }
            _ => "Programs".to_owned(),
        };
        let tab = |id: &'static str, label: String, tab: PanelTab, cx: &mut Context<Self>| {
            ui::segment(
                id,
                label,
                self.tab == tab,
                true,
                cx.listener(move |home, _, _, cx| {
                    // Files open as a panel at the right, over the page.
                    if tab == PanelTab::Files {
                        match home.selected_up() {
                            Some(name) => home.open_transfer(&name, cx),
                            None => home.say("Files move in and out while the sandbox runs", false),
                        }
                        cx.notify();
                        return;
                    }
                    home.tab = tab;
                    cx.notify();
                }),
            )
        };
        let quiet = |text: &'static str| {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(Theme::muted())
                .child(text)
        };
        let body = match self.tab {
            PanelTab::Activity => self.activity_body(cx),
            PanelTab::Programs if up => self.programs_body(&environment.name, cx),
            PanelTab::Programs => quiet("Programs show here while the sandbox runs."),
            PanelTab::Files => quiet("Files open in the panel at the right."),
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(PANEL_MIN_HEIGHT))
            .rounded(px(14.))
            .bg(Theme::panel())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_1()
                    .px_3()
                    .pt_2p5()
                    .child(tab(
                        "tab-activity",
                        "Activity".into(),
                        PanelTab::Activity,
                        cx,
                    ))
                    .child(tab("tab-programs", programs, PanelTab::Programs, cx))
                    .child(tab("tab-files", "Files".into(), PanelTab::Files, cx)),
            )
            .child(body)
    }

    /// The scroll position of the chosen tab's list.
    fn tab_scroll(&self) -> &ScrollHandle {
        match self.tab {
            PanelTab::Activity => &self.journal_scroll,
            PanelTab::Programs => &self.processes_scroll,
            PanelTab::Files => &self.files_scroll,
        }
    }

    /// The page of the selected sandbox.
    pub(crate) fn sandbox_page(&self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        // A window too short for everything scrolls the page; the tabs keep
        // a usable height of their own.
        let page = div()
            .id("sandbox-page")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_4()
            .px_8()
            .pb_6()
            .children(self.notices(cx));
        let Some(environment) = self.chosen() else {
            return page.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                        if self.connected.is_none() {
                            "Loading..."
                        } else {
                            "No sandbox yet"
                        },
                    ))
                    .when(self.connected == Some(true), |empty| {
                        empty.child(
                            div()
                                .text_sm()
                                .text_color(Theme::muted())
                                .child("Make one with New sandbox on the left."),
                        )
                    }),
            );
        };
        // The cover gives its room to a list scrolled away from its top, or
        // to a window too short to show it whole, and takes it back once
        // the list is at the top again.
        let short = window.viewport_size().height < px(COMPACT_BELOW);
        let compact = short || self.tab_scroll().offset().y < px(0.);
        if compact {
            page.child(self.cover_strip(environment, cx))
                .child(self.panel(environment, cx))
        } else {
            page.child(self.cover_view(environment, cx))
                .children(self.tour_hint(cx))
                .child(self.facts(environment, cx))
                .child(self.panel(environment, cx))
        }
    }

    /// Plays `name`: asks first when it would stop another sandbox or take
    /// the monitor away.
    pub(crate) fn play(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.tour_points_at_play() {
            self.end_tour(cx);
        }
        if let Some(other) = self.environments.iter().find(|e| e.running.is_some()) {
            self.modal = Some(Modal::Switch {
                from: other.name.clone(),
                to: name.to_owned(),
            });
            cx.notify();
            return;
        }
        let single = self
            .environments
            .iter()
            .find(|e| e.name == name)
            .is_some_and(|e| {
                self.settings.as_ref().is_ok_and(|(_, gpus)| {
                    gpus.iter()
                        .any(|g| g.address == e.settings.gpu && g.mode == SwitchMode::Single)
                })
            });
        if single && !self.app_config.single_gpu_explained {
            self.modal = Some(Modal::SingleGpu {
                name: name.to_owned(),
            });
            cx.notify();
            return;
        }
        self.start(name, cx);
    }

    /// Lets `name` reach the internet or cuts it off, at once when it runs.
    pub(crate) fn set_internet(&mut self, name: &str, on: bool, cx: &mut Context<Self>) {
        let Some(environment) = self.environments.iter_mut().find(|e| e.name == name) else {
            return;
        };
        environment.settings.internet = on;
        let request = Request::SetEnvironmentSettings {
            name: name.to_owned(),
            settings: environment.settings.clone(),
        };
        self.request(format!("{name}/internet"), "", request, cx);
    }
}

/// Copies `text` to the clipboard.
pub fn copy(text: String, cx: &mut gpui::App) {
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[test]
    fn the_last_play_reads_as_a_time_ago() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 11, 1, 18, 0, 0)
            .unwrap();
        let at = |h: i64| u64::try_from((now - chrono::Duration::hours(h)).timestamp()).ok();
        assert_eq!(played(None, now), "Never played");
        assert_eq!(played(at(2), now), "Played 2 h ago");
        assert_eq!(played(at(24), now), "Played yesterday");
        assert_eq!(played(at(72), now), "Played 3 days ago");
    }
}
