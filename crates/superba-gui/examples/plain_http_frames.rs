//! GH#24: the real provider form and saved detail, using the screenshot test service.
//! Run only in an isolated HOME, e.g. under the repository's Xvfb screenshot tools.
#[path = "../tests/support/mod.rs"]
mod support;

use std::sync::Arc;
use superba_core::{Locale, SwitchService, Timestamp};
use superba_gui::{
    design::{Palette, fonts, style},
    i18n::Tr,
    pages::{Cx, Data, providers},
};

struct Frame {
    service: Arc<dyn SwitchService>,
    data: Data,
    state: providers::State,
    tr: Tr,
    fonts: fonts::FontReport,
}
impl eframe::App for Frame {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let cx = Cx {
            tr: &self.tr,
            pal: Palette::of(ui.ctx()),
            data: &self.data,
            busy: false,
            now: Timestamp::now(),
            fonts: &self.fonts,
            service: &self.service,
            log: &[],
        };
        // The page normally sits on the app's panel; paint it here.
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
        providers::show(ui, &cx, &mut self.state, &mut Vec::new());
    }
}

fn main() -> eframe::Result {
    assert!(
        std::env::var_os("SUPERBA_DATA_DIR").is_some(),
        "use an isolated data directory"
    );
    let args: Vec<_> = std::env::args().collect();
    let saved = args.iter().any(|a| a == "--saved");
    let checked = saved || args.iter().any(|a| a == "--checked");
    let english = args.iter().any(|a| a == "--en");
    let service = Arc::new(support::fake::FakeService::new(
        support::fake::FakeOptions {
            fast: true,
            ..Default::default()
        },
    ));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut state = providers::State::default();
    let mut d = providers::AddDialog::new();
    d.step = providers::AddStep::Custom;
    d.name = "LAN API".into();
    d.url = "http://192.168.1.20:8080/v1".into();
    d.key = "sk-screenshot-fixture-only".into();
    d.allow_plain_http = checked;
    if saved {
        let form = providers::form::SiteForm {
            name: d.name,
            url: d.url,
            key: d.key,
            allow_plain_http: checked,
            ..Default::default()
        };
        let prepared =
            providers::form::prepare(&form, &[], None, superba_switch::new_compatible_site)
                .unwrap();
        let id = prepared.profile.id.clone();
        rt.block_on(async {
            service.save_site(prepared.site).await.unwrap();
            service
                .save_profile(prepared.profile, prepared.key)
                .await
                .unwrap();
        });
        state.select(superba_gui::pages::shared::providers::ProviderId::Profile(
            id,
        ));
    } else {
        state.add = Some(d);
    }
    let data = rt.block_on(async {
        let mut data = Data {
            sites: service.sites().await.unwrap(),
            ..Default::default()
        };
        data.profiles.finish(service.profile_set().await);
        data.overview.finish(service.overview().await);
        data
    });
    eframe::run_native(
        "Superba",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1060.0, 800.0])
                .with_position([0.0, 0.0])
                .with_decorations(false),
            ..Default::default()
        },
        Box::new(move |cc| {
            let fonts = fonts::discover_cjk();
            cc.egui_ctx.set_fonts(fonts::definitions(&fonts));
            cc.egui_ctx.set_pixels_per_point(1.0);
            style::install(&cc.egui_ctx, Some(egui::Theme::Light));
            Ok(Box::new(Frame {
                service,
                data,
                state,
                tr: Tr::new(if english { Locale::En } else { Locale::ZhCn }),
                fonts,
            }))
        }),
    )
}
