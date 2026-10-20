//! The router-off confirmation of user bug 103 in a real window, for the
//! dialog screenshots (`tools/screenshots/dialog_shots.py`).
//!
//!     cargo run -p superba-gui --example dialog_shots -- \
//!         --size 960x640 --theme dark --lang zh-CN [--scenario router-off|preview] \
//!         [--expand 1]
//!
//! It draws the window background, the sidebar and the Router page frame, then
//! the shell's own `confirm_full` dialog (or, with `--scenario preview`, the
//! hot-switch preview overlay) for a plan shaped like the one the alpha.7 user
//! got on Windows when turning the router off: the per-model catalog notes of a
//! relay that lists 32 older model names, the catalog file, a stopped WSL
//! distribution and an unprobed compaction capability. Every notice is a real
//! catalog key, rendered by the same localization as the app. Nothing runs
//! behind it: no service, no files, no network.

use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_core::{
    AuthMode, FullRestartReason, Locale, LocalizedText, Notice, PlanDestination, RestartKind,
    SwitchIntent, SwitchPlan, SwitchRequest, Timestamp,
};
use superba_gui::design::{backdrop, fonts, layout, style, tokens::Palette, tokens::size};
use superba_gui::flow::Flow;
use superba_gui::i18n::Tr;
use superba_gui::pages::{Cx, Data, Page};
use superba_gui::runtime::Command;
use superba_gui::shell::{self, BannerState};

// The tests' idle fake service (the dialog never calls it).
#[path = "../tests/support/mod.rs"]
mod support;

/// Model names an OpenAI-compatible relay lists that the desktop app's Codex
/// (0.163.0-alpha.10.2) has no built-in entry for.
const OLD_MODELS: [&str; 32] = [
    "chatgpt-4o-latest",
    "codex-mini-latest",
    "gpt-4.1",
    "gpt-4.1-mini",
    "gpt-4o",
    "gpt-5",
    "gpt-5-chat-latest",
    "gpt-5-codex",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-5-pro",
    "gpt-5-search-api",
    "gpt-5.1",
    "gpt-5.1-chat-latest",
    "gpt-5.1-codex",
    "gpt-5.1-codex-max",
    "gpt-5.1-codex-mini",
    "gpt-5.2",
    "gpt-5.2-chat-latest",
    "gpt-5.2-codex",
    "gpt-5.2-pro",
    "gpt-5.3-chat-latest",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.4-pro",
    "gpt-5.5-pro",
    "gpt-5.6",
    "o3",
    "o4-mini",
];

fn text(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(key, fallback)
}

/// The notices of the user's plan, in the order the planner produced them.
fn notices() -> Vec<Notice> {
    let site = text("test.site", "Intranet");
    let mut out = vec![
        Notice::info(text(
            "switch.settings.no_catalog",
            "No settings catalog is available; settings are not checked against your Codex version.",
        )),
        // Another model runs after the switch: a warning (planner check_model).
        Notice::warning(
            text(
                "switch.warn.model_missing",
                "“{site}” does not have the model you use now, {model}; {replacement} is used after the switch.",
            )
            .arg_text("site", site.clone())
            .arg("model", "provider/gpt-6.1-sol")
            .arg("replacement", "gpt-5"),
        ),
    ];
    out.extend(OLD_MODELS.iter().map(|m| {
        Notice::info(
            text(
                "models_note.generic_template",
                "{model}: no catalog entry is known; Codex's settings for unknown models are used",
            )
            .arg("model", *m),
        )
    }));
    out.push(Notice::info(
        text(
            "switch.models.catalog_written",
            "Model catalog file: {file}",
        )
        .arg("file", r"C:\Users\you\.codex\superba\models-c53dd0cb.json"),
    ));
    out.push(Notice::info(
        text(
            "switch.wsl.stopped_skipped",
            "{host} is not running and was not checked.",
        )
        .arg("host", "WSL: archlinux"),
    ));
    out.push(Notice::info(
        text(
            "switch.warn.compaction_unknown",
            "Remote compaction of “{site}” has not been checked yet.",
        )
        .arg_text("site", site),
    ));
    out
}

fn plan(full: bool) -> SwitchPlan {
    SwitchPlan {
        id: "dialog-shots".into(),
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
            label: text("test.site", "Intranet"),
        },
        primary: if full {
            RestartKind::FullAppRestart {
                reason: FullRestartReason::NoCdp,
            }
        } else {
            RestartKind::AppServerHot {
                host_id: "local".into(),
                kill_remote: false,
            }
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

struct Shots {
    tr: Tr,
    data: Data,
    fonts: fonts::InstalledFonts,
    flow: Flow,
    banner: BannerState,
    service: Arc<dyn superba_core::SwitchService>,
    preview: bool,
    started: Instant,
}

impl Shots {
    fn new(cc: &eframe::CreationContext<'_>, args: &Args) -> Self {
        let fonts = fonts::install(&cc.egui_ctx);
        style::install(&cc.egui_ctx, Some(args.theme));
        let preview = args.scenario == "preview";
        // A long countdown keeps the preview on screen.
        let mut flow = Flow::new(if preview { 255 } else { 5 });
        let plan = plan(!preview);
        flow.request(plan.request.clone(), Duration::ZERO);
        let commands = flow.take_commands();
        let [Command::Plan { seq, .. }] = commands.as_slice() else {
            panic!("the flow asks for one plan");
        };
        if args.expand {
            let id = if preview {
                shell::plan_notices_id(&plan)
            } else {
                shell::confirm_notices_id(&plan)
            };
            superba_gui::pages::shared::notices::expand_all(&cc.egui_ctx, id, &plan.warnings);
        }
        flow.on_planned(*seq, Ok(plan), Duration::ZERO);
        Self {
            tr: Tr::new(args.locale),
            data: Data::default(),
            fonts,
            flow,
            banner: BannerState::default(),
            service: support::idle_service(),
            preview,
            started: Instant::now(),
        }
    }
}

impl eframe::App for Shots {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fonts.poll(&ctx);
        let pal = Palette::of(&ctx);
        let now = self.started.elapsed().min(Duration::from_secs(1));
        let cx = Cx {
            tr: &self.tr,
            pal,
            data: &self.data,
            busy: false,
            now: Timestamp::EPOCH,
            fonts: self.fonts.report(),
            service: &self.service,
            log: &[],
        };
        backdrop::paint(ui.painter(), ui.max_rect(), &backdrop::Backdrop::of(&ctx));
        egui::Panel::left("nav")
            .exact_size(size::SIDEBAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(shell::sidebar_frame())
            .show(ui, |ui| {
                let mut page = Page::Router;
                shell::sidebar(ui, &cx, &mut page, &|_| Default::default(), &mut Vec::new());
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let full = ui.available_rect_before_wrap();
                let panel = backdrop::panel_rect(full);
                backdrop::content_card(ui.painter(), panel, &backdrop::Backdrop::of(ui.ctx()));
                let page = layout::page_rect(full, false);
                let mut area = ui.new_child(egui::UiBuilder::new().max_rect(page));
                area.add_space(size::PAGE_TOP - (page.min.y - full.min.y));
                superba_gui::pages::page_header(&mut area, &cx, Page::Router, |_| {});
                if self.preview {
                    let width = page.width().min(size::OVERLAY_MAX);
                    egui::Area::new(egui::Id::unique("preview"))
                        .order(egui::Order::Middle)
                        .pivot(egui::Align2::CENTER_BOTTOM)
                        .fixed_pos(egui::pos2(page.center().x, full.max.y - 20.0))
                        .constrain(false)
                        .show(ui.ctx(), |ui| {
                            ui.set_width(width);
                            let max_h = (full.height() * 0.62).max(160.0);
                            ui.set_max_height(max_h + 40.0);
                            shell::banner(ui, &cx, &mut self.flow, &mut self.banner, now, max_h);
                        });
                }
            });
        shell::confirm_full(&ctx, &cx, &mut self.flow, &mut self.banner, now);
        // Clicks would change the flow (switch, cancel); the picture keeps it.
        let _ = self.flow.take_commands();
    }
}

#[derive(Clone)]
struct Args {
    size: [f32; 2],
    theme: egui::Theme,
    locale: Locale,
    scenario: String,
    /// Open the notes and the groups (builds with grouped notices only).
    expand: bool,
}

fn args() -> Args {
    let mut out = Args {
        size: size::DEFAULT_WINDOW,
        theme: egui::Theme::Dark,
        locale: Locale::ZhCn,
        scenario: "router-off".into(),
        expand: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_default();
        match flag.as_str() {
            "--size" => {
                let (w, h) = value.split_once('x').expect("--size WxH");
                out.size = [w.parse().expect("width"), h.parse().expect("height")];
            }
            "--theme" => {
                out.theme = if value == "light" {
                    egui::Theme::Light
                } else {
                    egui::Theme::Dark
                }
            }
            "--lang" => out.locale = Locale::parse(&value).expect("--lang en|zh-CN"),
            "--scenario" => out.scenario = value,
            "--expand" => out.expand = value == "1",
            other => panic!("unknown argument {other}"),
        }
    }
    out
}

fn main() -> eframe::Result {
    let args = args();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Superba")
            .with_inner_size(args.size)
            .with_position([0.0, 0.0])
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native(
        "Superba",
        options,
        Box::new(move |cc| Ok(Box::new(Shots::new(cc, &args)))),
    )
}
