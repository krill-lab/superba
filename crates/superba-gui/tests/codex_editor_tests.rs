#![allow(missing_docs)]
//! "Edit Codex files" (GitHub issue 9) in the GUI: highlighting, the check as
//! you type (syntax with position, catalog warnings and errors), Format that
//! keeps comments, masked `auth.json` secrets (never drawn until revealed, a
//! partly changed placeholder blocks saving), review and save through the
//! service, backups with restore, the Settings button, palette and commands,
//! and the page fitting its rectangle in both languages and themes.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_core::{
    CatalogKey, CatalogSnapshot, CodexFileKind, CodexVersion, ConsumerKind, FileSchema, Locale,
    OwnedKey, ProblemKind, SaveReason, SettingKind, SettingsConsumer, StaticCatalog, SwitchService,
    TargetId, Timestamp,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::settings::files::{Dialog, Editor};
use superba_gui::pages::settings::files_highlight::{Token, tokens};
use superba_gui::pages::settings::{self as settings_page, State};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::fit::{SIZES, render_fit};

const CONFIG: &str = "# my settings\nmodel = \"gpt-5.5\"\nopenai_base_url = \"https://api.example/backend-api/codex\"\n\n[mcp_servers.fetch]\ncommand = \"uvx\" # runs it\n";
const KEY: &str = "sk-gui-test-000000000000wxyz";

fn auth() -> String {
    format!("{{\n  \"OPENAI_API_KEY\": \"{KEY}\",\n  \"auth_mode\": \"apikey\"\n}}\n")
}

fn fake() -> Arc<FakeService> {
    let f = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    f.set_codex_file(TargetId::local(), CodexFileKind::ConfigToml, Some(CONFIG));
    f.set_codex_file(TargetId::local(), CodexFileKind::AuthJson, Some(&auth()));
    f.set_owned(
        TargetId::local(),
        vec![OwnedKey {
            path: "openai_base_url".into(),
            written: Some("\"https://api.example/backend-api/codex\"".into()),
        }],
    );
    let snap = CatalogSnapshot::new(CodexVersion::new(0, 162, 0))
        .with_key(CatalogKey::new("model", SettingKind::String))
        .with_key(CatalogKey::new("openai_base_url", SettingKind::String))
        .with_key(CatalogKey::new("mcp_servers", SettingKind::Map))
        .with_key(CatalogKey::new("mcp_servers.<name>", SettingKind::Table))
        .with_key(CatalogKey::new(
            "mcp_servers.<name>.command",
            SettingKind::String,
        ));
    f.set_file_schema(FileSchema {
        consumers: vec![SettingsConsumer::new(
            ConsumerKind::DesktopApp,
            "ChatGPT app",
            Some(CodexVersion::new(0, 162, 0)),
        )],
        catalog: Some(Arc::new(StaticCatalog::new([snap]))),
    });
    f
}

async fn data(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

fn with_cx<R>(
    service: &Arc<FakeService>,
    data: &Data,
    locale: Locale,
    f: impl FnOnce(&Cx<'_>) -> R,
) -> R {
    let tr = Tr::new(locale);
    let fonts = superba_gui::design::fonts::FontReport::default();
    let svc: Arc<dyn SwitchService> = service.clone();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &fonts,
        service: &svc,
        log: &[],
    };
    f(&cx)
}

/// Poll the editor until its background work is done.
fn settle(ed: &mut Editor, cx: &Cx<'_>, out: &mut Vec<Action>) {
    let start = Instant::now();
    while ed.poll(cx, out) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "background work hangs"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open(ed: &mut Editor, cx: &Cx<'_>, kind: CodexFileKind) {
    ed.open_file(cx, TargetId::local(), kind);
    settle(ed, cx, &mut Vec::new());
}

// ------------------------------------------------------------- highlighting

fn token_of(kind: CodexFileKind, text: &str, needle: &str) -> Token {
    let at = text.find(needle).unwrap();
    tokens(kind, text)
        .into_iter()
        .find(|(r, _)| r.start <= at && at < r.end)
        .map(|(_, t)| t)
        .unwrap()
}

#[test]
fn toml_and_json_are_highlighted_by_role() {
    let c = CodexFileKind::ConfigToml;
    for (needle, token) in [
        ("# my settings", Token::Comment),
        ("model =", Token::Key),
        ("\"gpt-5.5\"", Token::String),
        ("[mcp_servers.fetch]", Token::Header),
        ("# runs it", Token::Comment),
    ] {
        assert_eq!(token_of(c, CONFIG, needle), token, "{needle}");
    }
    let t = "a = { b = 1, c = true }\nd.e = 2026-10-18\n";
    assert_eq!(token_of(c, t, "b ="), Token::Key);
    assert_eq!(token_of(c, t, "1,"), Token::Number);
    assert_eq!(token_of(c, t, "true"), Token::Literal);
    assert_eq!(token_of(c, t, "d.e"), Token::Key);
    assert_eq!(token_of(c, t, "2026"), Token::Number);
    let j = CodexFileKind::AuthJson;
    let a = "{\"k\": \"v\", \"n\": 1, \"t\": null, \"s\": \"••••••••••••wxyz\"}";
    assert_eq!(token_of(j, a, "\"k\""), Token::Key);
    assert_eq!(token_of(j, a, "\"v\""), Token::String);
    assert_eq!(token_of(j, a, "1,"), Token::Number);
    assert_eq!(token_of(j, a, "null"), Token::Literal);
    assert_eq!(token_of(j, a, "\"••"), Token::Masked);
    // Tokens cover every byte, in order.
    let all = tokens(c, CONFIG);
    assert_eq!(all.first().unwrap().0.start, 0);
    assert_eq!(all.last().unwrap().0.end, CONFIG.len());
    assert!(all.windows(2).all(|w| w[0].0.end == w[1].0.start));
}

// -------------------------------------------------------------- the editor

#[tokio::test]
async fn edit_check_format_review_and_save() {
    let service = fake();
    let data = data(&service).await;
    with_cx(&service, &data, Locale::En, |cx| {
        let mut ed = Editor::default();
        open(&mut ed, cx, CodexFileKind::ConfigToml);
        assert!(ed.open);
        assert_eq!(ed.buffer, CONFIG);
        assert!(!ed.dirty());
        // The managed key is marked, nothing else.
        assert!(
            ed.problems
                .iter()
                .all(|p| p.kind == ProblemKind::Owned && p.line == 3),
            "{:?}",
            ed.problems
        );

        // A typo is a warning with its position.
        ed.buffer.push_str("modle = \"x\"\n");
        ed.recheck();
        let w = ed
            .problems
            .iter()
            .find(|p| p.path.as_deref() == Some("mcp_servers.fetch.modle"))
            .expect("unknown key warned");
        assert_eq!(w.line, 7);
        assert_eq!(ed.blocking(), 0);

        // A syntax error blocks the review; Format refuses it.
        ed.buffer = CONFIG.replace("model = \"gpt-5.5\"", "model = ");
        ed.recheck();
        assert_eq!(ed.blocking(), 1);
        ed.review();
        assert_eq!(ed.dialog, None);
        ed.format();
        assert!(ed.format_error.is_some());

        // A wrong type needs "Save anyway".
        ed.buffer = CONFIG.replace("model = \"gpt-5.5\"", "model    =   5");
        ed.format_error = None;
        ed.format();
        assert!(ed.buffer.contains("model = 5\n"), "{}", ed.buffer);
        assert!(ed.buffer.contains("# runs it"), "comments survive");
        assert_eq!(ed.forced(), 1);
        ed.review();
        assert_eq!(ed.dialog, Some(Dialog::Review));
        let mut out = Vec::new();
        ed.save(cx);
        settle(&mut ed, cx, &mut out);
        assert!(service.file_saves().is_empty(), "not without Save anyway");
        ed.force = true;
        ed.save(cx);
        settle(&mut ed, cx, &mut out);
        assert_eq!(
            service.file_saves(),
            vec![(CodexFileKind::ConfigToml, SaveReason::Edit, true)]
        );
        assert_eq!(
            service
                .codex_file_text(&TargetId::local(), CodexFileKind::ConfigToml)
                .unwrap(),
            ed.buffer
        );
        assert!(!ed.dirty() && ed.dialog.is_none());
        assert!(ed.saved.is_some());
        assert!(out.iter().any(|a| matches!(a, Action::Toast { .. })));
        assert!(out.iter().any(|a| matches!(a, Action::Refresh)));
    });
}

#[tokio::test]
async fn the_managed_keys_an_edit_changes_are_named() {
    let service = fake();
    let data = data(&service).await;
    with_cx(&service, &data, Locale::En, |cx| {
        let mut ed = Editor::default();
        open(&mut ed, cx, CodexFileKind::ConfigToml);
        ed.buffer = CONFIG.replace("api.example", "relay.example");
        ed.recheck();
        let (owned, credential) = ed.ownership_changes();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].path, "openai_base_url");
        assert!(credential.is_none());
    });
}

#[tokio::test]
async fn backups_are_listed_and_restored_through_a_review() {
    let service = fake();
    let data = data(&service).await;
    with_cx(&service, &data, Locale::En, |cx| {
        let mut ed = Editor::default();
        open(&mut ed, cx, CodexFileKind::ConfigToml);
        ed.buffer = CONFIG.replace("gpt-5.5", "gpt-6");
        ed.review();
        ed.save(cx);
        settle(&mut ed, cx, &mut Vec::new());
        ed.open_backups(cx);
        settle(&mut ed, cx, &mut Vec::new());
        let list = ed.backups.clone().unwrap();
        assert_eq!(list.len(), 1);
        ed.preview_backup(cx, &list[0].id);
        settle(&mut ed, cx, &mut Vec::new());
        assert_eq!(ed.preview.as_ref().unwrap().1, CONFIG);
        ed.restore_preview();
        assert_eq!(ed.dialog, Some(Dialog::Review));
        assert_eq!(ed.buffer, CONFIG);
        ed.save(cx);
        settle(&mut ed, cx, &mut Vec::new());
        let saves = service.file_saves();
        assert_eq!(
            saves[1].1,
            SaveReason::Restore {
                backup: list[0].id.clone()
            }
        );
        assert_eq!(
            service
                .codex_file_text(&TargetId::local(), CodexFileKind::ConfigToml)
                .unwrap(),
            CONFIG
        );
    });
}

#[tokio::test]
async fn auth_secrets_stay_hidden_until_revealed() {
    let service = fake();
    let data = data(&service).await;
    with_cx(&service, &data, Locale::En, |cx| {
        let mut ed = Editor::default();
        open(&mut ed, cx, CodexFileKind::AuthJson);
        assert!(ed.masked());
        assert!(!ed.buffer.contains(KEY), "{}", ed.buffer);
        assert!(ed.buffer.contains("wxyz"));
        assert_eq!(ed.real_text(), auth());
        assert!(!ed.dirty());
        assert!(!format!("{ed:?}").contains(KEY));
        // Other fields can be edited and saved without revealing the key.
        ed.buffer = ed.buffer.replace("apikey", "chatgpt");
        ed.recheck();
        assert_eq!(ed.blocking(), 0, "{:?}", ed.problems);
        assert!(ed.real_text().contains(KEY));
        // A placeholder changed only partly blocks saving.
        let broken = ed.buffer.replacen("••••", "••", 1);
        ed.buffer = broken;
        ed.recheck();
        assert!(
            ed.problems
                .iter()
                .any(|p| p.kind == ProblemKind::Secret && p.blocks_save())
        );
        // Revealed: the real key; hidden again: the placeholder.
        ed.buffer = ed.real_text();
        let mut ed2 = Editor::default();
        open(&mut ed2, cx, CodexFileKind::AuthJson);
        ed2.toggle_secrets();
        assert!(ed2.buffer.contains(KEY));
        ed2.toggle_secrets();
        assert!(!ed2.buffer.contains(KEY));
    });
}

// ------------------------------------------------- Settings page integration

#[tokio::test]
async fn the_settings_page_opens_the_editor_from_its_commands() {
    let service = fake();
    let data = data(&service).await;
    with_cx(&service, &data, Locale::En, |cx| {
        let mut state = State::default();
        let mut out = Vec::new();
        settings_page::command(&mut state, cx, "files:auth", &mut out);
        assert!(state.files.open);
        assert_eq!(state.files.kind(), CodexFileKind::AuthJson);
        settle(&mut state.files, cx, &mut out);
        state.files.buffer.push(' ');
        assert_eq!(
            settings_page::status(&state).pending,
            1,
            "Ctrl+S reaches the editor"
        );
        settings_page::command(&mut state, cx, "save", &mut out);
        assert_eq!(state.files.dialog, Some(Dialog::Review));
        assert!(settings_page::status(&state).modal);
    });
}

/// An editor already open on `kind` (no background read).
async fn opened(service: &Arc<FakeService>, kind: CodexFileKind) -> State {
    let mut state = State::default();
    state.files.open = true;
    let doc = service.codex_file(TargetId::local(), kind).await.unwrap();
    state.files.loaded(doc);
    state
}

#[tokio::test]
async fn the_editor_fits_the_page_and_never_draws_a_hidden_key() {
    let service = fake();
    let data = data(&service).await;
    for kind in [CodexFileKind::ConfigToml, CodexFileKind::AuthJson] {
        for locale in [Locale::En, Locale::ZhCn] {
            for theme in [egui::Theme::Light, egui::Theme::Dark] {
                for size in SIZES {
                    let mut state = opened(&service, kind).await;
                    let report = render_fit(size, locale, theme, &data, |ui, cx, out| {
                        settings_page::show(ui, cx, &mut state, out);
                    });
                    assert!(report.fits(), "{kind:?} {locale:?} {size:?} overflows");
                    let all = report.all_text();
                    assert!(all.contains(kind.file_name()), "{all}");
                    assert!(!all.contains(KEY), "a hidden key was drawn");
                    let review = if locale == Locale::En {
                        "Review & save…"
                    } else {
                        "检查并保存…"
                    };
                    assert!(report.whole(review), "{all}");
                    assert!(
                        report.beyond_right().is_empty(),
                        "{:?}",
                        report.beyond_right()
                    );
                }
            }
        }
    }
    // The Settings page itself offers the editor.
    let mut state = State::default();
    let report = render_fit(
        SIZES[0],
        Locale::En,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        },
    );
    assert!(report.whole("Edit Codex files"), "{}", report.all_text());
}

#[tokio::test]
async fn the_review_shows_the_diff_and_the_managed_keys() {
    let service = fake();
    let data = data(&service).await;
    let mut state = opened(&service, CodexFileKind::ConfigToml).await;
    state.files.buffer = CONFIG.replace("api.example", "relay.example");
    state.files.recheck();
    state.files.review();
    let report = render_fit(
        SIZES[1],
        Locale::En,
        egui::Theme::Dark,
        &data,
        |ui, cx, out| {
            settings_page::show(ui, cx, &mut state, out);
        },
    );
    let all = report.all_text();
    for expected in [
        "Save config.toml?",
        "+1 −1 lines",
        "Keys Superba manages that this changes",
        "openai_base_url",
        "Superba first keeps an encrypted backup",
    ] {
        assert!(all.contains(expected), "{expected:?} missing:\n{all}");
    }
}

#[tokio::test]
async fn an_edit_saved_in_the_editor_is_a_quiet_to_do_not_a_dialog() {
    use superba_core::{ChangeSource, ExternalChange, KeyChange};
    use superba_gui::external::ExternalDialog;
    use superba_gui::pages::overview::todo;
    let change = ExternalChange {
        codex_home: "/home/u/.codex".into(),
        keys: vec![KeyChange {
            path: "openai_base_url".into(),
            before: Some("\"https://a\"".into()),
            after: Some("\"https://b\"".into()),
        }],
        source: ChangeSource::SuperbaEditor {
            at: Timestamp::from_millis(1_790_000_000_000),
        },
        fingerprint: "f1".into(),
        ..ExternalChange::default()
    };
    let mut dialog = ExternalDialog::default();
    dialog.offer(Some(&change), true);
    assert!(!dialog.is_open(), "never offered by itself");
    let service = fake();
    service.set_external(Some(change.clone()));
    let data = data(&service).await;
    with_cx(&service, &data, Locale::En, |cx| {
        let items = todo::todos(cx, None);
        let item = items
            .iter()
            .find(|i| i.kind == todo::TodoKind::ExternalChange)
            .expect("listed");
        assert_eq!(
            item.title,
            "You changed what Superba manages in the file editor"
        );
        assert_eq!(
            superba_gui::external::hand_off_label(cx, &change.source, "external.hand_off"),
            "Keep my edits, stop managing"
        );
    });
}
