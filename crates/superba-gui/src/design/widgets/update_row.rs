//! Compact update inventory rows. Expanding an item and running its operation
//! are independent actions; both remain named when text is elided.
use super::{Badge, Button, InlineProgress, Status, focus_ring, show_focus};
use crate::design::{
    a11y,
    fonts::{Weight, font, mono_font},
    icons::{self, Icon},
    tokens::{Palette, size, text},
};
use egui::{Align, Color32, Layout, Rect, Response, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

/// A group heading with a right-aligned summary or link slot.
pub struct UpdateGroup<'a>(pub &'a str);
impl UpdateGroup<'_> {
    /// Draw the muted heading and trailing content.
    pub fn show<R>(self, ui: &mut Ui, trailing: impl FnOnce(&mut Ui) -> R) -> R {
        ui.horizontal(|ui| {
            ui.label(super::Text::small(self.0, Palette::of(ui.ctx())));
            ui.with_layout(Layout::right_to_left(Align::Center), trailing)
                .inner
        })
        .inner
    }
}

/// Identity at the start of an update row.
#[derive(Clone, Copy)]
pub enum UpdateTile<'a> {
    /// Vector icon on a neutral tile.
    Icon(Icon),
    /// Two letters for tools without artwork.
    Monogram(&'a str),
}

/// A named install/update item, independent of tool backends.
pub struct UpdateRow<'a> {
    id: &'a str,
    name: &'a str,
    state: &'a str,
    versions: &'a str,
    description: &'a str,
    status: Status,
    hollow: bool,
    tile: UpdateTile<'a>,
    action: Option<(&'a str, Option<&'a str>)>,
    action_enabled: bool,
    progress: Option<(Option<f32>, &'a str)>,
}
/// The item expander and its independent operation.
pub struct UpdateResponse {
    /// Full row, with expanded state in AccessKit.
    pub response: Response,
    /// Operation button response (its name includes the tool).
    pub action: Option<Response>,
    /// Operation was clicked; expanding does not set this.
    pub action_clicked: bool,
}
impl<'a> UpdateRow<'a> {
    /// Stable id, item name and localized update state.
    pub fn new(id: &'a str, name: &'a str, state: &'a str) -> Self {
        Self {
            id,
            name,
            state,
            versions: "",
            description: "",
            status: Status::Idle,
            hollow: false,
            tile: UpdateTile::Icon(Icon::Download),
            action: None,
            action_enabled: true,
            progress: None,
        }
    }
    /// Versions, including the old/new separator.
    pub fn versions(mut self, value: &'a str) -> Self {
        self.versions = value;
        self
    }
    /// Explanation; error rows show it in red.
    pub fn description(mut self, value: &'a str) -> Self {
        self.description = value;
        self
    }
    /// Semantic status; busy displays a spinner.
    pub fn status(mut self, value: Status) -> Self {
        self.status = value;
        self
    }
    /// Empty status circle for an uninstalled or disconnected item.
    pub fn hollow(mut self) -> Self {
        self.hollow = true;
        self
    }
    /// Icon or two-letter identity.
    pub fn tile(mut self, value: UpdateTile<'a>) -> Self {
        self.tile = value;
        self
    }
    /// Visible action label; its accessible name also includes the tool name.
    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some((label, None));
        self
    }
    /// Explicit localized action name when its grammar needs more than appending the tool.
    pub fn action_name(mut self, name: &'a str) -> Self {
        if let Some((_, ref mut accessible)) = self.action {
            *accessible = Some(name);
        }
        self
    }
    /// Disable an unavailable operation without hiding it.
    pub fn action_enabled(mut self, enabled: bool) -> Self {
        self.action_enabled = enabled;
        self
    }
    /// Optional transfer fraction and localized size/progress description.
    pub fn progress(mut self, fraction: Option<f32>, description: &'a str) -> Self {
        self.progress = Some((fraction, description));
        self
    }
    /// Height before an expansion panel (progress occupies the bottom edge).
    pub fn height(width: f32) -> f32 {
        if width >= 700.0 {
            size::UPDATE_ROW
        } else {
            size::UPDATE_ROW_NARROW
        }
    }
    /// Unshortened name exposed by the expander.
    pub fn accessible_name(&self) -> String {
        [self.name, self.state, self.versions, self.description]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// Draw the row and expansion panel.
    pub fn show(
        self,
        ui: &mut Ui,
        expanded: &mut bool,
        detail: impl FnOnce(&mut Ui),
    ) -> UpdateResponse {
        let width = self.action.map_or(0.0, |(label, _)| {
            Button::new(label).small().desired_size(ui).x
        });
        let (response, action) = self.show_with_action(
            ui,
            expanded,
            width,
            |ui| {
                self.action.map(|(label, name)| {
                    let r = ui.add(Button::new(label).small().enabled(self.action_enabled));
                    let name = name
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{label} {}", self.name));
                    a11y::describe(&r, egui::Role::Button, &name);
                    r
                })
            },
            detail,
        );
        let action_clicked = action.as_ref().is_some_and(Response::clicked);
        UpdateResponse {
            response,
            action,
            action_clicked,
        }
    }
    /// Draw with a custom operation slot. The caller supplies its width and
    /// names its controls with the tool name. The slot never invokes expansion.
    pub fn show_with_action<R>(
        &self,
        ui: &mut Ui,
        expanded: &mut bool,
        action_width: f32,
        action: impl FnOnce(&mut Ui) -> R,
        detail: impl FnOnce(&mut Ui),
    ) -> (Response, R) {
        ui.push_id(self.id, |ui| {
            let narrow = ui.available_width() < 700.0;
            let width = ui.available_width();
            let height = Self::height(width);
            let (id, row) = ui.allocate_space(vec2(width, height));
            let response = ui.interact(row, id, Sense::click());
            let pal = Palette::of(ui.ctx());
            let action_width = action_width.clamp(0.0, (width - 80.0).max(0.0));
            let action_rect = Rect::from_min_size(
                pos2(row.right() - 28.0 - action_width, row.center().y - 14.0),
                vec2(action_width, 28.0),
            );
            let in_action = action_width > 0.0
                && ui
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| action_rect.contains(p));
            if ui.is_rect_visible(row) && response.hovered() && !in_action {
                ui.painter()
                    .rect_filled(row, 6, pal.hover.gamma_multiply(0.4));
            }
            let mut slot = ui.new_child(
                UiBuilder::new()
                    .id_salt("operation")
                    .max_rect(action_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            slot.set_clip_rect(action_rect.intersect(ui.clip_rect()));
            let result = action(&mut slot);
            if response.clicked()
                && !(response.clicked_by(egui::PointerButton::Primary) && in_action)
            {
                *expanded = !*expanded;
            }
            if ui.is_enabled() {
                ui.input_mut(|input| {
                    input.consume_accesskit_action_requests(id, |request| match request.action {
                        egui::accesskit::Action::Expand => {
                            *expanded = true;
                            true
                        }
                        egui::accesskit::Action::Collapse => {
                            *expanded = false;
                            true
                        }
                        _ => false,
                    })
                });
            }
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::Role::Button,
                    ui.is_enabled(),
                    self.accessible_name(),
                )
            });
            ui.ctx().accesskit_node_builder(id, |n| {
                n.set_expanded(*expanded);
                if self.status == Status::Busy {
                    n.set_busy();
                }
                n.add_action(if *expanded {
                    egui::accesskit::Action::Collapse
                } else {
                    egui::accesskit::Action::Expand
                });
            });
            if ui.is_rect_visible(row) {
                let center = pos2(row.left() + 19.0, row.center().y);
                match self.tile {
                    UpdateTile::Icon(icon) => super::IconTile::new(icon).paint_at(ui, center),
                    UpdateTile::Monogram(s) => {
                        super::MonogramTile::new(s).paint_at(ui, center);
                    }
                }
                let label_left = row.left() + 44.0;
                let before_action = action_rect.left() - 10.0;
                let state_g = ui.painter().layout_no_wrap(
                    self.state.to_owned(),
                    font(text::CAPTION, Weight::Medium),
                    pal.text_muted,
                );
                let badge_width = (state_g.size().x + 28.0)
                    .min((before_action - label_left) * 0.5)
                    .max(28.0);
                let state_rect = Rect::from_min_size(
                    pos2(
                        before_action - badge_width,
                        if narrow {
                            row.top() + 4.0
                        } else {
                            row.center().y - 10.0
                        },
                    ),
                    vec2(badge_width, 20.0),
                );
                let mut badge_ui = ui.new_child(
                    UiBuilder::new()
                        .id_salt("state")
                        .max_rect(state_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                badge_ui.set_clip_rect(state_rect.intersect(ui.clip_rect()));
                let mut badge = Badge::new(self.state).status(self.status);
                if self.hollow {
                    badge = badge.hollow();
                }
                badge_ui.add(badge);
                let version_width = if narrow {
                    0.0
                } else {
                    (width * 0.22).clamp(110.0, 190.0)
                };
                let title_right = state_rect.left() - 10.0 - version_width;
                paint_line(
                    ui,
                    self.name,
                    Rect::from_min_max(
                        pos2(
                            label_left,
                            if narrow {
                                row.top() + 2.0
                            } else if self.description.is_empty() {
                                row.top() + 12.0
                            } else {
                                row.top() + 3.0
                            },
                        ),
                        pos2(title_right, row.top() + 24.0),
                    ),
                    pal.text,
                    false,
                );
                let color = if self.status == Status::Error {
                    pal.status(Status::Error)
                } else {
                    pal.text_muted
                };
                if narrow {
                    let secondary = [self.versions, self.description]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    paint_line(
                        ui,
                        &secondary,
                        Rect::from_min_max(
                            pos2(label_left, row.top() + 25.0),
                            pos2(before_action, row.bottom() - 2.0),
                        ),
                        color,
                        true,
                    );
                } else {
                    if !self.description.is_empty() {
                        paint_line(
                            ui,
                            self.description,
                            Rect::from_min_max(
                                pos2(label_left, row.top() + 22.0),
                                pos2(title_right, row.bottom() - 2.0),
                            ),
                            color,
                            true,
                        );
                    }
                    paint_version(
                        ui,
                        self.versions,
                        Rect::from_min_max(
                            pos2(title_right + 8.0, row.top() + 12.0),
                            pos2(state_rect.left() - 8.0, row.bottom() - 10.0),
                        ),
                        pal.text_muted,
                    );
                }
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(
                        pos2(row.right() - 12.0, row.center().y),
                        vec2(14.0, 14.0),
                    ),
                    if *expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    pal.text_muted,
                );
                ui.painter()
                    .hline(row.x_range(), row.bottom(), Stroke::new(1.0, pal.border));
                if show_focus(&response) {
                    focus_ring(ui.painter(), row, 6, pal);
                }
            }
            if let Some((fraction, label)) = self.progress {
                let track = Rect::from_min_max(
                    pos2(row.left() + 44.0, row.bottom() - 3.0),
                    pos2(row.right() - 28.0, row.bottom()),
                );
                let mut progress_ui = ui.new_child(
                    UiBuilder::new()
                        .id_salt("progress")
                        .max_rect(track)
                        .layout(Layout::top_down(Align::Min)),
                );
                progress_ui.add(InlineProgress::new(fraction, label));
            }
            if *expanded {
                ui.push_id("details", detail);
            }
            (response.on_hover_text(self.accessible_name()), result)
        })
        .inner
    }
}
fn paint_version(ui: &Ui, value: &str, rect: Rect, color: Color32) {
    let g = super::log_table::elided(
        ui,
        value,
        mono_font(text::CAPTION, Weight::Regular),
        color,
        rect.width().max(0.0),
    );
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            pos2(rect.left(), rect.center().y - g.size().y / 2.0),
            g,
            color,
        );
}
fn paint_line(ui: &Ui, value: &str, rect: Rect, color: Color32, small: bool) {
    let f = font(
        if small { text::CAPTION } else { text::SMALL },
        if small {
            Weight::Regular
        } else {
            Weight::Medium
        },
    );
    let g = super::log_table::elided(ui, value, f, color, rect.width().max(0.0));
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            pos2(rect.left(), rect.center().y - g.size().y / 2.0),
            g,
            color,
        );
}
