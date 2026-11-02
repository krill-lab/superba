//! Password gate for the desktop management UI. Router traffic and its watchdog
//! do not depend on unlocking. Password hashing runs off the UI thread.
mod store;

use crate::i18n::Tr;
use crate::tray::{TrayAction, TrayItem, TrayMenu};
use egui::{Id, Ui};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use store::{Config, Credential, Mode, Store};
use zeroize::{Zeroize, Zeroizing};

enum Outcome {
    Unlocked,
    Saved(Config),
}

enum Failure {
    WrongPassword,
    Other(String),
}

pub(crate) struct AppLock {
    store: Option<Store>,
    config: Config,
    locked: bool,
    load_error: Option<String>,
    dialog: bool,
    enabled: bool,
    mode: Mode,
    password: Zeroizing<String>,
    new_password: Zeroizing<String>,
    confirm: Zeroizing<String>,
    pending: Option<mpsc::Receiver<Result<Outcome, Failure>>>,
    notice: Option<(&'static str, Option<String>)>,
    failures: u32,
    retry_at: Option<Instant>,
    focus_password: bool,
}

impl AppLock {
    pub fn load() -> Self {
        let store = superba_switch::detect_context()
            .map(|c| Store::new(c.data_dir.anchor().to_path_buf()))
            .map_err(|e| e.to_string());
        Self::from_store(store)
    }

    fn from_store(store: Result<Store, String>) -> Self {
        let loaded = store.as_ref().map_err(Clone::clone).and_then(Store::load);
        let load_error = loaded.as_ref().err().cloned();
        let config = loaded.unwrap_or_default();
        let locked = load_error.is_some()
            || (config.credential.is_some()
                && !store.as_ref().is_ok_and(|s| s.remembered(&config)));
        Self {
            store: store.ok(),
            enabled: config.credential.is_some(),
            mode: config.mode,
            config,
            locked,
            load_error,
            dialog: false,
            password: Zeroizing::new(String::new()),
            new_password: Zeroizing::new(String::new()),
            confirm: Zeroizing::new(String::new()),
            pending: None,
            notice: None,
            failures: 0,
            retry_at: None,
            focus_password: true,
        }
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog
    }

    pub fn open_settings(&mut self) {
        if self.locked {
            return;
        }
        self.clear_inputs();
        self.enabled = self.config.credential.is_some();
        self.mode = self.config.mode;
        self.notice = None;
        self.dialog = true;
    }

    fn clear_inputs(&mut self) {
        self.password.zeroize();
        self.new_password.zeroize();
        self.confirm.zeroize();
        self.focus_password = true;
    }

    /// A hide/close invalidates only the every-open session. Minimizing the
    /// window or changing focus is intentionally not a close.
    pub fn window_closed(&mut self) {
        // Also cancel a first-time setup while there is no stored credential
        // yet. Otherwise its completion could enable every-open protection in
        // a hidden window, leaving that newly protected window unlocked.
        self.pending = None;
        self.dialog = false;
        self.clear_inputs();
        if self.config.credential.is_some() && self.config.mode == Mode::EveryOpen {
            self.locked = true;
        }
    }

    fn lock_now(&mut self) {
        // Locking wins over any password completion queued in the same frame.
        self.pending = None;
        if let Some(store) = &self.store
            && let Err(e) = store.forget()
        {
            self.notice = Some(("app_lock.error", Some(e)));
            return;
        }
        self.locked = self.config.credential.is_some();
        self.dialog = false;
        self.clear_inputs();
        self.notice = None;
    }

    /// Restricted tray contents contain neither provider data nor operations.
    pub fn tray_menu(&self, tr: &Tr) -> TrayMenu {
        TrayMenu {
            items: vec![
                TrayItem::Action {
                    label: tr.t("app_lock.unlock"),
                    action: TrayAction::Open,
                    enabled: true,
                    checked: None,
                },
                TrayItem::Separator,
                TrayItem::Action {
                    label: tr.t("tray.menu.quit"),
                    action: TrayAction::Quit,
                    enabled: true,
                    checked: None,
                },
            ],
        }
    }

    fn busy(&self) -> bool {
        self.pending.is_some() || self.retry_at.is_some_and(|t| Instant::now() < t)
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        job: impl FnOnce() -> Result<Outcome, Failure> + Send + 'static,
    ) {
        if self.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        match std::thread::Builder::new()
            .name("superba-ui-password".into())
            .spawn(move || {
                let _ = tx.send(job());
                repaint.request_repaint();
            }) {
            Ok(_) => {
                self.pending = Some(rx);
                self.notice = None;
            }
            Err(e) => self.notice = Some(("app_lock.error", Some(e.to_string()))),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let Some(rx) = &self.pending else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(Failure::Other("password worker stopped".into()))
            }
        };
        self.pending = None;
        match result {
            Ok(outcome) => {
                let Some(store) = &self.store else { return };
                if let Outcome::Saved(config) = outcome {
                    if let Err(e) = store.save(&config) {
                        self.notice = Some(("app_lock.error", Some(e)));
                        return;
                    }
                    self.config = config;
                    self.enabled = self.config.credential.is_some();
                    self.mode = self.config.mode;
                    self.notice = Some(("app_lock.saved", None));
                } else {
                    self.notice = None;
                }
                if let Err(e) = store.remember(&self.config) {
                    self.notice = Some(("app_lock.remember_failed", Some(e)));
                }
                self.locked = false;
                self.failures = 0;
                self.retry_at = None;
                self.clear_inputs();
            }
            Err(Failure::WrongPassword) => {
                self.failures = self.failures.saturating_add(1);
                self.retry_at =
                    Some(Instant::now() + Duration::from_secs(1 << self.failures.min(5)));
                self.notice = Some(("app_lock.wrong", None));
                self.password.zeroize();
                self.focus_password = true;
            }
            Err(Failure::Other(e)) => self.notice = Some(("app_lock.error", Some(e))),
        }
        ctx.request_repaint();
    }

    fn unlock(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let Some(credential) = self.config.credential.clone() else {
            return;
        };
        let password = Zeroizing::new(std::mem::take(&mut *self.password));
        self.start(ctx, move || {
            if credential.verify(&password) {
                Ok(Outcome::Unlocked)
            } else {
                Err(Failure::WrongPassword)
            }
        });
    }

    fn save(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let changing_password = !self.new_password.is_empty() || self.config.credential.is_none();
        if self.enabled && changing_password {
            if self.new_password.chars().count() < 8 || self.new_password.len() > 1024 {
                self.notice = Some(("app_lock.length", None));
                return;
            }
            if *self.new_password != *self.confirm {
                self.notice = Some(("app_lock.mismatch", None));
                return;
            }
        }
        let old = self.config.credential.clone();
        let password = Zeroizing::new(std::mem::take(&mut *self.password));
        let new_password = Zeroizing::new(std::mem::take(&mut *self.new_password));
        self.confirm.zeroize();
        let mode = self.mode;
        let enabled = self.enabled;
        self.start(ctx, move || {
            if old.as_ref().is_some_and(|c| !c.verify(&password)) {
                return Err(Failure::WrongPassword);
            }
            let credential = if !enabled {
                None
            } else if new_password.is_empty() {
                old
            } else {
                Some(Credential::create(&new_password).map_err(Failure::Other)?)
            };
            Ok(Outcome::Saved(Config::changed(mode, credential)))
        });
    }

    fn status(&self, ui: &mut Ui, tr: &Tr) {
        if let Some((key, detail)) = &self.notice {
            ui.label(tr.t(key));
            if let Some(detail) = detail {
                ui.small(detail);
            }
        }
        if self.pending.is_some() {
            ui.spinner();
            ui.label(tr.t("app_lock.checking"));
        }
        if self.retry_at.is_some_and(|t| t > Instant::now()) {
            ui.label(tr.t("app_lock.wait"));
        }
    }

    pub fn locked_ui(&mut self, ui: &mut Ui, tr: &Tr) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.2).max(24.0));
                ui.set_max_width(440.0);
                ui.heading(tr.t("app_lock.locked"));
                ui.add_space(12.0);
                ui.label(tr.t("app_lock.background"));
                ui.add_space(20.0);
                if let Some(error) = &self.load_error {
                    ui.label(tr.t("app_lock.load_failed"));
                    ui.small(error);
                    if ui.button(tr.t("app_lock.retry")).clicked() {
                        *self = Self::load();
                    }
                    return;
                }
                let label = tr.t("app_lock.password");
                let response = ui.add_enabled(
                    !self.busy(),
                    egui::TextEdit::singleline(&mut *self.password)
                        .password(true)
                        .char_limit(1024)
                        .hint_text(&label)
                        .desired_width(320.0),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::PasswordInput, response.enabled(), &label)
                });
                clear_password_undo(ui.ctx(), response.id);
                if self.focus_password && !self.busy() {
                    response.request_focus();
                    self.focus_password = false;
                }
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui
                    .add_enabled(
                        !self.busy() && !self.password.is_empty(),
                        egui::Button::new(tr.t("app_lock.unlock")),
                    )
                    .clicked()
                    || enter
                {
                    self.unlock(ui.ctx());
                }
                self.status(ui, tr);
            });
        });
    }

    pub fn settings_ui(&mut self, ctx: &egui::Context, tr: &Tr) {
        if !self.dialog || self.locked {
            return;
        }
        egui::Modal::new(Id::unique("superba-app-lock-settings")).show(ctx, |ui| {
            ui.set_width(460.0_f32.min(ctx.content_rect().width() - 60.0).max(200.0));
            let title = ui.heading(tr.t("app_lock.title"));
            crate::design::a11y::name_dialog(ui, title.id);
            ui.label(tr.t("app_lock.description"));
            ui.separator();
            ui.add_enabled_ui(!self.busy(), |ui| {
                ui.checkbox(&mut self.enabled, tr.t("app_lock.enabled"));
                ui.add_enabled_ui(self.enabled, |ui| {
                    ui.radio_value(&mut self.mode, Mode::EveryOpen, tr.t("app_lock.every_open"));
                    ui.small(tr.t("app_lock.every_open_hint"));
                    let can_remember = self.store.as_ref().is_some_and(|s| s.boot_id.is_some());
                    ui.add_enabled_ui(can_remember, |ui| {
                        ui.radio_value(&mut self.mode, Mode::PerBoot, tr.t("app_lock.per_boot"));
                    });
                    ui.small(tr.t("app_lock.per_boot_hint"));
                    if !can_remember {
                        ui.small(tr.t("app_lock.no_boot_id"));
                    }
                });
                ui.add_space(12.0);
                if self.config.credential.is_some() {
                    password_field(ui, &tr.t("app_lock.current"), &mut self.password);
                    ui.small(tr.t("app_lock.current_hint"));
                }
                if self.enabled {
                    password_field(ui, &tr.t("app_lock.new"), &mut self.new_password);
                    password_field(ui, &tr.t("app_lock.confirm"), &mut self.confirm);
                    ui.small(tr.t("app_lock.new_hint"));
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(tr.t("app_lock.save")).clicked() {
                        self.save(ctx);
                    }
                    if ui.button(tr.t("app_lock.close")).clicked() {
                        self.dialog = false;
                        self.clear_inputs();
                    }
                    if self.config.credential.is_some()
                        && ui.button(tr.t("app_lock.lock_now")).clicked()
                    {
                        self.lock_now();
                    }
                });
            });
            self.status(ui, tr);
        });
    }
}

fn password_field(ui: &mut Ui, label: &str, value: &mut String) {
    let label = ui.label(label);
    let response = ui
        .add(
            egui::TextEdit::singleline(value)
                .password(true)
                .char_limit(1024)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
    clear_password_undo(ui.ctx(), response.id);
}

// egui retains undo history even for password fields. Clear it every frame so
// Ctrl+Z after locking or reopening cannot restore a previously entered secret.
fn clear_password_undo(ctx: &egui::Context, id: Id) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        state.clear_undoer();
        state.store(ctx, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn configured(mode: Mode) -> (tempfile::TempDir, AppLock) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().to_owned());
        store
            .save(&Config::changed(
                mode,
                Some(Credential::create("test-password").unwrap()),
            ))
            .unwrap();
        let lock = AppLock::from_store(Ok(store));
        (root, lock)
    }

    #[test]
    fn closing_window_relocks_only_every_open_and_manual_lock_forgets_boot_session() {
        for mode in [Mode::EveryOpen, Mode::PerBoot] {
            let (_root, mut lock) = configured(mode);
            assert!(lock.is_locked());
            lock.locked = false;
            lock.store.as_ref().unwrap().remember(&lock.config).unwrap();
            lock.window_closed();
            assert_eq!(lock.is_locked(), mode == Mode::EveryOpen);
            lock.lock_now();
            assert!(lock.is_locked());
            assert!(!lock.store.as_ref().unwrap().remembered(&lock.config));
        }
    }

    #[test]
    fn locked_tray_exposes_only_unlock_and_quit_and_settings_cannot_open() {
        let (_root, mut lock) = configured(Mode::EveryOpen);
        lock.open_settings();
        assert!(!lock.dialog_open());
        let menu = lock.tray_menu(&Tr::new(superba_core::Locale::En));
        let actions: Vec<_> = menu
            .actions()
            .into_iter()
            .map(|(_, a, _)| a.clone())
            .collect();
        assert_eq!(actions, [TrayAction::Open, TrayAction::Quit]);
    }

    fn finish(lock: &mut AppLock, ctx: &egui::Context) {
        let end = Instant::now() + Duration::from_secs(15);
        while lock.pending.is_some() {
            assert!(Instant::now() < end, "password worker timed out");
            lock.poll(ctx);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn wrong_password_cannot_unlock_disable_or_change_mode() {
        let (_root, mut lock) = configured(Mode::EveryOpen);
        let ctx = egui::Context::default();
        *lock.password = "wrong-password".into();
        lock.unlock(&ctx);
        finish(&mut lock, &ctx);
        assert!(lock.is_locked());
        assert!(lock.busy());
        lock.retry_at = None;
        *lock.password = "test-password".into();
        lock.unlock(&ctx);
        finish(&mut lock, &ctx);
        assert!(!lock.is_locked());
        lock.open_settings();
        lock.enabled = false;
        lock.mode = Mode::PerBoot;
        *lock.password = "wrong-password".into();
        lock.save(&ctx);
        finish(&mut lock, &ctx);
        let stored = lock.store.as_ref().unwrap().load().unwrap();
        assert!(stored.credential.is_some());
        assert!(stored.mode == Mode::EveryOpen);
        lock.retry_at = None;
        *lock.password = "test-password".into();
        lock.save(&ctx);
        finish(&mut lock, &ctx);
        assert!(
            lock.store
                .as_ref()
                .unwrap()
                .load()
                .unwrap()
                .credential
                .is_none()
        );
    }

    #[test]
    fn closing_during_verification_discards_completion_and_keeps_window_locked() {
        let (_root, mut lock) = configured(Mode::EveryOpen);
        let ctx = egui::Context::default();
        *lock.password = "test-password".into();
        lock.unlock(&ctx);
        lock.window_closed();
        assert!(lock.pending.is_none());
        lock.poll(&ctx);
        assert!(lock.is_locked());
    }

    #[test]
    fn closing_during_first_password_setup_does_not_save_an_unlocked_hidden_window() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().to_owned());
        let mut lock = AppLock::from_store(Ok(store.clone()));
        let ctx = egui::Context::default();
        lock.open_settings();
        lock.enabled = true;
        *lock.new_password = "new-test-password".into();
        *lock.confirm = "new-test-password".into();
        lock.save(&ctx);
        assert!(lock.pending.is_some());
        lock.window_closed();
        lock.poll(&ctx);
        assert!(lock.pending.is_none());
        assert!(!lock.dialog_open());
        assert!(store.load().unwrap().credential.is_none());
    }

    #[test]
    fn manual_lock_wins_over_a_queued_password_completion() {
        let (_root, mut lock) = configured(Mode::PerBoot);
        lock.locked = false;
        let (tx, rx) = mpsc::channel();
        lock.pending = Some(rx);
        tx.send(Ok(Outcome::Unlocked))
            .unwrap_or_else(|_| panic!("receiver is alive"));
        lock.lock_now();
        lock.poll(&egui::Context::default());
        assert!(lock.is_locked());
        assert!(lock.pending.is_none());
    }

    #[test]
    fn unreadable_settings_never_open_the_management_ui() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().to_owned());
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(store.dir.join("settings.json"), "invalid-json").unwrap();
        let lock = AppLock::from_store(Ok(store));
        assert!(lock.is_locked());
        assert!(lock.load_error.is_some());
    }
}
