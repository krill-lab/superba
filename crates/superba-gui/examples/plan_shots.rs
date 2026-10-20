//! The "write settings.json?" plans in a real window, for the screenshots of
//! the change list (the alpha.8 user report: keys took the row and values were
//! cut to four characters).
//!
//!     cargo run -p superba-gui --example plan_shots -- \
//!         --size 960x640 --theme dark --lang zh-CN [--scenario claude|tools]
//!
//! `claude` draws the Claude Code page (from the tests' fake service) with its
//! plan dialog open; `tools` draws the Tools review. Both show the same long
//! `env.ANTHROPIC_*` keys, a masked key, a removal and a key someone else
//! changed. Nothing is written: no files, no network.

use std::sync::Arc;
use std::time::Instant;

use superba_core::harness::{HarnessChange, HarnessId, HarnessPlan, HarnessPlanKind};
use superba_core::{Locale, SwitchService, Timestamp};
use superba_gui::design::{backdrop, fonts, layout, style, tokens::Palette, tokens::size};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Cx, Data, Page, claude, tools};
use superba_gui::shell;

#[path = "../tests/support/mod.rs"]
mod support;

fn change(slot: &str, before: Option<&str>, after: Option<&str>, drift: bool) -> HarnessChange {
    HarnessChange {
        file: "settings".into(),
        slot: slot.into(),
        before: before.map(Into::into),
        after: after.map(Into::into),
        drift,
        secret: slot.ends_with("TOKEN") || slot.ends_with("KEY"),
    }
}

fn plan(harness: HarnessId) -> HarnessPlan {
    HarnessPlan {
        kind: HarnessPlanKind::Apply,
        harness,
        provider: None,
        protocol: None,
        files: Vec::new(),
        changes: vec![
            change(
                "env.ANTHROPIC_BASE_URL",
                Some("https://relay.internal.example.com/v1/anthropic/old-gateway"),
                Some("https://relay.internal.example.com/claude/v2/anthropic-compatible/messages"),
                false,
            ),
            change(
                "env.ANTHROPIC_AUTH_TOKEN",
                Some("••••9c1e"),
                Some("••••a3f9"),
                false,
            ),
            change(
                "env.ANTHROPIC_DEFAULT_OPUS_MODEL",
                None,
                Some("orca/claude-opus-4-7-20260901-thinking-extended-context-1m"),
                false,
            ),
            change(
                "env.ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES",
                Some("effort,thinking,interleaved-thinking,context-1m"),
                None,
                false,
            ),
            change(
                "env.ANTHROPIC_DEFAULT_SONNET_MODEL",
                Some("orca/claude-sonnet-4-6-20260214"),
                Some("orca/claude-sonnet-4-7-20260901-thinking-extended-1m"),
                true,
            ),
            change(
                "env.ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
                None,
                Some("effort,thinking"),
                false,
            ),
        ],
        notes: Vec::new(),
        desired: None,
    }
}

struct Shots {
    tr: Tr,
    data: Data,
    fonts: fonts::InstalledFonts,
    service: Arc<dyn SwitchService>,
    tools: bool,
    claude: claude::State,
    tool: tools::State,
    started: Instant,
}

fn data(service: &support::fake::FakeService) -> Data {
    use superba_core::SwitchService as _;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut data = Data {
            sites: service.sites().await.expect("sites"),
            ..Data::default()
        };
        data.overview.finish(service.overview().await);
        data.profiles.finish(service.profile_set().await);
        data
    })
}

impl Shots {
    fn new(cc: &eframe::CreationContext<'_>, args: &Args) -> Self {
        let fonts = fonts::install(&cc.egui_ctx);
        style::install(&cc.egui_ctx, Some(args.theme));
        let fake = Arc::new(support::fake::FakeService::new(
            support::fake::FakeOptions {
                fast: true,
                ..Default::default()
            },
        ));
        let mut tool = tools::State::default();
        tool.plan = Some(plan(HarnessId::Gemini));
        Self {
            tr: Tr::new(args.locale),
            data: data(&fake),
            fonts,
            service: fake,
            tools: args.scenario == "tools",
            claude: claude::State::default(),
            tool,
            started: Instant::now(),
        }
    }
}

impl eframe::App for Shots {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fonts.poll(&ctx);
        let pal = Palette::of(&ctx);
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
        let page_id = if self.tools {
            Page::Tools
        } else {
            Page::ClaudeCode
        };
        backdrop::paint(ui.painter(), ui.max_rect(), &backdrop::Backdrop::of(&ctx));
        egui::Panel::left("nav")
            .exact_size(size::SIDEBAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(shell::sidebar_frame())
            .show(ui, |ui| {
                let mut page = page_id;
                shell::sidebar(ui, &cx, &mut page, &|_| Default::default(), &mut Vec::new());
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let full = ui.available_rect_before_wrap();
                let panel = backdrop::panel_rect(full);
                backdrop::content_card(ui.painter(), panel, &backdrop::Backdrop::of(ui.ctx()));
                let page = layout::page_rect(full, true);
                let mut area = ui.new_child(egui::UiBuilder::new().max_rect(page));
                area.add_space(size::PAGE_TOP - (page.min.y - full.min.y));
                let mut out = Vec::new();
                if self.tools {
                    superba_gui::pages::page_header(&mut area, &cx, Page::Tools, |_| {});
                    tools::detail::review(&mut area, &cx, &mut self.tool);
                } else {
                    if self.claude.view.is_some() && self.claude.connect.plan.is_none() {
                        self.claude.connect.plan = Some(plan(HarnessId::ClaudeCode));
                    }
                    claude::show(&mut area, &cx, &mut self.claude, Page::ClaudeCode, &mut out);
                }
            });
        if self.started.elapsed().as_secs() < 3 {
            ctx.request_repaint();
        }
    }
}

#[derive(Clone)]
struct Args {
    size: [f32; 2],
    theme: egui::Theme,
    locale: Locale,
    scenario: String,
}

fn args() -> Args {
    let mut out = Args {
        size: size::DEFAULT_WINDOW,
        theme: egui::Theme::Dark,
        locale: Locale::ZhCn,
        scenario: "claude".into(),
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
