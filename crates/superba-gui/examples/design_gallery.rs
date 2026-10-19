//! Shared components in a standalone window, using sample data only.
//! `cargo run -p superba-gui --example design_gallery -- [--dark] [--zh] [--narrow] [--output FILE]`
use std::path::PathBuf;

use egui::{Theme, Ui};
use superba_gui::design::{
    Palette, elide::Shape, fonts, icons::Icon, layout, style, tokens::size, widgets::*,
};

struct Gallery {
    fonts: Option<fonts::InstalledFonts>,
    zh: bool,
    dark: bool,
    selected: usize,
    expanded: [bool; 5],
    frames: usize,
    output: Option<PathBuf>,
}

fn words(zh: bool, en: &'static str, cn: &'static str) -> &'static str {
    if zh { cn } else { en }
}

impl Gallery {
    fn content(&mut self, ui: &mut Ui) {
        let zh = self.zh;
        let t = |en, cn| words(zh, en, cn);
        let pal = Palette::of(ui.ctx());
        ui.label(Text::title(t("Component gallery", "组件画廊"), pal));
        ui.label(Text::small(
            t("Shared controls · sample data", "共享组件 · 示例数据"),
            pal,
        ));
        ui.add_space(12.0);
        UpdateGroup(t("Updates", "更新")).show(ui, |ui| {
            link_button(ui, t("Check for updates", "检查更新"), false);
        });
        UpdateRow::new("superba", "Superba", t("Update available", "有新版本"))
            .tile(UpdateTile::Icon(Icon::Superba))
            .versions("0.1.0 → 0.1.1")
            .status(Status::Warn)
            .action(t("Update", "更新"))
            .show(ui, &mut self.expanded[0], |_| {});
        UpdateRow::new("claude", "Claude Code", t("Update available", "有新版本"))
            .tile(UpdateTile::Monogram("CC"))
            .versions("2.1.284 → 2.1.295")
            .description(t("Local command-line tool", "本机命令行工具"))
            .status(Status::Warn)
            .action(t("Update", "更新"))
            .show(ui, &mut self.expanded[1], |ui| {
                ui.label(Text::small(
                    t(
                        "Installed on this computer · ready to update",
                        "已安装在这台电脑上 · 可以更新",
                    ),
                    pal,
                ));
            });
        UpdateRow::new("opencode", "opencode", t("Downloading", "下载中"))
            .tile(UpdateTile::Monogram("OC"))
            .versions("2.0.1 → 2.0.2")
            .description("42 / 100 MB")
            .status(Status::Busy)
            .action(t("Cancel", "取消"))
            .progress(
                Some(0.42),
                t(
                    "Downloading opencode, 42 of 100 MB",
                    "正在下载 opencode，42 / 100 MB",
                ),
            )
            .show(ui, &mut self.expanded[2], |_| {});
        UpdateRow::new("pi", "pi", t("Update failed", "更新失败"))
            .tile(UpdateTile::Monogram("PI"))
            .versions("0.61.0 → 0.61.1")
            .description(t("Download verification failed", "下载文件校验失败"))
            .status(Status::Error)
            .action(t("Retry", "重试"))
            .show(ui, &mut self.expanded[3], |_| {});
        UpdateRow::new("workbuddy", "WorkBuddy", t("Not installed", "未安装"))
            .tile(UpdateTile::Monogram("WB"))
            .hollow()
            .action(t("Install", "安装"))
            .show(ui, &mut self.expanded[4], |_| {});

        ui.add_space(12.0);
        ui.label(Text::section(t("Facts", "信息卡片"), pal));
        ui.columns(3, |cols| {
            InfoCard::fact(t("Version", "版本"), "0.1.1-alpha.7")
                .shape(Shape::Token)
                .description(t("Preview channel", "预览版通道"))
                .link(t("Release notes", "更新说明"))
                .show(&mut cols[0]);
            InfoCard::fact(t("Local tools", "本机工具"), "7")
                .description(t("Codex, Claude Code and more", "Codex、Claude Code 等"))
                .link(t("View tools", "查看工具"))
                .show(&mut cols[1]);
            InfoCard::fact(
                t("Data directory", "数据目录"),
                "/home/demo/.local/share/superba",
            )
            .shape(Shape::Path)
            .description(t("Stored on this computer", "保存在这台电脑上"))
            .link(t("Open folder", "打开文件夹"))
            .show(&mut cols[2]);
        });
        ui.add_space(12.0);
        ui.label(Text::section(t("Status and identities", "状态与标识"), pal));
        ui.horizontal(|ui| {
            ui.add(Badge::new(t("Ready", "就绪")).status(Status::Ok));
            ui.add(Badge::new(t("Downloading", "下载中")).status(Status::Busy));
            ui.add(Badge::new(t("Not connected", "未连接")).hollow());
            ui.add_space(8.0);
            for icon in [Icon::Superba, Icon::Tools, Icon::Bell, Icon::Archive] {
                ui.add(IconTile::new(icon));
            }
            ui.add(IconTile::new(Icon::Warning).warning());
            for side in [22.0, 30.0, 40.0] {
                ui.add(MonogramTile::new("CC").size(side));
            }
        });
        ui.add_space(8.0);
        ui.add(
            InlineProgress::new(None, t("Checking releases", "正在检查版本"))
                .total(t("Checking releases…", "正在检查版本…")),
        );
        ui.add_space(12.0);
        ui.label(Text::section(t("Needs attention", "需要处理"), pal));
        AttentionRow {
            id: 0,
            title: t("Notification hook is not installed", "通知钩子尚未安装"),
            detail: Some(t(
                "Install it to receive completion notifications",
                "安装后可接收任务完成通知",
            )),
            label: t("Install", "安装"),
            tip: t("Install the notification hook", "安装通知钩子"),
            icon: Icon::Bell,
            color: pal.amber,
        }
        .show(ui);
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            cols[0].add(
                ListRow::new(Icon::Tools, "opencode")
                    .height(size::TOOL_ROW)
                    .subtitle(t("Follow Codex", "跟随 Codex"))
                    .status(Status::Ok, t("Connected", "已连接"))
                    .selected(true),
            );
            cols[1].add(
                ListRow::new(Icon::Terminal, "pi")
                    .height(size::TOOL_ROW)
                    .subtitle(t("Configuration changed", "配置有改动"))
                    .status(Status::Warn, t("Pending", "待应用")),
            );
        });
    }
}

impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if let Some(fonts) = &mut self.fonts {
            fonts.poll(ui.ctx());
        }
        let pal = Palette::of(ui.ctx());
        egui::Frame::new()
            .fill(pal.bg)
            .inner_margin(24)
            .show(ui, |ui| {
                let zh = self.zh;
                let t = |en, cn| words(zh, en, cn);
                let items = [
                    SubNavItem::new("home", "Superba", Icon::Superba).identity(),
                    SubNavItem::new("updates", t("Updates", "更新"), Icon::Download).badge("3"),
                    SubNavItem::new("diagnostics", t("Diagnostics", "诊断"), Icon::Diagnostics),
                    SubNavItem::new("data", t("Data & backups", "数据与备份"), Icon::Archive),
                    SubNavItem::new("general", t("General", "通用"), Icon::Settings)
                        .group(t("Preferences", "偏好设置")),
                    SubNavItem::new("appearance", t("Appearance", "外观"), Icon::Swatch),
                    SubNavItem::new("network", t("Network", "网络"), Icon::Lines),
                    SubNavItem::new("notify", t("Notifications", "通知"), Icon::Bell)
                        .dot(t("Hook missing", "未安装钩子")),
                ];
                let rail_w = if ui.available_width() < 760.0 {
                    size::RAIL_NARROW
                } else {
                    size::RAIL
                };
                let mut selected = self.selected;
                layout::rail_split(
                    ui,
                    rail_w,
                    |ui| {
                        SubNav::new(&items)
                            .footer(t("Changes save immediately", "更改立即保存"))
                            .show(ui, &mut selected);
                    },
                    |ui| self.content(ui),
                );
                self.selected = selected;
            });
        self.frames += 1;
        if self.frames >= 4
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
        // Progress widgets also repaint, but the gallery can capture a static
        // state if those examples are removed while iterating on a component.
        if self.output.is_some() {
            ui.ctx().request_repaint();
        }
    }
}

fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    let has = |flag| args.iter().any(|s| s == flag);
    let mut gallery = Gallery {
        fonts: None,
        zh: has("--zh"),
        dark: has("--dark"),
        selected: 1,
        expanded: [false, true, false, false, false],
        frames: 0,
        output: args
            .windows(2)
            .find(|a| a[0] == "--output")
            .map(|a| PathBuf::from(&a[1])),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([if has("--narrow") { 900.0 } else { 1280.0 }, 960.0])
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native(
        "Superba components",
        options,
        Box::new(move |cc| {
            style::install(
                &cc.egui_ctx,
                Some(if gallery.dark {
                    Theme::Dark
                } else {
                    Theme::Light
                }),
            );
            gallery.fonts = Some(fonts::install(&cc.egui_ctx));
            cc.egui_ctx.set_pixels_per_point(1.0);
            Ok(Box::new(gallery))
        }),
    )
}
