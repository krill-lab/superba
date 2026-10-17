#![allow(missing_docs)]
//! Dashboard pieces of the design system (user request 24, kept by the
//! Overview of request 27): KPI value/unit formatting, chart scales, and the
//! background themes (persisted in the settings, applied to the window, one
//! variant per light and dark mode). The Overview's own cards are tested in
//! overview_tests.rs.

use serde_json::json;
use superba_core::{Background, Locale, Settings};
use superba_gui::design::backdrop::{self, Backdrop};
use superba_gui::design::tokens::contrast_ratio;
use superba_gui::design::widgets::chart;
use superba_gui::design::widgets::kpi::{self, KpiValue};
use superba_gui::i18n::Tr;

// --- KPI formatting -----------------------------------------------------------

#[test]
fn kpi_values_split_into_number_and_unit() {
    let v = |number: &str, unit: &str| KpiValue::with_unit(number, unit);
    assert_eq!(kpi::si(0), KpiValue::plain("0"));
    assert_eq!(kpi::si(870), KpiValue::plain("870"));
    assert_eq!(kpi::si(1_000), v("1", "K"));
    assert_eq!(kpi::si(12_345), v("12.3", "K"));
    assert_eq!(
        kpi::si(999_999),
        v("1", "M"),
        "rounds up into the next unit"
    );
    assert_eq!(kpi::si(1_234_567), v("1.23", "M"));
    assert_eq!(kpi::si(3_250_000_000), v("3.25", "B"));
    assert_eq!(kpi::millis(97), v("97", "ms"));
    assert_eq!(kpi::millis(1_250), v("1.25", "s"));
    assert_eq!(kpi::millis(12_000), v("12", "s"));
    assert_eq!(kpi::usd(12_840_000).text(), "$12.84");
    assert_eq!(kpi::usd(0).text(), "$0.00");
    assert_eq!(kpi::usd(1_284_400_000).text(), "$1,284");
    assert_eq!(kpi::rate(0.0), KpiValue::plain("0"));
    assert_eq!(kpi::rate(2.43), KpiValue::plain("2.4"));
    assert_eq!(kpi::rate(3.0), KpiValue::plain("3"));
    assert_eq!(kpi::rate(87.4), KpiValue::plain("87"));
    assert_eq!(kpi::rate(f64::NAN), KpiValue::plain("0"));
    assert_eq!(kpi::rate(1_500.0), v("1.5", "K"));
    assert_eq!(kpi::group(12_345_678), "12,345,678");
    assert_eq!(kpi::group(-1_000), "-1,000");
    assert_eq!(KpiValue::with_unit("97", "ms").text(), "97 ms");
    assert_eq!(KpiValue::none().text(), "—");
    assert_eq!(kpi::sig3(2.0), "2");
    assert_eq!(kpi::sig3(123.4), "123");
}

#[test]
fn chart_scales_round_up_to_nice_maxima() {
    assert_eq!(chart::nice_max(0.0), 0.0);
    assert_eq!(chart::nice_max(0.7), 1.0);
    assert_eq!(chart::nice_max(1.2), 2.0);
    assert_eq!(chart::nice_max(3.0), 5.0);
    assert_eq!(chart::nice_max(4_288_173.0), 5_000_000.0);
    assert_eq!(chart::nice_max(10.0), 10.0);
    assert_eq!(chart::max_of(&[1.0, 4.0, 2.0]), 4.0);
    assert_eq!(chart::max_of(&[]), 0.0);
}

// --- Background themes ----------------------------------------------------------------

#[test]
fn backgrounds_persist_in_the_settings_and_default_to_neutral() {
    assert_eq!(Settings::default().background, Background::Neutral);
    let s = Settings {
        background: Background::Lavender,
        ..Settings::default()
    };
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["background"], "lavender");
    let back: Settings = serde_json::from_value(json).unwrap();
    assert_eq!(back.background, Background::Lavender);
    // Files written before the field existed read as the default.
    let old: Settings = serde_json::from_value(json!({"theme": "dark"})).unwrap();
    assert_eq!(old.background, Background::Neutral);
    assert_eq!(Background::ALL.len(), 5);
}

#[test]
fn every_background_has_a_light_and_a_dark_variant_and_applies_to_the_window() {
    let ctx = egui::Context::default();
    assert_eq!(backdrop::current(&ctx), Background::Neutral);
    for bg in Background::ALL {
        backdrop::set(&ctx, bg);
        assert_eq!(backdrop::current(&ctx), bg);
        let light = Backdrop::new(bg, false);
        let dark = Backdrop::new(bg, true);
        assert_ne!(light.top, dark.top, "{bg:?}");
        // Light themes are light, dark ones dark; text stays readable on both
        // the gradient (sidebar) and the cards.
        let luma = |c: egui::Color32| (c.r() as u32 + c.g() as u32 + c.b() as u32) / 3;
        assert!(luma(light.top) > 200 && luma(light.bottom) > 190, "{bg:?}");
        assert!(luma(dark.top) < 50 && luma(dark.bottom) < 50, "{bg:?}");
        for (b, pal) in [
            (light, &superba_gui::design::tokens::LIGHT),
            (dark, &superba_gui::design::tokens::DARK),
        ] {
            for surface in [b.top, b.bottom, b.card] {
                assert!(contrast_ratio(pal.text, surface) >= 4.5, "{bg:?}");
                assert!(contrast_ratio(pal.text_subtle, surface) >= 3.0, "{bg:?}");
            }
        }
        assert!(!backdrop::name_key(bg).is_empty());
    }
    let tr_en = Tr::new(Locale::En);
    let tr_zh = Tr::new(Locale::ZhCn);
    for bg in Background::ALL {
        let key = backdrop::name_key(bg);
        assert_ne!(tr_en.t(key), key);
        assert_ne!(tr_zh.t(key), key);
    }
}
