//! The whole app (`SuperbaApp`: sidebar, page, dialogs, wizard) on a fake
//! service, frame after frame with AccessKit on (user bug 56): every frame's
//! accessibility tree, the name a screen reader reads for each node (its label,
//! else the nodes it is labelled by, as `accesskit_consumer` and the platform
//! adapters resolve it), and clicks sent as AccessKit actions, the way UI
//! Automation's Invoke and the Windows harness reach a control.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use egui::accesskit::{Action, ActionData, ActionRequest, Node, NodeId, Role, TreeId, TreeUpdate};
use egui::{Event, Rect, pos2};
use superba_core::Locale;
use superba_gui::pages::Page;
use superba_gui::{AppOptions, SuperbaApp};

use crate::support::fake::FakeService;

/// One frame's accessibility tree.
#[derive(Debug)]
pub struct Tree {
    /// The root node.
    pub root: NodeId,
    /// Every node of the frame.
    pub nodes: HashMap<NodeId, Node>,
    /// The focused node.
    pub focus: NodeId,
}

/// Roles a user operates: every node with one of them needs a name.
pub const INTERACTIVE: &[Role] = &[
    Role::TextInput,
    Role::MultilineTextInput,
    Role::PasswordInput,
    Role::SearchInput,
    Role::NumberInput,
    Role::EmailInput,
    Role::UrlInput,
    Role::Button,
    Role::DefaultButton,
    Role::Link,
    Role::Tab,
    Role::CheckBox,
    Role::Switch,
    Role::RadioButton,
    Role::ComboBox,
    Role::EditableComboBox,
    Role::ListBoxOption,
    Role::ListItem,
    Role::Row,
    Role::Cell,
    Role::ColumnHeader,
    Role::MenuItem,
    Role::MenuItemCheckBox,
    Role::MenuItemRadio,
    Role::Slider,
    Role::SpinButton,
    Role::TreeItem,
];

impl Tree {
    /// The tree of `update` (egui sends the whole tree every frame).
    pub fn from_update(update: &TreeUpdate) -> Self {
        Self {
            root: update.tree.as_ref().expect("a full tree").root,
            nodes: update.nodes.iter().cloned().collect(),
            focus: update.focus,
        }
    }

    /// The node `id`.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[&id]
    }

    /// The name a screen reader reads: the node's label, else the names of
    /// the nodes it is labelled by (a text label names by its value).
    pub fn name(&self, id: NodeId) -> Option<String> {
        let node = self.nodes.get(&id)?;
        if let Some(label) = node.label().filter(|l| !l.trim().is_empty()) {
            return Some(label.to_owned());
        }
        if node.role() == Role::Label
            && let Some(v) = node.value().filter(|v| !v.trim().is_empty())
        {
            return Some(v.to_owned());
        }
        let parts: Vec<String> = node
            .labelled_by()
            .iter()
            .filter_map(|l| {
                let n = self.nodes.get(l)?;
                if n.role() == Role::Label {
                    n.value().map(str::to_owned)
                } else {
                    n.label().map(str::to_owned)
                }
            })
            .filter(|s| !s.trim().is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// Node ids reachable from the root, depth first in child order, with
    /// their depth.
    pub fn walk(&self) -> Vec<(NodeId, usize)> {
        let mut out = Vec::new();
        let mut stack = vec![(self.root, 0)];
        while let Some((id, depth)) = stack.pop() {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            out.push((id, depth));
            for child in node.children().iter().rev() {
                stack.push((*child, depth + 1));
            }
        }
        out
    }

    /// The node is something a user operates or moves the keyboard focus to:
    /// an interactive role, or any node that takes clicks or the focus (scroll
    /// bars aside).
    pub fn is_interactive(&self, id: NodeId) -> bool {
        let node = self.node(id);
        let role = node.role();
        if matches!(role, Role::ScrollBar | Role::Window | Role::Label) {
            return false;
        }
        INTERACTIVE.contains(&role)
            || node.supports_action(Action::Click)
            || node.supports_action(Action::Focus)
    }

    /// Interactive nodes without a name, described for a failure message.
    pub fn unnamed(&self) -> Vec<String> {
        self.walk()
            .into_iter()
            .filter(|(id, _)| self.is_interactive(*id) && self.name(*id).is_none())
            .map(|(id, _)| self.describe(id))
            .collect()
    }

    /// One line about `id`: role, name, value, placeholder, description, bounds.
    pub fn describe(&self, id: NodeId) -> String {
        let n = self.node(id);
        let mut s = format!("{:?} {:?}", n.role(), self.name(id).unwrap_or_default());
        if let Some(v) = n.value() {
            s.push_str(&format!(" value={v:?}"));
        }
        if let Some(p) = n.placeholder() {
            s.push_str(&format!(" placeholder={p:?}"));
        }
        if let Some(d) = n.description() {
            s.push_str(&format!(" description={d:?}"));
        }
        if let Some(b) = n.bounds() {
            s.push_str(&format!(
                " [{:.0},{:.0} {:.0}x{:.0}]",
                b.x0,
                b.y0,
                b.x1 - b.x0,
                b.y1 - b.y0
            ));
        }
        if n.supports_action(Action::Click) {
            s.push_str(" click");
        } else if n.supports_action(Action::Focus) {
            s.push_str(" focus");
        }
        if n.is_disabled() {
            s.push_str(" disabled");
        }
        s
    }

    /// The whole tree, indented (for failure messages and debugging).
    pub fn dump(&self) -> String {
        self.walk()
            .into_iter()
            .map(|(id, depth)| format!("{}{}", "  ".repeat(depth), self.describe(id)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Nodes named `name` (exactly), with `role` when given, in tree order.
    pub fn find_all(&self, role: Option<Role>, name: &str) -> Vec<NodeId> {
        self.walk()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| role.is_none_or(|r| self.node(*id).role() == r))
            .filter(|id| self.name(*id).as_deref() == Some(name))
            .collect()
    }

    /// The first node named `name` (with `role` when given).
    pub fn find(&self, role: Option<Role>, name: &str) -> Option<NodeId> {
        self.find_all(role, name).into_iter().next()
    }

    /// Some node is named `name`.
    pub fn has(&self, name: &str) -> bool {
        self.find(None, name).is_some()
    }

    /// Some node's name contains `needle`.
    pub fn has_containing(&self, needle: &str) -> bool {
        self.walk()
            .into_iter()
            .any(|(id, _)| self.name(id).is_some_and(|n| n.contains(needle)))
    }

    /// Nodes with `role`, in tree order.
    pub fn with_role(&self, role: Role) -> Vec<NodeId> {
        self.walk()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| self.node(*id).role() == role)
            .collect()
    }
}

static ISOLATE: Once = Once::new();

/// Superba's data directory of this test binary (after the first harness).
pub fn data_dir() -> std::path::PathBuf {
    isolate();
    std::env::var_os("SUPERBA_DATA_DIR")
        .map(std::path::PathBuf::from)
        .expect("isolated")
}

/// Point Superba's data dir and HOME at a throwaway directory for this test
/// binary (the app reads `superba_switch::detect_context()` for paths it shows),
/// before any app or thread exists.
fn isolate() {
    ISOLATE.call_once(|| {
        let dir = tempfile::Builder::new()
            .prefix("superba-a11y-")
            .tempdir()
            .unwrap()
            .keep();
        // SAFETY: runs once, first thing in every test of this binary, before
        // any app (and its backend thread) exists.
        unsafe {
            std::env::set_var("SUPERBA_DATA_DIR", dir.join("data"));
            std::env::set_var("HOME", &dir);
            std::env::set_var("XDG_DATA_HOME", dir.join(".local/share"));
            std::env::set_var("XDG_CONFIG_HOME", dir.join(".config"));
        }
    });
}

/// The app on a fake service.
pub struct AppHarness {
    ctx: egui::Context,
    app: SuperbaApp,
    frame: eframe::Frame,
    screen: Rect,
    pending: VecDeque<Vec<Event>>,
    /// The service the app talks to.
    pub service: Arc<FakeService>,
    /// The window is on screen (eframe runs only `logic` while it is not).
    visible: bool,
    /// The next frame carries a close request (the title bar's close button).
    close_next: bool,
    /// Viewport commands the app sent (CancelClose, Visible, Close, ...).
    commands: Vec<egui::ViewportCommand>,
    copied: Vec<String>,
}

impl AppHarness {
    /// The app in `locale` and `theme`, opened on `page`, in a 1280x800 window.
    pub fn new(service: Arc<FakeService>, locale: Locale, page: Page) -> Self {
        Self::with_options(
            service,
            AppOptions {
                theme: Some(egui::Theme::Light),
                locale: Some(locale),
                page: Some(page),
                ..AppOptions::default()
            },
        )
    }

    /// The app started with `options`, in a 1280x800 window.
    pub fn with_options(service: Arc<FakeService>, options: AppOptions) -> Self {
        isolate();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = SuperbaApp::new(&cc, service.clone(), options).expect("the app starts");
        Self {
            ctx,
            app,
            frame: eframe::Frame::_new_kittest(),
            screen: Rect::from_min_max(pos2(0.0, 0.0), pos2(1280.0, 800.0)),
            pending: VecDeque::new(),
            service,
            visible: true,
            close_next: false,
            commands: Vec::new(),
            copied: Vec::new(),
        }
    }

    /// The app (inspection).
    pub fn app(&self) -> &SuperbaApp {
        &self.app
    }

    /// The window is on screen.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Viewport commands since the last call.
    pub fn take_commands(&mut self) -> Vec<egui::ViewportCommand> {
        std::mem::take(&mut self.commands)
    }

    /// Clipboard writes issued by the application since the last call.
    pub fn take_copied_text(&mut self) -> Vec<String> {
        std::mem::take(&mut self.copied)
    }

    /// Press the window's close button (the next frame carries the request,
    /// as eframe passes it) and settle.
    pub fn close_window(&mut self) -> Tree {
        self.close_next = true;
        self.frame();
        self.settle()
    }

    /// Run one frame (with the next queued input); returns its tree.
    pub fn frame(&mut self) -> Tree {
        let events = self.pending.pop_front().unwrap_or_default();
        // A hidden window reads as occluded (what eframe sees on X11 and
        // macOS): only `logic` runs then.
        let mut info = egui::ViewportInfo {
            minimized: Some(false),
            occluded: Some(!self.visible),
            ..Default::default()
        };
        if std::mem::take(&mut self.close_next) {
            info.events.push(egui::ViewportEvent::Close);
        }
        let input = egui::RawInput {
            screen_rect: Some(self.screen),
            events,
            viewports: std::iter::once((egui::ViewportId::ROOT, info)).collect(),
            ..Default::default()
        };
        let (app, frame, visible) = (&mut self.app, &mut self.frame, self.visible);
        // Like eframe: `logic` every pass, `ui` only while the window shows.
        let output = self.ctx.run_ui(input, |ui| {
            eframe::App::logic(app, ui.ctx(), frame);
            if visible {
                eframe::App::ui(app, ui, frame);
            }
        });
        if let Some(out) = output.viewport_output.get(&egui::ViewportId::ROOT) {
            for command in &out.commands {
                if let egui::ViewportCommand::Visible(v) = command {
                    self.visible = *v;
                }
                self.commands.push(command.clone());
            }
        }
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                self.copied.push(text.clone());
            }
        }
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

    /// Frames until the queued input is used up, then three more (background
    /// reads answer, layouts settle); returns the last tree.
    pub fn settle(&mut self) -> Tree {
        while !self.pending.is_empty() {
            self.frame();
            std::thread::sleep(Duration::from_millis(2));
        }
        let mut last = self.frame();
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(5));
            last = self.frame();
        }
        last
    }

    /// Frames until `done` holds (at most 20 s); returns that tree.
    pub fn until(&mut self, what: &str, done: impl Fn(&Tree) -> bool) -> Tree {
        let start = Instant::now();
        loop {
            let tree = self.frame();
            if self.pending.is_empty() && done(&tree) {
                return tree;
            }
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "{what}: not reached in 20 s; last tree:\n{}",
                tree.dump()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Frames until a node named `name` (with `role` when given) is in the tree.
    pub fn wait_for(&mut self, role: Option<Role>, name: &str) -> Tree {
        self.until(&format!("{role:?} {name:?}"), |t| {
            t.find(role, name).is_some()
        })
    }

    /// Send `action` to `id` as an assistive technology does.
    pub fn act(&mut self, id: NodeId, action: Action) {
        self.pending
            .push_back(vec![Event::AccessKitActionRequest(ActionRequest {
                action,
                target_tree: TreeId::ROOT,
                target_node: id,
                data: None,
            })]);
    }

    /// Wait for the node named `name` (with `role` when given), click it
    /// through AccessKit and settle; returns the tree after the click.
    pub fn click(&mut self, role: Option<Role>, name: &str) -> Tree {
        let tree = self.wait_for(role, name);
        let id = tree.find(role, name).expect("found");
        self.act(id, Action::Click);
        self.settle()
    }

    /// Set the text of the field named `name` through AccessKit (UI
    /// Automation's ValuePattern.SetValue) and settle.
    pub fn set_value(&mut self, name: &str, text: &str) -> Tree {
        let tree = self.wait_for(None, name);
        let id = tree
            .find_all(None, name)
            .into_iter()
            .find(|id| tree.node(*id).supports_action(Action::SetValue))
            .unwrap_or_else(|| panic!("no field {name:?} takes a value:\n{}", tree.dump()));
        self.pending
            .push_back(vec![Event::AccessKitActionRequest(ActionRequest {
                action: Action::SetValue,
                target_tree: TreeId::ROOT,
                target_node: id,
                data: Some(ActionData::Value(text.into())),
            })]);
        self.settle()
    }

    /// Click the node `id` through AccessKit and settle.
    pub fn click_id(&mut self, id: NodeId) -> Tree {
        self.act(id, Action::Click);
        self.settle()
    }

    /// Raw input, one frame per entry (pointer presses, moves, releases), then
    /// settle.
    pub fn input(&mut self, frames: Vec<Vec<Event>>) -> Tree {
        self.pending.extend(frames);
        self.settle()
    }

    /// Type `text` into the focused widget.
    pub fn type_text(&mut self, text: &str) -> Tree {
        self.pending.push_back(vec![Event::Text(text.to_owned())]);
        self.settle()
    }

    /// Resize the content rectangle and settle the layout.
    pub fn resize(&mut self, size: [f32; 2]) -> Tree {
        self.screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]));
        self.settle()
    }

    /// Press a platform command shortcut.
    pub fn shortcut(&mut self, key: egui::Key) -> Tree {
        self.input(vec![vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }]])
    }

    /// Press `key`.
    pub fn key(&mut self, key: egui::Key) -> Tree {
        self.pending.push_back(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }]);
        self.settle()
    }

    /// Scroll the page content with the pointer over its center.
    pub fn scroll_page(&mut self, delta: f32) -> Tree {
        self.pending.push_back(vec![
            Event::PointerMoved(egui::pos2(900.0, 400.0)),
            Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, delta),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Default::default(),
            },
        ]);
        self.settle()
    }

    /// Go to `page` through its sidebar tab, the way a screen reader user does.
    pub fn go(&mut self, page: Page, tr: &superba_gui::i18n::Tr) -> Tree {
        if let Page::Superba(section) = page {
            use superba_gui::pages::superba::SuperbaTab;
            let tab = section.tab();
            if self.frame().find(Some(Role::TabList), "Superba").is_none() {
                self.click(Some(Role::Tab), "Superba");
            }
            // Overview and Settings also name main-sidebar pages; select within
            // the Superba tab list by taking its later matching tab.
            let name = tr.t(tab.title_key());
            let tree = self.frame();
            let id = tree
                .find_all(Some(Role::Tab), &name)
                .into_iter()
                .last()
                .unwrap();
            self.act(id, Action::Click);
            self.settle();
            if tab == SuperbaTab::Settings {
                self.click(Some(Role::Button), &tr.t(section.title_key()));
                return self.until("settings anchor", |tree| {
                    tree.find(Some(Role::Button), &tr.t(section.title_key()))
                        .is_some_and(|id| tree.node(id).is_selected() == Some(true))
                });
            }
            return self.settle();
        }
        let name = tr.t(page.title_key());
        self.click(Some(Role::Tab), &name)
    }
}
