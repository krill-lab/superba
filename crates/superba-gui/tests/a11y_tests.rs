#![allow(missing_docs)]

//! Accessible names (user bug 56): the whole app on a fake service with
//! AccessKit on, walked page by page, dialog by dialog and wizard step by
//! wizard step. Every node a user operates or moves the keyboard focus to —
//! text inputs, buttons, tabs, selectable rows, check boxes, combo boxes,
//! radio groups, lists and tables — must have a name, in English and Chinese.
//! Every step is reached the way a screen reader or the Windows UI Automation
//! harness does it: by name, through AccessKit actions (click, set value).
//!
//! The Usage page is checked in detail: its banner is text in the tree, the
//! request table is a named table with column headers, one row per request
//! named "time, session, status, model, cost" with its cells, and the detail
//! pane's General / Timing tabs are selectable, invokable tabs.

mod a11y_harness;
mod support;

use std::sync::Arc;

use a11y_harness::{AppHarness, Tree};
use egui::accesskit::{Action, ActionRequest, NodeId, Role, TreeId};
use superba_core::Locale;
use superba_gui::design::widgets::Toasts;
use superba_gui::i18n::Tr;
use superba_gui::notices;
use superba_gui::pages::Page;
use support::fake::{FakeOptions, FakeService};

const LOCALES: [Locale; 2] = [Locale::En, Locale::ZhCn];

fn fake(first_run: bool) -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        first_run,
        ..Default::default()
    }))
}

/// Fail with every unnamed control of `state` (and the whole tree).
#[track_caller]
fn assert_named(state: &str, t: &Tree) {
    let unnamed = t.unnamed();
    assert!(
        unnamed.is_empty(),
        "{state}: {} control(s) without an accessible name:\n  {}\n\nwhole tree:\n{}",
        unnamed.len(),
        unnamed.join("\n  "),
        t.dump()
    );
}

/// Content a page shows once its reads are done.
fn loaded(page: Page, t: &Tree) -> bool {
    match page {
        Page::Providers | Page::Hosts => !t.with_role(Role::ListItem).is_empty(),
        Page::Models | Page::Usage => !t.with_role(Role::Row).is_empty(),
        Page::Settings => t.with_role(Role::ListItem).len() > 2,
        Page::Sessions => !t.with_role(Role::Table).is_empty(),
        _ => true,
    }
}

#[test]
fn every_page_names_its_controls_in_both_languages() {
    for locale in LOCALES {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(fake(false), locale, Page::Overview);
        // The hero opens folded (user request 30): the conclusion and the
        // "detail" toggle are what a reader sees first, and both must be named.
        let t = h.wait_for(Some(Role::Button), &tr.t("overview.hero.detail"));
        assert_named(&format!("{locale:?} Overview (folded)"), &t);

        // Unfolding brings the request path back: its node buttons (This
        // computer, router, provider, model) stay named and keep their menus.
        h.click(Some(Role::Button), &tr.t("overview.hero.detail"));
        let this = tr.t("overview.hero.this");
        let t = h.wait_for(Some(Role::Button), &this);
        assert!(
            t.has(&tr.t("overview.hero.switch_menu")) && t.has(&tr.t("overview.hero.speed_menu")),
            "{}",
            t.dump()
        );
        assert_named(&format!("{locale:?} Overview › request path"), &t);

        // The Overview's menus: More, switch provider or line, speed tier.
        let t = h.click(Some(Role::Button), &tr.t("overview.menu.title"));
        assert_named(&format!("{locale:?} Overview › More"), &t);
        h.key(egui::Key::Escape);
        let t = h.click(Some(Role::Button), &tr.t("overview.hero.switch_menu"));
        assert!(t.has(&tr.t("overview.other_providers")), "{}", t.dump());
        assert_named(&format!("{locale:?} Overview › provider popover"), &t);
        h.key(egui::Key::Escape);
        let t = h.click(Some(Role::Button), &tr.t("overview.hero.speed_menu"));
        assert!(
            t.find(Some(Role::RadioGroup), &tr.t("overview.speed.label"))
                .is_some(),
            "{}",
            t.dump()
        );
        assert_named(&format!("{locale:?} Overview › speed popover"), &t);
        h.key(egui::Key::Escape);

        // Every page through its sidebar tab.
        for page in Page::DESTINATIONS {
            h.go(page, &tr);
            let t = h.until(&format!("{locale:?} {page:?} loaded"), |t| loaded(page, t));
            assert_named(&format!("{locale:?} {page:?}"), &t);
        }

        // The command palette's search field, from the Overview's menu (the
        // sidebar has no search field since user bug 85; Cmd/Ctrl+K opens it
        // too).
        h.go(Page::Overview, &tr);
        h.click(Some(Role::Button), &tr.t("overview.menu.title"));
        let search = format!(
            "{} {}",
            tr.t("shell.search"),
            superba_gui::shell::palette_shortcut_label()
        );
        let t = h.click(Some(Role::Button), &search);
        assert!(
            t.find(Some(Role::TextInput), &tr.t("palette.placeholder"))
                .is_some(),
            "{}",
            t.dump()
        );
        assert_named(&format!("{locale:?} command palette"), &t);
    }
}

#[test]
fn the_add_provider_dialog_names_its_fields_by_their_labels() {
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(fake(false), Locale::En, Page::Providers);
    let t = h.click(Some(Role::Button), &tr.t("providers.add.button"));
    // The dialog is named by its title.
    let dialog = t.with_role(Role::Dialog);
    assert!(
        dialog.iter().any(|d| t
            .name(*d)
            .is_some_and(|n| n.starts_with(&tr.t("providers.add.title")))),
        "{}",
        t.dump()
    );
    assert_named("add provider", &t);

    // Krill: the name and key fields.
    let t = h.click(Some(Role::Button), &tr.t("providers.add.continue"));
    assert!(
        t.find(Some(Role::TextInput), &tr.t("sites.form.name"))
            .is_some(),
        "{}",
        t.dump()
    );
    assert!(
        t.find(Some(Role::PasswordInput), &tr.t("providers.row.key"))
            .is_some(),
        "{}",
        t.dump()
    );
    assert_named("add provider › Krill", &t);

    // A custom site: name, base URL and key, each named by its label.
    h.click(Some(Role::Button), &tr.t("providers.add.back"));
    h.click(
        Some(Role::RadioButton),
        &tr.t("providers.preset.custom.title"),
    );
    let t = h.click(Some(Role::Button), &tr.t("providers.add.continue"));
    for (role, name) in [
        (Role::TextInput, tr.t("sites.form.name")),
        (Role::TextInput, tr.t("providers.base_url")),
        (Role::PasswordInput, tr.t("sites.form.key")),
    ] {
        assert!(
            t.find(Some(role), &name).is_some(),
            "no {role:?} {name:?}:\n{}",
            t.dump()
        );
    }
    assert_named("add provider › custom site", &t);
    // A field is reached and filled by its name, as UI Automation does it.
    let t = h.set_value(&tr.t("providers.base_url"), "https://api.example.com/v1");
    let url = t
        .find(Some(Role::TextInput), &tr.t("providers.base_url"))
        .unwrap();
    assert_eq!(t.node(url).value(), Some("https://api.example.com/v1"));
    h.key(egui::Key::Escape);

    // Replacing the provider's key on its Connection tab.
    let t = h.click(Some(Role::Button), &tr.t("providers.key.replace"));
    assert!(!t.with_role(Role::PasswordInput).is_empty(), "{}", t.dump());
    assert_named("providers › replace key", &t);
}

#[test]
fn settings_editors_name_their_fields() {
    let mut h = AppHarness::new(fake(false), Locale::En, Page::Settings);
    let t = h.until("settings", |t| loaded(Page::Settings, t));
    // The category rail (its buttons are as wide as the rail).
    let width = |t: &Tree, id: NodeId| {
        t.node(id)
            .bounds()
            .map_or(0.0, |b| ((b.x1 - b.x0) * 10.0).round() / 10.0)
    };
    let first = t.find(Some(Role::Button), "Common").expect("the rail");
    let rail = width(&t, first);
    let categories: Vec<String> = t
        .with_role(Role::Button)
        .into_iter()
        .filter(|id| width(&t, *id) == rail)
        .filter_map(|id| t.name(id))
        .collect();
    assert!(categories.len() > 10, "{categories:?}");
    let mut entries = 0;
    for c in &categories {
        let t = h.click(Some(Role::Button), c);
        let items = t.with_role(Role::ListItem);
        for item in items.iter().take(4) {
            let name = t.name(*item);
            let t = h.click_id(*item);
            entries += 1;
            assert_named(&format!("settings › {c} › {name:?}"), &t);
        }
    }
    assert!(entries > 30, "{entries} entries opened");
}

#[test]
fn the_wizard_names_its_fields_and_takes_a_key_through_accesskit() {
    for locale in LOCALES {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(fake(true), locale, Page::Overview);
        let next = tr.t("wizard.continue");
        // Screen 1 asks one question and answers it with two cards: the
        // recommended relay and the login the user already has.
        let t = h.wait_for(Some(Role::Button), &next);
        assert_named(&format!("{locale:?} wizard › provider"), &t);
        assert!(
            t.find(Some(Role::RadioButton), &tr.t("wizard.preset.krill.title"))
                .is_some(),
            "{}",
            t.dump()
        );
        let t = h.click(Some(Role::Button), &next);
        assert_named(&format!("{locale:?} wizard › key"), &t);
        // The key field is named by the step's title (it was `Edit ''`).
        let key = tr.f("wizard.credentials.key.title", &[("provider", &"Krill")]);
        assert!(
            t.find(Some(Role::PasswordInput), &key).is_some(),
            "{}",
            t.dump()
        );
        h.set_value(&key, "nb_test_key_0123456789abcdef");
        // Line & model are folded into this screen's 「高级」.
        let t = h.click(Some(Role::Button), &tr.t("wizard.advanced"));
        assert!(
            t.find(Some(Role::ComboBox), &tr.t("wizard.models.title"))
                .is_some(),
            "{}",
            t.dump()
        );
        assert_named(&format!("{locale:?} wizard › line and model"), &t);
        let t = h.click(Some(Role::Button), &next);
        assert!(t.has(&tr.t("wizard.apply")), "{}", t.dump());
        assert_named(&format!("{locale:?} wizard › confirm"), &t);
        // The detection report stays reachable inside 「详情」. The page behind
        // the wizard can carry a button with the same name, so take the last
        // match inside the wizard's column: the wizard is drawn over the page.
        let t = h.wait_for(Some(Role::Button), &tr.t("wizard.details"));
        let id = *t
            .find_all(Some(Role::Button), &tr.t("wizard.details"))
            .into_iter()
            .filter(|n| {
                t.node(*n)
                    .bounds()
                    .is_some_and(|b| b.x0 >= 299.0 && b.x1 <= 981.0)
            })
            .collect::<Vec<_>>()
            .last()
            .expect("the wizard's 「详情」");
        let t = h.click_id(id);
        assert!(
            t.has(&tr.t("wizard.detect.title")),
            "{locale:?}: no detection report:\n{}",
            t.dump()
        );

        // The custom site's fields, named by their labels: it lives under
        // 「其它方式」 on the first screen.
        let back = tr.t("wizard.back");
        h.click(Some(Role::Button), &back);
        h.click(Some(Role::Button), &back);
        let t = h.click(Some(Role::Button), &tr.t("wizard.provider.other"));
        assert!(
            t.find(Some(Role::RadioButton), &tr.t("wizard.preset.custom.title"))
                .is_some(),
            "{}",
            t.dump()
        );
        h.click(Some(Role::RadioButton), &tr.t("wizard.preset.custom.title"));
        let t = h.click(Some(Role::Button), &next);
        for (role, name) in [
            (Role::TextInput, tr.t("wizard.custom.name")),
            (Role::TextInput, tr.t("wizard.custom.url")),
            (Role::PasswordInput, tr.t("wizard.custom.key")),
        ] {
            assert!(
                t.find(Some(role), &name).is_some(),
                "{locale:?}: no {role:?} {name:?}:\n{}",
                t.dump()
            );
        }
        assert_named(&format!("{locale:?} wizard › custom site"), &t);
    }
}

#[test]
fn usage_banner_rows_cells_and_detail_tabs_are_readable_and_invokable() {
    for locale in LOCALES {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(fake(false), locale, Page::Usage);
        // Rows, with the session titles read from Codex's database (rows show
        // the short key until then).
        let t = h.until("request rows with their session titles", |t| {
            t.with_role(Role::Row)
                .iter()
                .any(|r| t.name(*r).is_some_and(|n| n.contains("登录页样式")))
        });
        assert_named(&format!("{locale:?} Usage"), &t);

        // The banner says where the rows come from, as text in the tree.
        let off = tr.t("usage.banner.off");
        assert!(
            t.with_role(Role::Label)
                .iter()
                .any(|id| t.node(*id).value() == Some(off.as_str())),
            "{locale:?}: no banner {off:?}:\n{}",
            t.dump()
        );
        // Filters and search are named by what they choose.
        for (role, name) in [
            (Role::TextInput, tr.t("requests.search")),
            (Role::RadioGroup, tr.t("requests.range.name")),
            (Role::ComboBox, tr.t("requests.col.provider")),
            (Role::ComboBox, tr.t("requests.col.model")),
            (Role::ComboBox, tr.t("requests.col.status")),
            (Role::ComboBox, tr.t("requests.col.client")),
        ] {
            assert!(
                t.find(Some(role), &name).is_some(),
                "{locale:?}: no {role:?} {name:?}:\n{}",
                t.dump()
            );
        }

        // A named table with its column headers; sortable ones are invokable.
        let table = t
            .find(Some(Role::Table), &tr.t("usage.tab.recent"))
            .unwrap_or_else(|| panic!("no request table:\n{}", t.dump()));
        assert!(t.node(table).row_count().is_some_and(|n| n > 0));
        for col in ["date", "session", "status", "cost"] {
            let name = tr.t(&format!("requests.col.{col}"));
            let header = t
                .find(Some(Role::ColumnHeader), &name)
                .unwrap_or_else(|| panic!("no column header {name:?}"));
            assert!(t.node(header).supports_action(Action::Click), "{name}");
        }

        // One row per request in view: named by its summary, selectable,
        // invokable, with its cells.
        let rows = t.with_role(Role::Row);
        let first = rows[0];
        let name = t.name(first).unwrap();
        let cells: Vec<String> = t
            .node(first)
            .children()
            .iter()
            .filter(|c| t.node(**c).role() == Role::Cell)
            .filter_map(|c| t.name(*c))
            .collect();
        assert!(cells.len() >= 10, "{locale:?}: cells {cells:?}");
        // time, session, status, model, cost
        let (time, session) = (&cells[1], &cells[2]);
        assert!(name.starts_with(&format!("{time}, {session}, ")), "{name}");
        assert!(name.contains("gpt-"), "the model is in {name:?}");
        assert!(name.contains(&tr.t("requests.col.cost")), "{name}");
        assert!(t.node(first).supports_action(Action::Click));
        assert_eq!(t.node(first).is_selected(), Some(false));

        // Invoke the row: it is selected and the detail pane opens on General.
        let t = h.click_id(first);
        let row = t.find(Some(Role::Row), &name).unwrap_or_else(|| {
            panic!(
                "row {name:?} gone after a click:\n{}",
                t.with_role(Role::Row)
                    .iter()
                    .filter_map(|r| t.name(*r))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
        assert_eq!(t.node(row).is_selected(), Some(true), "{}", t.dump());
        let general = tr.t("requests.detail.general");
        let timing = tr.t("requests.detail.timing");
        let g = t.find(Some(Role::Tab), &general).expect("General tab");
        let tm = t.find(Some(Role::Tab), &timing).expect("Timing tab");
        assert_eq!(t.node(g).is_selected(), Some(true));
        assert_eq!(t.node(tm).is_selected(), Some(false));
        assert!(t.find(Some(Role::TabPanel), &general).is_some());
        assert_named(&format!("{locale:?} Usage › General"), &t);

        // Invoke Timing: its panel replaces General and reads the marks.
        let t = h.click(Some(Role::Tab), &timing);
        let tm = t.find(Some(Role::Tab), &timing).unwrap();
        assert_eq!(t.node(tm).is_selected(), Some(true), "{}", t.dump());
        let panel = t
            .find(Some(Role::TabPanel), &timing)
            .unwrap_or_else(|| panic!("no Timing panel:\n{}", t.dump()));
        assert!(!t.node(panel).children().is_empty());
        assert_named(&format!("{locale:?} Usage › Timing"), &t);

        // The other views of the page.
        for tab in ["usage.tab.active", "usage.tab.stats"] {
            let t = h.click(Some(Role::RadioButton), &tr.t(tab));
            assert_named(&format!("{locale:?} Usage › {tab}"), &t);
        }
    }
}

/// One frame of the toast stack alone (AccessKit on), with `events`; returns
/// its tree.
fn toast_frame(
    ctx: &egui::Context,
    toasts: &mut Toasts<superba_gui::app::ToastAction>,
    close: &str,
    events: Vec<egui::Event>,
) -> Tree {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        events,
        ..Default::default()
    };
    let output = ctx.run_ui(input, |ui| {
        toasts.show(ui.ctx(), close);
    });
    let tree = Tree::from_update(
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("AccessKit is on"),
    );
    output.drop_without_applying_deltas();
    tree
}

/// A toast's close button is a named button that closes it through AccessKit.
/// Shown with the notice an elevated Windows token gets (as on the Windows CI
/// runner, where its unnamed close button failed the settings walk above).
#[test]
fn toasts_name_their_close_button() {
    for locale in LOCALES {
        let tr = Tr::new(locale);
        let close = tr.t("action.close");
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut toasts = Toasts::default();
        toasts.push(notices::elevated_notice(&tr));
        toast_frame(&ctx, &mut toasts, &close, Vec::new());
        let t = toast_frame(&ctx, &mut toasts, &close, Vec::new());
        assert!(t.has(&tr.t("notice.elevated_title")), "{}", t.dump());
        assert_named(&format!("{locale:?} elevated notice"), &t);
        let button = t
            .find(Some(Role::Button), &close)
            .unwrap_or_else(|| panic!("no {close:?} button:\n{}", t.dump()));
        let click = egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: button,
            data: None,
        });
        toast_frame(&ctx, &mut toasts, &close, vec![click]);
        assert!(toasts.is_empty(), "{locale:?}: the toast stays open");
    }
}

/// User bug 77: badges and labels that carry a state are text in the
/// accessibility tree, so UI Automation (and a screen reader) reads them — on
/// Remote hosts the WSL distribution's "Running" / "Stopped" and an SSH host's
/// "Connected", in both languages. They were painted only before.
#[test]
fn state_badges_are_readable_in_both_languages() {
    let starts_with = |t: &Tree, role: Role, text: &str| {
        t.with_role(role)
            .into_iter()
            .find(|id| t.name(*id).is_some_and(|n| n.starts_with(text)))
    };
    for locale in LOCALES {
        let tr = Tr::new(locale);
        for running in [true, false] {
            let service = fake(false);
            if running {
                service.with_running_wsl();
            } else {
                service.with_stopped_wsl_pending();
            }
            let mut h = AppHarness::new(service, locale, Page::Hosts);
            let t = h.until(&format!("{locale:?} hosts"), |t| {
                starts_with(t, Role::ListItem, "WSL: Ubuntu").is_some()
            });
            let row = starts_with(&t, Role::ListItem, "WSL: Ubuntu").unwrap();
            h.click_id(row);
            let state = tr.t(if running {
                "hosts.wsl.running"
            } else {
                "hosts.wsl.stopped"
            });
            let t = h.until(&format!("{locale:?} WSL badge {state:?}"), |t| {
                starts_with(t, Role::Label, &state).is_some()
            });
            assert_named(&format!("{locale:?} Remote hosts › WSL"), &t);

            // An SSH host the app is connected to.
            let row = starts_with(&t, Role::ListItem, "devbox").expect("devbox listed");
            h.click_id(row);
            let connected = tr.t("hosts.connected");
            h.until(&format!("{locale:?} SSH badge {connected:?}"), |t| {
                starts_with(t, Role::Label, &connected).is_some()
            });
        }
    }
}
