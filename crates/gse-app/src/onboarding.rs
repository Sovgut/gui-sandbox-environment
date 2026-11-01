//! The welcome tour on a first start: what a sandbox is, making the first
//! one, and pressing Play on it. Every step can skip the rest; the tour
//! does not come back once skipped or done, unless asked for in settings.

use gpui::{Context, Div, Entity, FontWeight, SharedString, div, prelude::*, px};
use gpui_kit::component::Sizable as _;
use gse_protocol::valid_name;

use crate::icons::Icon;
use crate::shell::cover;
use crate::text_input::TextInput;
use crate::ui::{self, Kind, Theme, button};
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
        let name = cx.new(|cx| {
            let mut input = TextInput::new("games", cx);
            input.set_text(suggested, cx);
            input
        });
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        self.tour = Some(Tour::FirstSandbox { preset, name });
        cx.notify();
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
                    .pr_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .child(crate::shell::mark(26.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("GSE")),
                    )
                    .child(crate::windowbar::not_dragging().child(button(
                        "skip-tour",
                        "Skip the tour",
                        Kind::Ghost,
                        true,
                        cx.listener(|home, _, _, cx| home.end_tour(cx)),
                    )))
                    .pl_4()
                    .into_any_element(),
            ),
            window,
            cx,
        )
    }

    fn step_label(step: u8) -> Div {
        div()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(Theme::accent())
            .child(format!("STEP {step} OF 3"))
    }

    /// A sandbox's screen with Play on it, beside the welcome.
    fn welcome_picture() -> Div {
        div()
            .relative()
            .flex_none()
            .w(px(420.))
            .h(px(300.))
            .rounded(px(18.))
            .bg(cover("demo"))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(gpui::white().opacity(0.35))
                    .child("[sandbox desktop]"),
            )
            .child(
                div()
                    .absolute()
                    .right(px(20.))
                    .bottom(px(20.))
                    .px_5()
                    .py_2p5()
                    .rounded_full()
                    .border_2()
                    .border_dashed()
                    .border_color(gpui::white().opacity(0.5))
                    .font_weight(FontWeight::BOLD)
                    .child("Play"),
            )
    }

    fn welcome_page(cx: &mut Context<Self>) -> Div {
        let point = |shown: Icon, title: &'static str, text: &'static str| {
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
                        .bg(Theme::selected())
                        .child(ui::icon(shown, Theme::accent())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_sm().text_color(Theme::muted()).child(text)),
                )
        };
        let story = div()
            .flex()
            .flex_col()
            .gap_7()
            .w(px(520.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(Self::step_label(1))
                    .child(
                        div()
                            .text_size(px(34.))
                            .font_weight(FontWeight::BOLD)
                            .line_height(px(40.))
                            .child("Play what you don't trust, without risking your computer"),
                    ),
            )
            .child(point(
                Icon::Monitor,
                "A sandbox is a sealed computer",
                "It has its own desktop and launchers and the whole graphics card, but cannot \
                 see your files or your home network.",
            ))
            .child(point(
                Icon::Reset,
                "Reset wipes it clean",
                "Anything a game left behind is gone in seconds, and the launchers are back as new.",
            ))
            .child(point(
                Icon::Transfer,
                "Files go through checks",
                "Saves and mods move in and out through the Inbox and Outbox; programs never \
                 come out.",
            ))
            .child(
                div().flex().child(
                    button(
                        "tour-next",
                        "Next: make your first sandbox",
                        Kind::Primary,
                        true,
                        cx.listener(|home, _, _, cx| home.tour_to_first_sandbox(cx)),
                    )
                    .large(),
                ),
            );
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_16()
            .pb_10()
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
                    .text_sm()
                    .text_color(Theme::muted())
                    .child("There is no set of apps yet. Skip the tour and make one in Settings."),
            );
        }
        for preset in &presets {
            sets = sets.child(self.tour_set_card(preset, chosen == Some(preset.as_str()), cx));
        }
        let typed = name.read(cx).text().trim().to_owned();
        let problem = if !valid_name(&typed) {
            Some("Names are lowercase letters, digits and hyphens, up to 32.")
        } else if self.environments.iter().any(|e| e.name == typed) {
            Some("A sandbox of that name exists.")
        } else {
            None
        };
        let create = chosen
            .filter(|_| problem.is_none())
            .map(|p| (p.to_owned(), typed.clone()));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .gap_7()
            .pt_6()
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
                            .font_weight(FontWeight::BOLD)
                            .child("What should your first sandbox hold?"),
                    )
                    .child(div().text_color(Theme::muted()).child(
                        "Pick a starting set. You can make more sandboxes later, each from any set.",
                    )),
            )
            .child(sets)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w(px(880.))
                    .child(div().text_sm().text_color(Theme::muted()).child("Name"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(div().flex_1().min_w_0().child(name.clone()))
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
                                .large(),
                            ),
                    )
                    .children(
                        problem.map(|p| div().text_xs().text_color(Theme::warning()).child(p)),
                    ),
            )
    }

    /// A set of apps to start the first sandbox from.
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
        let state = match self.set_state(preset) {
            ("Ready", color) => ("Ready, starts right away", color),
            ("Not built yet", color) => ("Builds first, in some minutes", color),
            other => other,
        };
        let pick = preset.to_owned();
        let mut chips = div().flex().flex_wrap().gap_1p5();
        for app in apps.iter().take(5) {
            chips = chips.child(ui::tag(app.clone()));
        }
        if apps.len() > 5 {
            chips = chips.child(ui::tag(format!("+{}", apps.len() - 5)));
        }
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
                    card.border_color(Theme::accent()).bg(Theme::chosen())
                } else {
                    card.border_color(Theme::border())
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
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child(preset.to_owned()),
            )
            .child(chips)
            .child(div().text_xs().text_color(state.1).child(state.0))
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
                .child(page),
        )
    }

    /// The last step, under the first sandbox's cover: press Play.
    pub(crate) fn tour_hint(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !matches!(self.tour, Some(Tour::PressPlay)) {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_5()
                .px_5()
                .py_4()
                .rounded(px(14.))
                .bg(Theme::chosen())
                .border_1()
                .border_color(Theme::accent().opacity(0.5))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .gap_1()
                        .child(Self::step_label(3))
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .child("Press Play to start it"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_sm()
                                .text_color(Theme::muted())
                                .child("The screen opens here. Games that hold the mouse let it go with")
                                .child(ui::key("Ctrl Alt G")),
                        ),
                )
                .child(button(
                    "tour-done",
                    "Got it",
                    Kind::Secondary,
                    true,
                    cx.listener(|home, _, _, cx| home.end_tour(cx)),
                )),
        )
    }

    /// Whether Play on the cover is the one the tour points at.
    pub(crate) fn tour_points_at_play(&self) -> bool {
        matches!(self.tour, Some(Tour::PressPlay))
    }
}
