//! Which providers can be router members (and why not), and the router doctor:
//! `superba-router`'s diagnostic facts as a localized, pasteable report.
//!
//! A member needs a key the router can send as an API key: a Krill or
//! OpenAI-compatible site, a stored key, an auth choice that is not "PAT only",
//! and a site with an API-key mode. The official ChatGPT login is never routed.
//! The GUI (Router page), `spb router enable` and `spb router doctor` all use
//! [`router_candidates`], so they explain the same thing the same way.

use superba_core::{
    AuthChoice, AuthMode, Error, LocalizedText, Notice, Profile, ProfileId, ProfileSet, Result,
    RouterDoctorReport, RouterDoctorRow, RouterDoctorSection, Severity, Site, SiteId, SiteKind,
};
use superba_router::doctor::{Finding, PortState, RouterFacts};
pub use superba_router::service::command_line;

use crate::Switcher;
use crate::messages::text;
use crate::planner::all_sites;

/// Why a provider can or cannot be a router member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Eligibility {
    /// It can: its key is sent to the site as an API key.
    Eligible,
    /// The official ChatGPT login (and the built-in profiles): never routed.
    OfficialLogin,
    /// No key entered yet.
    NoKey,
    /// The profile's auth is set to "PAT only"; the router sends an API key.
    PatOnly,
    /// The site has no API-key mode.
    NoApiKeyMode,
    /// The profile's site no longer exists.
    UnknownSite,
}

impl Eligibility {
    /// True for [`Eligibility::Eligible`].
    pub fn is_eligible(self) -> bool {
        self == Eligibility::Eligible
    }

    /// Stable id (catalog key suffix).
    pub fn id(self) -> &'static str {
        match self {
            Eligibility::Eligible => "eligible",
            Eligibility::OfficialLogin => "official",
            Eligibility::NoKey => "no_key",
            Eligibility::PatOnly => "pat_only",
            Eligibility::NoApiKeyMode => "no_api_key_mode",
            Eligibility::UnknownSite => "unknown_site",
        }
    }

    /// Why, for display.
    pub fn text(self) -> LocalizedText {
        text(&format!("switch.router_eligibility.{}", self.id()))
    }

    /// A key would fix it (the Providers page key field / `spb key set`).
    pub fn needs_key(self) -> bool {
        matches!(self, Eligibility::NoKey | Eligibility::PatOnly)
    }
}

/// A provider as a possible router member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouterCandidate {
    /// The profile; `None` for a site nobody entered a key for yet.
    pub profile_id: Option<ProfileId>,
    /// The site.
    pub site_id: SiteId,
    /// The site's kind.
    pub site_kind: Option<SiteKind>,
    /// The profile's name, else the site's.
    pub name: LocalizedText,
    /// The site's name.
    pub site_name: LocalizedText,
    /// Listed in the router's members.
    pub member: bool,
    /// Whether it can be a member.
    pub eligibility: Eligibility,
}

impl RouterCandidate {
    /// An eligible provider that is not a member yet.
    pub fn addable(&self) -> bool {
        !self.member && self.eligibility.is_eligible() && self.profile_id.is_some()
    }
}

/// Whether `profile` (on `site`) can be a router member.
pub fn eligibility(profile: &Profile, site: Option<&Site>) -> Eligibility {
    let Some(site) = site else {
        return Eligibility::UnknownSite;
    };
    if profile.is_builtin() || site.kind == SiteKind::Official {
        Eligibility::OfficialLogin
    } else if profile.key_ref.is_none() {
        Eligibility::NoKey
    } else if !site.supports(AuthMode::ApiKey) {
        Eligibility::NoApiKeyMode
    } else if profile.auth_choice() == AuthChoice::Pat {
        Eligibility::PatOnly
    } else {
        Eligibility::Eligible
    }
}

fn plain(value: impl ToString) -> LocalizedText {
    LocalizedText::new("", "{value}").arg("value", value)
}

/// Every provider as a possible member: the members in priority order, the
/// other profiles, then Krill and custom sites nobody entered a key for. The
/// built-in profiles (the official login) are left out unless listed as a
/// member: they are never routed and would only repeat that.
pub fn router_candidates(set: &ProfileSet, sites: &[Site]) -> Vec<RouterCandidate> {
    let members = &set.settings.router.members;
    let mut profiles: Vec<&Profile> = members.iter().filter_map(|id| set.profile(id)).collect();
    for p in &set.profiles {
        if !p.is_builtin() && !profiles.iter().any(|q| q.id == p.id) {
            profiles.push(p);
        }
    }
    let mut out: Vec<RouterCandidate> = profiles
        .into_iter()
        .map(|p| {
            let site = sites.iter().find(|s| s.id == p.site_id);
            let site_name = site
                .map(|s| s.name.clone())
                .unwrap_or_else(|| plain(&p.site_id));
            RouterCandidate {
                profile_id: Some(p.id.clone()),
                site_id: p.site_id.clone(),
                site_kind: site.map(|s| s.kind),
                name: if p.name.trim().is_empty() || p.is_builtin() {
                    site_name.clone()
                } else {
                    plain(&p.name)
                },
                site_name,
                member: members.contains(&p.id),
                eligibility: eligibility(p, site),
            }
        })
        .collect();
    // A Krill account is one provider, whichever Krill site entry a profile
    // uses: list a Krill site without a profile only when no Krill profile exists.
    let has_krill_profile = out.iter().any(|c| c.site_kind == Some(SiteKind::Krill));
    for site in sites {
        let routable = matches!(site.kind, SiteKind::Krill | SiteKind::Compatible)
            && site.supports(AuthMode::ApiKey)
            && !(site.kind == SiteKind::Krill && has_krill_profile);
        if routable && !set.profiles.iter().any(|p| p.site_id == site.id) {
            out.push(RouterCandidate {
                profile_id: None,
                site_id: site.id.clone(),
                site_kind: Some(site.kind),
                name: site.name.clone(),
                site_name: site.name.clone(),
                member: false,
                eligibility: Eligibility::NoKey,
            });
        }
    }
    out
}

/// At least one member can carry traffic.
pub fn has_eligible_member(candidates: &[RouterCandidate]) -> bool {
    candidates
        .iter()
        .any(|c| c.member && c.eligibility.is_eligible())
}

/// The error for turning the router on without an eligible member, with the
/// providers that could become one.
pub fn no_eligible_member_error(candidates: &[RouterCandidate]) -> Error {
    let render = |c: &RouterCandidate| {
        format!(
            "{} ({})",
            c.name.render_fallback(),
            c.eligibility.text().render_fallback()
        )
    };
    let addable: Vec<String> = candidates
        .iter()
        .filter(|c| c.addable())
        .map(render)
        .collect();
    let fixable: Vec<String> = candidates
        .iter()
        .filter(|c| c.eligibility.needs_key())
        .map(render)
        .collect();
    let mut message = text("switch.router_eligibility.none_eligible").render_fallback();
    if !addable.is_empty() {
        message.push_str(&format!(" Can be added: {}.", addable.join(", ")));
    }
    if !fixable.is_empty() {
        message.push_str(&format!(" Need an API key: {}.", fixable.join(", ")));
    }
    Error::invalid(message)
}

fn yes_no(value: bool) -> LocalizedText {
    text(if value {
        "switch.router_doctor.yes"
    } else {
        "switch.router_doctor.no"
    })
}

fn row(label: &str, value: LocalizedText) -> RouterDoctorRow {
    RouterDoctorRow {
        label: text(&format!("switch.router_doctor.row.{label}")),
        value,
    }
}

fn section(title: &str, rows: Vec<RouterDoctorRow>, lines: Vec<String>) -> RouterDoctorSection {
    RouterDoctorSection {
        title: text(&format!("switch.router_doctor.section.{title}")),
        rows,
        lines,
    }
}

fn finding_text(f: &Finding) -> LocalizedText {
    let key = |id: &str| text(&format!("switch.router_doctor.finding.{id}"));
    match f {
        Finding::ExeMissing { path } => key("exe_missing").arg("path", path.display()),
        Finding::ExeNotExecutable { path } => key("exe_not_executable").arg("path", path.display()),
        Finding::ExeDoesNotRun { detail } => key("exe_does_not_run").arg("detail", detail),
        Finding::ExeTransient { path } => key("exe_transient").arg("path", path.display()),
        Finding::Quarantined { path } => key("quarantined").arg("path", path.display()),
        Finding::ProtectedFolder { path } => key("protected_folder").arg("path", path.display()),
        Finding::NotConfigured => key("not_configured"),
        Finding::ConfigInvalid { detail } => key("config_invalid").arg("detail", detail),
        Finding::SecretsMissing => key("secrets_missing"),
        Finding::MembersWithoutKey { members } => {
            key("members_without_key").arg("members", members.join(", "))
        }
        Finding::ServiceNotInstalled => key("service_not_installed"),
        Finding::StaleDefinition { program, expected } => key("stale_definition")
            .arg("program", program.display())
            .arg("expected", expected.display()),
        Finding::ServiceNotLoaded => key("service_not_loaded"),
        Finding::CrashLoop { exit, signal } => {
            let how = match (exit, signal) {
                (_, Some(signal)) => signal.clone(),
                (Some(code), None) => code.to_string(),
                (None, None) => "?".into(),
            };
            key("crash_loop").arg("how", how)
        }
        Finding::NotStartedByServiceManager => key("not_started"),
        Finding::PortBusy { port, detail } => {
            key("port_busy").arg("port", port).arg("detail", detail)
        }
        Finding::RouterDown { detail } => key("router_down").arg("detail", detail),
        Finding::LogError { line } => key("log_error").arg("line", line),
    }
}

fn command_lines(record: &superba_router::service::CommandRecord) -> Vec<String> {
    let mut lines = vec![format!("$ {}", record.command)];
    match &record.result {
        Ok(o) => {
            lines.push(match o.code {
                Some(code) => format!("exit {code}"),
                None => "stopped (signal or timeout)".into(),
            });
            for stream in [&o.stdout, &o.stderr] {
                lines.extend(
                    stream
                        .lines()
                        .take(12)
                        .map(|l| format!("  {}", l.trim_end())),
                );
            }
        }
        Err(e) => lines.push(e.clone()),
    }
    lines
}

fn first_line(record: &superba_router::service::CommandRecord) -> String {
    match &record.result {
        Ok(o) => {
            let text = if o.stdout.trim().is_empty() {
                o.stderr.trim()
            } else {
                o.stdout.trim()
            };
            let exit = o
                .code
                .map_or_else(|| "stopped".to_owned(), |c| format!("exit {c}"));
            match text.lines().next() {
                Some(line) => format!("{line} ({exit})"),
                None => exit,
            }
        }
        Err(e) => e.clone(),
    }
}

fn program_section(facts: &RouterFacts) -> RouterDoctorSection {
    let exe = &facts.exe;
    let mut rows = vec![
        row("path", plain(exe.path.display())),
        row("exists", yes_no(exe.exists)),
    ];
    if let Some(x) = exe.executable {
        rows.push(row("executable", yes_no(x)));
    }
    if let Some(size) = exe.size {
        rows.push(row(
            "size",
            text("switch.router_doctor.value.size")
                .arg("mb", format!("{:.1}", size as f64 / 1_048_576.0)),
        ));
    }
    if let Some(v) = &exe.version {
        rows.push(row("version", plain(first_line(v))));
    }
    let location = if exe.transient {
        "switch.router_doctor.value.location_transient"
    } else if exe.protected_folder {
        "switch.router_doctor.value.location_protected"
    } else {
        "switch.router_doctor.value.location_ok"
    };
    rows.push(row("location", text(location)));
    if facts.platform.starts_with("macos") {
        let quarantine = |q: &Option<String>| match q {
            Some(v) => plain(v),
            None => text("switch.router_doctor.none"),
        };
        rows.push(row("quarantine", quarantine(&exe.quarantine)));
        if let Some((bundle, q)) = &exe.bundle {
            rows.push(row(
                "quarantine_app",
                text("switch.router_doctor.value.bundle")
                    .arg("app", bundle.display())
                    .arg_text("value", quarantine(q)),
            ));
        }
        if let Some(codesign) = &exe.codesign {
            let summary = match &codesign.result {
                Ok(o) => {
                    let text = format!("{}\n{}", o.stdout, o.stderr);
                    let picked: Vec<&str> = text
                        .lines()
                        .filter(|l| {
                            l.starts_with("Signature=")
                                || l.starts_with("CodeDirectory")
                                || l.starts_with("Format=")
                                || l.starts_with("TeamIdentifier=")
                                || l.contains("not signed")
                        })
                        .collect();
                    if picked.is_empty() {
                        first_line(codesign)
                    } else {
                        picked.join(" | ")
                    }
                }
                Err(e) => e.clone(),
            };
            rows.push(row("signature", plain(summary)));
        }
    }
    let mut lines: Vec<String> = exe
        .candidates
        .iter()
        .map(|(path, found)| {
            format!(
                "{} {}",
                if *found { "[found]" } else { "[-]" },
                path.display()
            )
        })
        .collect();
    // A program that does not run: its whole answer.
    if let Some(v) = exe.version.as_ref().filter(|v| !v.success()) {
        lines.extend(command_lines(v));
    }
    section("program", rows, lines)
}

fn service_section(facts: &RouterFacts) -> RouterDoctorSection {
    let s = &facts.service;
    let d = &s.diagnosis;
    let mut rows = vec![row("backend", plain(format!("{:?}", s.kind)))];
    if let Some(path) = &d.definition_path {
        let present = d.definition.is_some();
        rows.push(row(
            "definition",
            text(if present {
                "switch.router_doctor.value.file_present"
            } else {
                "switch.router_doctor.value.file_missing"
            })
            .arg("path", path.display()),
        ));
    }
    if let Some(program) = &d.program {
        rows.push(row("program", plain(program.display())));
    }
    if let Some(loaded) = d.loaded {
        rows.push(row("loaded", yes_no(loaded)));
    }
    match &s.status {
        Ok(status) => {
            rows.push(row("installed", yes_no(status.installed)));
            if let Some(pid) = status.pid {
                rows.push(row("pid", plain(pid)));
            }
        }
        Err(e) => rows.push(row("state", plain(e))),
    }
    let mut lines = Vec::new();
    if let Some(query) = &d.query {
        lines.push(format!("$ {}", query.command));
        match &query.result {
            Ok(o) if o.code == Some(0) => {
                const KEYS: [&str; 12] = [
                    "state",
                    "pid",
                    "runs",
                    "last exit code",
                    "last terminating signal",
                    "job state",
                    "program",
                    "path",
                    "stdout path",
                    "spawn type",
                    "immediate reason",
                    "properties",
                ];
                for key in KEYS {
                    if let Some(value) = d.field(key) {
                        lines.push(format!("  {key} = {value}"));
                    }
                }
            }
            Ok(o) => {
                lines.push(format!(
                    "exit {}",
                    o.code.map_or_else(|| "?".to_owned(), |c| c.to_string())
                ));
                lines.extend(
                    format!("{}{}", o.stdout, o.stderr)
                        .lines()
                        .take(6)
                        .map(|l| format!("  {l}")),
                );
            }
            Err(e) => lines.push(e.clone()),
        }
    } else {
        lines.extend(d.fields.iter().map(|(k, v)| format!("  {k} = {v}")));
    }
    if let Some(plist) = &d.definition {
        // The definition holds paths and RUST_LOG only, never a key.
        lines.push(String::new());
        lines.extend(plist.lines().map(str::to_owned));
    }
    section("service", rows, lines)
}

fn config_section(facts: &RouterFacts) -> RouterDoctorSection {
    let c = &facts.config;
    let mut rows = vec![row("data_dir", plain(facts.data_dir.display()))];
    rows.push(row(
        "config_json",
        if !c.config_exists {
            text("switch.router_doctor.missing")
        } else if let Some(e) = &c.config_error {
            plain(e)
        } else {
            text("switch.router_doctor.value.config")
                .arg("n", c.members.len())
                .arg(
                    "port",
                    c.port.map_or_else(|| "?".to_owned(), |p| p.to_string()),
                )
        },
    ));
    rows.push(row(
        "secrets_json",
        if !c.secrets_exists {
            text("switch.router_doctor.missing")
        } else if let Some(e) = &c.secrets_error {
            plain(e)
        } else {
            text("switch.router_doctor.value.secrets").arg(
                "mode",
                c.secrets_mode
                    .map_or_else(|| "-".to_owned(), |m| format!("{m:o}")),
            )
        },
    ));
    let lines = c
        .members
        .iter()
        .map(|m| {
            format!(
                "{} ({}): {} line(s), key {}",
                m.name,
                m.id,
                m.lines,
                if m.has_key { "present" } else { "MISSING" }
            )
        })
        .collect();
    section("config", rows, lines)
}

fn providers_section(candidates: &[RouterCandidate]) -> RouterDoctorSection {
    let rows = candidates
        .iter()
        .map(|c| RouterDoctorRow {
            label: text("switch.router_doctor.value.provider")
                .arg_text("name", c.name.clone())
                .arg(
                    "id",
                    c.profile_id
                        .as_ref()
                        .map_or_else(|| c.site_id.to_string(), |p| p.to_string()),
                ),
            value: text(if c.member {
                "switch.router_doctor.value.member"
            } else {
                "switch.router_doctor.value.not_member"
            })
            .arg_text("reason", c.eligibility.text()),
        })
        .collect();
    section("providers", rows, Vec::new())
}

fn health_section(facts: &RouterFacts) -> RouterDoctorSection {
    let mut rows = Vec::new();
    if let Some(port) = &facts.port {
        rows.push(row("port", plain(port.port)));
        rows.push(row(
            "port_state",
            match &port.state {
                PortState::Free => text("switch.router_doctor.value.port_free"),
                PortState::Router { version, instance } => {
                    text("switch.router_doctor.value.port_router")
                        .arg("version", version)
                        .arg("instance", instance)
                }
                PortState::Busy { detail } => {
                    text("switch.router_doctor.value.port_busy").arg("detail", detail)
                }
            },
        ));
    }
    let live = &facts.live;
    rows.push(row(
        "router",
        if live.up {
            text("switch.router_doctor.value.router_up")
                .arg("version", live.version.clone().unwrap_or_default())
                .arg("members", live.members.unwrap_or(0))
                .arg("uptime", live.uptime_ms.unwrap_or(0) / 1000)
        } else {
            text("switch.router_doctor.value.router_down")
                .arg("detail", live.detail.clone().unwrap_or_default())
        },
    ));
    rows.push(row(
        "runtime",
        match &facts.runtime {
            Some(r) => text("switch.router_doctor.value.runtime")
                .arg("pid", r.pid)
                .arg("port", r.port)
                .arg("version", &r.version),
            None => text("switch.router_doctor.missing"),
        },
    ));
    section("health", rows, Vec::new())
}

fn log_section(facts: &RouterFacts) -> RouterDoctorSection {
    let log = &facts.log;
    let rows = vec![row(
        "log_file",
        if log.exists {
            text("switch.router_doctor.value.log")
                .arg("path", log.path.display())
                .arg("kb", log.size.unwrap_or(0).div_ceil(1024))
        } else {
            text("switch.router_doctor.value.file_missing").arg("path", log.path.display())
        },
    )];
    RouterDoctorSection {
        title: text("switch.router_doctor.section.log").arg("n", log.tail.len()),
        rows,
        lines: log.tail.clone(),
    }
}

fn run_section(facts: &RouterFacts) -> RouterDoctorSection {
    let command = superba_router::service::command_line(
        facts.foreground.first().map(String::as_str).unwrap_or(""),
        &facts
            .foreground
            .iter()
            .skip(1)
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>(),
    );
    section("run", vec![row("command", plain(command))], Vec::new())
}

/// The report for `facts` (or the reason they could not be gathered) and the
/// member candidates.
pub fn report(
    facts: std::result::Result<&RouterFacts, &Error>,
    candidates: &[RouterCandidate],
) -> RouterDoctorReport {
    let mut findings = Vec::new();
    if !has_eligible_member(candidates) {
        findings.push(Notice::danger(text(
            "switch.router_eligibility.none_eligible",
        )));
        for c in candidates.iter().filter(|c| c.addable()) {
            findings.push(Notice::info(
                text("switch.router_doctor.finding.can_add")
                    .arg_text("name", c.name.clone())
                    .arg(
                        "id",
                        c.profile_id
                            .as_ref()
                            .map(ProfileId::to_string)
                            .unwrap_or_default(),
                    ),
            ));
        }
        for c in candidates.iter().filter(|c| c.eligibility.needs_key()) {
            findings.push(Notice::info(
                text("switch.router_doctor.finding.needs_key")
                    .arg_text("name", c.name.clone())
                    .arg_text("reason", c.eligibility.text()),
            ));
        }
    }
    let mut sections = Vec::new();
    let (title, foreground) = match facts {
        Ok(facts) => {
            for f in &facts.findings {
                // "Not configured" repeats the member explanation above.
                if matches!(f, Finding::NotConfigured) && !findings.is_empty() {
                    continue;
                }
                findings.push(Notice {
                    severity: if f.is_error() {
                        Severity::Danger
                    } else {
                        Severity::Warning
                    },
                    text: finding_text(f),
                });
            }
            sections.extend([
                program_section(facts),
                service_section(facts),
                config_section(facts),
                providers_section(candidates),
                health_section(facts),
                log_section(facts),
                run_section(facts),
            ]);
            (
                text("switch.router_doctor.title")
                    .arg("version", &facts.superba_version)
                    .arg(
                        "platform",
                        match &facts.os_version {
                            Some(v) => format!("{} {v}", facts.platform),
                            None => facts.platform.clone(),
                        },
                    ),
                facts.foreground.clone(),
            )
        }
        Err(e) => {
            findings.push(Notice::warning(
                text("switch.router_doctor.finding.no_facts").arg("detail", e.to_string()),
            ));
            sections.push(providers_section(candidates));
            (
                text("switch.router_doctor.title")
                    .arg("version", env!("CARGO_PKG_VERSION"))
                    .arg(
                        "platform",
                        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
                    ),
                Vec::new(),
            )
        }
    };
    RouterDoctorReport {
        title,
        findings,
        sections,
        foreground,
    }
}

impl Switcher {
    /// The member candidates of the stored profile set.
    pub(crate) async fn router_candidates_impl(&self) -> Result<Vec<RouterCandidate>> {
        let set = self.deps.profiles.load().await?;
        let sites = all_sites(&set, self.deps.ctx.env.as_ref());
        Ok(router_candidates(&set, &sites))
    }

    /// Refuse to turn router mode on without a member that can carry traffic.
    pub(crate) fn require_eligible_member(set: &ProfileSet, sites: &[Site]) -> Result<()> {
        let candidates = router_candidates(set, sites);
        if has_eligible_member(&candidates) {
            Ok(())
        } else {
            Err(no_eligible_member_error(&candidates))
        }
    }

    pub(crate) async fn router_doctor_impl(&self) -> Result<RouterDoctorReport> {
        let candidates = self.router_candidates_impl().await?;
        let facts = self.deps.router.doctor().await;
        Ok(report(facts.as_ref(), &candidates))
    }
}
