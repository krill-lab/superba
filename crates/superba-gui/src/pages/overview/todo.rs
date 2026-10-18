//! The Overview's 待办 (user request 27): what needs the user's attention, each
//! with the button that fixes it or opens the page that does, and the hero node
//! it concerns (problem markers). Built from what the shell and the dashboard
//! already read: the overview's notices, diagnostics findings, versions and the
//! app's Codex pin, line probes, the router's status and watchdog, and the
//! request log (models without a price). Errors first, then warnings, then
//! offers (updates).

use superba_core::{RouterLineHealth, RouterWatchState, Severity};

use super::usage::Snapshot;
use super::{Action, Cx, Page};
use crate::design::widgets::Status;

/// The hero node a problem belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Spot {
    /// This computer: Codex versions, keys, local configuration.
    ThisComputer,
    /// The local router.
    Router,
    /// The provider and its line.
    Provider,
    /// The model and its speed tier.
    Model,
}

/// What a to-do is about (decides its button).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TodoKind {
    /// Router mode is on but the router does not answer.
    RouterDown,
    /// Router mode is on but Codex does not go through the router.
    RouterDirect,
    /// A router member's line is skipped after failures.
    MemberDown,
    /// The applied line did not answer the last probe.
    LineUnreachable,
    /// The app runs another Codex than the pinned one.
    PinMismatch,
    /// Keys are stored in a plain file (no system keyring).
    PlainKeys,
    /// Another diagnostics finding (its id).
    Finding(String),
    /// A notice of the overview.
    Notice,
    /// Requests of models without a price (cost not counted).
    Unpriced,
    /// A newer Superba.
    SuperbaUpdate,
    /// A newer desktop app.
    AppUpdate,
    /// A newer Codex CLI.
    CliUpdate,
    /// GH#5: another tool changed what Superba manages.
    ExternalChange,
}

/// One entry of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Todo {
    /// What it is about.
    pub kind: TodoKind,
    /// Error, warning or an offer ([`Status::Idle`]).
    pub status: Status,
    /// Text (translated).
    pub title: String,
    /// A second line (translated).
    pub detail: Option<String>,
    /// The node it marks in the hero (offers mark none).
    pub spot: Option<Spot>,
}

impl Todo {
    /// The button's label (translated).
    pub fn fix_label(&self, cx: &Cx<'_>) -> String {
        cx.tr.t(match self.kind {
            TodoKind::SuperbaUpdate | TodoKind::AppUpdate | TodoKind::CliUpdate => {
                "overview.todo.fix.update"
            }
            TodoKind::LineUnreachable => "overview.todo.fix.probe",
            TodoKind::Unpriced => "overview.todo.fix.prices",
            TodoKind::RouterDown | TodoKind::RouterDirect | TodoKind::MemberDown => {
                "overview.todo.fix.router"
            }
            TodoKind::ExternalChange => "overview.todo.fix.review",
            _ => "overview.todo.fix.view",
        })
    }

    /// What the button does.
    pub fn fix(&self, cx: &Cx<'_>) -> Action {
        match &self.kind {
            TodoKind::RouterDown | TodoKind::RouterDirect | TodoKind::MemberDown => {
                Action::Go(Page::Router)
            }
            TodoKind::LineUnreachable => match cx.data.active_site_id() {
                Some(site) => Action::Probe(site.clone()),
                None => Action::Go(Page::Providers),
            },
            TodoKind::PinMismatch
            | TodoKind::SuperbaUpdate
            | TodoKind::AppUpdate
            | TodoKind::CliUpdate => Action::Go(Page::Updates),
            TodoKind::Unpriced => Action::Go(Page::Models),
            TodoKind::PlainKeys | TodoKind::Finding(_) | TodoKind::Notice => {
                Action::Go(Page::Diagnostics)
            }
            TodoKind::ExternalChange => Action::ShowExternal,
        }
    }
}

fn status_of(severity: Severity) -> Option<Status> {
    match severity {
        Severity::Danger => Some(Status::Error),
        Severity::Warning => Some(Status::Warn),
        _ => None,
    }
}

/// Every to-do, errors first, then warnings, then offers; no text twice.
pub fn todos(cx: &Cx<'_>, snap: Option<&Snapshot>) -> Vec<Todo> {
    let mut out: Vec<Todo> = Vec::new();
    let mut push = |todo: Todo| {
        if !out.iter().any(|t| t.title == todo.title) {
            out.push(todo);
        }
    };
    let tr = cx.tr;
    let data = cx.data;
    let todo = |kind, status, title: String, detail: Option<String>, spot| Todo {
        kind,
        status,
        title,
        detail,
        spot,
    };

    // The router.
    let router = snap.and_then(|s| s.router.as_ref());
    if data.router_enabled() {
        let suspect = match &data.router_watch {
            Some(RouterWatchState::Suspect { detail }) => Some(detail.clone()),
            _ => None,
        };
        let stopped = router.is_some_and(|r| r.configured && !r.running);
        if suspect.is_some() || stopped {
            push(todo(
                TodoKind::RouterDown,
                Status::Error,
                tr.t("overview.todo.router_down"),
                suspect.or_else(|| router.and_then(|r| r.detail.clone())),
                Some(Spot::Router),
            ));
        } else if super::routed_addr(cx).is_none() && data.active_profile().is_some() {
            push(todo(
                TodoKind::RouterDirect,
                Status::Warn,
                tr.t("overview.todo.router_direct"),
                None,
                Some(Spot::Router),
            ));
        }
        for member in router.map(|r| r.members.as_slice()).unwrap_or_default() {
            for line in &member.lines {
                if line.health == RouterLineHealth::Open {
                    push(todo(
                        TodoKind::MemberDown,
                        Status::Warn,
                        tr.f(
                            "overview.todo.member_down",
                            &[("member", &member.name), ("line", &line.name)],
                        ),
                        line.last_error.clone(),
                        Some(Spot::Router),
                    ));
                }
            }
        }
    }

    // GH#5: what another tool changed in what Superba manages.
    if let Some(x) = data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.external.as_ref())
    {
        push(todo(
            TodoKind::ExternalChange,
            Status::Warn,
            tr.f(
                "overview.todo.external",
                &[("source", &cx.localize(&x.source.text()))],
            ),
            Some(tr.f("overview.todo.external_detail", &[("n", &x.count())])),
            Some(Spot::ThisComputer),
        ));
    }

    // The applied line.
    if let (Some(site), Some(line)) = (
        data.active_site_id(),
        data.active_state().and_then(|a| a.line_id.as_ref()),
    ) && data.site_caps(site, line).is_some_and(|c| !c.reachable)
    {
        push(todo(
            TodoKind::LineUnreachable,
            Status::Error,
            tr.f(
                "overview.todo.line_unreachable",
                &[("line", &cx.line_name(line))],
            ),
            None,
            Some(Spot::Provider),
        ));
    }

    // This computer: the app's Codex pin, diagnostics, notices.
    if let Some(mismatch) = data
        .versions
        .value
        .as_ref()
        .and_then(|v| v.app_codex.mismatch.as_ref())
    {
        push(todo(
            TodoKind::PinMismatch,
            Status::Warn,
            tr.t("overview.todo.pin_mismatch"),
            Some(cx.localize(mismatch)),
            Some(Spot::ThisComputer),
        ));
    }
    for f in data.doctor.value.as_deref().unwrap_or_default() {
        let Some(status) = status_of(f.severity) else {
            continue;
        };
        // Shown with its own button above (GH#5).
        if f.id == "external_change" {
            continue;
        }
        if f.id == "plaintext_secrets" {
            push(todo(
                TodoKind::PlainKeys,
                status,
                tr.t("overview.todo.plain_keys"),
                Some(tr.t("overview.todo.plain_keys_detail")),
                Some(Spot::ThisComputer),
            ));
        } else {
            push(todo(
                TodoKind::Finding(f.id.clone()),
                status,
                cx.localize(&f.title),
                f.detail.as_ref().map(|d| cx.localize(d)),
                Some(Spot::ThisComputer),
            ));
        }
    }
    if let Some(o) = data.overview.value.as_ref() {
        for n in &o.notices {
            if let Some(status) = status_of(n.severity) {
                push(todo(
                    TodoKind::Notice,
                    status,
                    cx.localize(&n.text),
                    None,
                    Some(Spot::ThisComputer),
                ));
            }
        }
    }

    // Costs that are not counted.
    if let Some(s) = snap
        && !s.unpriced.is_empty()
    {
        let names: Vec<&str> = s.unpriced.iter().take(2).map(String::as_str).collect();
        let more = s.unpriced.len().saturating_sub(names.len());
        let mut list = names.join(", ");
        if more > 0 {
            list = tr.f(
                "overview.todo.unpriced_more",
                &[("models", &list), ("n", &more)],
            );
        }
        push(todo(
            TodoKind::Unpriced,
            Status::Idle,
            tr.f("overview.todo.unpriced", &[("n", &s.unpriced.len())]),
            Some(tr.f("overview.todo.unpriced_detail", &[("models", &list)])),
            None,
        ));
    }

    // Offers: updates (Superba first).
    if let Some(release) = data.self_update_offered() {
        push(todo(
            TodoKind::SuperbaUpdate,
            Status::Idle,
            tr.f(
                "overview.todo.superba_update",
                &[("version", &release.version)],
            ),
            Some(tr.t("overview.todo.superba_update_detail")),
            None,
        ));
    }
    if let Some(v) = data.versions.value.as_ref() {
        if v.app.update_available {
            let version = v.app.latest.as_ref().map(|l| l.version.to_string());
            push(todo(
                TodoKind::AppUpdate,
                Status::Idle,
                match &version {
                    Some(version) => tr.f("overview.todo.app_update", &[("version", version)]),
                    None => tr.t("overview.todo.app_update_any"),
                },
                None,
                None,
            ));
        }
        if let Some(version) = v.cli.iter().find_map(|r| match &r.action {
            superba_core::CliUpdateAction::None => None,
            _ => r.latest.clone(),
        }) {
            push(todo(
                TodoKind::CliUpdate,
                Status::Idle,
                tr.f("overview.todo.cli_update", &[("version", &version)]),
                None,
                None,
            ));
        }
    }

    out.sort_by_key(|t| match t.status {
        Status::Error => 0,
        Status::Warn => 1,
        _ => 2,
    });
    out
}

/// The problems (errors and warnings) of one hero node.
pub fn markers(todos: &[Todo], spot: Spot) -> Vec<&Todo> {
    todos
        .iter()
        .filter(|t| t.spot == Some(spot) && matches!(t.status, Status::Error | Status::Warn))
        .collect()
}

/// The worst status among `todos` (an error over a warning).
pub fn worst(todos: &[&Todo]) -> Option<Status> {
    if todos.iter().any(|t| t.status == Status::Error) {
        Some(Status::Error)
    } else if todos.is_empty() {
        None
    } else {
        Some(Status::Warn)
    }
}
