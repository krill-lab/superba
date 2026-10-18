//! Drawing of "Edit Codex files" (GitHub issue 9): the toolbar (file, home,
//! Format, Show secrets, Backups, Reload), the monospace editor with line
//! numbers and highlighting, the problems of the check under it, the footer
//! (status, Discard, Review & save) and the dialogs (review with the diff and
//! the managed keys an edit changes, backups with a diff to each, discard).
//! The state and its logic are in [`super::files`].

use egui::text::CCursorRange;
use egui::{Align, Id, Layout, RichText, ScrollArea, Sense, TextEdit, Ui, vec2};
use superba_core::{
    CodexFileKind, FileProblem, ProblemKind, ProblemSeverity, SaveReason, TargetId,
    auth_shape_label,
};
use superba_switch::codex_files::{self, DiffTag};

use super::files::{Dialog, Editor, Next};
use super::files_highlight;
use crate::design::fonts::{Weight, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::{radius, text};
use crate::design::widgets::{Badge, Button, Status, StatusDot, Text, Toggle, dialog};
use crate::pages::shared::loading;
use crate::pages::{Action, Cx, Page, page_header_with};

/// Id of the text field (cursor jumps, focus).
pub const EDITOR_ID: &str = "codex-files-editor-field";
/// Problem rows shown under the editor before the list scrolls.
const PROBLEM_ROWS: usize = 5;

/// "This computer", a host's name, "Ubuntu (WSL)".
pub fn target_label(cx: &Cx<'_>, target: &TargetId) -> String {
    if target.is_local() {
        return cx.tr.t("codex_editor.this_computer");
    }
    cx.data
        .hosts
        .value
        .as_ref()
        .and_then(|hosts| hosts.iter().find(|h| &h.host.target_id == target))
        .map(|h| cx.localize(&h.host.title()))
        .unwrap_or_else(|| target.to_string())
}

/// The homes the editor offers: this computer and every managed host.
fn targets(cx: &Cx<'_>) -> Vec<(TargetId, String)> {
    let mut out = vec![(TargetId::local(), cx.tr.t("codex_editor.this_computer"))];
    if let Some(hosts) = &cx.data.hosts.value {
        for h in hosts.iter().filter(|h| h.host.is_managed()) {
            out.push((h.host.target_id.clone(), cx.localize(&h.host.title())));
        }
    }
    out
}

fn owner_name(cx: &Cx<'_>, ed: &Editor) -> Option<String> {
    let id = ed.doc.as_ref()?.profile.as_ref()?;
    Some(
        cx.data
            .profile(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string()),
    )
}

/// Draw the editor in the Settings page's rectangle.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, ed: &mut Editor, out: &mut Vec<Action>) {
    if ed.poll(cx, out) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    let file = ed.kind().file_name();
    page_header_with(
        ui,
        cx,
        Page::Settings,
        &cx.tr.t("codex_editor.subtitle"),
        |ui| {
            if ui
                .add(
                    Button::ghost(cx.tr.t("codex_editor.back"))
                        .icon(Icon::Settings)
                        .small(),
                )
                .clicked()
            {
                ed.close();
            }
        },
    );
    toolbar(ui, cx, ed);
    notes(ui, cx, ed, out);
    if ed.doc.is_none() {
        // The chosen file is being read, or could not be: never the previous
        // choice's text (user bug 83).
        let target = ed.target();
        let host = (!target.is_local()).then(|| target_label(cx, &target));
        match ed.load_failure() {
            Some(failure) => {
                ui.add_space(12.0);
                let title = match &host {
                    Some(h) => cx.tr.f(
                        "codex_editor.load_failed_at",
                        &[("file", &file), ("host", h)],
                    ),
                    None => cx.tr.t("codex_editor.load_failed"),
                };
                if loading::failed_card(ui, cx, &title, &failure.text(cx)) {
                    ed.reload(cx);
                }
            }
            None => {
                ui.add_space(8.0);
                let text = match &host {
                    Some(h) => cx
                        .tr
                        .f("codex_editor.loading_at", &[("file", &file), ("host", h)]),
                    None => cx.tr.f("codex_editor.loading", &[("file", &file)]),
                };
                loading::spinner_row(ui, cx, &text);
            }
        }
        dialogs(ui.ctx(), cx, ed);
        return;
    }
    let shown = ed
        .problems
        .iter()
        .filter(|p| p.severity != ProblemSeverity::Info)
        .count();
    let problems_h = if shown == 0 {
        0.0
    } else {
        shown.min(PROBLEM_ROWS) as f32 * 22.0 + 12.0
    };
    // Below the scroll area: its frame (margins and stroke, 14), the footer
    // (6 + a 32 px button row) and the item spacing between them.
    let below = 14.0 + 6.0 + 32.0 + ui.spacing().item_spacing.y * 3.0;
    let problems_extra = if shown > 0 {
        4.0 + ui.spacing().item_spacing.y
    } else {
        0.0
    };
    let editor_h = (ui.available_height() - problems_h - problems_extra - below).max(120.0);
    editor(ui, cx, ed, editor_h);
    if shown > 0 {
        ui.add_space(4.0);
        problems(ui, cx, ed, problems_h);
    }
    footer(ui, cx, ed);
    dialogs(ui.ctx(), cx, ed);
}

fn toolbar(ui: &mut Ui, cx: &Cx<'_>, ed: &mut Editor) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let mut kind = ed.kind();
        crate::design::widgets::Segmented::new("codex-files-kind", &mut kind)
            .label(cx.tr.t("codex_editor.a11y.file"))
            .small()
            .option(CodexFileKind::ConfigToml, "config.toml")
            .option(CodexFileKind::AuthJson, "auth.json")
            .show(ui);
        if kind != ed.kind() {
            let target = ed.target();
            ed.open_file(cx, target, kind);
        }
        let list = targets(cx);
        if list.len() > 1 || !ed.target().is_local() {
            let current = target_label(cx, &ed.target());
            let mut pick = None;
            // The choice at once, busy while it is read (user bug 83).
            crate::design::widgets::Dropdown::new("codex-files-target")
                .label(cx.tr.t("codex_editor.where"))
                .selected_text(current)
                .width(180.0)
                .loading(ed.loading() && ed.switching())
                .show_ui(ui, |ui| {
                    for (id, label) in &list {
                        let on = *id == ed.target();
                        if ui.selectable_label(on, label).clicked() && !on {
                            pick = Some(id.clone());
                        }
                    }
                });
            if let Some(t) = pick {
                let kind = ed.kind();
                ed.open_file(cx, t, kind);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ready = ed.doc.is_some() && !ed.busy();
            if ui
                .add(
                    Button::ghost(cx.tr.t("codex_editor.reload"))
                        .icon(Icon::Refresh)
                        .small()
                        .loading(ed.loading())
                        .enabled(!ed.busy()),
                )
                .clicked()
            {
                if ed.dirty() {
                    ed.dialog = Some(Dialog::Discard(Next::Reload));
                } else {
                    ed.reload(cx);
                }
            }
            if ui
                .add(
                    Button::ghost(cx.tr.t("codex_editor.backups_button"))
                        .icon(Icon::Restore)
                        .small()
                        .enabled(ready),
                )
                .clicked()
            {
                ed.open_backups(cx);
            }
            if ed.kind() == CodexFileKind::AuthJson {
                let (label, icon) = if ed.revealed {
                    (cx.tr.t("codex_editor.hide_secrets"), Icon::EyeOff)
                } else {
                    (cx.tr.t("codex_editor.show_secrets"), Icon::Eye)
                };
                if ui
                    .add(Button::ghost(label).icon(icon).small().enabled(ready))
                    .clicked()
                {
                    ed.toggle_secrets();
                }
            }
            let editable = ready && ed.doc.as_ref().is_some_and(|d| d.read_only.is_none());
            if ui
                .add(
                    Button::ghost(cx.tr.t("codex_editor.format"))
                        .small()
                        .enabled(editable && ed.blocking() == 0),
                )
                .on_hover_text(cx.tr.t("codex_editor.format_tip"))
                .clicked()
            {
                ed.format();
            }
            // The path takes what is left, truncated next to the buttons.
            if let Some(doc) = &ed.doc {
                ui.add(
                    egui::Label::new(Text::mono_muted(&doc.path, cx.pal))
                        .truncate()
                        .sense(Sense::hover()),
                )
                .on_hover_text(&doc.path);
            }
        });
    });
    ui.add_space(4.0);
}

/// Read-only reasons, hidden secrets, managed keys, the last save's note.
fn notes(ui: &mut Ui, cx: &Cx<'_>, ed: &mut Editor, out: &mut Vec<Action>) {
    let Some(doc) = &ed.doc else {
        return;
    };
    let line = |ui: &mut Ui, status: Status, text: String| {
        ui.horizontal_top(|ui| {
            ui.add(StatusDot::new(status));
            ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
        });
    };
    if let Some(why) = &doc.read_only {
        line(ui, Status::Warn, cx.localize(why));
    }
    // Reading the shown file again failed: it stays, with the reason.
    if let Some(failure) = ed.load_failure() {
        line(
            ui,
            Status::Error,
            cx.tr.f(
                "codex_editor.reload_failed",
                &[
                    ("file", &doc.kind.file_name()),
                    ("reason", &failure.text(cx)),
                ],
            ),
        );
    }
    if let Some(e) = &ed.format_error {
        line(
            ui,
            Status::Error,
            cx.tr.f("codex_editor.format_failed", &[("at", e)]),
        );
    }
    if ed.masked() && doc.contents.is_some() {
        line(ui, Status::Idle, cx.tr.t("codex_editor.secrets_hidden"));
    }
    let present: Vec<&str> = ed
        .problems
        .iter()
        .filter(|p| p.kind == ProblemKind::Owned)
        .filter_map(|p| p.path.as_deref())
        .collect();
    if !present.is_empty() {
        let owner = owner_name(cx, ed);
        let keys = if present.len() > 5 {
            cx.tr.f(
                "codex_editor.owned_more",
                &[
                    ("keys", &present[..4].join(", ")),
                    ("n", &(present.len() - 4)),
                ],
            )
        } else {
            present.join(", ")
        };
        line(
            ui,
            Status::Idle,
            match owner {
                Some(o) => cx.tr.f(
                    "codex_editor.owned_named",
                    &[("keys", &keys), ("provider", &o)],
                ),
                None => cx.tr.f("codex_editor.owned", &[("keys", &keys)]),
            },
        );
    }
    if doc.owns_credential {
        line(ui, Status::Idle, cx.tr.t("codex_editor.owned_credential"));
    }
    let mut dismiss = false;
    if let Some(saved) = &ed.saved
        && saved.target == doc.target
    {
        egui::Frame::new()
            .fill(cx.pal.bg_subtle)
            .stroke(egui::Stroke::new(1.0, cx.pal.border))
            .corner_radius(radius::CONTROL)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.add(StatusDot::new(Status::Ok));
                    ui.vertical(|ui| {
                        ui.label(Text::strong(
                            cx.tr.f(
                                "codex_editor.saved.title",
                                &[("file", &saved.kind.file_name())],
                            ),
                            cx.pal,
                        ));
                        if let Some(view) = &saved.runtime {
                            let old: Vec<String> = view
                                .previous()
                                .map(|c| cx.tr.text(&c.role.text()))
                                .collect();
                            let text = if old.is_empty() {
                                cx.tr.t("codex_editor.saved.none_running")
                            } else {
                                cx.tr
                                    .f("codex_editor.saved.previous", &[("list", &old.join(", "))])
                            };
                            ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
                        }
                        for n in saved.notes.iter().filter(|n| {
                            saved.runtime.is_none() || n.text.key != "codex_files.restart_needed"
                        }) {
                            ui.add(
                                egui::Label::new(Text::small(cx.localize(&n.text), cx.pal)).wrap(),
                            );
                        }
                        ui.horizontal(|ui| {
                            if saved.runtime.is_some()
                                && ui
                                    .add(
                                        Button::new(cx.tr.t("codex_editor.saved.in_effect"))
                                            .small(),
                                    )
                                    .clicked()
                            {
                                out.push(Action::ShowRuntime);
                            }
                            if ui
                                .add(Button::ghost(cx.tr.t("codex_editor.saved.dismiss")).small())
                                .clicked()
                            {
                                dismiss = true;
                            }
                        });
                    });
                });
            });
        ui.add_space(4.0);
    }
    if dismiss {
        ed.saved = None;
    }
}

fn editor(ui: &mut Ui, cx: &Cx<'_>, ed: &mut Editor, height: f32) {
    let mono = mono_font(text::SMALL, Weight::Regular);
    let read_only = ed.doc.as_ref().is_some_and(|d| d.read_only.is_some()) || ed.saving();
    let id = Id::unique(EDITOR_ID);
    if let Some(at) = ed.jump.take() {
        let at = at.min(ed.buffer.len());
        let mut at = at;
        while !ed.buffer.is_char_boundary(at) {
            at -= 1;
        }
        let chars = ed.buffer[..at].chars().count();
        let mut state = TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(egui::text::CCursor::new(chars))));
        state.store(ui.ctx(), id);
        ui.ctx().memory_mut(|m| m.request_focus(id));
    }
    let lines = ed.buffer.split('\n').count().max(1);
    let width = lines.to_string().len();
    let numbers = (1..=lines)
        .map(|n| format!("{n:>width$}"))
        .collect::<Vec<_>>()
        .join("\n");
    let problems = ed.problems.clone();
    let kind = ed.kind();
    let pal = cx.pal;
    let font = mono.clone();
    let mut layouter = move |ui: &Ui, buf: &dyn egui::TextBuffer, _w: f32| {
        let job = files_highlight::layout(
            buf.as_str(),
            kind,
            &problems,
            pal,
            font.clone(),
            f32::INFINITY,
        );
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let name = cx
        .tr
        .f("codex_editor.a11y.editor", &[("file", &kind.file_name())]);
    egui::Frame::new()
        .fill(cx.pal.surface)
        .stroke(egui::Stroke::new(1.0, cx.pal.border))
        .corner_radius(radius::CONTROL)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ScrollArea::vertical()
                .id_salt("codex-files-scroll")
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        ui.add(
                            egui::Label::new(
                                RichText::new(numbers)
                                    .font(mono.clone())
                                    .color(cx.pal.text_subtle),
                            )
                            .selectable(false),
                        );
                        ScrollArea::horizontal()
                            .id_salt("codex-files-hscroll")
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                let edit = TextEdit::multiline(&mut ed.buffer)
                                    .id(id)
                                    .font(mono.clone())
                                    .code_editor()
                                    .layouter(&mut layouter)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(lines)
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::ZERO)
                                    .lock_focus(true)
                                    .interactive(!read_only);
                                ui.add(edit).accessible_name(name.clone());
                            });
                    });
                });
        });
}

fn problem_status(p: &FileProblem) -> Status {
    match p.severity {
        ProblemSeverity::Error => Status::Error,
        ProblemSeverity::Warning => Status::Warn,
        ProblemSeverity::Info => Status::Idle,
    }
}

fn problems(ui: &mut Ui, cx: &Cx<'_>, ed: &mut Editor, height: f32) {
    let mut jump = None;
    ScrollArea::vertical()
        .id_salt("codex-files-problems")
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for p in ed
                .problems
                .iter()
                .filter(|p| p.severity != ProblemSeverity::Info)
            {
                let r = ui
                    .horizontal(|ui| {
                        ui.add(StatusDot::new(problem_status(p)));
                        ui.label(Text::mono_muted(format!("{}:{}", p.line, p.column), cx.pal));
                        ui.add(
                            egui::Label::new(Text::small(cx.localize(&p.message), cx.pal))
                                .truncate(),
                        );
                    })
                    .response
                    .interact(Sense::click());
                if r.clicked() {
                    jump = Some(p.start);
                }
                r.on_hover_cursor(egui::CursorIcon::PointingHand);
            }
        });
    if jump.is_some() {
        ed.jump = jump;
    }
}

fn footer(ui: &mut Ui, cx: &Cx<'_>, ed: &mut Editor) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let lines = ed.buffer.split('\n').count();
        let errors = ed.blocking() + ed.forced();
        let mut parts = vec![cx.tr.f("codex_editor.status.lines", &[("n", &lines)])];
        if errors > 0 {
            parts.push(cx.tr.f("codex_editor.status.errors", &[("n", &errors)]));
        }
        if ed.warnings() > 0 {
            parts.push(
                cx.tr
                    .f("codex_editor.status.warnings", &[("n", &ed.warnings())]),
            );
        }
        if errors == 0 && ed.warnings() == 0 {
            parts.push(cx.tr.t("codex_editor.status.no_problems"));
        }
        let status = if errors > 0 {
            Status::Error
        } else if ed.warnings() > 0 {
            Status::Warn
        } else {
            Status::Ok
        };
        ui.add(StatusDot::new(status));
        ui.label(Text::small(parts.join(" · "), cx.pal));
        if let Some(doc) = &ed.doc
            && doc.kind == CodexFileKind::ConfigToml
        {
            let codex: Vec<String> = doc
                .schema
                .consumers
                .iter()
                .map(|c| match &c.version {
                    Some(v) => format!("{} {v}", c.label),
                    None => c.label.clone(),
                })
                .collect();
            let tip = if codex.is_empty() || doc.schema.catalog.is_none() {
                cx.tr.t("codex_editor.status.syntax_only")
            } else {
                cx.tr.f(
                    "codex_editor.status.checked",
                    &[("codex", &codex.join(", "))],
                )
            };
            ui.add(Badge::new(cx.tr.t("codex_editor.status.checks")).outline())
                .on_hover_text(tip);
        }
        if ed.dirty() {
            ui.add(Badge::new(cx.tr.t("codex_editor.status.unsaved")).status(Status::Warn));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let can = ed.dirty() && ed.blocking() == 0 && !ed.busy();
            let tip = if ed.blocking() > 0 {
                cx.tr.t("codex_editor.review_blocked")
            } else {
                cx.tr.t("codex_editor.review_tip")
            };
            if ui
                .add(
                    Button::primary(cx.tr.t("codex_editor.review_save"))
                        .kbd(if cfg!(target_os = "macos") {
                            "Cmd S"
                        } else {
                            "Ctrl S"
                        })
                        .enabled(can),
                )
                .on_hover_text(tip)
                .clicked()
            {
                ed.review();
            }
            if ed.dirty()
                && ui
                    .add(Button::ghost(cx.tr.t("codex_editor.discard")).enabled(!ed.busy()))
                    .clicked()
            {
                ed.dialog = Some(Dialog::Discard(Next::Revert));
            }
        });
    });
}

// ----------------------------------------------------------------- dialogs

fn dialogs(ctx: &egui::Context, cx: &Cx<'_>, ed: &mut Editor) {
    match ed.dialog.clone() {
        Some(Dialog::Review) => review(ctx, cx, ed),
        Some(Dialog::Backups) => backups(ctx, cx, ed),
        Some(Dialog::Discard(next)) => discard(ctx, cx, ed, next),
        None => {}
    }
}

/// The diff `old` -> `new` as colored lines with line numbers.
pub fn diff_view(ui: &mut Ui, cx: &Cx<'_>, salt: &str, old: &str, new: &str, height: f32) {
    let lines = codex_files::diff_lines(old, new);
    let stat = codex_files::stat(&lines);
    ui.label(Text::small(
        cx.tr.f(
            "codex_editor.diff.stat",
            &[("added", &stat.added), ("removed", &stat.removed)],
        ),
        cx.pal,
    ));
    let mono = mono_font(text::CAPTION, Weight::Regular);
    let hunks = codex_files::hunks(&lines, 3);
    egui::Frame::new()
        .fill(cx.pal.bg_subtle)
        .stroke(egui::Stroke::new(1.0, cx.pal.border))
        .corner_radius(radius::CONTROL)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ScrollArea::both()
                .id_salt(("codex-files-diff", salt))
                .max_height(height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if hunks.is_empty() {
                        ui.label(Text::small(cx.tr.t("codex_editor.diff.none"), cx.pal));
                    }
                    for (i, h) in hunks.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(4.0);
                        }
                        ui.label(
                            RichText::new(format!(
                                "@@ -{},{} +{},{} @@",
                                h.old_start, h.old_len, h.new_start, h.new_len
                            ))
                            .font(mono.clone())
                            .color(cx.pal.text_subtle),
                        );
                        for l in &h.lines {
                            let (sign, color, fill) = match l.tag {
                                DiffTag::Equal => (' ', cx.pal.text_muted, None),
                                DiffTag::Delete => {
                                    ('-', cx.pal.red, Some(cx.pal.red.gamma_multiply(0.12)))
                                }
                                DiffTag::Insert => {
                                    ('+', cx.pal.green, Some(cx.pal.green.gamma_multiply(0.12)))
                                }
                            };
                            let no =
                                |n: Option<usize>| n.map_or_else(String::new, |n| n.to_string());
                            let row =
                                format!("{:>4} {:>4} {sign} {}", no(l.old), no(l.new), l.text);
                            let mut rt = RichText::new(row).font(mono.clone()).color(color);
                            if let Some(f) = fill {
                                rt = rt.background_color(f);
                            }
                            ui.add(egui::Label::new(rt).extend());
                        }
                    }
                });
        });
}

fn review(ctx: &egui::Context, cx: &Cx<'_>, ed: &mut Editor) {
    let Some(doc) = ed.doc.clone() else {
        ed.dialog = None;
        return;
    };
    let file = doc.kind.file_name();
    let old = ed.original_view();
    let new = ed.buffer.clone();
    let (owned, credential) = ed.ownership_changes();
    let forced: Vec<FileProblem> = ed
        .problems
        .iter()
        .filter(|p| p.needs_force())
        .cloned()
        .collect();
    let warnings = ed.warnings();
    let width = (ctx.content_rect().width() - 80.0).clamp(420.0, 820.0);
    let diff_h = (ctx.content_rect().height() - 420.0).clamp(140.0, 360.0);
    let mut save = false;
    let mut cancel = false;
    let r = dialog(ctx, Id::unique("codex-files-review"), width, |ui| {
        ui.label(Text::h2(
            cx.tr.f("codex_editor.review.title", &[("file", &file)]),
            cx.pal,
        ));
        ui.add(
            egui::Label::new(Text::mono_muted(
                format!("{} · {}", target_label(cx, &doc.target), doc.path),
                cx.pal,
            ))
            .truncate(),
        );
        if let Some(SaveReason::Restore { backup }) = &ed.reason {
            ui.label(Text::small(
                cx.tr.f("codex_editor.review.restore", &[("id", backup)]),
                cx.pal,
            ));
        }
        diff_view(ui, cx, "review", &old, &new, diff_h);
        if ed.masked() {
            ui.label(Text::small(
                cx.tr.t("codex_editor.secrets_hidden_diff"),
                cx.pal,
            ));
        }
        if !owned.is_empty() || credential.is_some() {
            ui.add_space(4.0);
            ui.label(Text::strong(
                cx.tr.t("codex_editor.review.owned_title"),
                cx.pal,
            ));
            let unset = cx.tr.t("codex_editor.unset");
            for k in &owned {
                crate::pages::shared::values::change_row(
                    ui,
                    cx,
                    Id::unique(("codex-files-owned", &k.path)),
                    None,
                    &k.path,
                    k.before.as_deref(),
                    k.after.as_deref(),
                    None,
                    &unset,
                );
            }
            if let Some(c) = &credential {
                let before = cx.tr.text(&auth_shape_label(&c.written));
                let after = cx.tr.text(&auth_shape_label(&c.live));
                crate::pages::shared::values::change_row(
                    ui,
                    cx,
                    Id::unique("codex-files-owned-auth"),
                    None,
                    "auth.json",
                    Some(&before),
                    Some(&after),
                    None,
                    "—",
                );
            }
            ui.add(
                egui::Label::new(Text::small(
                    cx.tr
                        .text(&superba_core::codex_files::texts::owned_promise()),
                    cx.pal,
                ))
                .wrap(),
            );
        }
        if !forced.is_empty() {
            ui.add_space(4.0);
            ui.label(Text::strong(
                cx.tr
                    .f("codex_editor.review.errors", &[("n", &forced.len())]),
                cx.pal,
            ));
            for p in forced.iter().take(4) {
                ui.horizontal(|ui| {
                    ui.add(StatusDot::new(Status::Error));
                    ui.label(Text::mono_muted(format!("{}:{}", p.line, p.column), cx.pal));
                    ui.add(
                        egui::Label::new(Text::small(cx.localize(&p.message), cx.pal)).truncate(),
                    );
                });
            }
            ui.horizontal(|ui| {
                ui.add(Toggle::new(
                    &mut ed.force,
                    cx.tr.t("codex_editor.review.force"),
                ));
                ui.label(Text::small(cx.tr.t("codex_editor.review.force"), cx.pal));
            });
        } else if warnings > 0 {
            ui.label(Text::small(
                cx.tr.f("codex_editor.review.warnings", &[("n", &warnings)]),
                cx.pal,
            ));
        }
        ui.add(egui::Label::new(Text::muted(cx.tr.t("codex_editor.review.backup"), cx.pal)).wrap());
        ui.add(
            egui::Label::new(Text::muted(
                if doc.target.is_local() {
                    cx.tr.f("codex_editor.review.restart", &[("file", &file)])
                } else {
                    cx.tr.f(
                        "codex_editor.review.restart_remote",
                        &[("file", &file), ("host", &target_label(cx, &doc.target))],
                    )
                },
                cx.pal,
            ))
            .wrap(),
        );
        if let Some(e) = &ed.save_error {
            ui.horizontal_top(|ui| {
                ui.add(StatusDot::new(Status::Error));
                ui.add(egui::Label::new(Text::small(e, cx.pal)).wrap());
            });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can = forced.is_empty() || ed.force;
            if ui
                .add(
                    Button::primary(cx.tr.t("codex_editor.review.save"))
                        .loading(ed.saving())
                        .enabled(can && !ed.saving()),
                )
                .clicked()
            {
                save = true;
            }
            if ui
                .add(Button::ghost(cx.tr.t("codex_editor.review.cancel")).kbd("Esc"))
                .clicked()
            {
                cancel = true;
            }
        });
    });
    if save {
        ed.save(cx);
    }
    if (cancel || r.should_close()) && !ed.saving() {
        ed.dialog = None;
    }
}

fn backups(ctx: &egui::Context, cx: &Cx<'_>, ed: &mut Editor) {
    let file = ed.kind().file_name();
    let width = (ctx.content_rect().width() - 80.0).clamp(420.0, 820.0);
    let diff_h = (ctx.content_rect().height() - 360.0).clamp(140.0, 380.0);
    let mut close = false;
    let mut pick: Option<String> = None;
    let mut back = false;
    let mut restore = false;
    let current = ed.buffer.clone();
    let preview = ed.preview_view();
    let r = dialog(ctx, Id::unique("codex-files-backups"), width, |ui| {
        ui.label(Text::h2(
            cx.tr.f(
                "codex_editor.backups.title",
                &[("file", &file), ("where", &target_label(cx, &ed.target()))],
            ),
            cx.pal,
        ));
        if let Some(e) = &ed.backups_error {
            ui.horizontal_top(|ui| {
                ui.add(StatusDot::new(Status::Error));
                ui.add(egui::Label::new(Text::small(e, cx.pal)).wrap());
            });
        }
        match (&ed.preview, &preview) {
            (Some((b, _)), Some(view)) => {
                ui.label(Text::small(
                    cx.tr.f(
                        "codex_editor.backups.diff_title",
                        &[
                            ("time", &crate::pages::clock_time(b.at, false)),
                            ("date", &superba_switch::localtime::local(b.at).date()),
                            ("source", &cx.tr.text(&b.source.text())),
                        ],
                    ),
                    cx.pal,
                ));
                ui.label(Text::small(
                    cx.tr.t("codex_editor.backups.diff_hint"),
                    cx.pal,
                ));
                diff_view(ui, cx, "backup", &current, view, diff_h);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            Button::primary(cx.tr.t("codex_editor.backups.restore"))
                                .enabled(view != &current),
                        )
                        .clicked()
                    {
                        restore = true;
                    }
                    if ui
                        .add(Button::ghost(cx.tr.t("codex_editor.backups.back")))
                        .clicked()
                    {
                        back = true;
                    }
                });
            }
            _ => {
                match &ed.backups {
                    None => {
                        ui.label(Text::muted(cx.tr.t("codex_editor.backups.loading"), cx.pal));
                    }
                    Some(list) if list.is_empty() => {
                        ui.add(
                            egui::Label::new(Text::muted(
                                cx.tr.t("codex_editor.backups.empty"),
                                cx.pal,
                            ))
                            .wrap(),
                        );
                    }
                    Some(list) => {
                        ScrollArea::vertical()
                            .id_salt("codex-files-backup-list")
                            .max_height(diff_h + 40.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for b in list {
                                    let r = ui
                                        .horizontal(|ui| {
                                            ui.set_min_height(26.0);
                                            ui.add_sized(
                                                vec2(150.0, 20.0),
                                                egui::Label::new(Text::small(
                                                    format!(
                                                        "{} {}",
                                                        superba_switch::localtime::local(b.at)
                                                            .date(),
                                                        crate::pages::clock_time(b.at, false)
                                                    ),
                                                    cx.pal,
                                                )),
                                            );
                                            ui.add(
                                                egui::Label::new(Text::body(
                                                    cx.tr.text(&b.source.text()),
                                                    cx.pal,
                                                ))
                                                .truncate(),
                                            );
                                            ui.with_layout(
                                                Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    ui.label(Text::mono_muted(
                                                        cx.tr.f(
                                                            "codex_editor.backups.size",
                                                            &[("n", &b.size)],
                                                        ),
                                                        cx.pal,
                                                    ));
                                                },
                                            );
                                        })
                                        .response
                                        .interact(Sense::click());
                                    if r.hovered() {
                                        ui.painter().rect_filled(
                                            r.rect,
                                            radius::CONTROL,
                                            cx.pal.hover.gamma_multiply(0.5),
                                        );
                                    }
                                    if r.clicked() {
                                        pick = Some(b.id.clone());
                                    }
                                    r.on_hover_text(&b.id);
                                }
                            });
                        ui.label(Text::small(cx.tr.t("codex_editor.backups.hint"), cx.pal));
                    }
                }
                ui.add_space(8.0);
                if ui
                    .add(Button::ghost(cx.tr.t("codex_editor.close")).kbd("Esc"))
                    .clicked()
                {
                    close = true;
                }
            }
        }
    });
    if let Some(id) = pick {
        ed.preview_backup(cx, &id);
    }
    if back {
        ed.preview = None;
    }
    if restore {
        ed.restore_preview();
    } else if close || r.should_close() {
        ed.dialog = None;
        ed.preview = None;
    }
}

fn discard(ctx: &egui::Context, cx: &Cx<'_>, ed: &mut Editor, next: Next) {
    let file = ed.kind().file_name();
    let mut go = false;
    let mut keep = false;
    let r = dialog(ctx, Id::unique("codex-files-discard"), 440.0, |ui| {
        ui.label(Text::h2(
            cx.tr
                .f("codex_editor.discard_dialog.title", &[("file", &file)]),
            cx.pal,
        ));
        ui.add(
            egui::Label::new(Text::body(
                cx.tr.t("codex_editor.discard_dialog.body"),
                cx.pal,
            ))
            .wrap(),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add(Button::danger(
                    cx.tr.t("codex_editor.discard_dialog.confirm"),
                ))
                .clicked()
            {
                go = true;
            }
            if ui
                .add(Button::ghost(cx.tr.t("codex_editor.discard_dialog.keep")).kbd("Esc"))
                .clicked()
            {
                keep = true;
            }
        });
    });
    if go {
        ed.discard_and(cx, next);
    } else if keep || r.should_close() {
        ed.dialog = None;
    }
}
