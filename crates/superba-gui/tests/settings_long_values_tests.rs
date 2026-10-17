#![allow(missing_docs)]
//! Long setting values (user bug 15): the shortening helper (middle ellipsis for
//! paths and URLs, end ellipsis for prose, East Asian wide characters counted
//! twice), the large text editor saving multi-line text exactly (and cancelling
//! without an edit), list rows (add, remove, reorder) and map rows written as
//! TOML, and the list chip, the diff dialog, the switch preview rows and the
//! host sync dialog staying inside their widths with long values.

mod support;

use std::sync::Arc;

use egui::{Event, Key, Modifiers, Rect, pos2};
use pretty_assertions::assert_eq;
use superba_core::settings::SettingValue;
use superba_core::{
    HostSyncAction, HostSyncRow, Locale, ProfileSet, SwitchService, TargetId, Timestamp,
};
use superba_gui::design::elide::{
    self, DiffOp, Ellipsis, Shape, diff_lines, display_width, elide_cols,
};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::settings::model::{
    Change, Item, Kind, Model, ParseError, Rail, Value, list_items, list_value, map_rows,
    map_value, move_row,
};
use superba_gui::pages::settings::source::{self, Input};
use superba_gui::pages::settings::{self as settings_page, State};
use superba_gui::pages::shared::values::{self, More, Shown};
use superba_gui::pages::{Action, Cx, Data, hosts};
use support::fake::{FakeOptions, FakeService};

const INSTRUCTIONS: &str = "You are a careful assistant working in this repository.\n\n  \
    Keep answers short; quote \"exact\" names and 'single' ones; a \\ backslash stays.   \n\
    \tTabs stay too.\n\nLast paragraph without a trailing line break";
const NOTIFIER: &str = "/home/someone/projects/very/long/path/to/the/notifier/bin/notifier";
const MODEL_FILE: &str =
    "/home/someone/projects/superba/docs/instructions/very-long-folder-name/model_instructions.md";

fn config() -> toml::Table {
    let mut t = toml::Table::new();
    t.insert(
        "developer_instructions".into(),
        toml::Value::String(INSTRUCTIONS.into()),
    );
    t.insert(
        "model_instructions_file".into(),
        toml::Value::String(MODEL_FILE.into()),
    );
    t.insert(
        "notify".into(),
        toml::Value::Array(vec![
            NOTIFIER.into(),
            "--flag".into(),
            "--title".into(),
            "Codex finished".into(),
        ]),
    );
    let doc: toml::Table = toml::from_str(
        r#"
[sandbox_workspace_write]
writable_roots = ["/srv/data/projects/alpha", "/tmp/build-cache"]

[shell_environment_policy.set]
PATH = "/opt/tools/bin:/usr/local/bin:/usr/bin:/bin"
RUST_LOG = "info"
"#,
    )
    .unwrap();
    t.extend(doc);
    t
}

fn fake() -> FakeService {
    FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    })
}

async fn load_data(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

fn model(data: &Data) -> Model {
    let input = Input {
        overview: data.overview.value.clone(),
        profiles: data.profiles.value.clone(),
        profile: None,
    };
    let store = superba_catalog::CatalogStore::builtin().unwrap();
    source::build(&store, &input, Some(&config()), superba_catalog::Os::Linux).unwrap()
}

fn state(model: Model) -> State {
    let mut s = State::default();
    s.set_model(model);
    s.filter.rail = Rail::All;
    s
}

/// A headless window drawing one closure per frame with a page context.
struct Window {
    ctx: egui::Context,
    tr: Tr,
    fonts: fonts::FontReport,
    service: Arc<dyn SwitchService>,
    data: Data,
    size: [f32; 2],
}

impl Window {
    fn new(locale: Locale, data: Data) -> Self {
        let ctx = egui::Context::default();
        let fonts = fonts::FontReport::default();
        ctx.set_fonts(fonts::definitions(&fonts));
        style::install(&ctx, Some(egui::Theme::Light));
        Self {
            ctx,
            tr: Tr::new(locale),
            fonts,
            service: Arc::new(fake()),
            data,
            size: [1280.0, 800.0],
        }
    }

    /// One frame with `events`; returns every drawn text with its rectangle.
    fn frame(
        &self,
        events: Vec<Event>,
        show: &mut dyn FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
    ) -> Vec<(String, Rect)> {
        let screen = Rect::from_min_max(pos2(0.0, 0.0), pos2(self.size[0], self.size[1]));
        let output = self.ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            },
            |ui| {
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
                let page = Rect::from_min_max(pos2(220.0, 0.0), screen.max).shrink(24.0);
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(page)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                let mut out = Vec::new();
                show(&mut child, &cx, &mut out);
            },
        );
        let texts = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                // What is visible: the text clipped to its clip rectangle.
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley
                        .rect
                        .translate(t.pos.to_vec2())
                        .intersect(s.clip_rect),
                )),
                _ => None,
            })
            .filter(|(_, r)| r.is_positive())
            .collect();
        output.drop_without_applying_deltas();
        texts
    }

    fn frames(
        &self,
        n: usize,
        show: &mut dyn FnMut(&mut egui::Ui, &Cx<'_>, &mut Vec<Action>),
    ) -> Vec<(String, Rect)> {
        let mut last = Vec::new();
        for _ in 0..n {
            last = self.frame(Vec::new(), show);
        }
        last
    }

    fn area(&self, id: egui::Id) -> Option<Rect> {
        self.ctx.memory(|m| m.area_rect(id))
    }

    /// `n` frames of the settings page.
    fn page(&self, st: &mut State, n: usize) -> Vec<(String, Rect)> {
        self.frames(n, &mut |ui, cx, out| settings_page::show(ui, cx, st, out))
    }

    /// One frame of the settings page with `events`.
    fn page_with(&self, st: &mut State, events: Vec<Event>) -> Vec<(String, Rect)> {
        self.frame(events, &mut |ui, cx, out| {
            settings_page::show(ui, cx, st, out)
        })
    }
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

// ---------------------------------------------------------------------------
// The shortening helper
// ---------------------------------------------------------------------------

#[test]
fn paths_keep_their_file_name_and_urls_their_host() {
    let short = elide_cols(NOTIFIER, 32, Ellipsis::Path);
    assert!(display_width(&short) <= 32, "{short}");
    assert!(short.ends_with("/notifier"), "{short}");
    assert!(short.starts_with("/home/"), "{short}");
    assert!(short.contains('…'), "{short}");

    let url = "https://relay.very-long-provider-name.example.com/v1/responses/compact?beta=1";
    let short = elide_cols(url, 64, Ellipsis::Url);
    assert!(display_width(&short) <= 64, "{short}");
    assert!(
        short.starts_with("https://relay.very-long-provider-name.example.com…"),
        "{short}"
    );
    assert!(short.ends_with("beta=1"), "{short}");
    // Less room: the scheme goes, the host stays.
    let short = elide_cols(url, 52, Ellipsis::Url);
    assert!(display_width(&short) <= 52, "{short}");
    assert!(
        short.starts_with("relay.very-long-provider-name.example.com…"),
        "{short}"
    );
    // Even less: the start of the host.
    let short = elide_cols(url, 20, Ellipsis::Url);
    assert_eq!(short, "relay.very-long-pro…");
    assert_eq!(
        elide_cols("https://api.example.com/v1", 40, Ellipsis::Url),
        "https://api.example.com/v1"
    );

    let command = "notify-send --app-name Codex --urgency low --expire-time 4000 done";
    let short = elide_cols(command, 30, Ellipsis::Middle);
    assert!(display_width(&short) <= 30, "{short}");
    assert!(short.starts_with("notify-send"), "{short}");
    assert!(short.ends_with("done"), "{short}");

    // A file name longer than the room: a short head and the end of the name.
    let long_name = "/tmp/sbshots/home/.local/share/codex-notifier/bin/codex-desktop-notifier";
    let short = elide_cols(long_name, 20, Ellipsis::Path);
    assert!(display_width(&short) <= 20, "{short}");
    assert!(short.starts_with("/tmp"), "{short}");
    assert!(short.ends_with("sktop-notifier"), "{short}");

    // A text that fits is untouched.
    assert_eq!(
        elide_cols("/usr/bin/true", 40, Ellipsis::Path),
        "/usr/bin/true"
    );
}

#[test]
fn prose_loses_its_end_and_wide_characters_count_twice() {
    let prose = "Always answer in Chinese and keep every reply short.";
    let short = elide_cols(prose, 20, Ellipsis::End);
    assert_eq!(short, "Always answer in Ch…");
    // No dangling space before the ellipsis.
    assert_eq!(elide_cols(prose, 8, Ellipsis::End), "Always…");

    assert_eq!(display_width("你好世界"), 8);
    assert_eq!(display_width("a你b"), 4);
    let zh = "请始终使用中文回答，并保持简短";
    let short = elide_cols(zh, 9, Ellipsis::End);
    assert_eq!(short, "请始终使…");
    assert_eq!(display_width(&short), 9);
    // An odd budget never splits a wide character.
    let short = elide_cols(zh, 10, Ellipsis::End);
    assert_eq!(short, "请始终使…");
    let mixed = "/Users/张三/项目/很长的文件夹名字/说明.md";
    let short = elide_cols(mixed, 20, Ellipsis::Path);
    assert!(display_width(&short) <= 20, "{short}");
    assert!(short.ends_with("/说明.md"), "{short}");
    // Line breaks never reach a one-line display.
    assert!(!elide_cols("a\nb\tc", 10, Ellipsis::End).contains(['\n', '\t']));
}

#[test]
fn values_are_classified_by_shape() {
    assert_eq!(Shape::of(NOTIFIER), Shape::Path);
    assert_eq!(Shape::of("~/Library/Application Support/x"), Shape::Path);
    assert_eq!(Shape::of("C:\\Users\\me\\notes.md"), Shape::Path);
    assert_eq!(Shape::of("https://example.com/v1"), Shape::Url);
    assert_eq!(Shape::of("/usr/bin/notifier --flag"), Shape::Command);
    assert_eq!(Shape::of("notify-send --urgency low"), Shape::Command);
    assert_eq!(Shape::of("on-request"), Shape::Token);
    assert_eq!(Shape::of("Keep answers short and precise"), Shape::Prose);
    assert_eq!(Shape::of("请用中文回答"), Shape::Prose);
    assert_eq!(Shape::of(INSTRUCTIONS), Shape::Prose);
    assert_eq!(Shape::Path.ellipsis(), Ellipsis::Path);
    assert_eq!(Shape::Prose.ellipsis(), Ellipsis::End);
    assert!(!Shape::Prose.mono() && Shape::Path.mono() && Shape::Url.mono());
}

#[test]
fn multi_line_and_composite_values_show_one_line_and_a_count() {
    let s = Shown::string(INSTRUCTIONS);
    assert_eq!(
        s.head,
        "You are a careful assistant working in this repository."
    );
    assert_eq!(s.more, More::Lines(5));
    assert_eq!(s.full, INSTRUCTIONS);

    let repr = SettingValue::Array(vec![NOTIFIER.into(), "--flag".into()]).to_toml_repr();
    let l = Shown::repr(&repr);
    assert_eq!(l.head, NOTIFIER);
    assert_eq!(l.shape, Shape::Path);
    assert_eq!(l.more, More::Items(1));
    assert_eq!(
        l.items.as_deref(),
        Some(&[NOTIFIER.to_owned(), "--flag".into()][..])
    );

    let t = Shown::repr("{ PATH = \"/usr/bin\", RUST_LOG = \"info\" }");
    assert_eq!(t.head, "PATH = /usr/bin");
    assert_eq!(t.more, More::Items(1));
    assert_eq!(Shown::repr("[]").head, "[]");
    assert_eq!(Shown::string("").head, "\"\"");
    assert_eq!(elide::first_line("\n\nsecond\nthird\n"), ("second", 1));
}

#[test]
fn line_diffs_show_the_old_line_before_the_new_one() {
    let before = ["a", "b", "c"];
    let after = ["a", "B", "c", "d"];
    assert_eq!(
        diff_lines(&before, &after),
        vec![
            (DiffOp::Same, "a"),
            (DiffOp::Removed, "b"),
            (DiffOp::Added, "B"),
            (DiffOp::Same, "c"),
            (DiffOp::Added, "d"),
        ]
    );
    let added = Shown::string("x\ny");
    let rows = values::diff_rows(None, Some(&added));
    assert_eq!(rows, vec![(DiffOp::Added, "x"), (DiffOp::Added, "y")]);
}

// ---------------------------------------------------------------------------
// Editors
// ---------------------------------------------------------------------------

/// Lines and characters the editor reports for `text` (a trailing line break
/// starts no new line).
fn counts_of(text: &str) -> (usize, usize) {
    superba_gui::pages::settings::editor::TextEditor::new("p", "p", Some(text), None, false)
        .counts()
}

/// The TOML `config.toml` gets for `path` from the saved profile: the profile
/// setting's representation, parsed back as TOML.
fn written(st: &State, data: &Data, path: &str) -> toml::Value {
    let profiles: ProfileSet = data.profiles.value.clone().unwrap();
    let profile = source::updated_profile(&profiles, st.model().unwrap(), &st.pending).unwrap();
    let repr = profile
        .codex_settings
        .get(path)
        .and_then(|e| e.to_toml_repr())
        .unwrap_or_else(|| panic!("{path} not saved"));
    let doc: toml::Table = toml::from_str(&format!("v = {repr}")).unwrap();
    doc["v"].clone()
}

#[tokio::test]
async fn the_large_editor_saves_multi_line_text_exactly_and_cancel_changes_nothing() {
    let service = fake();
    let data = load_data(&service).await;
    let mut st = state(model(&data));
    let path = "developer_instructions";
    assert_eq!(
        st.model().unwrap().entry(path).unwrap().kind,
        Kind::Text { path: false }
    );

    // Opens on the exact file text.
    st.open_editor(path);
    let ed = st.editor_mut().unwrap();
    assert_eq!(ed.buffer, INSTRUCTIONS);
    assert!(!ed.read_only);
    assert_eq!(ed.counts(), (6, INSTRUCTIONS.chars().count()));
    assert_eq!(counts_of("a\nb\n"), (2, 4));

    // Cancel: nothing pending.
    ed.buffer.push_str("\nnot kept");
    st.cancel_editor();
    assert!(st.pending.is_empty());

    // Save: the text exactly as typed, through the profile into TOML.
    let edited = format!("{INSTRUCTIONS}\n\n  indented \"quoted\" line\t\n\n");
    st.open_editor(path);
    st.editor_mut().unwrap().buffer = edited.clone();
    st.save_editor();
    assert_eq!(
        st.pending.get(path),
        Some(&Change::Set(Value::Str(edited.clone())))
    );
    assert_eq!(
        written(&st, &data, path),
        toml::Value::String(edited.clone())
    );

    // Reopening shows the saved text; an empty save resets to the default.
    st.open_editor(path);
    assert_eq!(st.editor_mut().unwrap().buffer, edited);
    st.editor_mut().unwrap().buffer.clear();
    st.save_editor();
    assert_eq!(st.pending.get(path), Some(&Change::Unset));

    // A key owned elsewhere opens read-only and saving changes nothing.
    st.pending.clear();
    st.open_editor("openai_base_url");
    if let Some(ed) = st.editor_mut() {
        assert!(ed.read_only);
        ed.buffer.push('x');
        st.save_editor();
        assert!(st.pending.is_empty());
    }
}

#[tokio::test]
async fn ctrl_enter_saves_and_escape_cancels_in_the_dialog() {
    let service = fake();
    let data = load_data(&service).await;
    let mut st = state(model(&data));
    let path = "developer_instructions";
    st.focus(path);
    let w = Window::new(Locale::ZhCn, load_data(&service).await);
    w.page(&mut st, 2);

    st.open_editor(path);
    let edited = format!("{INSTRUCTIONS}\nline two  \n");
    st.editor_mut().unwrap().buffer = edited.clone();
    let texts = w.page(&mut st, 2);
    assert!(
        texts.iter().any(|(t, _)| t.contains("编辑「")),
        "the editor dialog is open"
    );
    // The shell leaves Enter / Escape (and Ctrl/Cmd+S) to the open dialog.
    assert!(settings_page::status(&st).modal);
    w.page_with(
        &mut st,
        vec![key(Key::Enter, Modifiers::CTRL | Modifiers::COMMAND)],
    );
    assert!(st.editor_mut().is_none(), "Ctrl+Enter closes the editor");
    assert_eq!(st.pending.get(path), Some(&Change::Set(Value::Str(edited))));

    // Escape: closed, nothing new pending.
    st.pending.clear();
    st.open_editor(path);
    st.editor_mut().unwrap().buffer.push_str("dropped");
    w.page(&mut st, 2);
    w.page_with(&mut st, vec![key(Key::Escape, Modifiers::NONE)]);
    assert!(st.editor_mut().is_none(), "Escape closes the editor");
    assert!(st.pending.is_empty());
}

#[tokio::test]
async fn list_rows_add_remove_and_reorder_into_toml_arrays() {
    let service = fake();
    let data = load_data(&service).await;
    let mut st = state(model(&data));
    let path = "notify";
    let kind = st.model().unwrap().entry(path).unwrap().kind.clone();
    assert_eq!(
        kind,
        Kind::List {
            item: Item::Text { path: false }
        }
    );
    let rows = st.rows_for(path).unwrap();
    assert_eq!(rows.list, [NOTIFIER, "--flag", "--title", "Codex finished"]);

    // Reorder: "--title" up.
    assert!(move_row(&mut rows.list, 2, -1));
    st.commit_rows(path);
    assert_eq!(
        written(&st, &data, path),
        toml::Value::Array(vec![
            NOTIFIER.into(),
            "--title".into(),
            "--flag".into(),
            "Codex finished".into()
        ])
    );
    // Remove and add (an added row stays out until it has text).
    let rows = st.rows_for(path).unwrap();
    rows.list.remove(1);
    rows.list.push(String::new());
    st.commit_rows(path);
    assert_eq!(
        st.rows_for(path).unwrap().list.len(),
        4,
        "the new row stays"
    );
    assert_eq!(
        written(&st, &data, path),
        toml::Value::Array(vec![
            NOTIFIER.into(),
            "--flag".into(),
            "Codex finished".into()
        ])
    );
    let rows = st.rows_for(path).unwrap();
    *rows.list.last_mut().unwrap() = " spaced \"quoted\" arg ".into();
    st.commit_rows(path);
    assert_eq!(
        written(&st, &data, path),
        toml::Value::Array(vec![
            NOTIFIER.into(),
            "--flag".into(),
            "Codex finished".into(),
            " spaced \"quoted\" arg ".into()
        ])
    );
    // Discarding rebuilds the rows from the file value.
    st.pending.clear();
    assert_eq!(st.rows_for(path).unwrap().list.len(), 4);
    assert_eq!(st.rows_for(path).unwrap().list[1], "--flag");

    // Path lists.
    let roots = "sandbox_workspace_write.writable_roots";
    assert_eq!(
        st.model().unwrap().entry(roots).unwrap().kind,
        Kind::List {
            item: Item::Text { path: true }
        }
    );
    let rows = st.rows_for(roots).unwrap();
    rows.list.push("/home/someone/work tree/with spaces".into());
    st.commit_rows(roots);
    assert_eq!(
        written(&st, &data, roots),
        toml::Value::Array(vec![
            "/srv/data/projects/alpha".into(),
            "/tmp/build-cache".into(),
            "/home/someone/work tree/with spaces".into()
        ])
    );

    // The pure helpers.
    let v = list_value(
        &["a".into(), String::new(), "b".into()],
        &Item::Text { path: false },
    )
    .unwrap();
    assert_eq!(v, Value::Other("[\"a\", \"b\"]".into()));
    assert_eq!(list_items(&v).unwrap(), ["a", "b"]);
    assert_eq!(
        list_value(&["1".into(), "x".into()], &Item::Integer),
        Err(ParseError::NotInteger)
    );
    assert_eq!(
        list_value(&["1".into(), " 20 ".into()], &Item::Integer).unwrap(),
        Value::Other("[1, 20]".into())
    );
    let mut r = vec![1, 2, 3];
    assert!(!move_row(&mut r, 0, -1));
    assert!(!move_row(&mut r, 2, 1));
    assert!(move_row(&mut r, 0, 1));
    assert_eq!(r, [2, 1, 3]);
}

#[tokio::test]
async fn map_rows_edit_tables_as_key_value_pairs() {
    let service = fake();
    let data = load_data(&service).await;
    let mut st = state(model(&data));
    let path = "shell_environment_policy.set";
    let entry = st
        .model()
        .unwrap()
        .entry(path)
        .cloned()
        .expect("maps are listed");
    assert_eq!(
        entry.kind,
        Kind::Map {
            value: Item::Text { path: false }
        }
    );
    assert!(entry.text.zh.is_some() && entry.name_zh.is_some());
    let rows = st.rows_for(path).unwrap();
    assert_eq!(
        rows.map,
        [
            (
                "PATH".to_owned(),
                "/opt/tools/bin:/usr/local/bin:/usr/bin:/bin".to_owned()
            ),
            ("RUST_LOG".to_owned(), "info".to_owned())
        ]
    );
    rows.map[1].1 = "debug,hyper=warn".into();
    rows.map.push(("EDITOR".into(), "code --wait".into()));
    st.commit_rows(path);
    let mut want = toml::Table::new();
    want.insert("EDITOR".into(), "code --wait".into());
    want.insert(
        "PATH".into(),
        "/opt/tools/bin:/usr/local/bin:/usr/bin:/bin".into(),
    );
    want.insert("RUST_LOG".into(), "debug,hyper=warn".into());
    assert_eq!(written(&st, &data, path), toml::Value::Table(want));

    // A duplicate key is refused (nothing new pending), removing works.
    let before = st.pending.get(path).cloned();
    let rows = st.rows_for(path).unwrap();
    rows.map.push(("PATH".into(), "/bin".into()));
    st.commit_rows(path);
    assert_eq!(st.pending.get(path).cloned(), before);
    let rows = st.rows_for(path).unwrap();
    rows.map.pop();
    rows.map.remove(0);
    st.commit_rows(path);
    let toml::Value::Table(t) = written(&st, &data, path) else {
        panic!()
    };
    assert_eq!(t.keys().collect::<Vec<_>>(), ["EDITOR", "RUST_LOG"]);

    // The pure helpers.
    let text = Item::Text { path: false };
    assert_eq!(
        map_value(&[("A".into(), "1".into()), ("A".into(), "2".into())], &text),
        Err(ParseError::DuplicateKey)
    );
    assert_eq!(
        map_value(&[(String::new(), "x".into())], &text),
        Err(ParseError::EmptyKey)
    );
    let v = map_value(
        &[
            ("B".into(), "2".into()),
            (String::new(), String::new()),
            ("A".into(), "1".into()),
        ],
        &text,
    )
    .unwrap();
    assert_eq!(v, Value::Other("{ A = \"1\", B = \"2\" }".into()));
    assert_eq!(
        map_rows(&v).unwrap(),
        [("A".to_owned(), "1".to_owned()), ("B".into(), "2".into())]
    );
    assert_eq!(
        map_value(&[("on".into(), "maybe".into())], &Item::Bool),
        Err(ParseError::NotBool)
    );
}

// ---------------------------------------------------------------------------
// Widths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn long_values_stay_inside_the_list_chip_and_the_detail_pane() {
    let service = fake();
    let data = load_data(&service).await;
    for locale in [Locale::ZhCn, Locale::En] {
        let mut st = state(model(&data));
        let w = Window::new(locale, load_data(&service).await);
        for path in [
            "developer_instructions",
            "model_instructions_file",
            "notify",
            "shell_environment_policy.set",
            "sandbox_workspace_write.writable_roots",
        ] {
            st.filter.query = path.into();
            st.focus(path);
            let texts = w.page(&mut st, 3);
            assert_eq!(
                st.selected().map(|e| e.path.as_str()),
                Some(path),
                "{locale:?}"
            );
            // Nothing is drawn past the window; the detail pane (the right 372
            // px of the page) keeps every line inside it.
            let page_right = w.size[0] - 24.0;
            for (t, r) in &texts {
                assert!(r.max.x <= w.size[0] + 0.5, "{path} {t:?} at {r:?}");
                if r.min.x >= page_right - 372.0 {
                    assert!(r.max.x <= page_right + 0.5, "{path}: {t:?} at {r:?}");
                }
            }
        }
        // The list chip of the long instructions: one line, at most 150 px.
        st.filter.query = "developer_instructions".into();
        st.focus("developer_instructions");
        let texts = w.page(&mut st, 3);
        let chip = texts
            .iter()
            .find(|(t, _)| t.starts_with("You are a") && t.ends_with('…'))
            .unwrap_or_else(|| panic!("{texts:?}"));
        assert!(chip.1.width() <= 150.0, "{chip:?}");
        let hint = if locale == Locale::ZhCn {
            "+5 行"
        } else {
            "+5 lines"
        };
        assert!(texts.iter().any(|(t, _)| t == hint), "{texts:?}");
    }
}

#[tokio::test]
async fn the_diff_dialog_never_grows_past_its_width() {
    let service = fake();
    let data = load_data(&service).await;
    let mut st = state(model(&data));
    let long = INSTRUCTIONS.repeat(30);
    st.edit("developer_instructions", Change::Set(Value::Str(long)));
    st.edit(
        "model_instructions_file",
        Change::Set(Value::Str(format!("{MODEL_FILE}{}", "/deeper".repeat(40)))),
    );
    let w = Window::new(Locale::ZhCn, load_data(&service).await);
    // The page behind the dialog, to tell its texts apart.
    let page = w.page(&mut st, 3);
    st.diff_open = true;
    // Open the full change of both rows.
    for path in ["developer_instructions", "model_instructions_file"] {
        w.ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique(("codex-settings-diff-open", path)), true)
        });
    }
    let texts = w.page(&mut st, 4);
    let r = w
        .area(egui::Id::unique("codex-settings-diff"))
        .expect("the diff dialog is drawn");
    // 600 px of content, 24 px of padding and the 1 px border on each side.
    assert!(r.width() <= 650.5, "{r:?}");
    assert!(r.height() <= w.size[1], "{r:?}");
    let dialog: Vec<&(String, Rect)> = texts.iter().filter(|t| !page.contains(t)).collect();
    assert!(dialog.len() > 5, "{dialog:?}");
    for (t, tr) in &dialog {
        assert!(r.contains_rect(*tr), "{t:?} at {tr:?} leaves {r:?}");
    }
    // The full change is shown line by line.
    assert!(dialog.iter().any(|(t, _)| t.contains("Keep answers short")));
}

#[test]
fn change_rows_stay_in_their_width() {
    let w = Window::new(Locale::En, Data::default());
    let before = SettingValue::String(INSTRUCTIONS.into()).to_toml_repr();
    let after = SettingValue::String(INSTRUCTIONS.replace("short", "brief")).to_toml_repr();
    let url = "\"https://relay.very-long-provider-name.example.com/v1/responses\"";
    let id = egui::Id::unique("test-change-row");
    w.ctx.data_mut(|d| d.insert_temp(id, true));
    let mut show = |ui: &mut egui::Ui, cx: &Cx<'_>, _out: &mut Vec<Action>| {
        let rect = Rect::from_min_size(ui.min_rect().min, egui::vec2(420.0, 600.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        values::change_row(
            &mut child,
            cx,
            id,
            Some("devbox"),
            "developer_instructions",
            Some(&before),
            Some(&after),
            Some("Profile setting"),
            "—",
        );
        values::change_row(
            &mut child,
            cx,
            egui::Id::unique("test-change-row-2"),
            None,
            "openai_base_url",
            None,
            Some(url),
            None,
            "—",
        );
    };
    let texts = w.frames(3, &mut show);
    let left = texts.iter().map(|(_, r)| r.min.x).fold(f32::MAX, f32::min);
    for (t, r) in &texts {
        assert!(r.max.x <= left + 420.5, "{t:?} at {r:?}");
    }
    // The URL keeps (the start of) its host; the open row shows the changed line.
    assert!(
        texts
            .iter()
            .any(|(t, _)| t.starts_with("relay.very") && t.ends_with('…')),
        "{texts:?}"
    );
    assert!(texts.iter().any(|(t, _)| t.contains("brief")), "{texts:?}");
}

#[tokio::test]
async fn the_host_sync_dialog_keeps_its_width_with_long_values() {
    let target = TargetId::remote("devbox-7c1e");
    let mut plan = FakeService::fixture_sync_plan(&target);
    plan.rows.push(HostSyncRow {
        path: "developer_instructions".into(),
        local: Some(SettingValue::String(INSTRUCTIONS.into()).to_toml_repr()),
        remote: Some(SettingValue::String("Be brief.".into()).to_toml_repr()),
        action: HostSyncAction::Write,
        selected: true,
    });
    plan.rows.push(HostSyncRow {
        path: "notify".into(),
        local: Some(SettingValue::Array(vec![NOTIFIER.into(), "--flag".into()]).to_toml_repr()),
        remote: None,
        action: HostSyncAction::Write,
        selected: true,
    });
    let service = fake();
    let data = load_data(&service).await;
    let w = Window::new(Locale::ZhCn, data);
    let mut state = hosts::State::default();
    state.sync = Some(hosts::sync::SyncDialog::with_plan(
        target.clone(),
        "devbox".into(),
        plan,
    ));
    w.ctx.data_mut(|d| {
        d.insert_temp(
            egui::Id::unique(("hosts-sync-open", target.as_str(), "developer_instructions")),
            true,
        )
    });
    let mut show = |ui: &mut egui::Ui, cx: &Cx<'_>, out: &mut Vec<Action>| {
        hosts::show(ui, cx, &mut state, out);
    };
    let texts = w.frames(4, &mut show);
    let r = w
        .area(egui::Id::unique(("hosts-sync", target.as_str())))
        .expect("the sync dialog is drawn");
    assert!(r.width() <= 720.0 + 50.5, "{r:?}");
    assert!(r.height() <= w.size[1], "{r:?}");
    assert!(texts.iter().any(|(t, _)| t == "+5 行"), "{texts:?}");
    assert!(
        texts.iter().any(|(t, _)| t.ends_with("/notifier")),
        "the notifier keeps its file name: {texts:?}"
    );
}
