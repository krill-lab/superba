#![allow(missing_docs)]

//! "Import from the Krill website" (feature 140): the confirmation dialog of
//! a `superba://import/v1` link shows the masked key (never the key), the
//! Krill line and what changes; Cancel and Escape write nothing; Import calls
//! the service once and then switches through the normal flow (or offers it);
//! a refused link only says why. The single-instance hand-over carries the
//! link to a running window and refuses a bad token.

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Rect;
use superba_core::{
    ConflictAction, ImportConflict, ImportLink, LineId, LinkError, Locale, ProfileId,
    SwitchService, Timestamp,
};
use superba_gui::design::{fonts, tokens::Palette};
use superba_gui::i18n::Tr;
use superba_gui::instance::{self, Request};
use superba_gui::pages::link_import::{
    LinkImportDialog, LinkImportOutcome, finished_actions, show_dialog,
};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

/// The website's sample link (nebula-api).
const SAMPLE: &str = "superba://import/v1?key=sk-krill-FAKE0000example&name=Krill";
const KEY: &str = "sk-krill-FAKE0000example";

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

async fn data_of(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

fn cx<'a>(
    ui: &egui::Ui,
    tr: &'a Tr,
    data: &'a Data,
    service: &'a Arc<dyn SwitchService>,
    report: &'a fonts::FontReport,
) -> Cx<'a> {
    Cx {
        tr,
        pal: Palette::of(ui.ctx()),
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: report,
        service,
        log: &[],
    }
}

fn texts(list: &[(String, Rect)]) -> Vec<String> {
    list.iter().map(|(t, _)| t.clone()).collect()
}

/// The dialog in a harness; frames run until `done` holds (the service
/// answers on its own thread) or a few seconds pass.
struct Scene {
    h: Harness,
    tr: Tr,
    data: Data,
    svc: Arc<dyn SwitchService>,
    dialog: Option<LinkImportDialog>,
    outcome: LinkImportOutcome,
    actions: Vec<Action>,
}

impl Scene {
    async fn new(service: &Arc<FakeService>, locale: Locale) -> Self {
        let data = data_of(service).await;
        let svc: Arc<dyn SwitchService> = service.clone();
        Self {
            h: Harness::new([1280.0, 900.0], egui::Theme::Dark),
            tr: Tr::new(locale),
            data,
            svc,
            dialog: None,
            outcome: LinkImportOutcome::Continue,
            actions: Vec::new(),
        }
    }

    fn open(&mut self, link: Result<ImportLink, LinkError>) {
        let (tr, data, svc, h) = (&self.tr, &self.data, &self.svc, &self.h);
        let mut link = Some(link);
        let dialog = &mut self.dialog;
        h.frame(Vec::new(), &mut |ui| {
            let cx = cx(ui, tr, data, svc, &h.fonts);
            if let Some(l) = link.take() {
                *dialog = Some(LinkImportDialog::open(&cx, l));
            }
        });
    }

    fn run(&mut self, events: Vec<egui::Event>) -> Vec<String> {
        let (tr, data, svc, h) = (&self.tr, &self.data, &self.svc, &self.h);
        let (dialog, outcome, actions) = (&mut self.dialog, &mut self.outcome, &mut self.actions);
        let shown = h.frame(events, &mut |ui| {
            let cx = cx(ui, tr, data, svc, &h.fonts);
            if let Some(d) = dialog.as_mut() {
                let o = show_dialog(ui.ctx(), &cx, d, actions);
                if o != LinkImportOutcome::Continue {
                    finished_actions(&cx, &o, actions);
                    *outcome = o;
                    *dialog = None;
                }
            }
        });
        texts(&shown)
    }

    /// Frames until `done` holds for a frame that drew something (a modal
    /// draws nothing in its first sizing frames).
    fn until(&mut self, mut done: impl FnMut(&Self, &[String]) -> bool) -> Vec<String> {
        let start = Instant::now();
        loop {
            let shown = self.run(Vec::new());
            if (done(self, &shown) && (!shown.is_empty() || self.dialog.is_none()))
                || start.elapsed() > Duration::from_secs(10)
            {
                return shown;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn click(&mut self, text: &str) {
        self.until(|_, s| s.iter().any(|t| t == text));
        let (tr, data, svc, h) = (&self.tr, &self.data, &self.svc, &self.h);
        let (dialog, outcome, actions) = (&mut self.dialog, &mut self.outcome, &mut self.actions);
        let mut f = |ui: &mut egui::Ui| {
            let cx = cx(ui, tr, data, svc, &h.fonts);
            if let Some(d) = dialog.as_mut() {
                let o = show_dialog(ui.ctx(), &cx, d, actions);
                if o != LinkImportOutcome::Continue {
                    finished_actions(&cx, &o, actions);
                    *outcome = o;
                    *dialog = None;
                }
            }
        };
        assert!(h.click_last_text(text, &mut f), "{text:?} not shown");
    }
}

fn existing_work() -> ImportConflict {
    ImportConflict {
        profile: ProfileId::new("work"),
        profile_name: "Work".into(),
        same_name: false,
        same_base_url: true,
    }
}

#[tokio::test]
async fn the_dialog_shows_the_masked_key_line_and_change_but_never_the_key() {
    for (locale, expect) in [
        (
            Locale::En,
            vec![
                "Import from the Krill website",
                "••••mple",
                "Krill",
                "Adds a new Krill provider, Krill, with this API key.",
                "Switch Codex to this provider now",
                "Import",
                "Cancel",
            ],
        ),
        (
            Locale::ZhCn,
            vec![
                "从 Krill 网站导入",
                "••••mple",
                "用这个 API Key 新增一个 Krill 供应商：Krill。",
                "立即把 Codex 切换到这个供应商",
                "导入",
                "取消",
            ],
        ),
    ] {
        let service = fake();
        let mut s = Scene::new(&service, locale).await;
        s.open(ImportLink::parse(SAMPLE));
        let shown = s.until(|s, _| {
            s.dialog
                .as_ref()
                .is_some_and(|d| d.preview.is_some() && !d.busy())
        });
        for t in &expect {
            assert!(
                shown.iter().any(|x| x == t),
                "{t:?} missing in {locale:?}: {shown:#?}"
            );
        }
        assert!(
            !shown
                .iter()
                .any(|t| t.contains(KEY) || t.contains("FAKE0000")),
            "the key is never drawn: {shown:#?}"
        );
        // The line: the default line until a probe measured a faster one.
        let line = if locale == Locale::En {
            "Overseas"
        } else {
            "海外线路"
        };
        assert!(
            shown.iter().any(|t| t.contains(line)),
            "{line} missing: {shown:#?}"
        );
        // Opening probed the Krill lines (the wizard's line pick) and only
        // dry runs reached the service.
        assert!(
            s.actions
                .iter()
                .any(|a| matches!(a, Action::Probe(site) if site.as_str() == "krill"))
        );
        assert!(!service.link_requests().is_empty());
        assert!(service.link_writes().is_empty());
        assert!(format!("{:?}", s.dialog).contains("mple"));
        assert!(!format!("{:?}", s.dialog).contains(KEY));
    }
}

#[tokio::test]
async fn cancel_and_escape_write_nothing() {
    for close in ["Cancel", "Escape"] {
        let service = fake();
        let mut s = Scene::new(&service, Locale::En).await;
        s.open(ImportLink::parse(SAMPLE));
        s.until(|s, _| s.dialog.as_ref().is_some_and(|d| d.preview.is_some()));
        if close == "Cancel" {
            s.click("Cancel");
        } else {
            s.run(vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }]);
            s.run(Vec::new());
        }
        assert_eq!(s.outcome, LinkImportOutcome::Close, "{close}");
        assert!(s.dialog.is_none());
        assert!(service.link_writes().is_empty(), "{close}");
        assert!(
            !s.actions
                .iter()
                .any(|a| matches!(a, Action::Switch { .. } | Action::Refresh)),
            "{close}: {:?}",
            s.actions
        );
    }
}

#[tokio::test]
async fn import_updates_the_krill_provider_then_switches_to_it() {
    let service = fake();
    service.set_link_existing(Some(existing_work()));
    let mut s = Scene::new(&service, Locale::En).await;
    s.open(ImportLink::parse(SAMPLE));
    let shown = s.until(|s, _| {
        s.dialog
            .as_ref()
            .is_some_and(|d| d.preview.is_some() && !d.busy())
    });
    for t in [
        "You already have a Krill provider, Work.",
        "Update its key",
        "Add as a new provider",
        "Replaces the API key of Work; its lines and settings stay.",
    ] {
        assert!(shown.iter().any(|x| x == t), "{t:?} missing: {shown:#?}");
    }
    s.click("Import");
    s.until(|s, _| s.dialog.is_none());
    assert_eq!(
        s.outcome,
        LinkImportOutcome::Done {
            profile: ProfileId::new("work"),
            name: "Work".into(),
            switch: true,
        }
    );
    let writes = service.link_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].on_conflict, ConflictAction::Update);
    assert_eq!(writes[0].link.key.expose(), KEY);
    assert!(s.actions.iter().any(|a| matches!(a, Action::Refresh)));
    assert!(s.actions.iter().any(|a| matches!(
        a,
        Action::Switch { profile, line: None, auth: None } if profile == "work"
    )));
    // No action carries the key.
    assert!(!format!("{:?}", s.actions).contains(KEY));
}

#[tokio::test]
async fn enter_imports_like_the_button() {
    let service = fake();
    let mut s = Scene::new(&service, Locale::En).await;
    s.open(ImportLink::parse(SAMPLE));
    s.until(|s, _| {
        s.dialog
            .as_ref()
            .is_some_and(|d| d.preview.is_some() && !d.busy())
    });
    s.run(vec![egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    }]);
    s.until(|s, _| s.dialog.is_none());
    assert_eq!(service.link_writes().len(), 1);
    assert!(matches!(
        s.outcome,
        LinkImportOutcome::Done { switch: true, .. }
    ));
}

#[tokio::test]
async fn add_as_new_without_switching_offers_the_switch() {
    let service = fake();
    service.set_link_existing(Some(existing_work()));
    let mut s = Scene::new(&service, Locale::En).await;
    s.open(ImportLink::parse(SAMPLE));
    s.until(|s, _| s.dialog.as_ref().is_some_and(|d| d.preview.is_some()));
    s.click("Add as a new provider");
    let shown = s.until(|_, shown| {
        shown
            .iter()
            .any(|t| t == "Adds a new Krill provider, Krill (Krill link), with this API key.")
    });
    assert!(
        shown
            .iter()
            .any(|t| t == "Adds a new Krill provider, Krill (Krill link), with this API key."),
        "{shown:#?}"
    );
    s.click("Switch Codex to this provider now");
    assert!(!s.dialog.as_ref().unwrap().switch_now);
    s.click("Import");
    s.until(|s, _| s.dialog.is_none());
    let writes = service.link_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].on_conflict, ConflictAction::Rename);
    // A new provider is imported on the line the dialog showed.
    assert_eq!(writes[0].line, Some(LineId::new("overseas")));
    assert!(matches!(
        &s.outcome,
        LinkImportOutcome::Done { switch: false, .. }
    ));
    assert!(
        s.actions
            .iter()
            .any(|a| matches!(a, Action::OfferSwitch { profile, .. } if profile == "krill-link"))
    );
    assert!(!s.actions.iter().any(|a| matches!(a, Action::Switch { .. })));
}

#[tokio::test]
async fn a_refused_link_only_says_why() {
    let service = fake();
    let mut s = Scene::new(&service, Locale::En).await;
    s.open(ImportLink::parse(
        "superba://import/v2?key=sk-krill-FAKE0000example",
    ));
    let shown = s.until(|_, shown| shown.iter().any(|t| t == "Close"));
    assert!(
        shown.iter().any(|t| t.contains("format \"v2\"")),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|t| t == "Import"));
    assert!(!shown.iter().any(|t| t.contains("FAKE0000")));
    s.click("Close");
    assert_eq!(s.outcome, LinkImportOutcome::Close);
    assert!(service.link_requests().is_empty());
}

#[test]
fn a_running_window_takes_the_link_over_the_authenticated_hand_over() {
    let dir = tempfile::tempdir().unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    let listener = instance::listen(dir.path(), move |r| sink.lock().unwrap().push(r)).unwrap();
    // The registration file holds no link, only the endpoint.
    let link = ImportLink::parse(SAMPLE);
    assert!(instance::forward(dir.path(), Request::link(link.clone())).is_some());
    let refused = ImportLink::parse("superba://import/v1?key=sk-krill-FAKE0000example&line=x");
    assert!(instance::forward(dir.path(), Request::link(refused.clone())).is_some());
    for _ in 0..200 {
        if received.lock().unwrap().len() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        *received.lock().unwrap(),
        vec![Request::link(link), Request::link(refused)]
    );
    let file = std::fs::read_to_string(dir.path().join(instance::FILE)).unwrap();
    assert!(!file.contains("FAKE0000"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join(instance::FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
    // A wrong token, a link the window cannot parse, extra words: refused or
    // re-validated, never trusted.
    let token = listener.endpoint().token.clone();
    assert_eq!(
        instance::parse_line(&format!("superba-gui/1 wrong import {SAMPLE}"), &token),
        None
    );
    assert_eq!(
        instance::parse_line(&format!("superba-gui/1 {token} import {SAMPLE} x"), &token),
        None
    );
    assert_eq!(
        instance::parse_line(&format!("superba-gui/1 {token} show extra"), &token),
        None
    );
    assert_eq!(
        instance::parse_line(
            &format!("superba-gui/1 {token} import superba://import/v9?key={KEY}"),
            &token
        ),
        Some(Request::link(Err(LinkError::UnsupportedVersion(
            "v9".into()
        ))))
    );
    // Debug of a request never shows the key.
    let debug = format!("{:?}", Request::link(ImportLink::parse(SAMPLE)));
    assert!(!debug.contains(KEY), "{debug}");
    drop(listener);
}

#[test]
fn the_command_line_takes_one_link_and_the_hand_over_variable_round_trips() {
    use superba_gui::args::{self, GuiArgs};
    let argv = |v: &[&str]| {
        v.iter()
            .map(Into::into)
            .collect::<Vec<std::ffi::OsString>>()
    };
    let a = GuiArgs::parse(&argv(&["superba", SAMPLE])).unwrap();
    assert_eq!(a.link, Some(ImportLink::parse(SAMPLE)));
    let a = GuiArgs::parse(&argv(&[
        "superba",
        "--lang",
        "zh-CN",
        "SUPERBA://import/v3",
    ]))
    .unwrap();
    assert!(matches!(
        a.link,
        Some(Err(LinkError::UnsupportedVersion(_)))
    ));
    assert!(GuiArgs::parse(&argv(&["superba", SAMPLE, SAMPLE])).is_err());
    assert!(!format!("{a:?}").contains(KEY));
    assert!(args::has_link_arg(&argv(&["superba", SAMPLE])));
    assert!(!args::has_link_arg(&argv(&[
        "superba",
        "--page",
        "providers"
    ])));
    assert_eq!(
        args::without_links(&argv(&["superba", "--lang", "en", SAMPLE])),
        argv(&["superba", "--lang", "en"])
    );
    let link = ImportLink::parse(SAMPLE).unwrap();
    assert_eq!(args::parse_pending(&link.to_url()), Ok(link));
    assert_eq!(
        args::parse_pending("refused:unknown_parameter:line"),
        Err(LinkError::UnknownParameter("line".into()))
    );
    // Not a CLI subcommand.
    assert!(!superba_cli::is_cli_invocation(&argv(&["superba", SAMPLE])));
}
