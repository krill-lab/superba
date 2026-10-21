//! 「故障转移与恢复」 / "Failover and recovery" (user bug 117): the rules that
//! were read-only text in a disclosure, now settings. Each change is saved to
//! the router configuration and a running router reloads it (no restart of
//! Codex): the first and the longest cooldown of a failed line, the statuses
//! that fail over to the next line, how long a conversation stays on its line
//! and whether it returns to the preferred member once that has recovered.

use egui::Ui;
use superba_core::RouterFailover;

use super::{Op, State};
use crate::design::tokens::space;
use crate::design::widgets::{Status, Text, Toggle};
use crate::pages::{Cx, section};

/// The rules shown: the edit being saved, else the saved ones.
fn current(state: &State) -> Option<RouterFailover> {
    state
        .failover_draft
        .clone()
        .or_else(|| state.snapshot.as_ref().and_then(|s| s.failover.clone()))
}

/// Save `next` when it differs from what is shown.
fn save(state: &mut State, cx: &Cx<'_>, next: RouterFailover) {
    if current(state).as_ref() == Some(&next) || next.validate().is_err() {
        return;
    }
    state.failover_draft = Some(next.clone());
    state.start(cx, Op::Failover, |b| b.set_failover(next));
}

/// A whole number typed into a text field, with − / + steppers (alpha.9: the
/// egui `DragValue` used before ignored UI Automation's
/// `ValuePattern.SetValue`, which sends text, and its ±1 steps rounded away at
/// a drag speed of 0.25, so neither automation nor a keyboard user could set
/// it reliably). The value is committed when the field loses focus (Enter,
/// Tab, a click elsewhere; Escape discards), when an assistive technology sets
/// the text, or when a stepper is clicked; out-of-range input is clamped and
/// text that is not a number is dropped.
fn number(
    ui: &mut Ui,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    suffix: &str,
    name: &str,
    enabled: bool,
) -> bool {
    let id = ui.make_persistent_id(("failover-number", name));
    let (lo, hi) = (*range.start(), *range.end());
    let mut committed = false;
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let minus = ui
                .add_enabled(*value > lo, egui::Button::new("−").small())
                .accessible_name(format!("{name} −"));
            let mut text: String = ui
                .data_mut(|d| d.get_temp::<String>(id))
                .unwrap_or_else(|| value.to_string());
            let r = ui
                .add(
                    egui::TextEdit::singleline(&mut text)
                        .id(id.with("edit"))
                        .desired_width(52.0)
                        .char_limit(9),
                )
                .accessible_name(name);
            // A numeric SetValue (UIA RangeValuePattern) also counts.
            let numeric = ui.input(|i| {
                i.accesskit_action_requests(r.id, egui::accesskit::Action::SetValue)
                    .filter_map(|q| match q.data {
                        Some(egui::accesskit::ActionData::NumericValue(v)) => Some(v),
                        _ => None,
                    })
                    .last()
            });
            let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if let Some(v) = numeric {
                text = format!("{}", v.round().max(0.0) as u64);
            }
            let set_from_outside = numeric.is_some() || (r.changed() && !r.has_focus());
            if (r.lost_focus() && !escaped) || set_from_outside {
                if let Some(n) = parse_number(&text) {
                    *value = n.clamp(u64::from(lo), u64::from(hi)) as u32;
                    committed = true;
                }
                ui.data_mut(|d| d.remove::<String>(id));
            } else if r.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, text));
            } else {
                ui.data_mut(|d| d.remove::<String>(id));
            }
            let plus = ui
                .add_enabled(*value < hi, egui::Button::new("+").small())
                .accessible_name(format!("{name} +"));
            if minus.clicked() {
                *value = value.saturating_sub(1).clamp(lo, hi);
                committed = true;
            }
            if plus.clicked() {
                *value = value.saturating_add(1).clamp(lo, hi);
                committed = true;
            }
            ui.label(suffix.trim());
        });
    });
    committed
}

/// The digits of a typed value (`"7"`, `" 7 秒"`, `"7s"`); `None` without any.
fn parse_number(text: &str) -> Option<u64> {
    let digits: String = text
        .trim()
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

pub(super) fn section_ui(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    section(
        ui,
        cx,
        &cx.tr.t("router.failover.title"),
        Some(&cx.tr.t("router.failover.caption")),
    );
    let saved = current(state);
    let enabled = saved.is_some() && state.op.is_none();
    if saved.is_none() {
        crate::pages::note(ui, cx, Status::Idle, cx.tr.t("router.failover.unknown"));
        ui.add_space(space::S);
    }
    let f = saved.unwrap_or_default();
    let secs = cx.tr.t("router.failover.seconds");
    let mins = cx.tr.t("router.failover.minutes");

    let title = cx.tr.t("router.failover.cooldown");
    let mut next = f.clone();
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.cooldown_hint"),
        false,
        |ui| {
            if number(
                ui,
                &mut next.cooldown_secs,
                RouterFailover::COOLDOWN,
                &secs,
                &title,
                enabled,
            ) {
                next.max_cooldown_secs = next.max_cooldown_secs.max(next.cooldown_secs);
                save(state, cx, next.clone());
            }
        },
    );

    let title = cx.tr.t("router.failover.max_cooldown");
    let mut next = f.clone();
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.max_cooldown_hint"),
        false,
        |ui| {
            let range = f.cooldown_secs..=*RouterFailover::MAX_COOLDOWN.end();
            if number(
                ui,
                &mut next.max_cooldown_secs,
                range,
                &secs,
                &title,
                enabled,
            ) {
                save(state, cx, next.clone());
            }
        },
    );

    // The statuses: one check box each, the gateway errors on by default.
    ui.add_space(space::M);
    ui.label(Text::strong(cx.tr.t("router.failover.statuses"), cx.pal));
    ui.add(
        egui::Label::new(Text::small(
            cx.tr.t("router.failover.statuses_hint"),
            cx.pal,
        ))
        .wrap(),
    );
    ui.add_space(space::XS);
    ui.horizontal_wrapped(|ui| {
        for code in RouterFailover::STATUSES {
            let mut on = f.failover_statuses.contains(&code);
            let label = cx.tr.f("router.failover.status", &[("code", &code)]);
            if ui
                .add_enabled(enabled, egui::Checkbox::new(&mut on, label))
                .changed()
            {
                let mut next = f.clone();
                next.failover_statuses.retain(|c| *c != code);
                if on {
                    next.failover_statuses.push(code);
                    next.failover_statuses.sort_unstable();
                }
                save(state, cx, next);
            }
        }
    });
    ui.add_space(space::M);
    crate::pages::divider(ui, cx);

    let title = cx.tr.t("router.failover.sticky");
    let mut next = f.clone();
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.sticky_hint"),
        false,
        |ui| {
            if number(
                ui,
                &mut next.sticky_idle_minutes,
                RouterFailover::STICKY_IDLE,
                &mins,
                &title,
                enabled,
            ) {
                save(state, cx, next.clone());
            }
        },
    );

    let title = cx.tr.t("router.failover.return");
    crate::pages::superba::row(
        ui,
        cx,
        &title,
        &cx.tr.t("router.failover.return_hint"),
        true,
        |ui| {
            let mut on = f.return_to_preferred;
            if ui
                .add(Toggle::new(&mut on, &title).enabled(enabled))
                .changed()
            {
                let mut next = f.clone();
                next.return_to_preferred = on;
                save(state, cx, next);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::parse_number;

    #[test]
    fn typed_values_keep_their_digits() {
        assert_eq!(parse_number("7"), Some(7));
        assert_eq!(parse_number(" 7 秒"), Some(7));
        assert_eq!(parse_number("120s"), Some(120));
        assert_eq!(parse_number("秒"), None);
        assert_eq!(parse_number(""), None);
    }
}
