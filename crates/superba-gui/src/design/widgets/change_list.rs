//! The changes a "write this file?" dialog is about to make: one row per key,
//! the key, then the value it gets (`before → after` when it had one). Nothing
//! is cut: keys and values wrap inside the width they get, and when the widest
//! key would leave the values too little room the value moves to its own line
//! under the key. Ids and URLs are set in the mono font, prose and the "(none)"
//! placeholder are not. A removal strikes the old value through and ends in a
//! red placeholder with a "removed" badge. Secrets come in already masked
//! (`••••a3f9`, [`superba_core::Secret::masked`]) and are shown as given.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, FontId, Label, Layout, Stroke, Ui, vec2};

use super::{Badge, Status};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::tokens::{Palette, space, text};

/// Below this many points beside the key column, values go under their key.
pub const MIN_VALUE_WIDTH: f32 = 260.0;
/// The key column never takes more than this share of the width.
const MAX_KEY_SHARE: f32 = 0.42;
/// Gap between the key and the value column.
const GAP: f32 = space::M;
/// Indent of a value under its key (stacked rows).
const INDENT: f32 = space::M;
/// The arrow between the old and the new value.
pub const ARROW: &str = " → ";

/// One changed key.
#[derive(Clone, Debug, Default)]
pub struct Change {
    /// The key (`env.ANTHROPIC_BASE_URL`, `config.toml · providers.superba`).
    pub key: String,
    /// The value now (`None`: absent).
    pub before: Option<String>,
    /// The value written (`None`: removed).
    pub after: Option<String>,
    /// A badge after the key ("changed by someone else").
    pub flag: Option<(String, Status)>,
}

impl Change {
    /// A change of `key` from `before` to `after`.
    pub fn new(key: impl Into<String>, before: Option<String>, after: Option<String>) -> Self {
        Self {
            key: key.into(),
            before,
            after,
            flag: None,
        }
    }

    /// Add a badge after the key.
    pub fn flag(mut self, text: impl Into<String>, status: Status) -> Self {
        self.flag = Some((text.into(), status));
        self
    }

    /// The key had a value and loses it.
    pub fn is_removal(&self) -> bool {
        self.before.is_some() && self.after.is_none()
    }
}

/// The words the list shows.
#[derive(Clone, Debug)]
pub struct ChangeLabels {
    /// An absent or removed value ("（无）", "(none)").
    pub none: String,
    /// The badge of a removal ("删除", "removed").
    pub removed: String,
}

/// The value is an id, a URL, a path, a number or a masked key: set in mono.
/// Text with spaces or outside ASCII (prose, a "(none)" placeholder) is not.
pub fn is_mono_value(s: &str) -> bool {
    !s.is_empty()
        && (s.starts_with('•')
            || s.chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '(' | ')')))
}

/// The value goes on its own line under the key: the widest key would wrap in
/// a key column of [`MAX_KEY_SHARE`] of `width`, or the values beside it would
/// get less than [`MIN_VALUE_WIDTH`].
pub fn stacked(width: f32, widest_key: f32) -> bool {
    widest_key > width * MAX_KEY_SHARE
        || width - key_column(width, widest_key) - GAP < MIN_VALUE_WIDTH
}

fn key_column(width: f32, widest_key: f32) -> f32 {
    widest_key.min(width * MAX_KEY_SHARE).ceil()
}

fn key_font() -> FontId {
    mono_font(text::CAPTION, Weight::Medium)
}

fn value_font(s: &str) -> FontId {
    if is_mono_value(s) {
        mono_font(text::CAPTION, Weight::Regular)
    } else {
        font(text::SMALL, Weight::Regular)
    }
}

/// The value of one change as one wrapping text.
pub fn value_job(c: &Change, labels: &ChangeLabels, pal: &Palette, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.break_anywhere = false;
    let plain = |s: &str, color: Color32| TextFormat {
        font_id: value_font(s),
        color,
        valign: Align::Center,
        ..Default::default()
    };
    match (&c.before, &c.after) {
        (Some(b), Some(a)) => {
            job.append(b, 0.0, plain(b, pal.text_muted));
            job.append(ARROW, 0.0, plain(ARROW, pal.text_subtle));
            job.append(a, 0.0, plain(a, pal.text));
        }
        (Some(b), None) => {
            let mut f = plain(b, pal.text_subtle);
            f.strikethrough = Stroke::new(1.0, pal.text_subtle);
            job.append(b, 0.0, f);
            job.append(ARROW, 0.0, plain(ARROW, pal.text_subtle));
            job.append(&labels.none, 0.0, plain(&labels.none, pal.red));
        }
        (None, Some(a)) => job.append(a, 0.0, plain(a, pal.text)),
        (None, None) => job.append(&labels.none, 0.0, plain(&labels.none, pal.text_subtle)),
    }
    job
}

/// The list. Draw it inside a dialog body; it takes the full width it gets.
pub struct ChangeList<'a> {
    changes: &'a [Change],
    labels: &'a ChangeLabels,
}

impl<'a> ChangeList<'a> {
    /// A list of `changes`.
    pub fn new(changes: &'a [Change], labels: &'a ChangeLabels) -> Self {
        Self { changes, labels }
    }

    /// Draw it.
    pub fn show(self, ui: &mut Ui) {
        let pal = Palette::of(ui.ctx());
        let width = ui.available_width();
        let widest = self
            .changes
            .iter()
            .map(|c| {
                ui.ctx()
                    .fonts_mut(|f| f.layout_no_wrap(c.key.clone(), key_font(), pal.text))
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        let stack = stacked(width, widest);
        let key_w = if stack {
            width
        } else {
            key_column(width, widest)
        };
        let value_w = if stack {
            width - INDENT
        } else {
            width - key_w - GAP
        };
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = space::XS;
            for (i, c) in self.changes.iter().enumerate() {
                if i > 0 {
                    let y = ui.cursor().top();
                    ui.painter()
                        .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, pal.border));
                    ui.add_space(space::XS);
                }
                let key = |ui: &mut Ui| {
                    ui.set_width(key_w);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = space::S;
                        ui.add(
                            Label::new(
                                egui::RichText::new(&c.key).font(key_font()).color(pal.text),
                            )
                            .wrap(),
                        );
                        if c.is_removal() {
                            ui.add(Badge::new(&self.labels.removed).status(Status::Error));
                        }
                        if let Some((t, s)) = &c.flag {
                            ui.add(Badge::new(t).status(*s));
                        }
                    });
                };
                let value = |ui: &mut Ui| {
                    ui.set_width(value_w);
                    ui.add(Label::new(value_job(c, self.labels, pal, value_w)).wrap());
                };
                if stack {
                    ui.allocate_ui_with_layout(vec2(key_w, 0.0), Layout::top_down(Align::Min), key);
                    ui.horizontal_top(|ui| {
                        ui.add_space(INDENT);
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.allocate_ui_with_layout(
                            vec2(value_w, 0.0),
                            Layout::top_down(Align::Min),
                            value,
                        );
                    });
                } else {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        ui.allocate_ui_with_layout(
                            vec2(key_w, 0.0),
                            Layout::top_down(Align::Min),
                            key,
                        );
                        ui.allocate_ui_with_layout(
                            vec2(value_w, 0.0),
                            Layout::top_down(Align::Min),
                            value,
                        );
                    });
                }
                ui.add_space(space::XS);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_urls_and_masked_keys_are_mono_prose_and_placeholders_are_not() {
        for v in [
            "https://api.example.com/anthropic",
            "orca/claude-opus-4-7",
            "••••a3f9",
            "200000",
            "[\"a\",\"b\"]",
        ] {
            assert!(is_mono_value(v), "{v}");
        }
        for v in ["（无）", "(none)", "two words", "", "思考"] {
            assert!(!is_mono_value(v), "{v}");
        }
    }

    #[test]
    fn a_wide_key_moves_values_under_it() {
        assert!(!stacked(700.0, 160.0));
        assert!(stacked(592.0, 400.0));
        assert!(stacked(300.0, 40.0));
    }

    #[test]
    fn a_removal_ends_in_the_placeholder() {
        let labels = ChangeLabels {
            none: "(none)".into(),
            removed: "removed".into(),
        };
        let pal = crate::design::tokens::LIGHT;
        let c = Change::new("k", Some("x".into()), None);
        assert!(c.is_removal());
        let job = value_job(&c, &labels, &pal, 100.0);
        assert_eq!(job.text, "x → (none)");
        assert!(job.sections[0].format.strikethrough.width > 0.0);
        let job = value_job(
            &Change::new("k", None, Some("y".into())),
            &labels,
            &pal,
            100.0,
        );
        assert_eq!(job.text, "y");
    }
}
