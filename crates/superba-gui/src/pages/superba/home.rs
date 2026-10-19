//! The application home: version, attention, local facts, recent log and about.
use super::{Action, Cx, Page, SuperbaSection, section};
use crate::design::widgets::{
    AttentionRow, Badge, Button, Card, InfoCard, Status, Text, link_button,
};
use crate::design::{elide::Shape, icons::Icon};
use crate::pages::{
    notify,
    overview::todo::{self, TodoKind},
};
use crate::runtime::{ReadTask, task};
use egui::Ui;
use superba_core::{SelfUpdateStatus, Settings, codex_files::CodexFileBackupQuery};

/// Repository and About links.
pub const PROJECT_URL: &str = "https://github.com/krill-lab/superba";

/// Home reads its facts in the background, never during layout.
#[derive(Debug, Default)]
pub struct State {
    facts: Option<Facts>,
    reading: Option<ReadTask<Facts>>,
    attempted: bool,
}
#[derive(Debug)]
struct Facts {
    backups: Option<usize>,
    bytes: Option<u64>,
    spb: Option<std::path::PathBuf>,
}

fn directory_bytes(dir: &std::path::Path) -> Option<u64> {
    let mut total = 0_u64;
    // Do not follow symlinks out of the data directory.
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let kind = entry.file_type().ok()?;
        if kind.is_dir() {
            total = total.saturating_add(directory_bytes(&entry.path())?);
        } else if kind.is_file() {
            total = total.saturating_add(entry.metadata().ok()?.len());
        }
    }
    Some(total)
}
fn poll(ui: &Ui, cx: &Cx<'_>, state: &mut State) {
    if !state.attempted {
        state.attempted = true;
        let service = cx.service.clone();
        let dir = cx.data.paths.as_ref().map(|p| p.data_dir.clone());
        state.reading = Some(task(async move {
            let backups = service
                .codex_file_backups(CodexFileBackupQuery::default())
                .await
                .ok()
                .map(|b| b.len());
            let (bytes, spb) = tokio::task::spawn_blocking(move || {
                let bytes = dir.as_deref().and_then(directory_bytes);
                let name = format!("spb{}", std::env::consts::EXE_SUFFIX);
                let spb = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|p| p.join(&name)))
                    .filter(|p| p.is_file())
                    .or_else(|| {
                        std::env::var_os("PATH").and_then(|p| {
                            std::env::split_paths(&p)
                                .map(|p| p.join(&name))
                                .find(|p| p.is_file())
                        })
                    });
                (bytes, spb)
            })
            .await
            .unwrap_or_default();
            Ok(Facts {
                backups,
                bytes,
                spb,
            })
        }));
    }
    if let Some(reading) = &state.reading {
        if let Some(result) = reading.poll() {
            state.facts = result.ok();
            state.reading = None;
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// The home summary, including the original About content.
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    settings: &mut Settings,
    notifications: &mut notify::State,
    out: &mut Vec<Action>,
) {
    poll(ui, cx, state);
    if notify::poll(cx, settings, notifications, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    ui.horizontal(|ui| {
        crate::shell::brand_mark(ui, cx.pal, 56.0);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.label(Text::title("Superba", cx.pal));
            ui.horizontal_wrapped(|ui| {
                ui.label(Text::mono_muted(
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    cx.pal,
                ));
                let channel = settings.update_channel_for(env!("CARGO_PKG_VERSION"));
                let key = if channel == superba_core::UpdateChannel::Stable {
                    "updates.superba.channel_stable"
                } else {
                    "updates.superba.channel_preview"
                };
                ui.add(Badge::new(cx.tr.t(key)).outline());
            });
        });
    });
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        let (status, text) = match cx.data.selfupdate.value.as_ref().map(|v| &v.status) {
            Some(SelfUpdateStatus::UpToDate) => (Status::Ok, cx.tr.t("superba.home.current")),
            _ if cx.data.self_update_offered().is_some() => {
                (Status::Warn, cx.tr.t("superba.home.available"))
            }
            _ => (Status::Idle, cx.tr.t("superba.home.check_hint")),
        };
        ui.add(Badge::new(text).status(status));
        if let Some(at) = cx.data.selfupdate.value.as_ref().and_then(|v| v.checked_at) {
            ui.label(Text::small(
                cx.tr.f("superba.home.checked", &[("age", &cx.ago(at))]),
                cx.pal,
            ));
        }
        if ui
            .add(
                Button::new(cx.tr.t("superba.home.check"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(cx.data.selfupdate.loading),
            )
            .clicked()
        {
            out.push(Action::CheckUpdates);
        }
    });
    ui.add(egui::Label::new(Text::small(cx.tr.t("superba.home.subtitle"), cx.pal)).wrap());
    ui.add_space(20.0);
    section(ui, cx, &cx.tr.t("superba.home.attention"), None);
    let todos: Vec<_> = todo::todos(cx, None)
        .into_iter()
        .filter(|t| {
            matches!(
                t.kind,
                TodoKind::SuperbaUpdate
                    | TodoKind::AppUpdate
                    | TodoKind::ToolUpdate(_)
                    | TodoKind::NotifyHooks
                    | TodoKind::PlainKeys
                    | TodoKind::OldConfig
                    | TodoKind::PinMismatch
                    | TodoKind::Finding(_)
            )
        })
        .collect();
    Card::new().padding(12).show(ui, |ui| {
        if todos.is_empty() {
            ui.label(Text::body(cx.tr.t("superba.home.all_good"), cx.pal));
        }
        for (i, t) in todos.iter().enumerate() {
            if (AttentionRow {
                id: i,
                title: &t.title,
                detail: t.detail.as_deref(),
                label: &t.fix_label(cx),
                tip: &t.title,
                icon: if t.status == Status::Idle {
                    Icon::Download
                } else {
                    Icon::Warning
                },
                color: cx.pal.status(t.status),
            })
            .show(ui)
            {
                out.push(t.fix(cx));
            }
        }
    });
    ui.add_space(20.0);
    section(ui, cx, &cx.tr.t("superba.home.computer"), None);
    facts(ui, cx, state, out);
    ui.add_space(20.0);
    crate::pages::section_with(ui, cx, &cx.tr.t("superba.home.recent"), None, |ui| {
        if link_button(ui, &cx.tr.t("superba.home.all_logs"), false).clicked() {
            out.push(Action::Go(Page::Superba(SuperbaSection::Diagnostics)));
        }
    });
    Card::new().flat().show(ui, |ui| {
        if cx.log.is_empty() {
            ui.label(Text::small(cx.tr.t("diag.log.empty"), cx.pal));
        }
        for line in cx.log.iter().rev().take(4) {
            ui.horizontal(|ui| {
                ui.label(Text::mono_muted(
                    crate::pages::clock_time(line.at, true),
                    cx.pal,
                ));
                ui.add(egui::Label::new(Text::mono(&line.text, cx.pal)).truncate())
                    .on_hover_text(&line.text);
            });
        }
    });
    ui.add_space(20.0);
    section(ui, cx, &cx.tr.t("settings.about.title"), None);
    ui.horizontal_wrapped(|ui| {
        for (key, suffix) in [
            ("prefs.about.project", ""),
            ("superba.about.changelog", "/releases"),
            ("superba.about.feedback", "/issues"),
            ("superba.about.notices", "/blob/main/about.toml"),
        ] {
            if link_button(ui, &cx.tr.t(key), true).clicked() {
                out.push(Action::OpenUrl(format!("{PROJECT_URL}{suffix}")));
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(Text::small(cx.tr.t("settings.about.license"), cx.pal));
        ui.label(Text::small("·", cx.pal));
        ui.label(Text::small(cx.tr.t("settings.about.fonts"), cx.pal));
    });
}

fn facts(ui: &mut Ui, cx: &Cx<'_>, state: &State, out: &mut Vec<Action>) {
    let unknown = cx.tr.t("superba.home.unknown");
    let dir = cx
        .data
        .paths
        .as_ref()
        .map(|p| superba_core::paths::display_path(&p.data_dir))
        .unwrap_or_else(|| unknown.clone());
    let bytes = state
        .facts
        .as_ref()
        .and_then(|f| f.bytes)
        .map(|n| format!("{:.1} MiB", n as f64 / 1_048_576.0))
        .unwrap_or_else(|| unknown.clone());
    let backups = state
        .facts
        .as_ref()
        .and_then(|f| f.backups)
        .map(|n| n.to_string())
        .unwrap_or_else(|| unknown.clone());
    let spb = state
        .facts
        .as_ref()
        .and_then(|f| f.spb.as_ref())
        .map(|p| superba_core::paths::display_path(p))
        .unwrap_or_else(|| unknown.clone());
    let routed = cx.data.router_enabled();
    let router = cx.tr.t(if routed {
        "superba.home.router_on"
    } else {
        "superba.home.router_off"
    });
    let address = if routed {
        cx.data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.config.as_ref())
            .and_then(|c| c.openai_base_url.clone())
            .unwrap_or_else(|| cx.tr.t("superba.home.router_desc"))
    } else {
        cx.tr.t("superba.home.router_desc")
    };
    let cards = [
        (
            "prefs.data.dir",
            dir,
            bytes,
            "prefs.data.open_dir",
            Shape::Path,
            Action::OpenDataDir,
        ),
        (
            "superba.home.backups",
            backups,
            cx.tr.t("superba.home.backups_desc"),
            "superba.home.manage_data",
            Shape::Prose,
            Action::Go(Page::Superba(SuperbaSection::Data)),
        ),
        (
            "superba.home.router",
            router,
            address,
            "prefs.data.open_router",
            Shape::Prose,
            Action::Go(Page::Router),
        ),
        (
            "superba.home.cli",
            spb,
            cx.tr.t("superba.home.cli_desc"),
            "superba.home.diagnostics",
            Shape::Path,
            Action::Go(Page::Superba(SuperbaSection::Diagnostics)),
        ),
    ];
    let columns = if ui.available_width() >= 740.0 { 4 } else { 2 };
    let mut cards = cards.into_iter();
    for _ in 0..4 / columns {
        ui.columns(columns, |uis| {
            for ui in uis {
                let (key, value, desc, link, shape, action) =
                    cards.next().expect("four fact cards");
                if InfoCard::fact(cx.tr.t(key), value)
                    .shape(shape)
                    .description(desc)
                    .link(cx.tr.t(link))
                    .show(ui)
                    .clicked()
                {
                    out.push(action);
                }
            }
        });
        ui.add_space(8.0);
    }
}
