#![allow(missing_docs)]

//! "Import from other tools" (user request 37) in the GUI: the dialog lists the
//! providers found per tool with checkboxes, what each becomes (Krill preset
//! with its line, or a custom site with its lines), whether its key comes along,
//! and conflicts with skip / rename / update (skip by default); importing calls
//! the service once and ends with a toast that offers a switch, never a switch.
//! Entry points: Providers → Add → Import from other tools, the command palette,
//! and the setup wizard's provider step.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Rect;
use superba_core::{
    ConflictAction, ForeignApp, ImportScan, ImportSource, ImportStore, Locale, MapEnv, ProfileSet,
    StoreStatus, SwitchService, Timestamp, catalog,
};
use superba_gui::design::{fonts, tokens::Palette};
use superba_gui::i18n::Tr;
use superba_gui::pages::import::{ImportDialog, ImportOutcomeUi, show_dialog};
use superba_gui::pages::providers::{self, AddDialog};
use superba_gui::pages::shared::providers::ProviderId;
use superba_gui::pages::wizard::{self, Step, Wizard, WizardOutcome};
use superba_gui::pages::{Action, Cx, Data, Page};
use superba_import::{ForeignKey, ForeignProvider, Os, ScanEnv, SourceRead};
use support::fake::{FakeOptions, FakeService};
use support::ui::Harness;

/// `path` (written with `/`) as the importer shows it on this system
/// (`\`-separated on Windows).
fn native(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// A scan built by the real importer: cc-switch's JSON v2 fixture, and an AI
/// Toolbox store with a Krill provider (current), a key kept in the keychain,
/// a provider named like an existing one and a Claude Code provider.
fn fixture_scan() -> ImportScan {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".cc-switch")).unwrap();
    std::fs::write(
        home.join(".cc-switch/config.json"),
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../superba-import/tests/fixtures/ccs/config.v2.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let env = MapEnv::new();
    let scratch = tmp.path().join("scratch");
    let ccs = superba_import::read_source(
        ImportSource::Ccs,
        &ScanEnv {
            home: &home,
            env: &env,
            os: Os::Linux,
            scratch: &scratch,
        },
    );
    let p = |id: &str, name: &str, url: &str| {
        let mut p = ForeignProvider::new(
            ImportSource::AiToolbox,
            format!("codex/{id}"),
            name,
            ForeignApp::Codex,
        );
        p.base_url = Some(url.into());
        p.key = ForeignKey::from_text(Some(&format!("sk-test-{id}-1234abcd")));
        p.wire_api = Some("responses".into());
        p
    };
    let mut krill = p("k", "Krill 海外", "https://api.kbsfdxc.net/v1");
    krill.current = true;
    krill.model = Some("gpt-5.5".into());
    let mut keychain = p("kc", "Keychain Relay", "https://kc.example.com/v1");
    keychain.key = ForeignKey::Keychain;
    let named = p("dup", "My Gateway", "https://dup.example.com/v1");
    let mut claude = ForeignProvider::new(
        ImportSource::AiToolbox,
        "claude/c",
        "Claude Relay",
        ForeignApp::Claude,
    );
    claude.current = true;
    let toolbox = SourceRead {
        source: ImportSource::AiToolbox,
        stores: vec![ImportStore {
            path: home.join(".local/share/com.ai-toolbox/ai-toolbox.db"),
            display_path: native("~/.local/share/com.ai-toolbox/ai-toolbox.db"),
            format: "AI Toolbox database, schema 22".into(),
            status: StoreStatus::Read,
            note: None,
        }],
        providers: vec![krill, keychain, named, claude],
    };
    let mut set = ProfileSet::default();
    let mut existing =
        superba_gui::pages::shared::new_profile(&catalog::krill_site(), "My Gateway".into());
    existing.id = superba_core::ProfileId::new("my-gateway");
    // On the CDN line, so only the name collides (the Krill provider found is
    // on the overseas line).
    existing.set_line_priority([superba_core::LineId::new(catalog::KRILL_LINE_CDN)]);
    set.profiles.push(existing);
    superba_import::preview(&[ccs, toolbox], &set, &catalog::builtin_sites())
}

fn fake() -> Arc<FakeService> {
    let f = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    f.set_import_scan(fixture_scan());
    f
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

fn texts(list: &[(String, Rect)]) -> Vec<&str> {
    list.iter().map(|(t, _)| t.as_str()).collect()
}

#[tokio::test]
async fn the_dialog_shows_each_tool_with_targets_keys_and_conflicts() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    for (locale, expect) in [
        (
            Locale::ZhCn,
            [
                "从其他工具导入",
                "Krill · 海外线路",
                "自定义站点 · codex-api.packycode.example · 3 条线路",
                "Key 在系统钥匙串中（不导入）",
                "已有同名供应商 My Gateway。",
                "当前使用",
                "导入 3 个",
            ],
        ),
        (
            Locale::En,
            [
                "Import from other tools",
                "Krill · Overseas",
                "Custom site · codex-api.packycode.example · 3 lines",
                "Key in the system keychain (not imported)",
                "A provider named My Gateway already exists.",
                "Current",
                "Import 3",
            ],
        ),
    ] {
        let tr = Tr::new(locale);
        // Tall enough for every row of the list.
        let h = Harness::new([1280.0, 2400.0], egui::Theme::Dark);
        let mut d = ImportDialog::with_scan(Ok(service_scan(&svc).await));
        let mut actions = Vec::new();
        let mut f = |ui: &mut egui::Ui| {
            let cx = cx(ui, &tr, &data, &svc, &h.fonts);
            show_dialog(ui.ctx(), &cx, &mut d, &mut actions);
        };
        // The scroll area grows to its content over a few frames.
        for _ in 0..30 {
            h.frame(Vec::new(), &mut f);
        }
        let shown = h.frame(Vec::new(), &mut f);
        let all = texts(&shown);
        for t in expect {
            assert!(all.contains(&t), "{t:?} missing in {locale:?}: {all:#?}");
        }
        // Tools and their stores.
        assert!(all.contains(&"CC Switch") && all.contains(&"AI Toolbox"));
        let store = native("~/.cc-switch/config.json");
        assert!(
            all.iter().any(|t| t.contains(&store)),
            "{store} missing: {all:#?}"
        );
        // Not importable ones are collapsed behind a count.
        assert!(
            all.iter()
                .any(|t| t.contains("个不可导入") || t.contains("not importable")),
            "{all:#?}"
        );
        assert!(
            !all.iter().any(|t| t.contains("sk-test")),
            "no key is shown"
        );
        assert!(actions.is_empty());
    }
}

async fn service_scan(svc: &Arc<dyn SwitchService>) -> ImportScan {
    svc.import_scan(Default::default()).await.unwrap()
}

#[tokio::test]
async fn ticks_and_conflict_choices_make_the_selections() {
    let service = fake();
    let svc: Arc<dyn SwitchService> = service.clone();
    let mut d = ImportDialog::with_scan(Ok(service_scan(&svc).await));
    let scan = service_scan(&svc).await;
    // Pre-selected: importable, no conflict, a Responses API; the official
    // login, Claude and the conflicting one are not.
    let ticked: Vec<&str> = scan
        .candidates()
        .filter(|c| d.ticked.contains(&c.id))
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(ticked, ["PackyCode", "Krill 海外", "Keychain Relay"]);
    assert_eq!(d.effective_count(), 3);
    let conflicting = scan
        .candidates()
        .filter(|c| c.conflict.is_some())
        .map(|c| c.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        conflicting.len(),
        2,
        "cc-switch's and AI Toolbox's My Gateway"
    );
    assert_eq!(d.action(&conflicting[0]), ConflictAction::Skip);
    let before = d.effective_count();
    d.ticked.insert(conflicting[0].clone());
    assert_eq!(
        d.effective_count(),
        before,
        "a conflict left on Skip imports nothing"
    );
    d.actions
        .insert(conflicting[0].clone(), ConflictAction::Rename);
    assert_eq!(d.effective_count(), before + 1);
    let sel = d.selections();
    assert!(
        sel.iter()
            .any(|s| s.candidate == conflicting[0] && s.on_conflict == ConflictAction::Rename)
    );
    assert!(
        sel.iter()
            .all(|s| scan.candidate(&s.candidate).unwrap().importable())
    );
}

/// Frames until `done` or 3 s passed.
fn run_until(h: &Harness, f: &mut dyn FnMut(&mut egui::Ui), mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) && !done() {
        h.frame(Vec::new(), f);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn importing_from_the_providers_page_saves_once_and_offers_a_switch() {
    for locale in [Locale::ZhCn, Locale::En] {
        let service = fake();
        let data = data_of(&service).await;
        let svc: Arc<dyn SwitchService> = service.clone();
        let tr = Tr::new(locale);
        let h = Harness::new([1280.0, 800.0], egui::Theme::Light);
        let mut state = providers::State::default();
        let mut actions = Vec::new();
        {
            let mut f = |ui: &mut egui::Ui| {
                let cx = cx(ui, &tr, &data, &svc, &h.fonts);
                if !state.is_modal_open() && actions.is_empty() && service.import_scans().is_empty()
                {
                    providers::command(&mut state, &cx, "import", &mut actions);
                }
                providers::show(ui, &cx, &mut state, &mut actions);
            };
            h.frame(Vec::new(), &mut f);
        }
        assert!(state.import.is_some());
        let mut page = |ui: &mut egui::Ui| {
            let cx = cx(ui, &tr, &data, &svc, &h.fonts);
            providers::show(ui, &cx, &mut state, &mut actions);
        };
        run_until(&h, &mut page, || !service.import_scans().is_empty());
        h.settle(&mut page);
        let button = if locale == Locale::ZhCn {
            "导入 3 个"
        } else {
            "Import 3"
        };
        assert!(h.click_text(button, &mut page), "{button}");
        run_until(&h, &mut page, || !service.import_requests().is_empty());
        let finish = if locale == Locale::ZhCn {
            "完成"
        } else {
            "Done"
        };
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if h.click_text(finish, &mut page) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let requests = service.import_requests();
        assert_eq!(requests.len(), 1, "one import");
        assert!(!requests[0].dry_run);
        assert_eq!(requests[0].selections.len(), 3);
        assert!(state.import.is_none(), "closed after Done");
        assert!(actions.iter().any(|a| matches!(a, Action::Refresh)));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::OfferSwitch { .. })),
            "a toast offers the switch: {actions:?}"
        );
        // Importing never plans or runs a switch.
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::Switch { .. } | Action::Plan(_)))
        );
        assert!(matches!(state.selected, Some(ProviderId::Profile(_))));
    }
}

#[tokio::test]
async fn the_add_dialog_and_the_import_command_open_the_import_dialog() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let tr = Tr::new(Locale::ZhCn);
    let h = Harness::new([1280.0, 800.0], egui::Theme::Dark);
    let mut state = providers::State::default();
    state.add = Some(AddDialog::new());
    let mut actions = Vec::new();
    let mut page = |ui: &mut egui::Ui| {
        let cx = cx(ui, &tr, &data, &svc, &h.fonts);
        providers::show(ui, &cx, &mut state, &mut actions);
    };
    assert!(h.click_text("从其他工具导入…", &mut page));
    assert!(state.add.is_none() && state.import.is_some());
    let mut out = Vec::new();
    h.frame(Vec::new(), &mut |ui| {
        let cx = cx(ui, &tr, &data, &svc, &h.fonts);
        providers::command(&mut state, &cx, "import", &mut out);
    });
    assert!(state.import.is_some());
}

#[tokio::test]
async fn the_wizard_offers_import_and_closes_on_the_providers_page_after_importing() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let tr = Tr::new(Locale::ZhCn);
    let h = Harness::new([1280.0, 800.0], egui::Theme::Light);
    let mut w = Wizard::start(&data);
    w.step = Step::Provider;
    let mut actions = Vec::new();
    let mut outcome = WizardOutcome::Continue;
    let mut f = |ui: &mut egui::Ui| {
        let cx = cx(ui, &tr, &data, &svc, &h.fonts);
        outcome = wizard::show(ui.ctx(), &cx, &mut w, &mut actions);
    };
    assert!(h.click_text("从其他工具导入…", &mut f));
    assert!(w.import.is_some());
    // Escape leaves the import view, not the wizard.
    let mut f = |ui: &mut egui::Ui| {
        let cx = cx(ui, &tr, &data, &svc, &h.fonts);
        outcome = wizard::show(ui.ctx(), &cx, &mut w, &mut actions);
    };
    run_until(&h, &mut f, || !service.import_scans().is_empty());
    let shown = h.settle(&mut f);
    assert!(
        texts(&shown).contains(&"从其他工具导入"),
        "{:#?}",
        texts(&shown)
    );
    h.key(egui::Key::Escape, &mut f);
    assert!(w.import.is_none());
    assert!(matches!(outcome, WizardOutcome::Continue));
    assert_eq!(w.step, Step::Provider);

    // Import and finish: the wizard closes on the Providers page.
    let mut d = ImportDialog::with_scan(Ok(service_scan(&svc).await));
    d.set_report(
        svc.import_apply(superba_core::ImportApplyRequest {
            selections: d.selections(),
            dry_run: false,
        })
        .await
        .unwrap(),
    );
    w.import = Some(d);
    let mut f = |ui: &mut egui::Ui| {
        let cx = cx(ui, &tr, &data, &svc, &h.fonts);
        let o = wizard::show(ui.ctx(), &cx, &mut w, &mut actions);
        if !matches!(o, WizardOutcome::Continue) {
            outcome = o;
        }
    };
    assert!(h.click_text("完成", &mut f));
    assert!(matches!(outcome, WizardOutcome::Close));
    assert!(actions.iter().any(|a| matches!(
        a,
        Action::PageCommand { page: Page::Providers, command } if command.starts_with("profile:")
    )));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::OfferSwitch { .. }))
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Switch { .. } | Action::Plan(_)))
    );
}

#[tokio::test]
async fn enter_imports_the_selection_and_then_finishes() {
    let service = fake();
    let data = data_of(&service).await;
    let svc: Arc<dyn SwitchService> = service.clone();
    let tr = Tr::new(Locale::En);
    let h = Harness::new([1280.0, 800.0], egui::Theme::Dark);
    let mut d = ImportDialog::with_scan(Ok(service_scan(&svc).await));
    let mut actions = Vec::new();
    let outcome = std::cell::RefCell::new(ImportOutcomeUi::Continue);
    let mut f = |ui: &mut egui::Ui| {
        let cx = cx(ui, &tr, &data, &svc, &h.fonts);
        let o = show_dialog(ui.ctx(), &cx, &mut d, &mut actions);
        if o != ImportOutcomeUi::Continue {
            *outcome.borrow_mut() = o;
        }
    };
    h.settle(&mut f);
    h.key(egui::Key::Enter, &mut f);
    run_until(&h, &mut f, || !service.import_requests().is_empty());
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) && *outcome.borrow() == ImportOutcomeUi::Continue
    {
        h.frame(Vec::new(), &mut f);
        h.key(egui::Key::Enter, &mut f);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(service.import_requests().len(), 1, "Enter imported once");
    assert!(
        matches!(*outcome.borrow(), ImportOutcomeUi::Done { imported: 3, .. }),
        "{:?}",
        outcome.borrow()
    );
}
