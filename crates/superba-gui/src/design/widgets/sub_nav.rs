//! Section rail: identity, groups, named tabs and keyboard navigation.
use super::{focus_ring, show_focus};
use crate::design::{
    a11y,
    fonts::{Weight, font, mono_font},
    icons::{self, Icon},
    tokens::{Palette, size, text},
};
use egui::{Key, Rect, Response, Sense, Ui, pos2, vec2};

/// One section; all labels and status descriptions are supplied by the page.
pub struct SubNavItem<'a> {
    id: &'a str,
    label: &'a str,
    icon: Icon,
    badge: Option<&'a str>,
    dot: Option<&'a str>,
    group: Option<&'a str>,
    identity: bool,
}
impl<'a> SubNavItem<'a> {
    /// A stable id, localized label and icon.
    pub fn new(id: &'a str, label: &'a str, icon: Icon) -> Self {
        Self {
            id,
            label,
            icon,
            badge: None,
            dot: None,
            group: None,
            identity: false,
        }
    }
    /// Count shown at the right, also exposed as an accessible description.
    pub fn badge(mut self, value: &'a str) -> Self {
        self.badge = Some(value);
        self
    }
    /// A pending dot, with a localized accessible description of its meaning.
    pub fn dot(mut self, description: &'a str) -> Self {
        self.dot = Some(description);
        self
    }
    /// Heading above this section.
    pub fn group(mut self, title: &'a str) -> Self {
        self.group = Some(title);
        self
    }
    /// Larger brand identity row for the home section.
    pub fn identity(mut self) -> Self {
        self.identity = true;
        self
    }
}

/// Fixed section navigation; the caller owns the selected index.
pub struct SubNav<'a> {
    items: &'a [SubNavItem<'a>],
    footer: Option<&'a str>,
}
impl<'a> SubNav<'a> {
    /// Sections in keyboard order.
    pub fn new(items: &'a [SubNavItem<'a>]) -> Self {
        Self {
            items,
            footer: None,
        }
    }
    /// Hint pinned below the items when space permits.
    pub fn footer(mut self, text: &'a str) -> Self {
        self.footer = Some(text);
        self
    }
    /// Draw tabs, returning the selected tab's response. Up/down wrap and move
    /// keyboard focus with selection; Enter, Space and UI Automation invoke a tab.
    pub fn show(self, ui: &mut Ui, selected: &mut usize) -> Option<Response> {
        if self.items.is_empty() {
            return None;
        }
        *selected = (*selected).min(self.items.len() - 1);
        let ids: Vec<_> = self
            .items
            .iter()
            .map(|item| ui.make_persistent_id(("section", item.id)))
            .collect();
        if ui.is_enabled()
            && let Some(focused) = ids.iter().position(|id| ui.memory(|m| m.has_focus(*id)))
        {
            let delta = ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                    1
                } else if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                    -1
                } else {
                    0
                }
            });
            if delta != 0 {
                *selected = (focused as isize + delta).rem_euclid(ids.len() as isize) as usize;
                ui.memory_mut(|m| {
                    // egui already scheduled spatial focus navigation at the
                    // start of this pass. Cancel it when the rail handles the
                    // key, including the first pass after focus changed.
                    m.move_focus(egui::FocusDirection::None);
                    m.request_focus(ids[*selected]);
                });
            }
        }
        let pal = Palette::of(ui.ctx());
        let mut rows = Vec::with_capacity(self.items.len());
        for (index, item) in self.items.iter().enumerate() {
            if let Some(group) = item.group {
                ui.add_space(8.0);
                ui.label(super::Text::small(group, pal));
                ui.add_space(2.0);
            }
            let height = if item.identity {
                size::IDENTITY_ROW
            } else {
                size::RAIL_ROW
            };
            let (_, r) = ui.allocate_space(vec2(ui.available_width(), height));
            let response = ui.interact(r, ids[index], Sense::click());
            if response.has_focus() {
                ui.memory_mut(|m| {
                    m.set_focus_lock_filter(
                        response.id,
                        egui::EventFilter {
                            vertical_arrows: true,
                            ..Default::default()
                        },
                    );
                });
            }
            if response.clicked() {
                *selected = index;
            }
            rows.push(response);
        }
        // Resolve clicks before publishing selection: there must be exactly
        // one selected Tab in this frame, even when a later row was clicked.
        let mut selected_response = None;
        for (index, (item, response)) in self.items.iter().zip(rows).enumerate() {
            let r = response.rect;
            let on = index == *selected;
            if on || response.hovered() {
                ui.painter()
                    .rect_filled(r, 6, if on { pal.active } else { pal.hover });
            }
            let icon_side = if item.identity { 26.0 } else { 16.0 };
            let left = if item.identity { 44.0 } else { 32.0 };
            icons::paint(
                ui.painter(),
                Rect::from_center_size(
                    r.left_center() + vec2(left / 2.0, 0.0),
                    vec2(icon_side, icon_side),
                ),
                item.icon,
                pal.text_muted,
            );
            let mut right = r.right() - 8.0;
            if let Some(badge) = item.badge {
                let g = ui.painter().layout_no_wrap(
                    badge.to_owned(),
                    mono_font(text::CAPTION, Weight::Regular),
                    pal.text_muted,
                );
                right -= g.size().x;
                ui.painter().galley(
                    pos2(right, r.center().y - g.size().y / 2.0),
                    g,
                    pal.text_muted,
                );
                right -= 8.0;
            }
            if item.dot.is_some() {
                ui.painter()
                    .circle_filled(pos2(right - 3.0, r.center().y), 3.0, pal.text_muted);
                right -= 14.0;
            }
            let g = super::log_table::elided(
                ui,
                item.label,
                font(
                    text::SMALL,
                    if on { Weight::Medium } else { Weight::Regular },
                ),
                pal.text,
                (right - r.left() - left).max(0.0),
            );
            ui.painter().galley(
                pos2(r.left() + left, r.center().y - g.size().y / 2.0),
                g,
                pal.text,
            );
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Tab, ui.is_enabled(), on, item.label)
            });
            a11y::selectable(&response, egui::Role::Tab, item.label, on);
            let description = [item.badge, item.dot]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
            if !description.is_empty() {
                ui.ctx()
                    .accesskit_node_builder(response.id, |n| n.set_description(description));
            }
            if show_focus(&response) {
                focus_ring(ui.painter(), r, 6, pal);
            }
            if on {
                selected_response = Some(response);
            }
        }
        if let Some(footer) = self.footer {
            ui.add_space((ui.available_height() - 36.0).max(8.0));
            ui.add(egui::Label::new(super::Text::small(footer, pal)).wrap());
        }
        selected_response
    }
}
