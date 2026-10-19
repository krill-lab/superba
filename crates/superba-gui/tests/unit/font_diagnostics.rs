use superba_core::{Locale, Timestamp};
use superba_gui::{
    design::{
        Palette,
        fonts::{CjkFace, FontReport},
    },
    i18n::Tr,
    pages::{Cx, Data, superba::diagnostics},
};

#[test]
fn diagnostics_displays_pingfang_ui_family_in_both_languages() {
    let service = crate::support::idle_service();
    let report = FontReport {
        cjk_regular: Some(CjkFace {
            family: "PingFang UI SC".into(),
            path: "/system/fonts/PingFangUI.ttc".into(),
            index: 2,
            weight: 400,
        }),
        ..Default::default()
    };
    for locale in [Locale::En, Locale::ZhCn] {
        let harness = crate::support::ui::Harness::new([1200.0, 3000.0], egui::Theme::Light);
        let tr = Tr::new(locale);
        let data = Data::default();
        let cx = Cx {
            tr: &tr,
            pal: Palette::of(&harness.ctx),
            data: &data,
            busy: false,
            now: Timestamp::EPOCH,
            fonts: &report,
            service: &service,
            log: &[],
        };
        let mut state = diagnostics::State::default();
        let text = harness.settle(&mut |ui| diagnostics::show(ui, &cx, &mut state, &mut Vec::new()));
        assert!(text.iter().any(|(s, _)| s.starts_with("PingFang UI SC · ")));
    }
}
