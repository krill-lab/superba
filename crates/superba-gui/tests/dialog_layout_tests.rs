#![allow(missing_docs)]

//! Dialogs stay inside the window (UI spec rule 8, user bug 103): turning the
//! router off on Windows opened the full-restart confirmation with one line
//! per model the relay lists, taller than the window, without scrolling, its
//! buttons off screen. At 960x640 and 1280x800 the card now ends inside the
//! window, the title and the buttons stay in view and can be clicked, and the
//! body between them scrolls.

mod support;

use std::time::Duration;

use egui::{Event, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, Rect, TouchPhase, pos2};
use superba_core::{
    AuthMode, FullRestartReason, Locale, LocalizedText, Notice, PlanDestination, RestartKind,
    SwitchIntent, SwitchPlan, SwitchRequest, Timestamp,
};
use superba_gui::design::widgets::{
    Button, DIALOG_WINDOW_MARGIN, Text, bounded, dialog, dialog_body,
};
use superba_gui::design::{fonts, style, tokens::Palette, tokens::space};
use superba_gui::flow::{Flow, Stage};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Cx, Data};
use superba_gui::runtime::Command;
use superba_gui::shell::{self, BannerState};

const SIZES: [[f32; 2]; 2] = [[960.0, 640.0], [1280.0, 800.0]];

/// A text drawn in a frame: where, and the clip rectangle it was drawn in.
#[derive(Clone, Debug)]
struct Drawn {
    text: String,
    rect: Rect,
    clip: Rect,
}

impl Drawn {
    /// Fully visible: inside its clip rectangle and the window.
    fn visible(&self, screen: Rect) -> bool {
        let shown = self.clip.intersect(screen);
        shown.expand(0.5).contains_rect(self.rect)
    }
}

/// A headless window that advances its clock 1/60 s per frame (smooth
/// scrolling needs time to pass).
struct Window {
    ctx: egui::Context,
    screen: Rect,
    time: std::cell::Cell<f64>,
}

impl Window {
    fn new(size: [f32; 2]) -> Self {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
        style::install(&ctx, Some(egui::Theme::Light));
        Self {
            ctx,
            screen: Rect::from_min_max(pos2(0.0, 0.0), pos2(size[0], size[1])),
            time: std::cell::Cell::new(0.0),
        }
    }

    fn frame(&self, events: Vec<Event>, f: &mut dyn FnMut(&egui::Context)) -> Vec<Drawn> {
        self.time.set(self.time.get() + 1.0 / 60.0);
        let output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(self.screen),
                time: Some(self.time.get()),
                events,
                ..Default::default()
            },
            |ui| f(ui.ctx()),
        );
        let drawn = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(Drawn {
                    text: t.galley.text().to_owned(),
                    rect: t.galley.rect.translate(t.pos.to_vec2()),
                    clip: s.clip_rect,
                }),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        drawn
    }

    fn settle(&self, f: &mut dyn FnMut(&egui::Context)) -> Vec<Drawn> {
        for _ in 0..3 {
            self.frame(Vec::new(), f);
        }
        self.frame(Vec::new(), f)
    }

    fn click(&self, pos: Pos2, f: &mut dyn FnMut(&egui::Context)) {
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(vec![Event::PointerMoved(pos), button(true)], f);
        self.frame(vec![button(false)], f);
        self.frame(Vec::new(), f);
    }

    /// Turn the wheel over `pos` (negative `dy`: towards the end) and let the
    /// smooth scroll run out.
    fn wheel(&self, pos: Pos2, dy: f32, f: &mut dyn FnMut(&egui::Context)) {
        self.frame(
            vec![
                Event::PointerMoved(pos),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, dy),
                    phase: TouchPhase::Move,
                    source: MouseWheelSource::Unknown,
                    modifiers: Default::default(),
                },
            ],
            f,
        );
        for _ in 0..90 {
            self.frame(Vec::new(), f);
        }
    }
}

fn find<'a>(drawn: &'a [Drawn], text: &str) -> &'a Drawn {
    drawn
        .iter()
        .find(|d| d.text == text)
        .unwrap_or_else(|| panic!("{text:?} not drawn"))
}

/// Whether a text starting with `prefix` is fully visible (widgets scrolled
/// out of view are not painted at all).
fn shown(drawn: &[Drawn], prefix: &str, screen: Rect) -> bool {
    drawn
        .iter()
        .any(|d| d.text.starts_with(prefix) && d.visible(screen))
}

fn find_prefix<'a>(drawn: &'a [Drawn], prefix: &str) -> &'a Drawn {
    drawn
        .iter()
        .find(|d| d.text.starts_with(prefix))
        .unwrap_or_else(|| panic!("{prefix:?}… not drawn"))
}

/// 60 notices like the router-off plan of user bug 103, made so that the
/// body cannot fit: 26 distinct warnings (no two of one kind), 2 blocking
/// notices, 30 model notes and 2 other notes.
fn notices() -> Vec<Notice> {
    let mut out = Vec::new();
    for i in 1..=26 {
        out.push(Notice::warning(LocalizedText::new(
            format!("test.warning_{i}"),
            format!("Warning {i}: something about this switch needs attention"),
        )));
    }
    for i in 1..=2 {
        out.push(Notice::danger(LocalizedText::new(
            format!("test.danger_{i}"),
            format!("Blocking {i}: this has to be dealt with first"),
        )));
    }
    for i in 0..30 {
        out.push(Notice::info(
            LocalizedText::new("models_note.generic_template", "{model}: generic")
                .arg("model", format!("gpt-test-{i:02}")),
        ));
    }
    out.push(Notice::info(
        LocalizedText::new(
            "switch.models.catalog_written",
            "Model catalog file: {file}",
        )
        .arg("file", "models-c53dd0cb.json"),
    ));
    out.push(Notice::info(LocalizedText::new(
        "switch.settings.no_catalog",
        "No settings catalog is available.",
    )));
    assert_eq!(out.len(), 60);
    out
}

fn router_off_plan() -> SwitchPlan {
    SwitchPlan {
        id: "router-off".into(),
        created_at: Timestamp::EPOCH,
        request: SwitchRequest {
            intent: SwitchIntent::Router { enable: false },
            ..SwitchRequest::profile("intranet".into())
        },
        from: None,
        to: PlanDestination {
            profile_id: Some("intranet".into()),
            site_id: Some("intranet".into()),
            line_id: None,
            auth_mode: Some(AuthMode::ApiKey),
            label: LocalizedText::new("test.site", "Intranet"),
        },
        primary: RestartKind::FullAppRestart {
            reason: FullRestartReason::NoCdp,
        },
        restarts: vec![],
        config: None,
        env: None,
        certificate_env: None,
        migration: None,
        remotes: vec![],
        active_threads: vec![],
        model_check: None,
        choices: vec![],
        alternatives: vec![],
        warnings: notices(),
        cleanup: vec![],
    }
}

fn confirming(plan: SwitchPlan) -> Flow {
    let mut flow = Flow::new(5);
    flow.request(plan.request.clone(), Duration::ZERO);
    let commands = flow.take_commands();
    let [Command::Plan { seq, .. }] = commands.as_slice() else {
        panic!("one plan command");
    };
    flow.on_planned(*seq, Ok(plan), Duration::ZERO);
    assert!(matches!(flow.stage(), Stage::ConfirmFull { .. }));
    flow
}

/// Draw the confirmation of `flow` in `locale`.
fn confirm_ui(flow: &mut Flow, locale: Locale) -> Box<dyn FnMut(&egui::Context) + '_> {
    let tr = Tr::new(locale);
    let data = Data::default();
    let fonts = fonts::FontReport::default();
    let service = support::idle_service();
    let mut banner = BannerState::default();
    Box::new(move |ctx| {
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(ctx),
            data: &data,
            busy: false,
            now: Timestamp::EPOCH,
            fonts: &fonts,
            service: &service,
            log: &[],
        };
        shell::confirm_full(ctx, &cx, flow, &mut banner, Duration::from_secs(1));
    })
}

struct Copy {
    title: String,
    full: String,
    cancel: String,
    notes: String,
}

fn copy(locale: Locale) -> Copy {
    let tr = Tr::new(locale);
    Copy {
        title: tr.t("confirm.title"),
        full: tr.t("confirm.full_restart"),
        cancel: tr.t("action.cancel"),
        notes: tr.f("notice.info_show", &[("n", &32)]),
    }
}

/// The card ends inside the window, the title and the buttons are fully
/// visible above the bottom margin.
fn assert_pinned(drawn: &[Drawn], screen: Rect, c: &Copy, what: &str) {
    for text in [&c.title, &c.full, &c.cancel] {
        let d = find(drawn, text);
        assert!(d.visible(screen), "{what}: {text:?} hidden at {:?}", d.rect);
        assert!(
            d.rect.bottom() <= screen.bottom() - DIALOG_WINDOW_MARGIN,
            "{what}: {text:?} below the margin at {:?}",
            d.rect
        );
        assert!(
            d.rect.top() >= DIALOG_WINDOW_MARGIN,
            "{what}: {text:?} above the margin at {:?}",
            d.rect
        );
    }
}

#[test]
fn sixty_notices_keep_the_confirmation_inside_the_window_and_its_body_scrolls() {
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let what = format!("{size:?} {locale:?}");
            let c = copy(locale);
            let w = Window::new(size);
            let mut flow = confirming(router_off_plan());
            let mut ui = confirm_ui(&mut flow, locale);
            let drawn = w.settle(&mut ui);
            assert_pinned(&drawn, w.screen, &c, &what);
            // Blocking first, then the warnings in planner order; the notes
            // are one collapsed row with their count.
            let danger = find_prefix(&drawn, "Blocking 1");
            let first = find_prefix(&drawn, "Warning 1:");
            assert!(danger.rect.top() < first.rect.top(), "{what}");
            assert!(danger.visible(w.screen), "{what}");
            assert!(
                !shown(&drawn, "Warning 26:", w.screen),
                "{what}: the body should overflow"
            );
            assert!(!shown(&drawn, &c.notes, w.screen), "{what}");
            assert!(
                !drawn.iter().any(|d| d.text.contains("gpt-test-")),
                "{what}: notes are collapsed"
            );
            let buttons = (find(&drawn, &c.full).rect, find(&drawn, &c.cancel).rect);

            // Wheel over the body: the end comes into view, the title and
            // the buttons stay where they were.
            w.wheel(first.rect.center(), -4000.0, &mut ui);
            let drawn = w.settle(&mut ui);
            assert_pinned(&drawn, w.screen, &c, &what);
            assert!(shown(&drawn, "Warning 26:", w.screen), "{what}");
            assert!(shown(&drawn, &c.notes, w.screen), "{what}");
            assert!(!shown(&drawn, "Blocking 1", w.screen), "{what}");
            assert_eq!(
                (find(&drawn, &c.full).rect, find(&drawn, &c.cancel).rect),
                buttons,
                "{what}: the buttons moved"
            );

            // The buttons answer clicks.
            w.click(buttons.1.center(), &mut ui);
            drop(ui);
            assert!(matches!(flow.stage(), Stage::Idle), "{what}: cancelled");

            let mut flow = confirming(router_off_plan());
            let mut ui = confirm_ui(&mut flow, locale);
            let drawn = w.settle(&mut ui);
            w.click(find(&drawn, &c.full).rect.center(), &mut ui);
            drop(ui);
            assert!(
                flow.take_commands()
                    .iter()
                    .any(|cmd| matches!(cmd, Command::Execute { .. })),
                "{what}: full restart starts"
            );
        }
    }
}

#[test]
fn opened_notes_and_model_group_stay_inside_the_window() {
    for size in SIZES {
        let what = format!("{size:?}");
        let c = copy(Locale::En);
        let w = Window::new(size);
        let mut flow = confirming(router_off_plan());
        let mut ui = confirm_ui(&mut flow, Locale::En);
        let drawn = w.settle(&mut ui);
        let body = find_prefix(&drawn, "Warning 1:").rect.center();
        // Scroll to the "32 notes" row and open it.
        w.wheel(body, -4000.0, &mut ui);
        let drawn = w.settle(&mut ui);
        w.click(find(&drawn, &c.notes).rect.center(), &mut ui);
        w.wheel(body, -4000.0, &mut ui);
        let drawn = w.settle(&mut ui);
        assert_pinned(&drawn, w.screen, &c, &what);
        // 30 model notes are one row with a count and an expander.
        let group = find_prefix(&drawn, "30 models are not in the built-in catalog");
        assert!(group.visible(w.screen), "{what}");
        assert!(
            !drawn.iter().any(|d| d.text.contains("gpt-test-")),
            "{what}"
        );
        assert!(
            find_prefix(&drawn, "Model catalog file").visible(w.screen),
            "{what}"
        );
        w.click(find(&drawn, "Show all").rect.center(), &mut ui);
        w.wheel(body, -4000.0, &mut ui);
        let drawn = w.settle(&mut ui);
        assert_pinned(&drawn, w.screen, &c, &what);
        let models = find_prefix(&drawn, "gpt-test-00");
        assert!(
            models.text.contains("gpt-test-29"),
            "{what}: {}",
            models.text
        );
        assert!(find(&drawn, "Hide").visible(w.screen), "{what}");
        drop(ui);
    }
}

/// The widget itself: a long body and a footer whose buttons wrap onto
/// several rows at the minimum width. The footer is measured, so the card
/// still ends inside the window and every button stays clickable.
#[test]
fn a_wrapping_footer_is_measured_and_stays_visible() {
    for size in SIZES {
        let w = Window::new(size);
        let mut clicked = None;
        let labels: Vec<String> = (1..=12)
            .map(|i| format!("Action number {i} of the footer"))
            .collect();
        let mut ui = |ctx: &egui::Context| {
            let pal = Palette::of(ctx);
            dialog(ctx, egui::Id::unique("wrapping-footer"), 760.0, |ui| {
                ui.label(Text::h2("A dialog title", pal));
                dialog_body(ui, |ui| {
                    for i in 0..80 {
                        ui.label(Text::body(format!("Body line {i}"), pal));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for (i, l) in labels.iter().enumerate() {
                        if ui.add(Button::new(l.clone())).clicked() {
                            clicked = Some(i);
                        }
                    }
                });
            });
        };
        let drawn = w.settle(&mut ui);
        let last = find(&drawn, "Action number 12 of the footer");
        let first = find(&drawn, "Action number 1 of the footer");
        assert!(
            last.rect.top() > first.rect.bottom() + 2.0 * first.rect.height(),
            "{size:?}: the footer wraps onto three rows or more"
        );
        // The card fills the window exactly to its margins: its title sits
        // one margin plus the card's padding (both XL) below the top.
        let title = find(&drawn, "A dialog title");
        assert!(
            title.rect.top() >= DIALOG_WINDOW_MARGIN + space::XL - 1.0,
            "{size:?}: the card overflows (title at {:?})",
            title.rect
        );
        for l in &labels {
            let d = find(&drawn, l);
            assert!(d.visible(w.screen), "{size:?}: {l} hidden at {:?}", d.rect);
            assert!(d.rect.bottom() <= w.screen.bottom() - DIALOG_WINDOW_MARGIN);
        }
        assert!(find(&drawn, "A dialog title").visible(w.screen));
        assert!(find(&drawn, "Body line 0").visible(w.screen));
        assert!(!shown(&drawn, "Body line 79", w.screen));
        w.click(last.rect.center(), &mut ui);
        assert_eq!(clicked, Some(11), "{size:?}");
    }
}

/// The switch preview (hot restart countdown) pins its header with the
/// buttons and scrolls the rest within its height.
#[test]
fn the_switch_preview_keeps_its_buttons_while_its_notices_scroll() {
    for size in SIZES {
        let w = Window::new(size);
        let mut plan = router_off_plan();
        plan.primary = RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        };
        let mut flow = Flow::new(30);
        flow.request(plan.request.clone(), Duration::ZERO);
        let commands = flow.take_commands();
        let [Command::Plan { seq, .. }] = commands.as_slice() else {
            panic!("one plan command");
        };
        flow.on_planned(*seq, Ok(plan), Duration::ZERO);
        assert!(matches!(flow.stage(), Stage::Armed { .. }));
        let tr = Tr::new(Locale::En);
        let data = Data::default();
        let fonts = fonts::FontReport::default();
        let service = support::idle_service();
        let mut banner = BannerState::default();
        let max_h = size[1] * 0.62;
        let mut ui = |ctx: &egui::Context| {
            let cx = Cx {
                tr: &tr,
                pal: Palette::of(ctx),
                data: &data,
                busy: false,
                now: Timestamp::EPOCH,
                fonts: &fonts,
                service: &service,
                log: &[],
            };
            egui::Area::new(egui::Id::unique("preview"))
                .fixed_pos(pos2(40.0, 40.0))
                .constrain(false)
                .show(ctx, |ui| {
                    ui.set_width(620.0);
                    shell::banner(ui, &cx, &mut flow, &mut banner, Duration::ZERO, max_h);
                });
        };
        let drawn = w.settle(&mut ui);
        let now = find(&drawn, &tr.t("banner.switch_now"));
        let warning = find_prefix(&drawn, "Warning 1:");
        assert!(now.visible(w.screen));
        assert!(!shown(&drawn, "Warning 26:", w.screen));
        // The card is at most `max_h` (plus its padding) high.
        let lowest = drawn
            .iter()
            .filter(|d| d.visible(w.screen))
            .map(|d| d.rect.bottom())
            .fold(0.0, f32::max);
        assert!(lowest <= 40.0 + max_h + 40.0, "{size:?}: {lowest}");
        w.wheel(warning.rect.center(), -4000.0, &mut ui);
        let drawn = w.settle(&mut ui);
        assert!(shown(&drawn, "Warning 26:", w.screen), "{size:?}");
        assert_eq!(find(&drawn, &tr.t("banner.switch_now")).rect, now.rect);
    }
}

/// Outside a dialog or a bounded area, `dialog_body` draws in place.
#[test]
fn a_body_outside_a_dialog_draws_in_place() {
    let w = Window::new(SIZES[0]);
    let mut ui = |ctx: &egui::Context| {
        egui::Area::new(egui::Id::unique("inline"))
            .fixed_pos(pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let pal = Palette::of(ui.ctx());
                dialog_body(ui, |ui| {
                    for i in 0..60 {
                        ui.label(Text::body(format!("Inline {i}"), pal));
                    }
                });
            });
    };
    let drawn = w.settle(&mut ui);
    // Not a scrolling viewport of its own: the lines run on below the window
    // like any page content.
    assert!(shown(&drawn, "Inline 0", w.screen));
    assert!(!shown(&drawn, "Inline 59", w.screen));
    // And a bounded area with no body is just its contents.
    let mut ui = |ctx: &egui::Context| {
        egui::Area::new(egui::Id::unique("plain"))
            .fixed_pos(pos2(0.0, 0.0))
            .show(ctx, |ui| {
                let pal = Palette::of(ui.ctx());
                bounded(ui, egui::Id::unique("plain"), 100.0, |ui| {
                    ui.label(Text::body("Only text", pal));
                });
            });
    };
    assert!(shown(&w.settle(&mut ui), "Only text", w.screen));
}
