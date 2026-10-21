//! The tray menu as plain data (item 70), built from the app's data and
//! rendered by the platform icon (muda menus on Windows and macOS, a dbusmenu
//! on Linux):
//!
//! ```text
//! Open Superba
//! ───────────────
//! Krill · Hong Kong                (what Codex uses now)
//! Switch to                      ▸ providers, their lines, official ChatGPT
//! ───────────────
//! Open / Reopen ChatGPT with Superba
//! ───────────────
//! Router: on · 127.0.0.1:17421
//! Turn router off                  (off: "Turn router on…" opens the Router page)
//! ───────────────
//! Quit Superba
//! ```

use superba_core::{LineId, ProfileId, RouterWatchState};

use crate::i18n::Tr;
use crate::launch::{AppLaunch, app_launch};
use crate::pages::Data;
use crate::pages::router::model::Mode;

/// What a menu item does.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TrayAction {
    /// Show the window.
    Open,
    /// Switch to a provider (its default or last line) or one of its lines.
    Switch {
        /// The provider.
        profile: ProfileId,
        /// The line; `None`: the provider's own choice.
        line: Option<LineId>,
    },
    /// Back to the official ChatGPT login.
    RestoreOfficial,
    /// Open, reopen or bring forward ChatGPT through Superba (GH#7).
    LaunchApp,
    /// Turn router mode off.
    RouterOff,
    /// Open the Router page, where router mode is turned on (it checks the
    /// members first).
    RouterPage,
    /// Open a Superba section in the window.
    Superba(crate::pages::SuperbaSection),
    /// Quit Superba (the router keeps running as its own service).
    Quit,
}

/// One entry of the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayItem {
    /// A clickable item; `checked` makes it a check item.
    Action {
        /// Text.
        label: String,
        /// What it does.
        action: TrayAction,
        /// Clickable.
        enabled: bool,
        /// A check mark (`Some(true)` = checked).
        checked: Option<bool>,
    },
    /// A line of information (shown disabled).
    Label(String),
    /// A submenu.
    Submenu {
        /// Text.
        label: String,
        /// Can be opened.
        enabled: bool,
        /// Entries.
        items: Vec<TrayItem>,
    },
    /// A separator.
    Separator,
}

impl TrayItem {
    fn action(label: String, action: TrayAction, enabled: bool) -> Self {
        TrayItem::Action {
            label,
            action,
            enabled,
            checked: None,
        }
    }

    fn check(label: String, action: TrayAction, enabled: bool, checked: bool) -> Self {
        TrayItem::Action {
            label,
            action,
            enabled,
            checked: Some(checked),
        }
    }
}

/// The whole menu.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayMenu {
    /// Entries, top to bottom.
    pub items: Vec<TrayItem>,
}

impl TrayMenu {
    /// Every clickable action, depth first (tests, the platform id tables).
    pub fn actions(&self) -> Vec<(&str, &TrayAction, bool)> {
        fn walk<'a>(items: &'a [TrayItem], out: &mut Vec<(&'a str, &'a TrayAction, bool)>) {
            for item in items {
                match item {
                    TrayItem::Action {
                        label,
                        action,
                        enabled,
                        ..
                    } => out.push((label, action, *enabled)),
                    TrayItem::Submenu { items, .. } => walk(items, out),
                    TrayItem::Label(_) | TrayItem::Separator => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, &mut out);
        out
    }

    /// The item for `action`, if the menu has it.
    pub fn find(&self, action: &TrayAction) -> Option<&TrayItem> {
        fn walk<'a>(items: &'a [TrayItem], want: &TrayAction) -> Option<&'a TrayItem> {
            items.iter().find_map(|item| match item {
                TrayItem::Action { action, .. } if action == want => Some(item),
                TrayItem::Submenu { items, .. } => walk(items, want),
                _ => None,
            })
        }
        walk(&self.items, action)
    }
}

/// What Codex uses now, in one line ("Krill · Hong Kong", "Official
/// ChatGPT login"), for the menu's first line and the icon's tooltip.
pub fn current_text(tr: &Tr, data: &Data) -> String {
    if data.official_active() {
        return tr.t("tray.current.official");
    }
    let Some(profile) = data.active_profile() else {
        return if data.active_state().is_some() {
            tr.t("tray.current.unknown")
        } else {
            tr.t("tray.current.none")
        };
    };
    let Some(site) = data.site(&profile.site_id) else {
        return profile.name.clone();
    };
    let provider = crate::app::provider_name(tr, profile, &site);
    let line = data
        .active_state()
        .and_then(|a| a.line_id.as_ref())
        .and_then(|id| site.lines.iter().find(|l| &l.id == id))
        .filter(|_| site.lines.len() > 1)
        .map(|l| tr.text(&l.name));
    match line {
        Some(line) => format!("{provider} · {line}"),
        None => provider,
    }
}

/// The icon's tooltip.
pub fn tooltip(tr: &Tr, data: &Data) -> String {
    format!("Superba · {}", current_text(tr, data))
}

/// Build the menu. `busy`: a switch is planned or running (switching, the
/// router and ChatGPT items are disabled).
pub fn build(tr: &Tr, data: &Data, busy: bool) -> TrayMenu {
    let mut items = vec![
        TrayItem::action(tr.t("tray.menu.open"), TrayAction::Open, true),
        TrayItem::Separator,
        TrayItem::Label(current_text(tr, data)),
    ];
    let switch = switch_items(tr, data, busy);
    if !switch.is_empty() {
        items.push(TrayItem::Submenu {
            label: tr.t("tray.menu.switch"),
            enabled: !busy,
            items: switch,
        });
    }
    if let Some(label) = app_label(tr, data) {
        items.push(TrayItem::Separator);
        items.push(TrayItem::action(
            label,
            TrayAction::LaunchApp,
            !busy && !data.launching_app,
        ));
    }
    let router = router_items(tr, data, busy);
    if !router.is_empty() {
        items.push(TrayItem::Separator);
        items.extend(router);
    }
    items.push(TrayItem::Separator);
    items.push(TrayItem::Submenu {
        label: tr.t("superba.title"),
        enabled: true,
        items: crate::pages::SuperbaSection::ALL
            .into_iter()
            .map(|section| {
                TrayItem::action(
                    tr.t(section.title_key()),
                    TrayAction::Superba(section),
                    true,
                )
            })
            .collect(),
    });
    items.push(TrayItem::action(
        tr.t("tray.menu.quit"),
        TrayAction::Quit,
        true,
    ));
    TrayMenu { items }
}

/// The switch submenu: every provider (a submenu of lines when it has
/// several), the one in use checked, then the official ChatGPT login.
fn switch_items(tr: &Tr, data: &Data, busy: bool) -> Vec<TrayItem> {
    let active = data.active_state();
    let active_profile = data.active_profile().map(|p| p.id.clone());
    let mut items = Vec::new();
    for profile in data.providers() {
        let Some(site) = data.site(&profile.site_id) else {
            continue;
        };
        let name = crate::app::provider_name(tr, profile, &site);
        let is_active = active_profile.as_ref() == Some(&profile.id);
        if site.lines.len() > 1 {
            let lines = site
                .lines
                .iter()
                .map(|line| {
                    let current =
                        is_active && active.is_some_and(|a| a.line_id.as_ref() == Some(&line.id));
                    TrayItem::check(
                        tr.text(&line.name),
                        TrayAction::Switch {
                            profile: profile.id.clone(),
                            line: Some(line.id.clone()),
                        },
                        // Choosing the line in use would only restart Codex.
                        !busy && !current,
                        current,
                    )
                })
                .collect();
            items.push(TrayItem::Submenu {
                label: if is_active {
                    tr.f("tray.menu.provider_current", &[("provider", &name)])
                } else {
                    name
                },
                enabled: !busy,
                items: lines,
            });
        } else {
            items.push(TrayItem::check(
                name,
                TrayAction::Switch {
                    profile: profile.id.clone(),
                    line: None,
                },
                !busy && !is_active,
                is_active,
            ));
        }
    }
    if !items.is_empty() {
        items.push(TrayItem::Separator);
    }
    let official = data.official_active();
    items.push(TrayItem::check(
        tr.t("tray.menu.official"),
        TrayAction::RestoreOfficial,
        !busy && !official,
        official,
    ));
    items
}

/// "Open / Reopen ChatGPT with Superba", "Show ChatGPT", or nothing without a
/// desktop app (GH#7's [`app_launch`]).
fn app_label(tr: &Tr, data: &Data) -> Option<String> {
    let overview = data.overview.value.as_ref()?;
    match app_launch(overview) {
        Some(AppLaunch::Open) => Some(tr.t("tray.menu.open_app")),
        Some(AppLaunch::Reopen(_)) => Some(tr.t("tray.menu.reopen_app")),
        None if overview.app.session.is_some() => Some(tr.t("tray.menu.show_app")),
        None => None,
    }
}

/// The router's state and its on/off; nothing before the configuration is
/// known.
fn router_items(tr: &Tr, data: &Data, busy: bool) -> Vec<TrayItem> {
    let mode = Mode::from_config(
        data.overview.value.as_ref().and_then(|o| o.config.as_ref()),
        data.router_enabled(),
        data.overview
            .value
            .as_ref()
            .and_then(|o| o.router_base_url.as_ref()),
    );
    match mode {
        Mode::Unknown => Vec::new(),
        Mode::Router { port } => {
            let state = match &data.router_watch {
                Some(RouterWatchState::Suspect { .. }) => tr.t("tray.router.suspect"),
                _ => tr.f("tray.router.on", &[("addr", &format!("127.0.0.1:{port}"))]),
            };
            vec![
                TrayItem::Label(state),
                TrayItem::action(tr.t("tray.router.turn_off"), TrayAction::RouterOff, !busy),
            ]
        }
        Mode::Direct | Mode::Local { .. } => vec![
            TrayItem::Label(tr.t("tray.router.off")),
            TrayItem::action(tr.t("tray.router.turn_on"), TrayAction::RouterPage, true),
        ],
    }
}
