//! Opening ChatGPT through Superba (GH#7, user bug 66): hot switching needs a
//! ChatGPT that Superba opened (with its debugging port and the line's auth
//! address), so that is one click away wherever the user looks for it — the
//! Overview's route hero, the command palette, the "ChatGPT (Superba)"
//! Start-menu shortcut (`superba --launch-chatgpt`), the Linux desktop action
//! and, when chosen in Preferences, every start of Superba.
//!
//! - ChatGPT not running: open it through Superba.
//! - Running without what hot switching needs (opened elsewhere, or without the
//!   line's auth address): reopen it through Superba — a full restart, planned
//!   and confirmed like any other (running turns are waited for).
//! - Already open through Superba: bring it to the front.
//!
//! `superba --launch-chatgpt` hands the request to a running Superba window
//! ([`crate::instance`]); without one it opens ChatGPT itself and shows no
//! window, unless the user has to confirm a reopen or something failed.

use superba_core::{Error, FullRestartReason, HotSwitchStatus, Overview, Result, SwitchService};

/// What the Overview offers for the desktop app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppLaunch {
    /// ChatGPT is installed and not running: open it through Superba.
    Open,
    /// ChatGPT runs without what hot switching needs, and reopening it through
    /// Superba fixes that (a full restart).
    Reopen(FullRestartReason),
}

impl AppLaunch {
    /// i18n key of the action's label.
    pub fn label_key(self) -> &'static str {
        match self {
            AppLaunch::Open => "action.open_app",
            AppLaunch::Reopen(_) => "action.reopen_app",
        }
    }

    /// i18n key of the action's hover text.
    pub fn tip_key(self) -> &'static str {
        match self {
            AppLaunch::Open => "action.open_app_tip",
            AppLaunch::Reopen(_) => "action.reopen_app_tip",
        }
    }
}

/// The action the Overview shows for the desktop app, if any: nothing while
/// it can be hot-switched, without a desktop app, in safe mode, or when a
/// reopen would not change anything (an app without the restart bridge, a
/// platform without hot switching).
pub fn app_launch(overview: &Overview) -> Option<AppLaunch> {
    match overview.app.hot_switch {
        HotSwitchStatus::NotRunning => Some(AppLaunch::Open),
        _ => crate::pages::overview::todo::reopen_fixes(overview).map(AppLaunch::Reopen),
    }
}

/// Who asked to open ChatGPT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchOrigin {
    /// A button or the palette in the window.
    Window,
    /// Another start: the Start-menu shortcut, the desktop action
    /// (`superba --launch-chatgpt`).
    Shortcut,
    /// The preference "Open ChatGPT through Superba when Superba starts": only
    /// opens a ChatGPT that is not running, never restarts one.
    Startup,
    /// The tray icon's menu (item 70), also while the window is hidden.
    Tray,
}

impl LaunchOrigin {
    /// Asked from outside the window (a later start, the tray menu): the
    /// window comes to the front when the user has to answer (confirm a
    /// reopen) or read an error.
    pub fn from_outside(self) -> bool {
        matches!(self, LaunchOrigin::Shortcut | LaunchOrigin::Tray)
    }
}

/// What a request does with the desktop app as it is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchStep {
    /// Open it through Superba.
    Open,
    /// Plan a reopen through Superba (a full restart, confirmed first).
    Reopen,
    /// It runs: bring it to the front.
    Activate,
    /// Nothing (the start-up preference while ChatGPT runs, or no app).
    Nothing,
}

/// The step for `origin` on `overview` (the state machine of GH#7).
pub fn launch_step(overview: &Overview, origin: LaunchOrigin) -> LaunchStep {
    match (app_launch(overview), origin) {
        (Some(AppLaunch::Open), _) => LaunchStep::Open,
        (Some(AppLaunch::Reopen(_)), LaunchOrigin::Startup) => LaunchStep::Nothing,
        (Some(AppLaunch::Reopen(_)), _) => LaunchStep::Reopen,
        (None, LaunchOrigin::Startup) => LaunchStep::Nothing,
        (None, _) if overview.app.session.is_some() => LaunchStep::Activate,
        // No desktop app: opening says why.
        (None, _) => LaunchStep::Open,
    }
}

/// What [`launch`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchResult {
    /// ChatGPT was opened through Superba.
    Opened,
    /// ChatGPT was already running and was brought to the front.
    Activated,
    /// ChatGPT runs without hot switching: the caller plans the reopen.
    NeedsReopen,
    /// Nothing to do.
    Nothing,
}

/// Look at the desktop app now and do what `origin` asks for (never the
/// reopen itself: that is planned and confirmed by the caller).
pub async fn launch(service: &dyn SwitchService, origin: LaunchOrigin) -> Result<LaunchResult> {
    let overview = service.overview().await?;
    match launch_step(&overview, origin) {
        LaunchStep::Open => service.open_app().await.map(|_| LaunchResult::Opened),
        // Opened by Superba: `open_app` brings it forward. Running otherwise
        // (safe mode, a reason a reopen does not fix): nothing to change.
        LaunchStep::Activate => match service.open_app().await {
            Ok(_) | Err(Error::Conflict { .. }) => Ok(LaunchResult::Activated),
            Err(e) => Err(e),
        },
        LaunchStep::Reopen => Ok(LaunchResult::NeedsReopen),
        LaunchStep::Nothing => Ok(LaunchResult::Nothing),
    }
}

/// How `superba --launch-chatgpt` ended without a window of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// A running Superba window took the request.
    Forwarded,
    /// ChatGPT was opened or brought to the front; nothing else to show.
    Done,
    /// The user has to confirm a reopen, or opening failed: show the window
    /// (it asks again and shows the confirmation or the error).
    ShowWindow,
}

/// `superba --launch-chatgpt`: hand the request to a running Superba window,
/// else open ChatGPT through Superba directly.
pub fn forward_or_launch() -> Handled {
    let Ok(context) = superba_switch::detect_context() else {
        return Handled::ShowWindow;
    };
    if crate::instance::forward(
        context.data_dir.anchor(),
        crate::instance::Request::LaunchChatGpt,
    )
    .is_some()
    {
        return Handled::Forwarded;
    }
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return Handled::ShowWindow;
    };
    let service = superba_switch::default_service(context);
    match runtime.block_on(launch(service.as_ref(), LaunchOrigin::Shortcut)) {
        Ok(LaunchResult::Opened | LaunchResult::Activated | LaunchResult::Nothing) => Handled::Done,
        Ok(LaunchResult::NeedsReopen) => Handled::ShowWindow,
        Err(e) => {
            tracing::warn!(error = %e, "opening ChatGPT through Superba failed");
            Handled::ShowWindow
        }
    }
}
