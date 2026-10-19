//! The real Superba update page with isolated fixtures. No real tool is probed.
//! `cargo run -p superba-gui --example updates_gallery -- --zh --dark --narrow --output FILE`
#![allow(dead_code)]
#[path = "../tests/support/mod.rs"]
mod support;
use egui::{Align, Layout, Ui, UiBuilder};
use std::{path::PathBuf, sync::Arc, time::Instant};
use superba_core::{Locale, SelfUpdateStatus, SelfUpdateView, Settings, SwitchService, Timestamp};
use superba_gui::pages::superba::updates::{UpdateItem, items, tools::Source};
use superba_gui::{
    app::PageStates,
    design::{
        Palette,
        backdrop::{self, Backdrop},
        fonts, layout, style,
        tokens::size,
    },
    i18n::Tr,
    pages::{Cx, Data, Page, PageStatus, SuperbaSection},
    shell,
};
use support::{
    fake::{FakeOptions, FakeService},
    update_tools::DemoTools,
};

struct Gallery {
    data: Data,
    service: Arc<dyn SwitchService>,
    pages: PageStates,
    settings: Settings,
    fonts: Option<fonts::InstalledFonts>,
    tr: Tr,
    frames: usize,
    output: Option<PathBuf>,
}
impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut Ui, _: &mut eframe::Frame) {
        if let Some(fonts) = &mut self.fonts {
            fonts.poll(ui.ctx());
        }
        let pal = Palette::of(ui.ctx());
        let page = Page::Superba(SuperbaSection::Updates);
        let cx = Cx {
            tr: &self.tr,
            pal,
            data: &self.data,
            busy: false,
            now: Timestamp::now(),
            fonts: self.fonts.as_ref().expect("fonts installed").report(),
            service: &self.service,
            log: &[],
        };
        let tools = self.pages.updates.tools.view(&cx);
        self.data.update_inventory_count = Some(
            items::inventory(&cx, &self.pages.updates, &tools)
                .iter()
                .filter(|e| e.status.actionable())
                .count() as u32,
        );
        let cx = Cx {
            tr: &self.tr,
            pal,
            data: &self.data,
            busy: false,
            now: Timestamp::now(),
            fonts: self.fonts.as_ref().expect("fonts installed").report(),
            service: &self.service,
            log: &[],
        };
        backdrop::paint(ui.painter(), ui.max_rect(), &Backdrop::of(ui.ctx()));
        let badges = superba_gui::app::shell_badges(&self.tr, &self.data);
        egui::Panel::left("nav")
            .exact_size(size::SIDEBAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(shell::sidebar_frame())
            .show(ui, |ui| {
                shell::sidebar(
                    ui,
                    &cx,
                    &mut page.clone(),
                    &|p| {
                        let mut s = PageStatus::default();
                        if let Some((_, badge, tip)) = badges.iter().find(|(page, ..)| *page == p) {
                            s.badge = *badge;
                            s.badge_tip = Some(tip.clone());
                        }
                        s
                    },
                    &mut vec![],
                );
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| {
                let full = ui.available_rect_before_wrap();
                backdrop::content_card(
                    ui.painter(),
                    backdrop::panel_rect(full),
                    &Backdrop::of(ui.ctx()),
                );
                let mut area = ui.new_child(
                    UiBuilder::new()
                        .max_rect(layout::page_rect(full, true))
                        .layout(Layout::top_down(Align::Min)),
                );
                layout::fit_area(&mut area, page.min_height(), |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    self.pages
                        .show(page, ui, &cx, &mut self.settings, &mut vec![]);
                });
            });
        self.frames += 1;
        if self.frames >= 8
            && let Some(path) = self.output.take()
        {
            let ctx = ui.ctx().clone();
            ui.ctx().request_screenshot(move |image| {
                let size =
                    resvg::tiny_skia::IntSize::from_wh(image.width() as u32, image.height() as u32)
                        .unwrap();
                let pixels = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                resvg::tiny_skia::Pixmap::from_vec(pixels, size)
                    .unwrap()
                    .save_png(path)
                    .unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            });
        }
        if self.output.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
        }
    }
}
fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    let has = |flag| args.iter().any(|s| s == flag);
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        first_run: false,
        ..Default::default()
    }));
    let mut versions = service.versions_view();
    versions.app.action = Some(superba_core::AppUpdateAction::MacSparkle { package: None });
    service.set_versions(versions.clone());
    let mut data = Data::default();
    data.versions.finish(Ok(versions.clone()));
    data.selfupdate.finish(Ok(SelfUpdateView {
        current: "0.1.1-alpha.7".into(),
        status: if has("--unknown") {
            SelfUpdateStatus::CheckFailed
        } else if has("--skipped") {
            SelfUpdateStatus::Available
        } else {
            SelfUpdateStatus::UpToDate
        },
        detail: has("--unknown").then(|| {
            if has("--zh") {
                "版本服务器暂时不可用".into()
            } else {
                "Version server unavailable".into()
            }
        }),
        latest: has("--skipped").then(|| superba_core::SelfRelease {
            tag: "v0.1.1-alpha.8".into(),
            version: "0.1.1-alpha.8".into(),
            prerelease: true,
            notes: String::new(),
            published_at: None,
            url: "https://example.com/release".into(),
            asset: Some("package.tar.gz".into()),
            asset_size: None,
        }),
        skipped: has("--skipped").then(|| "0.1.1-alpha.8".into()),
        ..Default::default()
    }));
    let mut pages = PageStates::default();
    pages.updates.view.value = Some(versions);
    pages.updates.view.at = Some(Instant::now());
    pages.updates.tools = Source(Arc::new(DemoTools::default()));
    if has("--expanded") {
        pages.updates.expanded.insert(UpdateItem::AppPin);
        pages.updates.focus = Some(UpdateItem::AppPin);
    }
    if has("--tools") {
        pages.updates.focus = Some(UpdateItem::Tool(
            superba_gui::pages::superba::updates::tools::ToolId::Zcode,
        ));
    }
    let gallery = Gallery {
        data,
        service,
        pages,
        settings: Settings::default(),
        fonts: None,
        tr: Tr::new(if has("--zh") {
            Locale::ZhCn
        } else {
            Locale::En
        }),
        frames: 0,
        output: args
            .windows(2)
            .find(|a| a[0] == "--output")
            .map(|a| PathBuf::from(&a[1])),
    };
    let dark = has("--dark");
    let narrow = has("--narrow");
    eframe::run_native(
        "Superba updates",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(if narrow {
                    [960.0, 640.0]
                } else {
                    [1280.0, 800.0]
                })
                .with_decorations(false),
            ..Default::default()
        },
        Box::new(move |cc| {
            style::install(
                &cc.egui_ctx,
                Some(if dark {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                }),
            );
            let mut gallery = gallery;
            gallery.fonts = Some(fonts::install(&cc.egui_ctx));
            cc.egui_ctx.set_pixels_per_point(1.0);
            Ok(Box::new(gallery))
        }),
    )
}
