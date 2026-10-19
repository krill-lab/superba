//! Shared attention row used by the overview and application settings.
use super::Button;
use crate::design::{
    fonts::{Weight, font},
    icons::{self, Icon},
    tokens::text,
};
use egui::{Align, Color32, Layout, Rect, Sense, Ui, UiBuilder, pos2, vec2};
/// A named attention item with a single repair action.
pub struct AttentionRow<'a> {
    /// Stable identity within the list.
    pub id: usize,
    /// Title.
    pub title: &'a str,
    /// Optional explanation.
    pub detail: Option<&'a str>,
    /// Action label.
    pub label: &'a str,
    /// Action help.
    pub tip: &'a str,
    /// State icon.
    pub icon: Icon,
    /// State tint.
    pub color: Color32,
}
impl AttentionRow<'_> {
    /// Allocate a standard attention row.
    pub fn show(self, ui: &mut Ui) -> bool {
        let (_, row) = ui.allocate_space(vec2(ui.available_width(), 52.0));
        self.show_at(ui, row)
    }

    /// Paint into a preallocated row; returns whether its action was clicked.
    pub fn show_at(self, ui: &mut Ui, row: Rect) -> bool {
        let Self {
            title: title_text,
            detail,
            label,
            tip,
            icon,
            color,
            id,
        } = self;
        let i = id;
        let pal = crate::design::tokens::Palette::of(ui.ctx());
        let button = Button::new(label).small();
        let bw = button.desired_size(ui).x;
        // The button on the right, centred vertically.
        let brect =
            Rect::from_min_size(pos2(row.max.x - bw, row.center().y - 14.0), vec2(bw, 28.0));
        // The to-do's text first (screen readers read it before its button; the
        // whole text is on hover too, rows are one line each).
        let text_rect = Rect::from_min_max(
            pos2(row.min.x, row.min.y),
            pos2(brect.min.x - 6.0, row.max.y),
        );
        let full = match &detail {
            Some(d) if !d.is_empty() => format!("{}\n{d}", title_text),
            _ => title_text.to_owned(),
        };
        let text_resp = ui.interact(
            text_rect,
            ui.scope_id().with(("attention-text", i)),
            Sense::hover(),
        );
        crate::design::a11y::text(&text_resp, &full.replace('\n', ". "));
        text_resp.on_hover_text(full);
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(brect)
                .id_salt(("attention-fix", i))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let action = child.add(button).on_hover_text(tip);
        let clicked = action.clicked();
        let p = ui.painter();
        let ic = pos2(row.min.x + 10.0, row.center().y);
        icons::paint(p, Rect::from_center_size(ic, vec2(16.0, 16.0)), icon, color);
        let text_x = row.min.x + 28.0;
        let max_w = (brect.min.x - 10.0 - text_x).max(40.0);
        let galley = |s: &str, size: f32, weight: Weight, color: Color32| {
            let mut job = egui::text::LayoutJob::single_section(
                s.to_owned(),
                egui::TextFormat {
                    font_id: font(size, weight),
                    color,
                    ..Default::default()
                },
            );
            job.wrap.max_width = max_w;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            p.layout_job(job)
        };
        let title = galley(title_text, text::SMALL, Weight::Medium, pal.text);
        match &detail {
            Some(d) if !d.is_empty() => {
                let detail = galley(d, text::CAPTION, Weight::Regular, pal.text_subtle);
                let h = title.size().y + detail.size().y + 1.0;
                let y = row.center().y - h / 2.0;
                let th = title.size().y;
                p.galley(pos2(text_x, y), title, pal.text);
                p.galley(pos2(text_x, y + th + 1.0), detail, pal.text_subtle);
            }
            _ => {
                let th = title.size().y;
                p.galley(pos2(text_x, row.center().y - th / 2.0), title, pal.text);
            }
        }
        clicked
    }
}
