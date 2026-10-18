//! Windows and macOS: the icon through tray-icon, the menu through muda.
//!
//! Both live on the UI thread (tray-icon's icon is not `Send`; on Windows its
//! hidden message window is pumped by winit's message loop, on macOS the
//! status item belongs to the running `NSApplication`). Menu and icon events
//! arrive through tray-icon's global handlers, on that same thread, and go to
//! the [`Sink`].
//!
//! Windows: a left click (or double click) on the icon opens the window, a
//! right click the menu. macOS: a click opens the menu, like every menu-bar
//! item.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use super::{HideSupport, Sink, TrayAction, TrayEvent, TrayItem, TrayMenu, Unavailable};

/// Menu item ids of the menu shown now, to their actions. Read by the menu
/// event handler (any thread tray-icon calls it on).
type Ids = Arc<Mutex<HashMap<String, TrayAction>>>;

/// The icon and the id table of its menu.
pub(super) struct Backend {
    icon: TrayIcon,
    ids: Ids,
}

impl Backend {
    pub(super) fn start(
        sink: Sink,
        menu: &TrayMenu,
        tooltip: &str,
    ) -> Result<(Self, HideSupport), Unavailable> {
        let ids: Ids = Arc::default();
        let (muda_menu, table) = build(menu);
        *super::lock(&ids) = table;
        {
            let ids = Arc::clone(&ids);
            let sink = sink.clone();
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = super::lock(&ids).get(event.id().as_ref()).cloned();
                if let Some(action) = action {
                    sink.push(TrayEvent::Action(action));
                }
            }));
        }
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if opens_window(&event) {
                sink.push(TrayEvent::Activate);
            }
        }));
        let builder = TrayIconBuilder::new()
            .with_id("superba")
            .with_menu(Box::new(muda_menu))
            .with_tooltip(tooltip);
        // macOS: a template image, tinted for light and dark menu bars; the
        // menu bar remembers where the user dragged the item.
        #[cfg(target_os = "macos")]
        let builder = builder
            .with_icon_templated(platform_icon()?)
            .with_autosave_name("superba");
        // Windows: the left button opens the window, the right one the menu.
        #[cfg(windows)]
        let builder = builder
            .with_icon(platform_icon()?)
            .with_menu_on_left_click(false);
        let icon = builder
            .build()
            .map_err(|e| Unavailable::Failed(e.to_string()))?;
        Ok((Self { icon, ids }, HideSupport::InPlace))
    }

    pub(super) fn hide_support(&self) -> HideSupport {
        HideSupport::InPlace
    }

    pub(super) fn set_menu(&mut self, menu: &TrayMenu) {
        let (muda_menu, table) = build(menu);
        *super::lock(&self.ids) = table;
        self.icon.set_menu(Some(Box::new(muda_menu)));
    }

    pub(super) fn set_tooltip(&mut self, tooltip: &str) {
        if let Err(e) = self.icon.set_tooltip(Some(tooltip)) {
            tracing::debug!(error = %e, "tray tooltip not set");
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // The handlers hold the sink (and its egui context): let them go with
        // the icon.
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    }
}

/// A click on the icon that opens the window (Windows: left button up or a
/// double click; macOS opens the menu instead).
fn opens_window(event: &TrayIconEvent) -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        }
    )
}

/// The icon for this platform.
fn platform_icon() -> Result<tray_icon::Icon, Unavailable> {
    #[cfg(windows)]
    {
        let px = small_icon_px();
        if let Ok(icon) =
            tray_icon::Icon::from_resource(super::icon::WINDOWS_RESOURCE_ID, Some((px, px)))
        {
            return Ok(icon);
        }
        let rgba = super::icon::colour(px);
        tray_icon::Icon::from_rgba(rgba.rgba, rgba.size, rgba.size)
            .map_err(|e| Unavailable::Failed(e.to_string()))
    }
    #[cfg(target_os = "macos")]
    {
        let rgba = super::icon::template();
        tray_icon::Icon::from_rgba(rgba.rgba, rgba.size, rgba.size)
            .map_err(|e| Unavailable::Failed(e.to_string()))
    }
}

/// The notification area's icon size in pixels at the current scale.
#[cfg(windows)]
fn small_icon_px() -> u32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    // SAFETY: GetSystemMetrics takes a constant index and has no
    // memory-safety preconditions.
    let px = unsafe { GetSystemMetrics(SM_CXSMICON) };
    u32::try_from(px).ok().filter(|&p| p > 0).unwrap_or(16)
}

/// Windows menus underline the letter after `&`: show `&` itself.
fn label(text: &str) -> String {
    if cfg!(windows) {
        text.replace('&', "&&")
    } else {
        text.to_owned()
    }
}

/// The muda menu of `menu` and its id table.
fn build(menu: &TrayMenu) -> (Menu, HashMap<String, TrayAction>) {
    let mut table = HashMap::new();
    let mut next = 0usize;
    let root = Menu::new();
    for item in &menu.items {
        let entry = entry(item, &mut table, &mut next);
        if let Err(e) = root.append(entry.as_ref()) {
            tracing::warn!(error = %e, "tray menu item not added");
        }
    }
    (root, table)
}

fn entry(
    item: &TrayItem,
    table: &mut HashMap<String, TrayAction>,
    next: &mut usize,
) -> Box<dyn IsMenuItem> {
    match item {
        TrayItem::Action {
            label: text,
            action,
            enabled,
            checked,
        } => {
            *next += 1;
            let id = format!("superba-{next}");
            table.insert(id.clone(), action.clone());
            match checked {
                Some(checked) => Box::new(CheckMenuItem::with_id(
                    id,
                    label(text),
                    *enabled,
                    *checked,
                    None,
                )),
                None => Box::new(MenuItem::with_id(id, label(text), *enabled, None)),
            }
        }
        TrayItem::Label(text) => Box::new(MenuItem::new(label(text), false, None)),
        TrayItem::Submenu {
            label: text,
            enabled,
            items,
        } => {
            let sub = Submenu::new(label(text), *enabled);
            for item in items {
                let entry = entry(item, table, next);
                if let Err(e) = sub.append(entry.as_ref()) {
                    tracing::warn!(error = %e, "tray submenu item not added");
                }
            }
            Box::new(sub)
        }
        TrayItem::Separator => Box::new(PredefinedMenuItem::separator()),
    }
}
