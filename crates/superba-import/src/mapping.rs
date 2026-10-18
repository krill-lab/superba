//! From foreign providers to Superba: the import target of each provider (the
//! Krill preset when its address is a Krill line, else a custom
//! OpenAI-compatible site), conflicts with existing providers, and the profiles
//! and sites an import saves. Pure: no I/O.

use std::collections::BTreeMap;

use superba_core::catalog::{self, KRILL_SITE_ID};
use superba_core::{
    AuthChoice, AuthMode, CandidateStatus, CodexSettings, ConflictAction, ForeignApp,
    ImportCandidate, ImportConflict, ImportKey, ImportLine, ImportScan, ImportSelection,
    ImportTarget, Line, LineId, LineSource, LocalizedText, ModelDefaults, Origin, Profile,
    ProfileId, ProfileSet, Secret, Site, SiteId, SiteKind, SourceScan,
};
use url::Url;

use crate::foreign::{ForeignKey, ForeignProvider, SourceRead};
use crate::text;

/// Ids no imported profile or site may take: the built-in sites and profiles and
/// the quick-pick preset ids (a profile saved under a preset id is treated as a
/// setup-wizard re-run and merged into an existing one).
const RESERVED_IDS: [&str; 5] = ["krill", "official", "original", "custom", "compatible"];

/// Longest generated id (a numeric suffix included).
const ID_LIMIT: usize = 48;

/// Normalised address of a Responses base URL: scheme and host as the URL parser
/// writes them (lower case), no trailing slash.
fn normalize(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

/// The base URL of a compatible site (line origin + the path its models
/// endpoint was derived from).
fn compatible_base(site: &Site, line: Option<&LineId>) -> Option<Url> {
    let line = match line {
        Some(id) => site.line(id).or_else(|| site.default_line()),
        None => site.default_line(),
    }?;
    let path = site
        .endpoints
        .models_unauth
        .strip_suffix("/models")
        .unwrap_or("")
        .trim_end_matches('/');
    Some(if path.is_empty() {
        line.origin.url().clone()
    } else {
        line.origin.join(path)
    })
}

/// What an existing provider points at, for conflict detection.
#[derive(Debug)]
struct Existing<'a> {
    profile: &'a Profile,
    kind: Option<SiteKind>,
    /// Krill: the primary line's origin; compatible: the normalised base URL.
    address: Option<String>,
}

fn existing<'a>(set: &'a ProfileSet, sites: &'a [Site]) -> Vec<Existing<'a>> {
    set.profiles
        .iter()
        .filter(|p| !p.is_builtin())
        .map(|p| {
            let site = sites.iter().find(|s| s.id == p.site_id);
            let first = p.line_priority().into_iter().next();
            let address = site.and_then(|s| match s.kind {
                SiteKind::Krill => first
                    .as_ref()
                    .and_then(|l| s.line(l))
                    .or_else(|| s.default_line())
                    .map(|l| l.origin.to_string()),
                SiteKind::Compatible => p
                    .base_url_override
                    .clone()
                    .or_else(|| compatible_base(s, first.as_ref()))
                    .map(|u| normalize(&u)),
                _ => None,
            });
            Existing {
                profile: p,
                kind: site.map(|s| s.kind),
                address,
            }
        })
        .collect()
}

/// Names and ids already used, plus those proposed earlier in the same scan.
#[derive(Debug, Default)]
struct Taken {
    names: Vec<String>,
    ids: Vec<String>,
}

impl Taken {
    fn new(set: &ProfileSet, sites: &[Site]) -> Self {
        let mut t = Taken::default();
        for p in &set.profiles {
            t.names.push(p.name.trim().to_lowercase());
            t.ids.push(p.id.as_str().to_owned());
        }
        for s in sites.iter().chain(&set.custom_sites) {
            t.ids.push(s.id.as_str().to_owned());
        }
        t.ids.extend(RESERVED_IDS.iter().map(|s| (*s).to_owned()));
        t
    }

    fn name_taken(&self, name: &str) -> bool {
        self.names.contains(&name.trim().to_lowercase())
    }

    /// `name`, else `name (Tool)`, `name (Tool) 2`, ...; reserved.
    fn unique_name(&mut self, name: &str, tool: &str) -> String {
        let name = name.trim();
        let mut candidate = name.to_owned();
        if self.name_taken(&candidate) {
            let base = format!("{name} ({tool})");
            candidate = base.clone();
            let mut n = 2;
            while self.name_taken(&candidate) {
                candidate = format!("{base} {n}");
                n += 1;
            }
        }
        self.names.push(candidate.to_lowercase());
        candidate
    }

    /// An ASCII id from `name` (`imported` when nothing is left), unique; reserved.
    fn unique_id(&mut self, name: &str) -> String {
        let mut base = String::new();
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                base.push(c.to_ascii_lowercase());
            } else if !base.is_empty() && !base.ends_with('-') {
                base.push('-');
            }
            if base.len() >= ID_LIMIT {
                break;
            }
        }
        base.truncate(base.trim_end_matches('-').len());
        if base.is_empty() {
            base.push_str("imported");
        }
        let mut id = base.clone();
        let mut n = 2;
        while self.ids.contains(&id) {
            let tail = format!("-{n}");
            let prefix = &base[..base.len().min(ID_LIMIT - tail.len())];
            id = format!("{}{tail}", prefix.trim_end_matches('-'));
            n += 1;
        }
        self.ids.push(id.clone());
        id
    }
}

/// The Krill site and line an origin belongs to: an exact line match on any
/// Krill-kind site (the built-in one first), else the built-in site's default
/// line for another Krill host (`exact` false).
fn krill_line(origin: &Origin, sites: &[Site]) -> Option<(SiteId, LineId, bool)> {
    let mut krill: Vec<&Site> = sites.iter().filter(|s| s.kind == SiteKind::Krill).collect();
    krill.sort_by_key(|s| s.id.as_str() != KRILL_SITE_ID);
    for site in &krill {
        if let Some(line) = site.lines.iter().find(|l| &l.origin == origin) {
            return Some((site.id.clone(), line.id.clone(), true));
        }
    }
    if catalog::is_krill_host(origin.host()) {
        let site = krill
            .iter()
            .find(|s| s.id.as_str() == KRILL_SITE_ID)
            .or(krill.first())?;
        let line = site.default_line()?;
        return Some((site.id.clone(), line.id.clone(), false));
    }
    None
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Parse and check a base URL the way custom sites are checked: https (plain
/// http only to loopback), no credentials, query or fragment.
fn parse_base(raw: &str) -> Result<Url, LocalizedText> {
    let raw = raw.trim();
    let url = Url::parse(raw).map_err(|_| {
        text(
            "import.reason.bad_url",
            "The address {url} is not a valid URL.",
        )
        .arg("url", raw)
    })?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        "http" => {
            return Err(text(
                "import.reason.insecure_url",
                "The address {url} uses plain http; Superba sends keys over https only.",
            )
            .arg("url", raw));
        }
        _ => {
            return Err(text(
                "import.reason.bad_url",
                "The address {url} is not a valid URL.",
            )
            .arg("url", raw));
        }
    }
    if url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(text(
            "import.reason.bad_url",
            "The address {url} is not a valid URL.",
        )
        .arg("url", raw));
    }
    Ok(url)
}

fn origin_of(url: &Url) -> Option<Origin> {
    Origin::parse(&url.origin().ascii_serialization()).ok()
}

fn key_state(key: &ForeignKey) -> ImportKey {
    match key {
        ForeignKey::Secret(s) => ImportKey::Present { masked: s.masked() },
        ForeignKey::Keychain => ImportKey::Keychain,
        ForeignKey::Env(v) => ImportKey::EnvVar { name: v.clone() },
        ForeignKey::Missing => ImportKey::Missing,
    }
}

fn not_importable(p: &ForeignProvider) -> Option<LocalizedText> {
    if let Some(reason) = &p.blocked {
        return Some(reason.clone());
    }
    if p.app != ForeignApp::Codex {
        return Some(
            text(
                "import.reason.not_codex",
                "A {app} provider: Superba sets up Codex only.",
            )
            .arg("app", p.app.label()),
        );
    }
    if p.official {
        return Some(text(
            "import.reason.official",
            "The official ChatGPT login: Superba has it built in (Official ChatGPT), sign in there.",
        ));
    }
    if p.base_url.as_deref().is_none_or(|u| u.trim().is_empty()) {
        return Some(text(
            "import.reason.no_base_url",
            "No base URL is set for it.",
        ));
    }
    None
}

/// Build the target of an importable provider (`Err` = not importable).
fn target_of(
    p: &ForeignProvider,
    sites: &[Site],
    taken: &mut Taken,
    notes: &mut Vec<LocalizedText>,
) -> Result<ImportTarget, LocalizedText> {
    let raw = p.base_url.as_deref().unwrap_or_default();
    let url = parse_base(raw)?;
    let origin = origin_of(&url).ok_or_else(|| {
        text(
            "import.reason.bad_url",
            "The address {url} is not a valid URL.",
        )
        .arg("url", raw)
    })?;
    let profile_name = taken.unique_name(&p.name, p.source.name());
    if let Some((site_id, line_id, exact)) = krill_line(&origin, sites) {
        let site = sites.iter().find(|s| s.id == site_id);
        let mut lines: Vec<ImportLine> = Vec::new();
        let mut push = |id: &LineId| {
            if let Some(l) = site.and_then(|s| s.line(id))
                && !lines.iter().any(|x| &x.id == id)
            {
                lines.push(ImportLine {
                    id: l.id.clone(),
                    name: l.name.clone(),
                    origin: l.origin.clone(),
                });
            }
        };
        push(&line_id);
        // Alternative Krill addresses of the same provider become backup lines.
        for extra in &p.extra_urls {
            if let Some(o) = Url::parse(extra.trim()).ok().as_ref().and_then(origin_of)
                && let Some((sid, lid, true)) = krill_line(&o, sites)
                && sid == site_id
            {
                push(&lid);
            }
        }
        if !exact {
            notes.push(
                text(
                    "import.note.krill_unknown_line",
                    "{url} is a Krill address but not one of its lines; the default line is used.",
                )
                .arg("url", raw.trim()),
            );
        }
        if !p.model_map.is_empty() {
            notes.push(text(
                "import.note.krill_model_map",
                "Krill serves Codex's model names; the model mapping is not imported.",
            ));
        }
        return Ok(ImportTarget {
            kind: SiteKind::Krill,
            site_id,
            profile_name,
            base_url: None,
            lines,
            model: p.model.clone(),
            reasoning_effort: p.reasoning_effort.clone(),
            model_map: BTreeMap::new(),
        });
    }
    let site_id = SiteId::new(taken.unique_id(&profile_name));
    let base_path = url.path().trim_end_matches('/').to_owned();
    let mut lines = vec![ImportLine {
        id: LineId::new("default"),
        name: LocalizedText::new("", origin.host()),
        origin: origin.clone(),
    }];
    let mut other_paths = Vec::new();
    for extra in &p.extra_urls {
        let Ok(u) = parse_base(extra) else {
            continue;
        };
        let Some(o) = origin_of(&u) else {
            continue;
        };
        if lines.iter().any(|l| l.origin == o) {
            continue;
        }
        if u.path().trim_end_matches('/') != base_path {
            other_paths.push(extra.trim().to_owned());
            continue;
        }
        lines.push(ImportLine {
            id: LineId::new(format!("line-{}", lines.len() + 1)),
            name: LocalizedText::new("", o.host()),
            origin: o,
        });
    }
    if !other_paths.is_empty() {
        notes.push(
            text(
                "import.note.extra_urls_skipped",
                "Other addresses with a different path are not added as lines: {urls}",
            )
            .arg("urls", other_paths.join(", ")),
        );
    }
    Ok(ImportTarget {
        kind: SiteKind::Compatible,
        site_id,
        profile_name,
        base_url: Some(url),
        lines,
        model: p.model.clone(),
        reasoning_effort: p.reasoning_effort.clone(),
        model_map: p.model_map.clone(),
    })
}

fn target_address(t: &ImportTarget) -> Option<String> {
    match t.kind {
        SiteKind::Krill => t.lines.first().map(|l| l.origin.to_string()),
        _ => t.base_url.as_ref().map(normalize),
    }
}

fn conflict_of(name: &str, t: &ImportTarget, existing: &[Existing<'_>]) -> Option<ImportConflict> {
    let addr = target_address(t);
    let lname = name.trim().to_lowercase();
    let mut best: Option<ImportConflict> = None;
    for e in existing {
        let same_name = e.profile.name.trim().to_lowercase() == lname;
        let same_base_url = e.kind == Some(t.kind) && addr.is_some() && e.address == addr;
        if !same_name && !same_base_url {
            continue;
        }
        let c = ImportConflict {
            profile: e.profile.id.clone(),
            profile_name: e.profile.name.clone(),
            same_name,
            same_base_url,
        };
        let score = |c: &ImportConflict| (c.same_base_url as u8) * 2 + c.same_name as u8;
        if best.as_ref().is_none_or(|b| score(&c) > score(b)) {
            best = Some(c);
        }
    }
    best
}

/// A provider pre-selected earlier in the same scan.
struct Picked {
    kind: SiteKind,
    address: String,
    key: ForeignKey,
    name: String,
    tool: &'static str,
}

fn candidate(
    p: &ForeignProvider,
    sites: &[Site],
    existing: &[Existing<'_>],
    taken: &mut Taken,
    picked: &mut Vec<Picked>,
) -> ImportCandidate {
    let mut notes = p.notes.clone();
    let mut status = CandidateStatus::Ready;
    let mut target = None;
    match not_importable(p) {
        Some(reason) => status = CandidateStatus::NotImportable { reason },
        None => match target_of(p, sites, taken, &mut notes) {
            Ok(t) => target = Some(t),
            Err(reason) => status = CandidateStatus::NotImportable { reason },
        },
    }
    let importable = target.is_some();
    let chat = p
        .wire_api
        .as_deref()
        .is_some_and(|w| w.eq_ignore_ascii_case("chat"));
    if importable {
        if chat {
            notes.push(text(
                "import.note.chat_wire_api",
                "It is set up for the Chat Completions API; Superba uses the Responses API. Test it before you switch.",
            ));
        }
        match &p.key {
            ForeignKey::Secret(_) => {}
            ForeignKey::Keychain => notes.push(
                text(
                    "import.note.keychain",
                    "{tool} keeps this key in the system keychain, which Superba does not read: it is imported without a key. Add the key in Superba afterwards.",
                )
                .arg("tool", p.source.name()),
            ),
            ForeignKey::Env(var) => notes.push(
                text(
                    "import.note.env_key",
                    "The key comes from the environment variable {name}; it is imported without a key.",
                )
                .arg("name", var),
            ),
            ForeignKey::Missing => notes.push(text(
                "import.note.no_key",
                "No key is stored for it; it is imported without a key.",
            )),
        }
    }
    let conflict = target
        .as_ref()
        .and_then(|t| conflict_of(&p.name, t, existing));
    let mut preselected = importable && conflict.is_none() && !chat && !p.deselect;
    // The same provider kept by both tools (AI Toolbox imports cc-switch's):
    // only the first one is pre-selected.
    if let Some(t) = &target
        && let Some(addr) = target_address(t)
    {
        // Same address and same key: one provider, kept twice.
        if let Some(first) = picked
            .iter()
            .find(|x| x.kind == t.kind && x.address == addr && x.key == p.key)
        {
            if preselected {
                notes.push(
                    text(
                        "import.note.duplicate",
                        "Same address and key as {name} from {tool}, which is selected instead.",
                    )
                    .arg("name", &first.name)
                    .arg("tool", first.tool),
                );
            }
            preselected = false;
        } else if preselected {
            picked.push(Picked {
                kind: t.kind,
                address: addr,
                key: p.key.clone(),
                name: p.name.clone(),
                tool: p.source.name(),
            });
        }
    }
    ImportCandidate {
        id: p.candidate_id(),
        source: p.source,
        name: p.name.clone(),
        app: p.app.clone(),
        current: p.current,
        base_url: p.base_url.clone(),
        model: p.model.clone(),
        key: key_state(&p.key),
        status,
        target,
        conflict,
        notes,
        preselected,
    }
}

/// The preview of `reads` against Superba's providers (`set`) and sites (all of
/// them, built-in and custom, as the switching service lists them).
pub fn preview(reads: &[SourceRead], set: &ProfileSet, sites: &[Site]) -> ImportScan {
    let existing = existing(set, sites);
    let mut taken = Taken::new(set, sites);
    let mut picked = Vec::new();
    let sources = reads
        .iter()
        .map(|read| SourceScan {
            source: read.source,
            stores: read.stores.clone(),
            candidates: read
                .providers
                .iter()
                .map(|p| candidate(p, sites, &existing, &mut taken, &mut picked))
                .collect(),
        })
        .collect();
    ImportScan { sources }
}

/// What an import does with one selected candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlannedAction {
    /// Save `site` (a new custom site) then `profile` with `key`.
    Create {
        /// New custom site.
        site: Option<Site>,
        /// New profile.
        profile: Profile,
        /// Key for the secret store.
        key: Option<Secret>,
    },
    /// Save `site` (the updated custom site) then the updated `profile`.
    Update {
        /// Updated custom site.
        site: Option<Site>,
        /// Updated profile.
        profile: Profile,
        /// New key for the secret store (`None` keeps the stored one).
        key: Option<Secret>,
    },
    /// Nothing.
    Skip {
        /// Why.
        reason: LocalizedText,
    },
}

/// One planned item of an import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// The selection.
    pub selection: ImportSelection,
    /// The candidate, when it is still there.
    pub candidate: Option<ImportCandidate>,
    /// What to do.
    pub action: PlannedAction,
}

/// Builds a custom site from (id, name, base URL) with the backend's validation
/// (`superba_switch::new_compatible_site`).
pub type MakeSite<'a> = &'a dyn Fn(SiteId, &str, &Url) -> superba_core::Result<Site>;

fn new_key_profile(site: &Site, id: ProfileId, name: String) -> Profile {
    let krill = site.kind == SiteKind::Krill;
    let mut p = Profile {
        id,
        name,
        builtin: None,
        site_id: site.id.clone(),
        line_id: None,
        backup_lines: Vec::new(),
        auth_mode: AuthMode::ApiKey,
        auth: AuthChoice::ApiKey,
        key_ref: None,
        base_url_override: None,
        model: ModelDefaults::default(),
        codex_settings: if krill {
            CodexSettings::krill_recommended()
        } else {
            CodexSettings::new()
        },
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
    };
    // Like the Add dialog and the wizard: Automatic where the site offers PAT,
    // else a plain API key.
    if site.supports(AuthMode::Pat) {
        p.auth_mode = AuthMode::Pat;
        p.set_auth(AuthChoice::Auto);
    }
    p
}

fn apply_model(profile: &mut Profile, t: &ImportTarget) {
    if t.model.is_some() {
        profile.model.model = t.model.clone();
    }
    if t.reasoning_effort.is_some() {
        profile.model.reasoning_effort = t.reasoning_effort.clone();
    }
}

fn add_lines(site: &mut Site, t: &ImportTarget) {
    for l in t.lines.iter().skip(1) {
        if site.lines.iter().any(|x| x.origin == l.origin) {
            continue;
        }
        let mut id = l.id.clone();
        let mut n = site.lines.len() + 1;
        while site.line(&id).is_some() {
            id = LineId::new(format!("line-{n}"));
            n += 1;
        }
        site.lines.push(Line {
            id,
            name: l.name.clone(),
            origin: l.origin.clone(),
            source: LineSource::User,
        });
    }
}

fn skip(reason: LocalizedText) -> PlannedAction {
    PlannedAction::Skip { reason }
}

/// Plan an import of `selections` from a fresh preview (`scan`) of `reads`.
/// Writes nothing; the switching service saves the result.
pub fn plan(
    reads: &[SourceRead],
    scan: &ImportScan,
    set: &ProfileSet,
    sites: &[Site],
    selections: &[ImportSelection],
    make_site: MakeSite<'_>,
) -> Vec<Planned> {
    // Every name and id the preview proposed is reserved: a created provider
    // takes the name and site id its preview row showed.
    let mut taken = Taken::new(set, sites);
    for t in scan.candidates().filter_map(|c| c.target.as_ref()) {
        taken.names.push(t.profile_name.trim().to_lowercase());
        if t.kind != SiteKind::Krill {
            taken.ids.push(t.site_id.as_str().to_owned());
        }
    }
    let providers: BTreeMap<String, &ForeignProvider> = reads
        .iter()
        .flat_map(|r| r.providers.iter())
        .map(|p| (p.candidate_id(), p))
        .collect();
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for sel in selections {
        let candidate = scan.candidate(&sel.candidate).cloned();
        let foreign = providers.get(&sel.candidate).copied();
        let action = if seen.contains(&sel.candidate) {
            skip(text(
                "import.skip.duplicate",
                "Selected twice; imported once.",
            ))
        } else {
            seen.push(sel.candidate.clone());
            match (&candidate, foreign) {
                (Some(c), Some(f)) => {
                    plan_one(c, f, sel.on_conflict, set, sites, &mut taken, make_site)
                }
                _ => skip(text(
                    "import.skip.gone",
                    "It is no longer in the other tool's configuration.",
                )),
            }
        };
        out.push(Planned {
            selection: sel.clone(),
            candidate,
            action,
        });
    }
    out
}

fn plan_one(
    c: &ImportCandidate,
    f: &ForeignProvider,
    on_conflict: ConflictAction,
    set: &ProfileSet,
    sites: &[Site],
    taken: &mut Taken,
    make_site: MakeSite<'_>,
) -> PlannedAction {
    let Some(t) = c.target.as_ref().filter(|_| c.importable()) else {
        let reason = c
            .not_importable_reason()
            .cloned()
            .unwrap_or_else(|| text("import.reason.unknown", "It cannot be imported."));
        return skip(
            text("import.skip.not_importable", "Not importable: {reason}")
                .arg_text("reason", reason),
        );
    };
    let key = f.key.secret().cloned();
    match (&c.conflict, on_conflict) {
        (Some(conflict), ConflictAction::Skip) => skip(
            text(
                "import.skip.conflict",
                "{name} already exists in Superba; it was left as it is.",
            )
            .arg("name", &conflict.profile_name),
        ),
        (Some(conflict), ConflictAction::Update) => {
            match update(conflict, t, key, set, sites, make_site) {
                Ok(action) => action,
                Err(reason) => skip(reason),
            }
        }
        (Some(_), _) | (None, _) => create(c, t, key, sites, taken, make_site),
    }
}

fn create(
    c: &ImportCandidate,
    t: &ImportTarget,
    key: Option<Secret>,
    sites: &[Site],
    taken: &mut Taken,
    make_site: MakeSite<'_>,
) -> PlannedAction {
    let _ = c;
    let name = t.profile_name.clone();
    match t.kind {
        SiteKind::Krill => {
            let Some(site) = sites.iter().find(|s| s.id == t.site_id) else {
                return skip(text("import.reason.unknown", "It cannot be imported."));
            };
            let id = ProfileId::new(taken.unique_id(&name));
            let mut profile = new_key_profile(site, id, name);
            profile.set_line_priority(t.lines.iter().map(|l| l.id.clone()));
            apply_model(&mut profile, t);
            PlannedAction::Create {
                site: None,
                profile,
                key,
            }
        }
        _ => {
            let Some(base) = &t.base_url else {
                return skip(text(
                    "import.reason.no_base_url",
                    "No base URL is set for it.",
                ));
            };
            let id = t.site_id.as_str().to_owned();
            let mut site = match make_site(SiteId::new(&id), &name, base) {
                Ok(s) => s,
                Err(e) => {
                    return skip(
                        text(
                            "import.skip.site_rejected",
                            "The address was refused: {detail}",
                        )
                        .arg("detail", e),
                    );
                }
            };
            site.model_map = t.model_map.clone();
            add_lines(&mut site, t);
            let mut profile = new_key_profile(&site, ProfileId::new(&id), name);
            apply_model(&mut profile, t);
            PlannedAction::Create {
                site: Some(site),
                profile,
                key,
            }
        }
    }
}

fn update(
    conflict: &ImportConflict,
    t: &ImportTarget,
    key: Option<Secret>,
    set: &ProfileSet,
    sites: &[Site],
    make_site: MakeSite<'_>,
) -> Result<PlannedAction, LocalizedText> {
    let kind_mismatch = || {
        text(
            "import.skip.update_kind",
            "{name} is a different kind of provider and cannot be updated from this one; choose Rename to import it next to it.",
        )
        .arg("name", &conflict.profile_name)
    };
    let mut profile = set.profile(&conflict.profile).cloned().ok_or_else(|| {
        text(
            "import.skip.gone",
            "It is no longer in the other tool's configuration.",
        )
    })?;
    let site = sites
        .iter()
        .chain(&set.custom_sites)
        .find(|s| s.id == profile.site_id)
        .ok_or_else(kind_mismatch)?;
    if site.kind != t.kind {
        return Err(kind_mismatch());
    }
    let new_site = match t.kind {
        SiteKind::Krill => {
            if site.id != t.site_id {
                return Err(kind_mismatch());
            }
            // `set_line_priority` drops duplicates: the imported lines first,
            // then the provider's other lines.
            let lines: Vec<LineId> = t
                .lines
                .iter()
                .map(|l| l.id.clone())
                .chain(profile.line_priority())
                .collect();
            profile.set_line_priority(lines);
            None
        }
        _ => {
            let base = t
                .base_url
                .as_ref()
                .ok_or_else(|| text("import.reason.no_base_url", "No base URL is set for it."))?;
            let name = site.name.render_fallback();
            let mut updated = make_site(site.id.clone(), &name, base).map_err(|e| {
                text(
                    "import.skip.site_rejected",
                    "The address was refused: {detail}",
                )
                .arg("detail", e)
            })?;
            updated.homepage = site.homepage.clone();
            updated.name = site.name.clone();
            let mut map = site.model_map.clone();
            map.extend(t.model_map.clone());
            updated.model_map = map;
            add_lines(&mut updated, t);
            if profile.base_url_override.is_some() {
                profile.base_url_override = Some(base.clone());
            }
            if profile
                .line_id
                .as_ref()
                .is_some_and(|l| updated.line(l).is_none())
            {
                profile.set_line_priority(Vec::new());
            }
            Some(updated)
        }
    };
    apply_model(&mut profile, t);
    Ok(PlannedAction::Update {
        site: new_site,
        profile,
        key,
    })
}
