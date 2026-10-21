//! The app's side of the tray (item 70): start the icon, keep its menu in
//! step with the data, turn its events into the app's actions, and carry out
//! what [`crate::tray::WindowLife`] decides about the window.

use superba_core::RestoreTarget;

use super::{Action, SuperbaApp, bring_to_front};
use crate::launch::LaunchOrigin;
use crate::pages::Page;
use crate::tray::close::{CloseContext, Unset};
use crate::tray::{
    HideSupport, Tray, TrayAction, TrayEvent, TrayOption, WindowCommand, menu, prompt,
};

impl SuperbaApp {
    /// Show the tray icon (once, from [`SuperbaApp::new`]).
    pub(super) fn start_tray(&mut self, ctx: &egui::Context) {
        let busy = self.tray_busy();
        let menu = menu::window_menu(&self.tr, &self.data, busy, self.life.is_hidden());
        let tooltip = menu::tooltip(&self.tr, &self.data);
        self.tray = match &self.opts.tray {
            TrayOption::Off => Tray::disabled(),
            TrayOption::System => Tray::start(ctx, menu, tooltip),
            TrayOption::Simulated(sink) => Tray::simulated(ctx, sink.clone(), menu, tooltip),
        };
        self.data.tray = self.tray.status().clone();
    }

    /// The tray's menu as shown now (tests, screenshots).
    pub fn tray_menu(&self) -> Option<&crate::tray::TrayMenu> {
        self.tray.menu()
    }

    /// The tray icon's tooltip (tests).
    pub fn tray_tooltip(&self) -> &str {
        self.tray.tooltip()
    }

    /// The window hides in the tray.
    pub fn is_hidden_in_tray(&self) -> bool {
        self.life.is_hidden()
    }

    /// What a close request means now.
    pub(super) fn close_context(&self) -> CloseContext {
        CloseContext {
            setting: self.settings.close_action,
            hide: self.tray.status().hide_support(),
            unset: Unset::platform(),
        }
    }

    /// A switch is planned or running, or ChatGPT is being opened.
    fn tray_busy(&self) -> bool {
        self.flow.is_running() || self.flow.is_planning()
    }

    /// Show the window (the tray, another start, a question or an error that
    /// needs the user).
    pub(super) fn show_window(&mut self, ctx: &egui::Context) {
        if self.life.is_hidden() {
            self.life.show();
        } else {
            bring_to_front(ctx);
        }
    }

    /// Every pass (also while hidden): tray events, the window's close
    /// request, "start in the tray".
    pub(super) fn tray_tick(&mut self, ctx: &egui::Context) {
        for event in self.tray.take_events() {
            self.on_tray_event(ctx, event);
        }
        self.data.tray = self.tray.status().clone();
        if !self.start_hidden_checked {
            self.start_hidden_checked = true;
            // A restart into the tray (Wayland) opened through XWayland; a
            // native Wayland window never hides at start.
            let hide = self
                .tray
                .status()
                .hide_support()
                .filter(|h| *h == HideSupport::InPlace);
            if self.opts.start_in_tray {
                if hide.is_some() {
                    self.life.start_hidden(hide);
                } else {
                    // The native window started invisible. Without a usable
                    // tray, show it so the application remains reachable.
                    self.life.show();
                }
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            let cx = self.close_context();
            self.life.close_requested(&cx);
        }
        // A switch that waits for an answer brings a hidden window back.
        if self.life.is_hidden() && self.flow_needs_window() {
            self.life.show();
        }
    }

    /// The flow shows something the user has to answer.
    fn flow_needs_window(&self) -> bool {
        use crate::flow::Stage;
        match self.flow.stage() {
            Stage::Blocked { .. }
            | Stage::Choose { .. }
            | Stage::ConfirmFull { .. }
            | Stage::WaitingBeforeSwitch { .. } => true,
            Stage::Running(run) => run.decision.is_some(),
            Stage::Idle | Stage::Planning | Stage::Armed { .. } => false,
        }
    }

    fn on_tray_event(&mut self, ctx: &egui::Context, event: TrayEvent) {
        match event {
            TrayEvent::Activate | TrayEvent::Action(TrayAction::Open) => self.show_window(ctx),
            TrayEvent::Action(TrayAction::SwitchChatGptAccount(id)) => {
                self.handle_action(ctx, Action::SwitchChatGptAccount(id));
            }
            TrayEvent::Action(TrayAction::LightMode(enabled)) => {
                if enabled {
                    self.life.start_hidden(self.tray.status().hide_support());
                } else {
                    self.show_window(ctx);
                }
            }
            TrayEvent::HostLost => self.life.tray_lost(),
            TrayEvent::HostBack => {}
            TrayEvent::Action(TrayAction::Superba(section)) => {
                self.show_window(ctx);
                self.handle_action(ctx, Action::Go(Page::Superba(section)));
            }
            TrayEvent::Action(TrayAction::Quit) => self.life.quit(),
            TrayEvent::Action(TrayAction::Switch { profile, line }) => {
                self.handle_action(
                    ctx,
                    Action::Switch {
                        profile,
                        line,
                        auth: None,
                    },
                );
            }
            TrayEvent::Action(TrayAction::RestoreOfficial) => {
                self.handle_action(ctx, Action::Restore(RestoreTarget::Official));
            }
            TrayEvent::Action(TrayAction::LaunchApp) => self.launch_app(LaunchOrigin::Tray),
            TrayEvent::Action(TrayAction::RouterOff) => {
                let request = crate::pages::router::backend::router_request(false);
                self.handle_action(ctx, Action::Plan(Box::new(request)));
            }
            TrayEvent::Action(TrayAction::RouterPage) => {
                self.show_window(ctx);
                self.go(Page::Router);
            }
        }
    }

    /// The first-close question, while it is open (drawn with the other
    /// overlays).
    pub(super) fn tray_prompt(&mut self, ctx: &egui::Context) {
        if !self.life.is_asking() {
            return;
        }
        if let Some(answer) = prompt::show(ctx, &self.tr) {
            let hide = self.tray.status().hide_support();
            self.life.answer(answer.choice(), hide);
        }
    }

    /// End of every pass: carry out the window commands and refresh the
    /// tray's menu and tooltip.
    pub(super) fn tray_finish(&mut self, ctx: &egui::Context) {
        for command in self.life.take_commands() {
            match command {
                WindowCommand::CancelClose => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
                WindowCommand::Hide => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    crate::tray::set_in_dock(false);
                }
                WindowCommand::Show => {
                    crate::tray::set_in_dock(true);
                    bring_to_front(ctx);
                    ctx.request_repaint();
                }
                WindowCommand::Close => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    ctx.request_repaint();
                }
                WindowCommand::Remember(choice) => {
                    self.settings.close_action = Some(choice);
                    if let Some(set) = self.data.profiles.value.as_mut() {
                        set.settings.close_action = Some(choice);
                    }
                    self.backend
                        .send(super::Command::SaveSettings(self.settings.clone()));
                }
                WindowCommand::RelaunchHidden => crate::tray::request_relaunch_hidden(),
            }
        }
        if self.tray.menu().is_some() {
            let busy = self.tray_busy();
            let menu = menu::window_menu(&self.tr, &self.data, busy, self.life.is_hidden());
            let tooltip = menu::tooltip(&self.tr, &self.data);
            self.tray.update(menu, tooltip);
        }
    }
}
