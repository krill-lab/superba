//! Shared brand, version and update status above every Superba tab.
use super::Cx;
use crate::design::{
    fonts::{Weight, font},
    widgets::{Badge, Status, StatusDot, Text},
};
use egui::{Align, Layout, Ui};
use superba_core::{SelfUpdateStatus, Settings, UpdateChannel};

pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, settings: &Settings) {
    let compact = ui.available_width() < 860.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = if compact { 8.0 } else { 12.0 };
        ui.set_min_height(if compact { 36.0 } else { 48.0 });
        crate::shell::brand_mark(ui, cx.pal, if compact { 28.0 } else { 40.0 });
        ui.label(
            egui::RichText::new("Superba")
                .font(font(if compact { 20.0 } else { 24.0 }, Weight::Semibold))
                .color(cx.pal.text),
        );
        ui.label(Text::mono_muted(
            format!("v{}", env!("CARGO_PKG_VERSION")),
            cx.pal,
        ));
        let channel = settings.update_channel_for(env!("CARGO_PKG_VERSION"));
        ui.add(
            Badge::new(cx.tr.t(if channel == UpdateChannel::Stable {
                "updates.superba.channel_stable"
            } else {
                "updates.superba.channel_preview"
            }))
            .outline(),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (status, key) = if cx.data.self_update_offered().is_some() {
                (Status::Warn, "superba.home.available")
            } else if cx
                .data
                .selfupdate
                .value
                .as_ref()
                .is_some_and(|v| v.status == SelfUpdateStatus::UpToDate)
            {
                (Status::Ok, "superba.home.current")
            } else {
                (Status::Idle, "superba.home.check_hint")
            };
            let mut text = cx.tr.t(key);
            if let Some(at) = cx.data.selfupdate.value.as_ref().and_then(|v| v.checked_at) {
                text.push_str(" · ");
                text.push_str(&cx.tr.f("superba.home.checked", &[("age", &cx.ago(at))]));
            }
            ui.label(Text::small(text, cx.pal));
            ui.add(StatusDot::new(if cx.data.selfupdate.loading {
                Status::Busy
            } else {
                status
            }));
        });
    });
}
