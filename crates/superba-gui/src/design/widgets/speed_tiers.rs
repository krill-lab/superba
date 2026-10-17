//! Speed control (UX §3.13, plan §11): Standard / Fast / Ultrafast as a segmented
//! control showing only the tiers some listed model offers. The profile's current
//! tier stays visible (disabled, with the reason) when the list no longer offers it.
//!
//! Strokes (user bug 8): the track and the selected tier are painted inside the
//! control's rectangle, so a row exactly as tall as the control cuts nothing. A
//! click selects a tier without a focus ring; the keyboard ring (Tab, or an arrow
//! key after a click) follows [`super::focus`] like every other [`Segmented`]:
//! outside with the 2 px gap where the container leaves `focus::BLEED`, otherwise
//! just inside the track, never clipped (tests/speed_tiers_tests.rs).

use egui::{Response, Ui};
use superba_core::SpeedTier;

use super::Segmented;

/// Translated label and tooltip of one tier.
#[derive(Clone, Debug, Default)]
pub struct TierText {
    /// Label ("标准").
    pub label: String,
    /// Hover text (per-consumer effect, billing note).
    pub tooltip: Option<String>,
}

/// The tiers to show for `offered` (Standard always first) and the current value.
pub fn shown_tiers(offered: &[SpeedTier], current: SpeedTier) -> Vec<(SpeedTier, bool)> {
    SpeedTier::ALL
        .into_iter()
        .filter_map(|t| {
            let on = t == SpeedTier::Standard || offered.contains(&t);
            (on || t == current).then_some((t, on))
        })
        .collect()
}

/// A speed control bound to `value`.
#[must_use = "call .show(ui)"]
pub struct SpeedTiers<'a> {
    id: egui::Id,
    value: &'a mut SpeedTier,
    offered: Vec<SpeedTier>,
    texts: Vec<(SpeedTier, TierText)>,
    not_offered: String,
    small: bool,
    enabled: bool,
}

impl<'a> SpeedTiers<'a> {
    /// A control over the tiers in `offered`.
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        value: &'a mut SpeedTier,
        offered: &[SpeedTier],
    ) -> Self {
        Self {
            id: egui::Id::unique(("superba-speed-tiers", id_salt)),
            value,
            offered: offered.to_vec(),
            texts: Vec::new(),
            not_offered: String::new(),
            small: false,
            enabled: true,
        }
    }

    /// Label and tooltip of `tier` (the default label is the tier's English name).
    pub fn text(mut self, tier: SpeedTier, text: TierText) -> Self {
        self.texts.retain(|(t, _)| *t != tier);
        self.texts.push((tier, text));
        self
    }

    /// Why a tier the list does not offer is disabled.
    pub fn not_offered(mut self, why: impl Into<String>) -> Self {
        self.not_offered = why.into();
        self
    }

    /// 28 px height.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// Disable every tier (switch running, official login).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Draw; `changed()` when the tier changed.
    pub fn show(self, ui: &mut Ui) -> Response {
        let current = *self.value;
        let mut seg = Segmented::new(self.id, self.value);
        if self.small {
            seg = seg.small();
        }
        for (tier, on) in shown_tiers(&self.offered, current) {
            let text = self
                .texts
                .iter()
                .find(|(t, _)| *t == tier)
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| TierText {
                    label: tier.text().render_fallback(),
                    tooltip: None,
                });
            seg = if !on {
                seg.disabled_option(tier, text.label, self.not_offered.clone())
            } else if !self.enabled {
                seg.disabled_option(tier, text.label, text.tooltip.unwrap_or_default())
            } else {
                match text.tooltip {
                    Some(tip) => seg.option_with_tooltip(tier, text.label, tip),
                    None => seg.option(tier, text.label),
                }
            };
        }
        seg.show(ui)
    }
}
