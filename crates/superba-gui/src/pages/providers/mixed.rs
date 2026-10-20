//! Mixed login on the official login's detail (Providers page): one setting,
//! "use the official account for remote features". While the local router is
//! on, Codex then keeps the user's own ChatGPT login (remote control, Codex
//! Cloud, the app's browser plugin and usage pages talk to chatgpt.com with
//! it) and only model traffic goes through the router's providers.
//!
//! The setting is stored at once ([`superba_core::SwitchService::router_set_mixed_login`])
//! and takes effect with the next switch to a routed provider; the status line
//! says which state the computer is in and offers that switch.

use egui::Ui;
use superba_core::{AuthMode, AuthShape, ProfileId, RouterView, Timestamp};

use crate::design::fonts::{Weight, font};
use crate::design::tokens::text;
use crate::design::widgets::{Button, FormRow, Status, StatusDot, Text, Toggle};
use crate::pages::{Action, Cx};
use crate::runtime::ReadTask;

/// A login not refreshed for this long may have expired (Codex refreshes it
/// every eight days while it is used).
const STALE_DAYS: i64 = 30;

/// Page state of the setting.
#[derive(Default)]
pub(crate) struct MixedLogin {
    /// The value being saved (shown until the save ends).
    pending: Option<bool>,
    task: Option<ReadTask<RouterView>>,
}

impl MixedLogin {
    /// Collect a finished save; true while one runs.
    pub(crate) fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) -> bool {
        let Some(task) = &self.task else {
            return false;
        };
        match task.poll() {
            Some(Ok(_)) => {
                self.task = None;
                self.pending = None;
                out.push(Action::Refresh);
                false
            }
            Some(Err(e)) => {
                self.task = None;
                self.pending = None;
                out.push(Action::Toast {
                    status: Status::Error,
                    text: crate::pages::error_text(cx.tr, &e),
                });
                false
            }
            None => true,
        }
    }
}

/// Where the computer stands (pure, for the status line and tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MixedStatus {
    /// The setting is off.
    Off,
    /// On, but the local router is off: nothing changes until it is on.
    RouterOff,
    /// On, but no official login is saved or signed in.
    NoLogin,
    /// On; Codex still holds the router token: the next switch applies it.
    Pending,
    /// Codex runs with mixed login (the account's email, if known).
    Active(Option<String>),
}

/// Facts the status is derived from.
pub(crate) struct Facts<'a> {
    pub on: bool,
    pub router_on: bool,
    pub live: Option<&'a AuthShape>,
    pub stash_account: Option<&'a str>,
    pub applied: Option<AuthMode>,
}

pub(crate) fn status(f: &Facts<'_>) -> MixedStatus {
    if !f.on {
        return MixedStatus::Off;
    }
    if !f.router_on {
        return MixedStatus::RouterOff;
    }
    let live = match f.live {
        Some(AuthShape::ChatGpt { email, account_id }) if account_id.is_some() => {
            Some(email.clone())
        }
        _ => None,
    };
    if f.applied == Some(AuthMode::MixedOfficial) {
        return MixedStatus::Active(live.flatten());
    }
    if live.is_none() && f.stash_account.is_none_or(str::is_empty) {
        return MixedStatus::NoLogin;
    }
    MixedStatus::Pending
}

/// Days since the saved login was last refreshed, when it is old enough to
/// mention.
pub(crate) fn stale_days(last_refresh: Option<Timestamp>, now: Timestamp) -> Option<i64> {
    let days = (now.as_millis() - last_refresh?.as_millis()) / 86_400_000;
    (days > STALE_DAYS).then_some(days)
}

/// The setting row and its status line.
pub(crate) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut MixedLogin, out: &mut Vec<Action>) {
    let profiles = cx.data.profiles.value.as_ref();
    let stored = profiles.is_some_and(|p| p.settings.router.mixed_login);
    let mut on = state.pending.unwrap_or(stored);
    let overview = cx.data.overview.value.as_ref();
    let config = overview.and_then(|o| o.config.as_ref());
    let stash = overview.and_then(|o| o.login_stash.as_ref());
    let label = cx.tr.t("providers.mixed.toggle");
    let changed = FormRow::new(Text::small(&label, cx.pal).font(font(text::SMALL, Weight::Medium)))
        .label_width(0.0)
        .right()
        .show(ui, |ui| {
            ui.add(Toggle::new(&mut on, &label).enabled(!cx.busy && state.task.is_none()))
                .changed()
        })
        .inner;
    // The help spans the whole width (a hint under the control column is
    // squeezed at 960 px).
    ui.add(
        egui::Label::new(
            Text::small(cx.tr.t("providers.mixed.hint"), cx.pal).color(cx.pal.text_subtle),
        )
        .wrap(),
    );
    if changed {
        state.pending = Some(on);
        let service = cx.service.clone();
        state.task = Some(crate::runtime::task(async move {
            service.router_set_mixed_login(on).await
        }));
    }
    let facts = Facts {
        on,
        router_on: cx.data.router_enabled(),
        live: config.map(|c| &c.auth),
        stash_account: stash.and_then(|s| s.account_id.as_deref()),
        applied: config
            .and_then(|c| c.active.as_ref())
            .and_then(|a| a.auth_mode),
    };
    let st = status(&facts);
    let (dot, line) = match &st {
        MixedStatus::Off => return,
        MixedStatus::RouterOff => (Status::Idle, cx.tr.t("providers.mixed.router_off")),
        MixedStatus::NoLogin => (Status::Warn, cx.tr.t("providers.mixed.no_login")),
        MixedStatus::Pending => (Status::Warn, cx.tr.t("providers.mixed.pending")),
        MixedStatus::Active(email) => (
            Status::Ok,
            cx.tr.f(
                "providers.mixed.active",
                &[(
                    "email",
                    &email
                        .clone()
                        .unwrap_or_else(|| cx.tr.t("providers.mixed.account")),
                )],
            ),
        ),
    };
    ui.add_space(crate::design::tokens::space::S);
    ui.horizontal_wrapped(|ui| {
        ui.add(StatusDot::new(dot));
        ui.add(egui::Label::new(Text::small(line, cx.pal)).wrap());
    });
    if let Some(days) = stale_days(stash.and_then(|s| s.last_refresh), cx.now)
        .filter(|_| !matches!(st, MixedStatus::NoLogin))
    {
        ui.add(
            egui::Label::new(
                Text::small(cx.tr.f("providers.mixed.stale", &[("days", &days)]), cx.pal)
                    .color(cx.pal.status(Status::Warn)),
            )
            .wrap(),
        );
    }
    let applied_profile: Option<ProfileId> = config
        .and_then(|c| c.active.as_ref())
        .and_then(|a| a.profile_id.clone());
    if matches!(st, MixedStatus::Pending | MixedStatus::Active(_))
        && let Some(profile) = applied_profile
    {
        // Re-apply the routed provider: Codex restarts with the new login form.
        let key = if st == MixedStatus::Pending {
            "providers.mixed.apply"
        } else {
            "providers.mixed.reapply"
        };
        ui.add_space(crate::design::tokens::space::S);
        if ui
            .add(Button::new(cx.tr.t(key)).small().enabled(!cx.busy))
            .clicked()
        {
            out.push(Action::Switch {
                profile,
                line: None,
                auth: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> AuthShape {
        AuthShape::ChatGpt {
            email: Some("me@example.com".into()),
            account_id: Some("acct".into()),
        }
    }

    fn facts<'a>(live: Option<&'a AuthShape>, applied: Option<AuthMode>) -> Facts<'a> {
        Facts {
            on: true,
            router_on: true,
            live,
            stash_account: None,
            applied,
        }
    }

    #[test]
    fn states() {
        let l = login();
        let pat = AuthShape::Pat {
            key_last4: "abcd".into(),
        };
        assert_eq!(
            status(&Facts {
                on: false,
                ..facts(Some(&l), None)
            }),
            MixedStatus::Off
        );
        assert_eq!(
            status(&Facts {
                router_on: false,
                ..facts(Some(&l), None)
            }),
            MixedStatus::RouterOff
        );
        assert_eq!(
            status(&facts(Some(&pat), Some(AuthMode::Pat))),
            MixedStatus::NoLogin
        );
        assert_eq!(
            status(&Facts {
                stash_account: Some("acct"),
                ..facts(Some(&pat), Some(AuthMode::Pat))
            }),
            MixedStatus::Pending
        );
        assert_eq!(
            status(&facts(Some(&l), Some(AuthMode::MixedOfficial))),
            MixedStatus::Active(Some("me@example.com".into()))
        );
    }

    #[test]
    fn stale_after_thirty_days() {
        let now = Timestamp::from_millis(100 * 86_400_000);
        assert_eq!(
            stale_days(Some(Timestamp::from_millis(90 * 86_400_000)), now),
            None
        );
        assert_eq!(
            stale_days(Some(Timestamp::from_millis(40 * 86_400_000)), now),
            Some(60)
        );
        assert_eq!(stale_days(None, now), None);
    }
}
