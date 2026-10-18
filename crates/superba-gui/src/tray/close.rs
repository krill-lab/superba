//! What closing the main window does (item 70): a pure state machine.
//!
//! The app feeds it the window's close requests (the title bar's close
//! button, Alt+F4, Cmd+W), explicit quits (the tray's and the palette's
//! "Quit Superba"), show requests (the tray, a second start of `superba`, a
//! switch that needs an answer) and the first-close answer; it returns
//! [`WindowCommand`]s for the app to carry out.
//!
//! | `close_action` | tray shown | close request does |
//! |---|---|---|
//! | any | no | quit |
//! | `Quit` | yes | quit |
//! | `Tray` | yes | hide into the tray |
//! | unset | yes | macOS: hide into the menu bar; Windows, Linux: ask once |
//!
//! Hiding on a native Wayland window is a restart through XWayland
//! ([`HideSupport::Relaunch`]): the window closes and Superba starts again
//! hidden.

use superba_core::CloseAction;

/// How a window of this process hides into the tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HideSupport {
    /// The window hides and shows again in place (Windows, macOS, X11).
    InPlace,
    /// A native Wayland window cannot hide: close it and start Superba again
    /// hidden through XWayland.
    Relaunch,
}

/// What an unset [`CloseAction`] means on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unset {
    /// Ask on the first close (Windows, Linux).
    Ask,
    /// Keep running in the menu bar, like other Mac apps (macOS).
    Tray,
}

impl Unset {
    /// The behaviour of this build's platform.
    pub fn platform() -> Self {
        if cfg!(target_os = "macos") {
            Unset::Tray
        } else {
            Unset::Ask
        }
    }
}

/// What the app knows when a close request arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseContext {
    /// The saved choice.
    pub setting: Option<CloseAction>,
    /// How the window hides; `None` without a tray.
    pub hide: Option<HideSupport>,
    /// What an unset choice means.
    pub unset: Unset,
}

impl CloseContext {
    /// What a close request does now.
    pub fn outcome(&self) -> CloseOutcome {
        let Some(hide) = self.hide else {
            return CloseOutcome::Quit;
        };
        match (self.setting, self.unset) {
            (Some(CloseAction::Quit), _) => CloseOutcome::Quit,
            (Some(CloseAction::Tray), _) | (None, Unset::Tray) => CloseOutcome::Hide(hide),
            (None, Unset::Ask) => CloseOutcome::Ask,
        }
    }

    /// The choice in effect, as Preferences shows it (`None`: asked on the
    /// first close).
    pub fn effective(&self) -> Option<CloseAction> {
        match (self.hide, self.setting, self.unset) {
            (None, ..) => Some(CloseAction::Quit),
            (Some(_), Some(a), _) => Some(a),
            (Some(_), None, Unset::Tray) => Some(CloseAction::Tray),
            (Some(_), None, Unset::Ask) => None,
        }
    }
}

/// What a close request does ([`CloseContext::outcome`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Let the window close: Superba quits.
    Quit,
    /// Hide into the tray.
    Hide(HideSupport),
    /// Ask first.
    Ask,
}

/// What the app does with the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    /// Cancel the close request the window got in this frame.
    CancelClose,
    /// Hide the window (macOS: also leave the Dock).
    Hide,
    /// Show the window and bring it to the front (macOS: back in the Dock).
    Show,
    /// Close the window: Superba quits.
    Close,
    /// Save the choice as `close_action`.
    Remember(CloseAction),
    /// Start Superba again hidden in the tray once this window is gone
    /// ([`HideSupport::Relaunch`]).
    RelaunchHidden,
}

/// Whether the window is on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    /// On screen.
    #[default]
    Shown,
    /// Hidden in the tray.
    Hidden,
}

/// The window's life: shown, hidden in the tray, asking, quitting.
#[derive(Debug, Default)]
pub struct WindowLife {
    visibility: Visibility,
    asking: bool,
    quitting: bool,
    commands: Vec<WindowCommand>,
}

impl WindowLife {
    /// A shown window.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hidden in the tray.
    pub fn is_hidden(&self) -> bool {
        self.visibility == Visibility::Hidden
    }

    /// The first-close question is open.
    pub fn is_asking(&self) -> bool {
        self.asking
    }

    /// Superba is quitting: the next close request goes through.
    pub fn is_quitting(&self) -> bool {
        self.quitting
    }

    /// Commands for the app (drained).
    pub fn take_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.commands)
    }

    /// The window got a close request (title bar, Alt+F4, Cmd+W).
    pub fn close_requested(&mut self, cx: &CloseContext) {
        if self.quitting {
            return;
        }
        if self.asking {
            // The question is on screen: a second close keeps it there.
            self.commands.push(WindowCommand::CancelClose);
            return;
        }
        match cx.outcome() {
            CloseOutcome::Quit => self.quitting = true,
            CloseOutcome::Hide(hide) => {
                self.commands.push(WindowCommand::CancelClose);
                self.hide_with(hide);
            }
            CloseOutcome::Ask => {
                self.commands.push(WindowCommand::CancelClose);
                self.visibility = Visibility::Shown;
                self.asking = true;
            }
        }
    }

    /// Quit Superba (the tray's or the palette's "Quit Superba"): close the
    /// window for real, also while it is hidden.
    pub fn quit(&mut self) {
        self.asking = false;
        if !self.quitting {
            self.quitting = true;
            self.commands.push(WindowCommand::Close);
        }
    }

    /// Show the window and bring it forward (the tray's "Open Superba", a
    /// click on the icon, a second start, a switch that needs an answer).
    pub fn show(&mut self) {
        if self.quitting {
            return;
        }
        self.visibility = Visibility::Shown;
        self.commands.push(WindowCommand::Show);
    }

    /// Hide into the tray without a close request ("start in the tray").
    /// Nothing without a tray.
    pub fn start_hidden(&mut self, hide: Option<HideSupport>) {
        if let Some(hide) = hide
            && !self.quitting
        {
            self.hide_with(hide);
        }
    }

    /// The first-close answer: `Some` remembers the choice and acts on it,
    /// `None` (Escape, "Cancel") leaves the window open.
    pub fn answer(&mut self, choice: Option<CloseAction>, hide: Option<HideSupport>) {
        if !self.asking {
            return;
        }
        self.asking = false;
        let Some(choice) = choice else {
            return;
        };
        self.commands.push(WindowCommand::Remember(choice));
        match (choice, hide) {
            (CloseAction::Tray, Some(hide)) => self.hide_with(hide),
            // Quit, or the tray went away while asking.
            _ => self.quit(),
        }
    }

    /// The tray went away (Linux: the host stopped): a hidden window would be
    /// out of reach, so it comes back.
    pub fn tray_lost(&mut self) {
        if self.is_hidden() {
            self.show();
        }
    }

    fn hide_with(&mut self, hide: HideSupport) {
        match hide {
            HideSupport::InPlace => {
                self.visibility = Visibility::Hidden;
                self.commands.push(WindowCommand::Hide);
            }
            HideSupport::Relaunch => {
                // The window closes for real; the next start is hidden.
                self.commands.retain(|c| *c != WindowCommand::CancelClose);
                self.commands.push(WindowCommand::RelaunchHidden);
                self.quitting = true;
                self.commands.push(WindowCommand::Close);
            }
        }
    }
}
