//! Linux: a StatusNotifierItem with a dbusmenu on the session bus, through
//! ksni (pure Rust over zbus; no GTK, no libappindicator).
//!
//! KDE Plasma, Ubuntu's GNOME (the AppIndicator extension), Xfce, Cinnamon,
//! MATE, Budgie, LXQt, waybar and most panels host these items. Without a
//! `org.kde.StatusNotifierWatcher` on the bus (GNOME without the extension, a
//! bare window manager, no session bus at all) there is no tray: [`start`]
//! says so and the app quits when its window closes. A watcher that goes
//! away later (the extension turned off) and comes back is reported as
//! [`TrayEvent::HostLost`] / [`TrayEvent::HostBack`].
//!
//! ksni runs its D-Bus connection on its own thread; menu clicks call back
//! there and go to the [`Sink`]. Updates (a new menu or tooltip) are handed
//! to a small thread of ours, so the UI thread never waits for D-Bus.

use std::sync::mpsc;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{MenuItem, OfflineReason, ToolTip};

use super::{HideSupport, Sink, TrayAction, TrayEvent, TrayItem, TrayMenu, Unavailable, icon};

/// The item as ksni serves it.
struct Item {
    menu: TrayMenu,
    tooltip: String,
    icons: Vec<ksni::Icon>,
    sink: Sink,
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        "superba".into()
    }

    fn title(&self) -> String {
        "Superba".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Superba".into(),
            description: self.tooltip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.sink.push(TrayEvent::Activate);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        self.menu.items.iter().map(entry).collect()
    }

    fn watcher_online(&self) {
        self.sink.push(TrayEvent::HostBack);
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        tracing::info!(?reason, "the tray host went away");
        self.sink.push(TrayEvent::HostLost);
        // Keep listening: the host may come back (a shell restart).
        true
    }
}

/// A dbusmenu label: `_` marks an access key, `__` is one underscore.
fn label(text: &str) -> String {
    text.replace('_', "__")
}

fn activate(action: &TrayAction) -> Box<dyn Fn(&mut Item) + Send> {
    let action = action.clone();
    Box::new(move |item: &mut Item| item.sink.push(TrayEvent::Action(action.clone())))
}

fn entry(item: &TrayItem) -> MenuItem<Item> {
    match item {
        TrayItem::Action {
            label: text,
            action,
            enabled,
            checked: Some(checked),
        } => CheckmarkItem {
            label: label(text),
            enabled: *enabled,
            checked: *checked,
            activate: activate(action),
            ..Default::default()
        }
        .into(),
        TrayItem::Action {
            label: text,
            action,
            enabled,
            checked: None,
        } => StandardItem {
            label: label(text),
            enabled: *enabled,
            activate: activate(action),
            ..Default::default()
        }
        .into(),
        TrayItem::Label(text) => StandardItem {
            label: label(text),
            enabled: false,
            ..Default::default()
        }
        .into(),
        TrayItem::Submenu {
            label: text,
            enabled,
            items,
        } => SubMenu {
            label: label(text),
            enabled: *enabled,
            submenu: items.iter().map(entry).collect(),
            ..Default::default()
        }
        .into(),
        TrayItem::Separator => MenuItem::Separator,
    }
}

/// A change for the tray thread.
enum Update {
    Menu(TrayMenu),
    Tooltip(String),
}

/// The running item.
pub(super) struct Backend {
    updates: mpsc::Sender<Update>,
    hide: HideSupport,
}

impl Backend {
    pub(super) fn start(
        sink: Sink,
        menu: &TrayMenu,
        tooltip: &str,
    ) -> Result<(Self, HideSupport), Unavailable> {
        let wayland = std::env::var("WAYLAND_DISPLAY").ok();
        let display = std::env::var("DISPLAY").ok();
        let hide =
            super::linux_hide_support(wayland.as_deref(), display.as_deref(), super::forced_x11())
                .ok_or(Unavailable::CannotHide)?;
        let item = Item {
            menu: menu.clone(),
            tooltip: tooltip.to_owned(),
            icons: icon::colour_all()
                .into_iter()
                .map(|i| ksni::Icon {
                    width: i.size as i32,
                    height: i.size as i32,
                    data: icon::to_argb32(&i.rgba),
                })
                .collect(),
            sink,
        };
        // Not `assume_sni_available`: without a watcher there is no tray.
        let handle = item.spawn().map_err(|e| match e {
            ksni::Error::Watcher(_) | ksni::Error::WontShow => Unavailable::NoHost,
            other => {
                tracing::info!(error = %other, "no session bus for a tray icon");
                Unavailable::NoHost
            }
        })?;
        let (updates, rx) = mpsc::channel::<Update>();
        std::thread::Builder::new()
            .name("superba-tray".into())
            .spawn(move || {
                while let Ok(update) = rx.recv() {
                    // Only the newest menu matters.
                    let mut latest = vec![update];
                    latest.extend(rx.try_iter());
                    let menu = latest.iter().rev().find_map(|u| match u {
                        Update::Menu(m) => Some(m.clone()),
                        Update::Tooltip(_) => None,
                    });
                    let tooltip = latest.iter().rev().find_map(|u| match u {
                        Update::Tooltip(t) => Some(t.clone()),
                        Update::Menu(_) => None,
                    });
                    let alive = handle.update(|item| {
                        if let Some(m) = menu {
                            item.menu = m;
                        }
                        if let Some(t) = tooltip {
                            item.tooltip = t;
                        }
                    });
                    if alive.is_none() {
                        break;
                    }
                }
                // The app is gone: take the item off the bus.
                handle.shutdown().wait();
            })
            .map_err(|e| Unavailable::Failed(e.to_string()))?;
        Ok((Self { updates, hide }, hide))
    }

    pub(super) fn hide_support(&self) -> HideSupport {
        self.hide
    }

    pub(super) fn set_menu(&mut self, menu: &TrayMenu) {
        let _ = self.updates.send(Update::Menu(menu.clone()));
    }

    pub(super) fn set_tooltip(&mut self, tooltip: &str) {
        let _ = self.updates.send(Update::Tooltip(tooltip.to_owned()));
    }
}
