//! The welcome tour on a first start: what a sandbox is, making the first
//! one, and pressing Play on it. Every step can skip the rest; the tour
//! does not come back once skipped or done, unless asked for in settings.

use gpui::{
    AnyElement, Context, Div, Entity, FontWeight, Hsla, SharedString, div, linear_color_stop,
    linear_gradient, prelude::*, px, relative, rgb,
};
use gpui_kit::component::ActiveTheme as _;
use gse_protocol::valid_name;

use crate::icons::Icon;
use crate::text_input::{Look, TextInput};
use crate::ui::{self, Kind, Size, Theme, Tone, button};
use crate::{Home, Page};

/// Where the tour is.
pub enum Tour {
    /// What a sandbox is.
    Welcome,
    /// Making the first sandbox: the set of apps picked and its name.
    FirstSandbox {
        preset: Option<String>,
        name: Entity<TextInput>,
    },
    /// The first sandbox exists; Play is pointed out on its page.
    PressPlay,
}

/// How far the box of the tour's last step stands from Play: below its
/// bottom edge, and in from its right one.
const CALLOUT_BELOW: f32 = 56.;
const CALLOUT_IN: f32 = 52.;

/// The narrowest the welcome's story gets beside its picture.
const STORY_MIN: f32 = 420.;

/// The width of a space between the words of the last step's text: Inter's
/// space, 0.24 em, at its 14 px.
const SPACE: f32 = 3.35;

/// What the tour says a set the service ships is for; nothing for any
/// other set.
fn set_pitch(preset: &str) -> Option<&'static str> {
    match preset {
        "launchers" => {
            Some("Everything to install and run Windows games: stores and tools to run them.")
        }
        "desktop" => Some(
            "Just the desktop and a browser. For one program you bring yourself through the Inbox.",
        ),
        _ => None,
    }
}

/// Where the tour is among its three steps: the step it is at as a pill,
/// the others as dots.
fn step_dots(at: u8) -> Div {
    let mut dots = div().flex().flex_none().items_center().gap_1p5();
    for step in 1..=3 {
        let here = step == at;
        dots = dots.child(
            div()
                .h(px(6.))
                .w(px(if here { 22. } else { 6. }))
                .rounded(px(3.))
                .bg(if here {
                    Theme::accent()
                } else {
                    Theme::ahead()
                }),
        );
    }
    dots
}

/// One point the welcome makes: its icon on a tinted tile, a title, and a
/// line or two under it.
fn welcome_point(shown: Icon, title: &'static str, text: &'static str) -> Div {
    div()
        .flex()
        .gap_3p5()
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(36.))
                .rounded(px(10.))
                .bg(Tone::Accent.ground())
                .child(
                    ui::kit_icon(shown)
                        .size(px(18.))
                        .text_color(Theme::accent()),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_base()
                        .line_height(ui::normal_line(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(3.))
                        .line_height(relative(1.5))
                        .text_color(Theme::secondary_text())
                        .child(text),
                ),
        )
}

/// The title of a set's card in the tour, with a check at its end once the
/// set is `picked`.
fn card_title(title: String, picked: bool) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_lg()
                .line_height(ui::normal_line(18.))
                .font_weight(FontWeight::BOLD)
                .child(title),
        )
        .when(picked, |row| {
            row.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(22.))
                    .rounded_full()
                    .bg(Theme::accent())
                    .child(
                        ui::kit_icon(Icon::Check)
                            .size(px(13.))
                            .text_color(Theme::on_accent()),
                    ),
            )
        })
}

/// The first five of a set's apps, and how many more there are.
fn app_chips(apps: &[String]) -> Div {
    let chip = |label: String| {
        ui::tag(label)
            .py(px(4.))
            .line_height(ui::normal_line(12.))
            .text_color(Theme::text())
    };
    let mut chips = div().flex().flex_wrap().gap_1p5();
    for app in apps.iter().take(5) {
        chips = chips.child(chip(app.clone()));
    }
    if apps.len() > 5 {
        chips = chips.child(chip(format!("+{}", apps.len() - 5)));
    }
    chips
}

/// A key of the keyboard among the words of the last step's text.
fn text_key(label: &'static str) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(5.))
        .bg(Theme::callout_key())
        .text_size(px(13.))
        .line_height(ui::normal_line(13.))
        .child(label)
}

/// What the last step says, word by word so its keys stand in the line
/// like words: GPUI lays no boxes inside a run of text.
fn press_play_text() -> Div {
    let words = |text: &'static str| text.split(' ').map(|word| div().child(word));
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(SPACE))
        .line_height(relative(1.55))
        .text_color(Theme::body_text())
        .children(words(
            "Your screen goes over to the sandbox's desktop. To come back, press",
        ))
        .child(text_key("Ctrl"))
        .child(text_key("Alt"))
        .child(div().flex().items_center().child(text_key("G")).child("."))
        .children(words(
            "Games that grab the mouse need the same keys to let it go.",
        ))
}

/// The window under the tour's last step: dimmed in `color`, and taking
/// every click that does not go to what it points at. It lies over the
/// whole of the window inside its frame, wherever its owner stands.
fn tour_shade(color: Hsla) -> impl IntoElement {
    gpui::canvas(
        |_, window, _| {
            // The frame's shadow, then its border wherever there is one.
            let padding = gpui_kit::component::window_paddings(window);
            let inset = |side: gpui::Pixels| {
                if side > px(0.) { side + px(1.) } else { side }
            };
            let (left, top) = (inset(padding.left), inset(padding.top));
            let (right, bottom) = (inset(padding.right), inset(padding.bottom));
            let viewport = window.viewport_size();
            let inside = gpui::Bounds::new(
                gpui::point(left, top),
                gpui::size(
                    viewport.width - left - right,
                    viewport.height - top - bottom,
                ),
            );
            window.insert_hitbox(inside, gpui::HitboxBehavior::BlockMouse);
            inside
        },
        move |_, inside, window, _| {
            let corners = gpui_kit::component::window_corner_radius(window);
            window.paint_quad(gpui::fill(inside, color).corner_radii(corners));
        },
    )
    .absolute()
}

impl Home {
    /// Starts the tour from its first step.
    pub(crate) fn start_tour(&mut self, cx: &mut Context<Self>) {
        self.tour = Some(Tour::Welcome);
        self.page = Page::Sandbox;
        cx.notify();
    }

    /// Ends the tour for good.
    pub(crate) fn end_tour(&mut self, cx: &mut Context<Self>) {
        self.tour = None;
        if !self.app_config.tour_done {
            self.app_config.tour_done = true;
            if let Err(e) = self.app_config.save() {
                self.say(format!("The setting was not saved: {e:#}"), true);
            }
        }
        cx.notify();
    }

    fn tour_to_first_sandbox(&mut self, cx: &mut Context<Self>) {
        let presets = self.presets_list().unwrap_or_default();
        let preset = presets
            .iter()
            .find(|p| self.images.contains(p))
            .or_else(|| presets.first())
            .cloned();
        // A name no sandbox has yet: games, then games-2 and on.
        let suggested = std::iter::once("games".to_owned())
            .chain((2..=self.environments.len() + 1).map(|n| format!("games-{n}")))
            .find(|n| !self.environments.iter().any(|e| e.name == *n))
            .unwrap_or_default();
        let name = Self::first_name_field(suggested, cx);
        self.tour = Some(Tour::FirstSandbox { preset, name });
        cx.notify();
    }

    /// The field that names the first sandbox, holding `text`.
    pub(crate) fn first_name_field(text: String, cx: &mut Context<Self>) -> Entity<TextInput> {
        let name = cx.new(|cx| {
            let mut input = TextInput::new("games", cx).look(Look::PageField);
            input.set_text(text, cx);
            input
        });
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        name
    }

    /// The bar of a tour page: the app's name and the way out.
    fn tour_bar(window: &gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        Self::window_bar(
            Some(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_between()
                    .pl_4()
                    .pr_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .child(crate::shell::mark(26.))
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("GSE"),
                            ),
                    )
                    .child(
                        crate::windowbar::not_dragging().child(
                            button(
                                "skip-tour",
                                "Skip the tour",
                                Kind::Ghost,
                                true,
                                cx.listener(|home, _, _, cx| home.end_tour(cx)),
                            )
                            .h(px(33.))
                            .px_3()
                            .text_size(px(14.))
                            .text_color(Theme::secondary_text()),
                        ),
                    )
                    .into_any_element(),
            ),
            window,
            cx,
        )
    }

    fn step_label(step: u8) -> Div {
        div()
            .text_size(px(13.))
            .line_height(ui::normal_line(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(Theme::accent())
            .child(format!("STEP {step} OF 3"))
    }

    /// A sandbox's desktop, drawn small: its launchers' shortcuts, a
    /// launcher's window and the panel at the bottom, over the colours of a
    /// cover, which stay dark in either look.
    fn sandbox_desktop() -> Div {
        let faint = |alpha: f32| gpui::white().opacity(alpha);
        let shortcut = || {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(div().size(px(22.)).rounded(px(6.)).bg(faint(0.22)))
                .child(div().w(px(26.)).h(px(3.)).rounded_full().bg(faint(0.25)))
        };
        let text_line = |width: f32| div().w(px(width)).h(px(4.)).rounded_full().bg(faint(0.18));
        div()
            .relative()
            .overflow_hidden()
            .rounded(px(12.))
            .border_2()
            .border_color(Theme::accent())
            .bg(linear_gradient(
                135.,
                linear_color_stop(rgb(0x24_31_4f), 0.),
                linear_color_stop(rgb(0x3b_2a_4c), 1.),
            ))
            .child(
                div()
                    .absolute()
                    .left(px(14.))
                    .top(px(14.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(shortcut())
                    .child(shortcut())
                    .child(shortcut()),
            )
            .child(
                div()
                    .absolute()
                    .left(px(84.))
                    .top(px(24.))
                    .w(px(196.))
                    .h(px(110.))
                    .flex()
                    .flex_col()
                    .rounded(px(6.))
                    .overflow_hidden()
                    .bg(gpui::black().opacity(0.3))
                    .border_1()
                    .border_color(faint(0.1))
                    .child(div().h(px(14.)).bg(faint(0.08)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p_3()
                            .child(text_line(96.))
                            .child(text_line(140.))
                            .child(text_line(120.))
                            .child(
                                div()
                                    .mt_1()
                                    .w(px(52.))
                                    .h(px(14.))
                                    .rounded(px(4.))
                                    .bg(Theme::accent().opacity(0.7)),
                            ),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .bg(gpui::black().opacity(0.35))
                    .child(
                        div()
                            .size(px(12.))
                            .rounded(px(3.))
                            .bg(Theme::accent().opacity(0.8)),
                    )
                    .child(div().size(px(12.)).rounded(px(3.)).bg(faint(0.3)))
                    .child(div().size(px(12.)).rounded(px(3.)).bg(faint(0.3)))
                    .child(div().flex_1())
                    .child(div().w(px(28.)).h(px(4.)).rounded_full().bg(faint(0.3))),
            )
    }

    /// The app's window with a sandbox's desktop in it, and Play under
    /// them, beside the welcome.
    fn welcome_picture() -> Div {
        let play = Theme::accent().opacity(0.8);
        // The window and the desktop are as large as the board's with their
        // borders, which lie outside the sizes the board gives them.
        div()
            .relative()
            .flex_none()
            .size(px(460.))
            .child(
                div()
                    .absolute()
                    .left(px(30.))
                    .top(px(60.))
                    .w(px(402.))
                    .h(px(252.))
                    .rounded(px(14.))
                    .bg(Theme::surface())
                    .border_1()
                    .border_color(Theme::outline()),
            )
            .child(
                Self::sandbox_desktop()
                    .absolute()
                    .left(px(70.))
                    .top(px(110.))
                    .w(px(324.))
                    .h(px(194.)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(150.))
                    .top(px(340.))
                    .w(px(160.))
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded(px(22.))
                    .border_2()
                    .border_dashed()
                    .border_color(Theme::accent().opacity(0.6))
                    .text_base()
                    .text_color(play)
                    .font_weight(FontWeight::BOLD)
                    .child(ui::kit_icon(Icon::Play).size(px(16.)).text_color(play))
                    .child("Play"),
            )
    }

    fn welcome_page(cx: &mut Context<Self>) -> Div {
        // The story takes what the picture leaves, and the picture goes
        // under it once that is less than its own width.
        let story = div()
            .flex()
            .flex_col()
            .flex_grow_1()
            .flex_shrink_1()
            .flex_basis(px(STORY_MIN))
            .min_w_0()
            .gap_7()
            .child(Self::step_label(1))
            .child(
                div()
                    .text_size(px(40.))
                    .line_height(relative(1.15))
                    .font_weight(FontWeight::BOLD)
                    .child("Play what you don't trust, without risking your computer"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .child(welcome_point(
                        Icon::Monitor,
                        "A sandbox is a sealed computer",
                        "It has its own desktop and launchers and the whole graphics card, \
                         but cannot see your files or your home network.",
                    ))
                    .child(welcome_point(
                        Icon::Reset,
                        "Reset wipes it clean",
                        "Anything a game left behind is gone in seconds, and the launchers \
                         are back as new.",
                    ))
                    .child(welcome_point(
                        Icon::Transfer,
                        "Files go through checks",
                        "Saves and mods move in and out through the Inbox and Outbox; \
                         programs never come out.",
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .mt_1()
                    .child(
                        button(
                            "tour-next",
                            "Next: make your first sandbox",
                            Kind::Primary,
                            true,
                            cx.listener(|home, _, _, cx| home.tour_to_first_sandbox(cx)),
                        )
                        .size(Size::Hero),
                    )
                    .child(step_dots(1)),
            );
        div()
            .flex()
            .flex_wrap()
            .min_h_full()
            .items_center()
            .gap_16()
            .px(px(96.))
            .pb_12()
            .child(story)
            .child(Self::welcome_picture())
    }

    fn first_sandbox_page(
        &self,
        chosen: Option<&str>,
        name: &Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut sets = div().flex().flex_wrap().gap_4().w(px(880.));
        let presets = self.presets_list().unwrap_or_default();
        if presets.is_empty() {
            sets = sets.child(
                div()
                    .text_size(px(13.))
                    .text_color(Theme::muted())
                    .child("There is no set of apps yet. Skip the tour and make one in Settings."),
            );
        }
        for preset in &presets {
            sets = sets.child(self.tour_set_card(preset, chosen == Some(preset.as_str()), cx));
        }
        let typed = name.read(cx).text().trim().to_owned();
        // An empty field only waits for a name; it is no mistake yet.
        let problem = if typed.is_empty() {
            None
        } else if !valid_name(&typed) {
            Some("Names are lowercase letters, digits and hyphens, up to 32.")
        } else if self.environments.iter().any(|e| e.name == typed) {
            Some("A sandbox of that name exists.")
        } else {
            None
        };
        let create = chosen
            .filter(|_| !typed.is_empty() && problem.is_none())
            .map(|p| (p.to_owned(), typed.clone()));
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_7()
            .pt_6()
            .pb_10()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .w(px(880.))
                    .child(Self::step_label(2))
                    .child(
                        div()
                            .text_size(px(34.))
                            .line_height(ui::normal_line(34.))
                            .font_weight(FontWeight::BOLD)
                            .child("What should your first sandbox hold?"),
                    )
                    .child(
                        div()
                            .text_size(px(15.))
                            .line_height(ui::normal_line(15.))
                            .text_color(Theme::secondary_text())
                            .child(
                        "Pick a starting set. You can make more sandboxes later, each from any set.",
                    )),
            )
            .child(sets)
            .child(Self::first_name_row(name, create, problem, cx))
    }

    /// The first sandbox's name and the button that makes it from the set
    /// picked, `create` once both are good; what is wrong with the name
    /// goes under them.
    fn first_name_row(
        name: &Entity<TextInput>,
        create: Option<(String, String)>,
        problem: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(880.))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .line_height(ui::normal_line(13.))
                                    .text_color(Theme::secondary_text())
                                    .child("Name"),
                            )
                            .child(name.clone()),
                    )
                    .child(
                        button(
                            "tour-create",
                            "Create sandbox",
                            Kind::Primary,
                            create.is_some(),
                            cx.listener(move |home, _, _, cx| {
                                if let Some((preset, name)) = &create {
                                    home.create_from_tour(preset, name, cx);
                                }
                            }),
                        )
                        .size(Size::Hero)
                        .h(px(44.)),
                    ),
            )
            .children(problem.map(|p| div().text_xs().text_color(Theme::warning()).child(p)))
    }

    /// A set of apps to start the first sandbox from: its title, what it is
    /// for, its first apps, and whether it is ready.
    fn tour_set_card(
        &self,
        preset: &str,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let apps: Vec<String> = self
            .preset_apps(preset)
            .unwrap_or_default()
            .iter()
            .map(|a| a.name.clone())
            .collect();
        // A set that builds shows its step, as its row in settings does.
        let steps = self.build_steps(preset);
        let state = match (self.set_state(preset), &steps) {
            (_, Some((labels, reached))) => (
                format!(
                    "Building \u{b7} {}",
                    labels.get(*reached).copied().unwrap_or_default()
                ),
                Theme::warning(),
            ),
            (("Ready", color), None) => ("Ready, starts right away".to_owned(), color),
            (("Not built yet", _), None) => (
                "Builds in about 5 minutes".to_owned(),
                Theme::secondary_text(),
            ),
            ((text, color), None) => (text.to_owned(), color),
        };
        let line = steps.map(|(labels, reached)| {
            ui::timeline(
                &format!("tour-build-{preset}"),
                &labels,
                Some(reached),
                false,
                Tone::Warning,
            )
        });
        let pick = preset.to_owned();
        div()
            .id(SharedString::from(format!("tour-set-{preset}")))
            .flex()
            .flex_col()
            .gap_3p5()
            .w(px(432.))
            .p_5()
            .rounded(px(16.))
            .border_2()
            .cursor_pointer()
            .map(|card| {
                if selected {
                    card.border_color(Theme::accent()).bg(Theme::picked())
                } else {
                    card.border_color(Theme::outline())
                        .bg(Theme::panel())
                        .hover(|c| c.bg(Theme::surface()))
                }
            })
            .on_click(cx.listener(move |home, _, _, cx| {
                if let Some(Tour::FirstSandbox { preset, .. }) = home.tour.as_mut() {
                    *preset = Some(pick.clone());
                    cx.notify();
                }
            }))
            .child(card_title(crate::presets::set_title(preset), selected))
            .children(set_pitch(preset).map(|pitch| {
                div()
                    .line_height(relative(1.5))
                    .text_color(Theme::secondary_text())
                    .child(pitch)
            }))
            .child(app_chips(&apps))
            .child(
                div()
                    .text_xs()
                    .line_height(ui::normal_line(12.))
                    .text_color(state.1)
                    .child(state.0),
            )
            .children(line)
    }

    fn create_from_tour(&mut self, preset: &str, name: &str, cx: &mut Context<Self>) {
        self.create(preset, name, cx);
        self.tour = Some(Tour::PressPlay);
        cx.notify();
    }

    /// The whole window while the tour is on one of its own pages.
    pub(crate) fn tour_page(&self, window: &gpui::Window, cx: &mut Context<Self>) -> Option<Div> {
        let page = match self.tour.as_ref()? {
            Tour::Welcome => Self::welcome_page(cx),
            Tour::FirstSandbox { preset, name } => {
                self.first_sandbox_page(preset.as_deref(), &name.clone(), cx)
            }
            Tour::PressPlay => return None,
        };
        Some(
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(Self::tour_bar(window, cx))
                .child(
                    // A window too small for the step scrolls it.
                    div()
                        .id("tour")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(page),
                ),
        )
    }

    /// The box of the tour's last step: what Play does and how to come
    /// back, with the way out of the tour and Play itself.
    fn press_play_box(name: &str, cx: &mut Context<Self>) -> Div {
        let play = name.to_owned();
        div()
            .flex()
            .flex_col()
            .gap_3()
            // The border is outside the width, as the board draws it.
            .w(px(420. + 22. * 2. + 2.))
            .p(px(22.))
            .rounded(px(16.))
            .bg(Theme::callout())
            .border_1()
            .border_color(Theme::callout_line())
            .child(Self::step_label(3))
            .child(
                div()
                    .text_xl()
                    .line_height(ui::normal_line(20.))
                    .font_weight(FontWeight::BOLD)
                    .child("Press Play to start it"),
            )
            .child(press_play_text())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt(px(6.))
                    .child(
                        div()
                            .id("tour-skip-last")
                            .text_color(Theme::secondary_text())
                            .cursor_pointer()
                            .hover(|l| l.text_color(Theme::text()))
                            .on_click(cx.listener(|home, _, _, cx| home.end_tour(cx)))
                            .child("Skip the tour"),
                    )
                    .child(
                        button(
                            "tour-play-now",
                            "Play now",
                            Kind::Primary,
                            true,
                            cx.listener(move |home, _, _, cx| home.play(&play, cx)),
                        )
                        .size(Size::Dialog)
                        .px(px(20.)),
                    ),
            )
    }

    /// Play on `name`'s cover as the tour's last step shows it: the window
    /// dimmed around it, a ring around it and the step's box under it,
    /// over everything else on the window. Play that cannot be pressed,
    /// and any Play once the tour is past this step, stays as it is.
    pub(crate) fn tour_spotlight(
        &self,
        name: &str,
        enabled: bool,
        play: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !enabled || !self.tour_points_at_play() || self.modal.is_some() {
            return play.into_any_element();
        }
        let shade =
            Hsla::from(rgb(0x05_06_0a)).opacity(if cx.theme().is_dark() { 0.72 } else { 0.35 });
        // The ring stands 6 px off Play, 6 px wide.
        let ring = div()
            .absolute()
            .top(px(-12.))
            .left(px(-12.))
            .right(px(-12.))
            .bottom(px(-12.))
            .rounded_full()
            .border(px(6.))
            .border_color(Theme::accent().opacity(0.35));
        let callout = Self::press_play_box(name, cx)
            .absolute()
            .top(relative(1.))
            .mt(px(CALLOUT_BELOW))
            .right(px(CALLOUT_IN));
        div()
            .relative()
            .child(gpui::deferred(tour_shade(shade)).with_priority(1))
            .child(
                gpui::deferred(div().relative().child(ring).child(play).child(callout))
                    .with_priority(2),
            )
            .into_any_element()
    }

    /// Whether Play on the cover is the one the tour points at.
    pub(crate) fn tour_points_at_play(&self) -> bool {
        matches!(self.tour, Some(Tour::PressPlay))
    }
}
