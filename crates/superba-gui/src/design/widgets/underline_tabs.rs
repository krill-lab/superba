//! A single row of named tabs with an underline and optional status markers.
use super::{focus_ring, show_focus};
use crate::design::{
    a11y,
    fonts::{Weight, font},
    tokens::{Palette, text},
};
use egui::{Key, Modifiers, Response, Sense, Stroke, Ui, pos2, vec2};

/// A tab's stable identity, visible name and accessible status description.
pub struct UnderlineTab<'a> {
    /// Stable identity, independent of the locale.
    pub id: &'a str,
    /// Visible and accessible name.
    pub label: &'a str,
    /// An optional count, drawn beside the name.
    pub count: Option<u32>,
    /// An optional attention dot.
    pub attention: bool,
    /// Localized explanation of the count or dot.
    pub description: Option<&'a str>,
}

/// A horizontal tab list. Left/right, Home and End move selection and focus.
pub struct UnderlineTabs<'a> {
    /// Tabs in visual and keyboard order.
    pub items: &'a [UnderlineTab<'a>],
    /// Accessible name of the tab list.
    pub label: &'a str,
}

impl UnderlineTabs<'_> {
    /// Draw the list and return the selected tab for entering/leaving its panel.
    pub fn show(self, ui: &mut Ui, selected: &mut usize) -> Option<Response> {
        if self.items.is_empty() {
            return None;
        }
        *selected = (*selected).min(self.items.len() - 1);
        ui.scope_builder(
            egui::UiBuilder::new()
                .id_salt("underline-tabs")
                .accessibility_role(egui::Role::TabList)
                .accessibility_label(self.label),
            |ui| {
                let ids: Vec<_> = self
                    .items
                    .iter()
                    .map(|t| ui.make_persistent_id(t.id))
                    .collect();
                if ui.is_enabled()
                    && let Some(focused) = ids.iter().position(|id| ui.memory(|m| m.has_focus(*id)))
                {
                    let next = ui.input_mut(|i| {
                        if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
                            Some((focused + 1) % ids.len())
                        } else if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
                            Some((focused + ids.len() - 1) % ids.len())
                        } else if i.consume_key(Modifiers::NONE, Key::Home) {
                            Some(0)
                        } else if i.consume_key(Modifiers::NONE, Key::End) {
                            Some(ids.len() - 1)
                        } else {
                            None
                        }
                    });
                    if let Some(next) = next {
                        *selected = next;
                        ui.memory_mut(|m| {
                            m.move_focus(egui::FocusDirection::None);
                            m.request_focus(ids[next]);
                        });
                    }
                }
                let pal = Palette::of(ui.ctx());
                let (track, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
                ui.painter().hline(
                    track.x_range(),
                    track.bottom() - 0.5,
                    Stroke::new(1.0, pal.border),
                );
                let mut x = track.left();
                let mut rows = Vec::new();
                for (item, id) in self.items.iter().zip(ids) {
                    let name = ui.painter().layout_no_wrap(
                        item.label.into(),
                        font(text::BODY, Weight::Medium),
                        pal.text,
                    );
                    let count = item.count.map(|n| {
                        ui.painter().layout_no_wrap(
                            n.to_string(),
                            font(text::CAPTION, Weight::Medium),
                            pal.text_muted,
                        )
                    });
                    let marker = count
                        .as_ref()
                        .map_or(if item.attention { 14.0 } else { 0.0 }, |g| {
                            g.size().x + 14.0
                        });
                    let rect = egui::Rect::from_min_size(
                        pos2(x, track.top()),
                        vec2(name.size().x + marker + 24.0, track.height()),
                    );
                    let response = ui
                        .interact(rect, id, Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if response.has_focus() {
                        ui.memory_mut(|m| {
                            m.set_focus_lock_filter(
                                id,
                                egui::EventFilter {
                                    horizontal_arrows: true,
                                    ..Default::default()
                                },
                            )
                        });
                    }
                    if response.clicked() {
                        *selected = rows.len();
                        response.request_focus();
                    }
                    x = rect.right() + 6.0;
                    rows.push((response, name, count));
                }
                let mut current = None;
                for (index, (item, (response, name, count))) in
                    self.items.iter().zip(rows).enumerate()
                {
                    let on = index == *selected;
                    let rect = response.rect;
                    let color = if on || response.hovered() {
                        pal.text
                    } else {
                        pal.text_muted
                    };
                    let marker_x = rect.left() + 12.0 + name.size().x + 8.0;
                    ui.painter().galley_with_override_text_color(
                        pos2(rect.left() + 12.0, rect.center().y - name.size().y / 2.0),
                        name,
                        color,
                    );
                    if let Some(count) = count {
                        ui.painter().galley(
                            pos2(marker_x, rect.center().y - count.size().y / 2.0),
                            count,
                            pal.text_muted,
                        );
                    } else if item.attention {
                        ui.painter().circle_filled(
                            pos2(marker_x + 3.0, rect.center().y),
                            3.0,
                            pal.amber,
                        );
                    }
                    if on {
                        ui.painter().hline(
                            (rect.left() + 8.0)..=(rect.right() - 8.0),
                            track.bottom() - 1.0,
                            Stroke::new(2.0, pal.text),
                        );
                    }
                    a11y::selectable(&response, egui::Role::Tab, item.label, on);
                    if let Some(description) = item.description {
                        ui.ctx().accesskit_node_builder(response.id, |n| {
                            n.set_description(description)
                        });
                    }
                    if show_focus(&response) {
                        focus_ring(ui.painter(), rect.shrink(3.0), 4, pal);
                    }
                    if on {
                        current = Some(response);
                    }
                }
                current
            },
        )
        .inner
    }
}
