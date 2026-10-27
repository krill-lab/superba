//! A three-stop speed picker, styled like the Codex model slider.

use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::text;
use crate::design::widgets::{Button, Text, focus_ring, show_focus};
use crate::pages::{Action, Cx};
use egui::{Align, Align2, Color32, Layout, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::time::Duration;
use superba_core::SpeedTier;

#[path = "simple_speed/starfield.rs"]
mod starfield;

#[derive(Clone)]
struct Draft {
    profile: Option<superba_core::ProfileId>,
    applied: SpeedTier,
    selected: SpeedTier,
}

/// Color of a speed stop, shared by its title, fill and option label.
pub fn color(tier: SpeedTier) -> Color32 {
    match tier {
        SpeedTier::Standard => Color32::from_rgb(48, 145, 250),
        SpeedTier::Fast => Color32::from_rgb(31, 171, 111),
        SpeedTier::Ultrafast => Color32::from_rgb(151, 87, 226),
        _ => Color32::from_rgb(48, 145, 250),
    }
}

fn label(cx: &Cx<'_>, tier: SpeedTier) -> String {
    cx.tr.t(match tier {
        SpeedTier::Standard => "overview.simple.speed.standard",
        SpeedTier::Fast => "overview.simple.speed.fast",
        SpeedTier::Ultrafast => "overview.simple.speed.ultrafast",
        _ => "overview.simple.speed.standard",
    })
}

pub(super) fn popover(
    response: &egui::Response,
    ui: &Ui,
    cx: &Cx<'_>,
    model: &str,
    out: &mut Vec<Action>,
) {
    egui::Popup::from_toggle_button_response(response)
        .frame(
            egui::Frame::popup(ui.style())
                .corner_radius(16)
                .inner_margin(12),
        )
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(300.0);
            let active = super::speed_support(cx);
            let current = active.map_or(SpeedTier::Standard, |(p, _)| {
                p.model.speed.unwrap_or_default()
            });
            let profile = active.map(|(p, _)| p.id.clone());
            let support = active.and_then(|(_, s)| s);
            let offered = support.map_or(&[SpeedTier::Standard][..], |s| s.offered.as_slice());
            let id = response.id.with("speed-draft");
            let mut draft = ui.ctx().data(|d| d.get_temp::<Draft>(id)).unwrap_or(Draft {
                profile: profile.clone(),
                applied: current,
                selected: current,
            });
            if response.clicked() || draft.profile != profile || draft.applied != current {
                draft = Draft {
                    profile: profile.clone(),
                    applied: current,
                    selected: current,
                };
            }
            let enabled = !cx.busy && support.is_some();
            let mut apply = false;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(label(cx, draft.selected))
                        .font(font(16.0, Weight::Semibold))
                        .color(color(draft.selected)),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    apply = ui
                        .add(
                            Button::ghost(cx.tr.t("overview.simple.speed.apply"))
                                .icon(Icon::Check)
                                .small()
                                .enabled(enabled && draft.selected != current),
                        )
                        .clicked();
                });
            });
            let reduced = ui
                .ctx()
                .data(|data| data.get_temp::<bool>(egui::Id::unique("superba-reduce-motion")))
                .unwrap_or_else(|| {
                    cx.data
                        .profiles
                        .value
                        .as_ref()
                        .is_some_and(|p| p.settings.reduce_motion)
                });
            if let Some(tier) = picker(ui, cx, draft.selected, offered, model, enabled, reduced) {
                draft.selected = tier;
            }
            if apply {
                if let Some(profile) = profile {
                    out.push(Action::SetSpeed {
                        profile,
                        tier: draft.selected,
                    });
                    ui.close();
                }
            }
            if active.is_none() {
                ui.add(
                    egui::Label::new(Text::small(cx.tr.t("overview.hero.tier_app"), cx.pal)).wrap(),
                );
            }
            ui.ctx().data_mut(|d| d.insert_temp(id, draft));
        });
}

/// Return a supported user selection; all three stops remain visible.
pub fn picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    current: SpeedTier,
    offered: &[SpeedTier],
    _model: &str,
    enabled: bool,
    reduced_motion: bool,
) -> Option<SpeedTier> {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
    let track = Rect::from_min_max(
        pos2(rect.min.x + 3.0, rect.min.y + 10.0),
        pos2(rect.max.x - 3.0, rect.min.y + 38.0),
    );
    let stops = [track.min.x + 14.0, track.center().x, track.max.x - 14.0];
    let index = SpeedTier::ALL
        .iter()
        .position(|t| *t == current)
        .unwrap_or(0);
    let position = ui
        .ctx()
        .animate_value_with_time(
            ui.scope_id().with("simple-speed-position"),
            index as f32,
            if reduced_motion { 0.0 } else { 0.24 },
        )
        .clamp(0.0, 2.0);
    let left = position.floor() as usize;
    let right = (left + 1).min(2);
    let blend = position.fract();
    let fill_end = egui::lerp(stops[left]..=stops[right], blend);
    let accent = color(SpeedTier::ALL[left]).lerp_to_gamma(color(SpeedTier::ALL[right]), blend);
    let painter = ui.painter();
    painter.rect_filled(track, 14, cx.pal.active);
    let fill = Rect::from_min_max(track.min, pos2(fill_end + 14.0, track.max.y));
    painter.rect_filled(
        fill,
        14,
        accent.gamma_multiply(if enabled { 0.88 } else { 0.3 }),
    );
    if current != SpeedTier::Standard && enabled {
        starfield::paint(
            painter,
            fill,
            accent,
            current,
            if reduced_motion {
                0.0
            } else {
                ui.input(|i| i.time)
            },
        );
        if !reduced_motion {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
    }
    let mut selected = None;
    for (i, tier) in SpeedTier::ALL.into_iter().enumerate() {
        let on = enabled && (tier == SpeedTier::Standard || offered.contains(&tier));
        let hit = Rect::from_min_max(
            pos2(
                if i == 0 {
                    track.min.x
                } else {
                    (stops[i - 1] + stops[i]) / 2.0
                },
                track.min.y - 6.0,
            ),
            pos2(
                if i == 2 {
                    track.max.x
                } else {
                    (stops[i] + stops[i + 1]) / 2.0
                },
                rect.max.y,
            ),
        );
        let r = ui.interact(
            hit,
            ui.scope_id().with(("simple-speed-stop", i)),
            Sense::click_and_drag(),
        );
        let name = label(cx, tier);
        r.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, on, current == tier, &name)
        });
        if on && r.clicked() {
            selected = Some(tier);
        }
        if enabled
            && r.dragged()
            && let Some(at) = r.interact_pointer_pos()
        {
            let nearest = stops
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| (*a - at.x).abs().total_cmp(&(*b - at.x).abs()))
                .map(|(i, _)| i)
                .unwrap_or(0);
            let tier = SpeedTier::ALL[nearest];
            if tier == SpeedTier::Standard || offered.contains(&tier) {
                selected = Some(tier);
            }
        }
        if !on {
            r.clone()
                .on_hover_text(cx.tr.t("overview.speed.not_offered"));
        }
        if on && r.has_focus() {
            let step = ui.input(|input| {
                if input.key_pressed(egui::Key::ArrowRight) {
                    1
                } else if input.key_pressed(egui::Key::ArrowLeft) {
                    -1
                } else {
                    0
                }
            });
            if step != 0 {
                let next = (i as i32 + step).clamp(0, 2) as usize;
                let tier = SpeedTier::ALL[next];
                if tier == SpeedTier::Standard || offered.contains(&tier) {
                    selected = Some(tier);
                }
            }
        }
        painter.circle_filled(
            pos2(stops[i], track.center().y),
            3.0,
            if i <= index {
                Color32::WHITE.gamma_multiply(0.55)
            } else {
                cx.pal.text_subtle
            },
        );
        painter.text(
            pos2(
                match i {
                    0 => rect.min.x + 3.0,
                    2 => rect.max.x - 3.0,
                    _ => rect.center().x,
                },
                rect.min.y + 55.0,
            ),
            match i {
                0 => Align2::LEFT_CENTER,
                2 => Align2::RIGHT_CENTER,
                _ => Align2::CENTER_CENTER,
            },
            name,
            font(
                text::CAPTION,
                if i == index {
                    Weight::Semibold
                } else {
                    Weight::Regular
                },
            ),
            if on {
                color(tier)
            } else {
                cx.pal.text_disabled
            },
        );
        if show_focus(&r) {
            focus_ring(painter, hit, 8, cx.pal);
        }
    }
    let knob = pos2(fill_end, track.center().y);
    painter.circle_filled(
        knob + vec2(0.0, 1.5),
        14.5,
        Color32::BLACK.gamma_multiply(0.08),
    );
    painter.circle_filled(knob, 13.5, Color32::WHITE);
    painter.circle_stroke(knob, 13.5, Stroke::new(1.0, cx.pal.border));
    selected
}
