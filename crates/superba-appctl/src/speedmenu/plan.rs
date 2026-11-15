//! Find the gates Superba patches in the app's renderer bundle and build
//! patched copies of the module factories that hold them: the four speed-tier
//! gates ([`PatchSet::SpeedMenu`]) and the Statsig gate of composer
//! predictions ([`PatchSet::Predictions`]).
//!
//! The renderer (app 26.1103+) is an Rspack build: every chunk under
//! `webview/assets/` is an ES module exporting `__webpack_modules__ = {id(e,t,n)
//! {...}, ...}`, and the runtime copies those factories into its module table
//! when the chunk loads. The same module may ship in several chunks.
//!
//! The gates (verified on app 26.1106) all compare the sign-in method with a
//! fixed list (`chatgpt`, `personalAccessToken`, `amazonBedrock`), so an
//! API-key sign-in never gets the speed tier:
//!
//! | Gate | What it decides |
//! |---|---|
//! | [`GateKind::UiAccess`] | the composer hook that yields `serviceTierAccess` (the speed menu) |
//! | [`GateKind::Requirements`] | reading the service-tier requirements for a host |
//! | [`GateKind::QueueDefault`] | `readServiceTier`: whether the configured tier applies to a turn |
//! | [`GateKind::QueueExplicit`] | whether the tier chosen in the composer is sent with a turn |
//!
//! Each patch keeps the original condition and adds `|| allow(host, method)`,
//! a call into the page policy Superba installs (local host and API-key
//! sign-in only, see `hook.js`). The Copilot checks in front of the queue
//! gates stay as they are. Each gate must match in exactly one module, and
//! the four go together: when one is missing none is patched.
//!
//! Composer predictions ("cumberbatch", verified on app 26.1103 and 26.1106:
//! after an assistant reply the composer suggests the next message, Tab
//! accepts it) are behind the Statsig gate [`PREDICTION_GATE`] (a hashed
//! name) in the renderer, then behind the app's main process, which asks
//! chatgpt.com (`/wham/predictions/config`) whether they are on for the
//! signed-in ChatGPT account and runs them through Codex (`thread/sample`).
//! Superba can only reach the renderer: it forces the gate there and puts its
//! own prediction service in front of the main process's (`hook.js`): where
//! the main process gives no prediction (no ChatGPT sign-in, or the account is
//! not enabled), the service sends `thread/sample` through the window's
//! app-server client itself. Each of the three gate checks ([`GateKind::
//! PredictionSettings`], [`GateKind::PredictionLive`], [`GateKind::
//! PredictionComposer`]) is patched on its own when it matches in exactly one
//! module, as `(check||gate(id))`; the service swap in `appServices`
//! ([`GateKind::PredictionServices`]) and the client's registration
//! ([`GateKind::PredictionClient`]) only together. The cloud-task variant of
//! the gate (predictions computed by chatgpt.com for cloud threads) is left
//! alone.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};

use super::asar::Archive;
use super::jslex;

/// Version of the plan format and rules; a cached plan of another version is
/// computed again.
pub const PLAN_VERSION: u32 = 2;

/// Chunks larger than this are not considered (the largest app chunk is ~9 MB).
const CHUNK_LIMIT: u64 = 48 * 1024 * 1024;

/// Literal text every chunk holding a gate contains; chunks without any of
/// them are not lexed.
const CHUNK_ANCHORS: &[&str] = &[
    "serviceTierAccess",
    "personalAccessToken",
    "writeServiceTier",
    "submissionOptions",
    PREDICTION_GATE_ARG,
    SERVICES_ANCHOR,
    CLIENT_ANCHOR,
];

/// Where the renderer stores the app's services (`appServices`, a proxy of the
/// main process's services).
const SERVICES_ANCHOR: &str = "setAppServices:function(";

/// The end of the renderer's app-server request client constructor.
const CLIENT_ANCHOR: &str = ",this.getRequestRejection=";

/// The Statsig gate (hashed name) that turns composer predictions on in the
/// renderer.
pub const PREDICTION_GATE: &str = "4290031608";

/// [`PREDICTION_GATE`] as the last argument of a gate check.
const PREDICTION_GATE_ARG: &str = "\"4290031608\")";

/// The page policy call: `true` only for the local host signed in with an API
/// key (see `hook.js`).
fn allow(host: &str, method: &str) -> String {
    format!("globalThis.__superbaSpeedMenu?.allow?.({host},{method})===!0")
}

/// The page policy call of the prediction gate (`true` when the predictions
/// are forced, see `hook.js`).
fn force_gate() -> String {
    format!("globalThis.__superbaSpeedMenu?.gate?.(\"{PREDICTION_GATE}\")===!0")
}

/// One of the patched gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    /// The speed menu in the composer.
    UiAccess,
    /// Service-tier requirements of a host.
    Requirements,
    /// The configured tier of a queued turn.
    QueueDefault,
    /// The tier chosen in the composer for a turn.
    QueueExplicit,
    /// The "Show predictions" row in Settings > General.
    PredictionSettings,
    /// The prediction requested after each finished local turn (two checks in
    /// one function, before and after reading the thread).
    PredictionLive,
    /// The composer's prediction state (shows and requests predictions).
    PredictionComposer,
    /// The renderer's `appServices` (Superba's own prediction service in front
    /// of the app's `cumberbatch`, see `hook.js`).
    PredictionServices,
    /// The renderer's app-server request client registers itself, so the
    /// prediction service can send `thread/sample` (see `hook.js`).
    PredictionClient,
}

/// A group of gates turned on together by one setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchSet {
    /// The app's speed menu with an API key (all four gates or none).
    SpeedMenu,
    /// Forced composer predictions (each check on its own).
    Predictions,
}

/// Which patch sets a launch wants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PatchSets {
    /// [`PatchSet::SpeedMenu`].
    pub speed_menu: bool,
    /// [`PatchSet::Predictions`].
    pub predictions: bool,
}

impl PatchSets {
    /// Whether `set` is wanted.
    pub fn has(self, set: PatchSet) -> bool {
        match set {
            PatchSet::SpeedMenu => self.speed_menu,
            PatchSet::Predictions => self.predictions,
        }
    }

    /// Any set is wanted.
    pub fn any(self) -> bool {
        self.speed_menu || self.predictions
    }
}

impl GateKind {
    /// The speed-menu gates.
    pub const SPEED_MENU: [GateKind; 4] = [
        GateKind::UiAccess,
        GateKind::Requirements,
        GateKind::QueueDefault,
        GateKind::QueueExplicit,
    ];
    /// The prediction checks.
    pub const PREDICTIONS: [GateKind; 5] = [
        GateKind::PredictionSettings,
        GateKind::PredictionLive,
        GateKind::PredictionComposer,
        GateKind::PredictionServices,
        GateKind::PredictionClient,
    ];

    /// Gates that are only patched together (the service needs the client and
    /// the other way round).
    const PAIRED: [(GateKind, GateKind); 1] =
        [(GateKind::PredictionServices, GateKind::PredictionClient)];

    /// The set the gate belongs to.
    pub fn set(self) -> PatchSet {
        if Self::SPEED_MENU.contains(&self) {
            PatchSet::SpeedMenu
        } else {
            PatchSet::Predictions
        }
    }

    /// Matches the module holding the gate must have.
    fn hits_per_module(self) -> usize {
        match self {
            GateKind::PredictionLive => 2,
            _ => 1,
        }
    }

    /// The snake_case name (`ui_access`).
    pub fn name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// A patched module factory, as method source (`id(e,t,n){...}`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchedModule {
    /// The module id as written in the chunk (an identifier, a number or a
    /// quoted string).
    pub key: String,
    /// The id as the runtime sees it (property name).
    pub id: String,
    /// Length of the original factory source in UTF-16 code units (what
    /// `Function.prototype.toString().length` reports in the page).
    pub original_len: usize,
    /// FNV-1a (32-bit) of the original source's UTF-16 code units.
    pub original_fnv: u32,
    /// The patched factory, method syntax.
    pub source: String,
    /// Gates patched in it.
    pub gates: Vec<GateKind>,
}

/// What Superba patches in one app build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// [`PLAN_VERSION`].
    pub version: u32,
    /// The patched modules; each holds gates of one [`PatchSet`].
    pub modules: Vec<PatchedModule>,
    /// Why gates are not patched (`ui_access: 0 matches`), per set; empty
    /// when every gate is.
    #[serde(default)]
    pub skipped: BTreeMap<PatchSet, String>,
}

impl Plan {
    /// The set of a module (all its gates are in one).
    fn module_set(m: &PatchedModule) -> Option<PatchSet> {
        m.gates.first().map(|g| g.set())
    }

    /// The plan restricted to `sets`, or why it has nothing for them.
    pub fn select(&self, sets: PatchSets) -> std::result::Result<Plan, NoPlan> {
        let modules: Vec<PatchedModule> = self
            .modules
            .iter()
            .filter(|m| Self::module_set(m).is_some_and(|s| sets.has(s)))
            .cloned()
            .collect();
        let skipped: BTreeMap<PatchSet, String> = self
            .skipped
            .iter()
            .filter(|(s, _)| sets.has(**s))
            .map(|(s, r)| (*s, r.clone()))
            .collect();
        if modules.is_empty() {
            let reason = if skipped.is_empty() {
                "nothing to patch".to_owned()
            } else {
                skipped.values().cloned().collect::<Vec<_>>().join(", ")
            };
            return Err(NoPlan {
                version: self.version,
                reason,
            });
        }
        Ok(Plan {
            version: self.version,
            modules,
            skipped,
        })
    }

    /// The modules whose patching means the window is ready: the speed menu's
    /// (loaded at start), else the live prediction check's, else all.
    pub fn settle_ids(&self) -> Vec<String> {
        let ids = |pred: &dyn Fn(&PatchedModule) -> bool| -> Vec<String> {
            let mut v: Vec<String> = self
                .modules
                .iter()
                .filter(|m| pred(m))
                .map(|m| m.id.clone())
                .collect();
            v.dedup();
            v
        };
        let speed = ids(&|m| Self::module_set(m) == Some(PatchSet::SpeedMenu));
        if !speed.is_empty() {
            return speed;
        }
        let live = ids(&|m| m.gates.contains(&GateKind::PredictionLive));
        if !live.is_empty() {
            return live;
        }
        ids(&|_| true)
    }
}

/// Why there is no plan for an app build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoPlan {
    /// [`PLAN_VERSION`].
    pub version: u32,
    /// Short reason code with detail, e.g. `ui_access: 0 matches`.
    pub reason: String,
}

/// FNV-1a (32-bit) over UTF-16 code units, as `hook.js` computes it.
pub fn fnv1a_utf16(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in text.encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// A module factory found in a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Factory<'a> {
    /// The key as written (`ipV4`, `123`, `"x"`).
    pub key: &'a str,
    /// The method source, key included (what `toString` returns).
    pub source: &'a str,
}

impl Factory<'_> {
    /// The property name the key defines.
    pub fn id(&self) -> String {
        let k = self.key;
        if k.len() >= 2 && (k.starts_with('"') || k.starts_with('\'')) {
            k[1..k.len() - 1].to_owned()
        } else {
            k.to_owned()
        }
    }
}

/// The module factories of an Rspack chunk (`export const
/// __webpack_modules__={...}`), or `None` when the chunk has another shape or
/// a factory this module cannot delimit.
pub fn chunk_factories(text: &str) -> Option<Vec<Factory<'_>>> {
    const MARKER: &str = "__webpack_modules__={";
    let start = text.find(MARKER)? + MARKER.len() - 1;
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = start + 1;
    loop {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if *bytes.get(i)? == b'}' {
            return Some(out);
        }
        let key_start = i;
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while *bytes.get(i)? != quote {
                    if bytes[i] == b'\\' {
                        return None;
                    }
                    i += 1;
                }
                i += 1;
            }
            _ => {
                while bytes
                    .get(i)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
                {
                    i += 1;
                }
            }
        }
        if i == key_start || bytes.get(i) != Some(&b'(') {
            return None;
        }
        let key = &text[key_start..i];
        let close_paren = jslex::matching_paren(bytes, i)?;
        if bytes.get(close_paren + 1) != Some(&b'{') {
            return None;
        }
        let close = jslex::matching_brace(bytes, close_paren + 1)?;
        out.push(Factory {
            key,
            source: &text[key_start..=close],
        });
        i = close + 1;
        match bytes.get(i)? {
            b',' => i += 1,
            b'}' => return Some(out),
            _ => return None,
        }
    }
}

/// An identifier in minified output.
const ID: &str = r"[A-Za-z_$][A-Za-z0-9_$]*";

fn re(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("ID", ID)).expect("valid gate pattern")
}

/// A rule: where to look (`anchor` with a window around it), what to match and
/// how to rewrite the `gate` capture.
struct Rule {
    kind: GateKind,
    anchor: &'static str,
    before: usize,
    after: usize,
    regex: &'static LazyLock<Regex>,
    /// Checks beyond the regex (same identifiers, declarations, context);
    /// `window` is the searched text, `at` the match start in it.
    accept: fn(&Captures<'_>, &str, usize) -> bool,
    /// The replacement for the `gate` capture.
    replace: fn(&Captures<'_>, &str, usize) -> Option<String>,
}

static UI_ACCESS: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"(?P<gate>(?P<p>ID)\?\.authMethod==="chatgpt"\|\|ID\?\.authMethod==="personalAccessToken"(?:\|\|ID\?\.authMethod==="[A-Za-z]\w*")*),"#,
        r#"(?P<m>ID)=(?P<p2>ID)\?\.authMethod\?\?null[;,][^{}]{0,300}?\{authMethod:(?P<m2>ID),hostId:(?P<h>ID)\}"#,
    ))
});

static REQUIREMENTS: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"async function(?: ID)?\((?P<s>ID),(?P<h>ID)\)\{let (?P<a>ID)=await [^;{}]{1,80};"#,
        r#"if\((?P<gate>"chatgpt"!==ID&&"personalAccessToken"!==ID(?:&&"[A-Za-z]\w*"!==ID)*)\)return null;"#,
        r#".{0,240}?\.query\.setData\(ID(?:\.ID)?,\{authMethod:(?P<a2>ID),hostId:(?P<h2>ID)\}"#,
    ))
});

static QUEUE_DEFAULT: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"if\("local"===(?P<mgr>ID)\.getHostId\(\)&&await (?P<rt>ID)\.isUsingCopilotInference\(\)\)return!1;"#,
        r#"let (?P<r>ID)=await (?P<mgr2>ID)\.getAccount\(\{priority:"critical"\}\);"#,
        r#"return (?P<gate>ID\.account\?\.type==="chatgpt"(?:\|\|ID\.account\?\.type==="[A-Za-z]\w*")*)\}\)\},async writeServiceTier\("#,
    ))
});

static QUEUE_EXPLICIT: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"if\(null==(?P<o>ID)\.serviceTier\)return null;let\{account:(?P<acc>ID)\}=await (?P<mgr>ID)\.getAccount\(\{priority:"critical"\}\);"#,
        r#"return (?P<gate>ID\?\.type==="chatgpt"(?:\|\|ID\?\.type==="[A-Za-z]\w*")*)\?(?P<o2>ID)\.serviceTier:null"#,
    ))
});

static PREDICTION_SETTINGS: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?P<gate>\(0,ID\.ID\)\("4290031608"\))"#));

static PREDICTION_LIVE: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?P<gate>\(0,ID\.ID\)\(ID,"4290031608"\))"#));

static PREDICTION_COMPOSER: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"\(\{get:(?P<g>ID)\}\)=>(?P<gate>(?P<g2>ID)\(ID\.ID,"4290031608"\))"#,
        r#"&&\(0,ID\.ID\)\((?P<g3>ID),\(0,ID\.ID\)\(\)\.enabled\)&&null!=ID\.appServices\.cumberbatch\)"#,
    ))
});

static PREDICTION_SERVICES: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"setAppServices:function\((?P<a>ID)\)\{(?P<gate>(?P<o>ID)=(?P<a2>ID))\}"#)
});

static PREDICTION_CLIENT: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"constructor\((?P<h>ID),(?P<d>ID)=null,(?P<u>ID)=!1,(?P<r>ID)=\(\)=>void 0,(?P<i>ID)\)\{"#,
        r#"(?P<gate>this\.hostId=(?P<h2>ID),this\.dispatchMessage=(?P<d2>ID),this\.useHostRequestScheduler=(?P<u2>ID),"#,
        r#"this\.getThreadAccess=(?P<r2>ID),this\.getRequestRejection=(?P<i2>ID))\}"#,
    ))
});

static IMPORT_CALL: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|[^A-Za-z0-9_$.])import\s*\("));

/// Every identifier `x` in `text` written as `x<suffix>` equals `want`.
fn all_same(text: &str, suffix: &str, want: &str) -> bool {
    let pattern = Regex::new(&format!(r"({ID}){}", regex::escape(suffix))).expect("valid");
    let mut any = false;
    for c in pattern.captures_iter(text) {
        any = true;
        if &c[1] != want {
            return false;
        }
    }
    any
}

/// Every identifier `x` in `text` written as `<prefix>x` equals `want`.
fn all_same_after(text: &str, prefix: &str, want: &str) -> bool {
    let pattern = Regex::new(&format!(r"{}({ID})", regex::escape(prefix))).expect("valid");
    let mut any = false;
    for c in pattern.captures_iter(text) {
        any = true;
        if &c[1] != want {
            return false;
        }
    }
    any
}

/// `name` is bound (declared or a parameter) shortly before `at`.
fn declared_before(window: &str, at: usize, name: &str) -> bool {
    let head = &window[at.saturating_sub(800)..at];
    let escaped = regex::escape(name);
    Regex::new(&format!(
        r"(?:\blet |\bconst |\bvar |[,(]){escaped}(?:=|,|\))"
    ))
    .expect("valid")
    .is_match(head)
}

fn distinct(names: &[&str]) -> bool {
    let set: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    set.len() == names.len()
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        Rule {
            kind: GateKind::UiAccess,
            anchor: "\"personalAccessToken\"",
            before: 400,
            after: 1200,
            regex: &UI_ACCESS,
            accept: |c, window, at| {
                all_same(&c["gate"], "?.authMethod", &c["p"])
                    && c["p"] == c["p2"]
                    && c["m"] == c["m2"]
                    && distinct(&[&c["p"], &c["m"], &c["h"]])
                    && declared_before(window, at, &c["h"])
                    && window[at..].contains("serviceTierAccess:")
            },
            replace: |c, _, _| {
                Some(format!(
                    "({}||{})",
                    &c["gate"],
                    allow(&c["h"], &format!("{}?.authMethod", &c["p"]))
                ))
            },
        },
        Rule {
            kind: GateKind::Requirements,
            anchor: "\"personalAccessToken\"!==",
            before: 300,
            after: 500,
            regex: &REQUIREMENTS,
            accept: |c, _, _| {
                all_same_after(&c["gate"], "!==", &c["a"])
                    && c["a"] == c["a2"]
                    && c["h"] == c["h2"]
                    && distinct(&[&c["s"], &c["h"], &c["a"]])
            },
            replace: |c, _, _| Some(format!("{}&&!({})", &c["gate"], allow(&c["h"], &c["a"]))),
        },
        Rule {
            kind: GateKind::QueueDefault,
            anchor: "},async writeServiceTier(",
            before: 2400,
            after: 30,
            regex: &QUEUE_DEFAULT,
            accept: |c, window, at| {
                all_same(&c["gate"], ".account?.type", &c["r"])
                    && c["mgr"] == c["mgr2"]
                    && distinct(&[&c["mgr"], &c["rt"], &c["r"]])
                    && window[..at].contains("async readServiceTier(")
            },
            replace: |c, _, _| {
                Some(format!(
                    "{}||{}",
                    &c["gate"],
                    allow(
                        &format!("{}.getHostId()", &c["mgr"]),
                        &format!("{}.account?.type", &c["r"])
                    )
                ))
            },
        },
        Rule {
            kind: GateKind::QueueExplicit,
            anchor: ".serviceTier:null",
            before: 3600,
            after: 20,
            regex: &QUEUE_EXPLICIT,
            accept: |c, window, at| {
                all_same(&c["gate"], "?.type", &c["acc"])
                    && c["o"] == c["o2"]
                    && explicit_context(c, window, at).is_some()
            },
            replace: |c, window, at| {
                let (host, runtime) = explicit_context(c, window, at)?;
                let (mgr, acc) = (&c["mgr"], &c["acc"]);
                Some(format!(
                    "({}||{host}===\"local\"&&{}&&typeof {runtime}.isUsingCopilotInference==\"function\"&&!(await {runtime}.isUsingCopilotInference()))",
                    &c["gate"],
                    allow(&format!("{mgr}.getHostId()"), &format!("{acc}?.type"))
                ))
            },
        },
        Rule {
            kind: GateKind::PredictionSettings,
            anchor: PREDICTION_GATE_ARG,
            before: 20,
            after: 1500,
            regex: &PREDICTION_SETTINGS,
            accept: |_, window, at| {
                let rest = &window[at..];
                rest.contains("\"cumberbatch-availability\"")
                    && rest.contains(".isEnabledByRemoteConfig()")
            },
            replace: |c, _, _| Some(format!("({}||{})", &c["gate"], force_gate())),
        },
        Rule {
            kind: GateKind::PredictionLive,
            anchor: PREDICTION_GATE_ARG,
            before: 900,
            after: 3000,
            regex: &PREDICTION_LIVE,
            accept: |_, window, at| {
                let (head, rest) = window.split_at(at);
                head.contains(".appServices?.cumberbatch")
                    && rest.contains(".requestUserMessagePrediction(")
                    && rest.contains("generationTrigger:\"live\"")
            },
            replace: |c, _, _| Some(format!("({}||{})", &c["gate"], force_gate())),
        },
        Rule {
            kind: GateKind::PredictionComposer,
            anchor: PREDICTION_GATE_ARG,
            before: 80,
            after: 160,
            regex: &PREDICTION_COMPOSER,
            accept: |c, _, _| c["g"] == c["g2"] && c["g"] == c["g3"],
            replace: |c, _, _| Some(format!("({}||{})", &c["gate"], force_gate())),
        },
        Rule {
            kind: GateKind::PredictionServices,
            anchor: SERVICES_ANCHOR,
            before: 200,
            after: 60,
            regex: &PREDICTION_SERVICES,
            accept: |c, window, at| {
                c["a"] == c["a2"]
                    && c["a"] != c["o"]
                    && window[..at].contains("appServices:()=>")
                    && window[..at].contains(&format!("appServices:()=>{}", &c["o"]))
            },
            replace: |c, _, _| {
                Some(format!(
                    "{o}=globalThis.__superbaSpeedMenu?.services?.({a})??{a}",
                    o = &c["o"],
                    a = &c["a"]
                ))
            },
        },
        Rule {
            kind: GateKind::PredictionClient,
            anchor: CLIENT_ANCHOR,
            before: 900,
            after: 40,
            regex: &PREDICTION_CLIENT,
            accept: |c, window, at| {
                ["h", "d", "u", "r", "i"]
                    .iter()
                    .all(|k| c[*k] == c[format!("{k}2").as_str()])
                    && distinct(&[&c["h"], &c["d"], &c["u"], &c["r"], &c["i"]])
                    && window[..at].contains("sendConfigReadRequestMemoized=")
            },
            replace: |c, _, _| {
                Some(format!(
                    "{},globalThis.__superbaSpeedMenu?.client?.(this)",
                    &c["gate"]
                ))
            },
        },
    ]
});

/// The execution host and runtime bindings of the explicit gate: `H =
/// options?.executionHostId ?? manager.getHostId()` and the `runtime` of the
/// enclosing queue function's options (`function(manager,{...runtime:R})`).
fn explicit_context(c: &Captures<'_>, window: &str, at: usize) -> Option<(String, String)> {
    let head = &window[..at];
    let (o, mgr, acc) = (&c["o"], &c["mgr"], &c["acc"]);
    let host_re = Regex::new(&format!(
        r"[,;{{(](?:let )?({ID})={}\?\.executionHostId\?\?{}\.getHostId\(\)",
        regex::escape(o),
        regex::escape(mgr)
    ))
    .ok()?;
    let host = host_re.captures_iter(head).last()?[1].to_owned();
    let header_re = Regex::new(&format!(
        r"function\({},\{{([^{{}}]{{0,200}})\}}",
        regex::escape(mgr)
    ))
    .ok()?;
    let fields = header_re.captures_iter(head).last()?[1].to_owned();
    let runtime = Regex::new(&format!(r"(?:^|,)runtime:({ID})(?:,|$)"))
        .ok()?
        .captures(&fields)?[1]
        .to_owned();
    if !head.contains(".submissionOptions") {
        return None;
    }
    distinct(&[o, mgr, acc, &host, &runtime]).then_some((host, runtime))
}

/// One gate found in a factory: byte range of the `gate` capture in the
/// factory source and its replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Hit {
    kind: GateKind,
    start: usize,
    end: usize,
    text: String,
}

fn char_floor(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Gates in one factory source.
fn factory_hits(source: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = Vec::new();
    for rule in RULES.iter() {
        for (pos, _) in source.match_indices(rule.anchor) {
            let from = char_floor(source, pos.saturating_sub(rule.before));
            let to = char_floor(
                source,
                (pos + rule.anchor.len() + rule.after).min(source.len()),
            );
            let window = &source[from..to];
            for c in rule.regex.captures_iter(window) {
                let whole = c.get(0).expect("match");
                if !(rule.accept)(&c, window, whole.start()) {
                    continue;
                }
                let Some(text) = (rule.replace)(&c, window, whole.start()) else {
                    continue;
                };
                let gate = c.name("gate").expect("gate capture");
                let hit = Hit {
                    kind: rule.kind,
                    start: from + gate.start(),
                    end: from + gate.end(),
                    text,
                };
                if !hits.contains(&hit) {
                    hits.push(hit);
                }
            }
        }
    }
    hits
}

/// Apply `hits` to `source` (they must not overlap).
fn apply(source: &str, hits: &[Hit]) -> Option<String> {
    let mut sorted: Vec<&Hit> = hits.iter().collect();
    sorted.sort_by_key(|h| h.start);
    let mut out = String::with_capacity(source.len() + 512);
    let mut at = 0;
    for h in sorted {
        if h.start < at || h.text.contains(['\n', '\r']) {
            return None;
        }
        out.push_str(&source[at..h.start]);
        out.push_str(&h.text);
        at = h.end;
    }
    out.push_str(&source[at..]);
    Some(out)
}

/// The plan for a set of chunk texts (path, text).
///
/// A module that ships in several chunks is minified separately in each (its
/// local names differ), so each distinct source is a variant with its own
/// patched copy. Every variant of a module must hold the same gates, and each
/// gate must be in exactly one module. The speed-menu gates go together (one
/// problem drops all four); each prediction check stands on its own. A module
/// holding gates of both sets is not used for the predictions. There is no
/// plan when nothing at all can be patched.
pub fn plan_from_chunks<'a>(
    chunks: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> std::result::Result<Plan, NoPlan> {
    let no = |reason: String| NoPlan {
        version: PLAN_VERSION,
        reason,
    };
    // id -> variants (key, original source, hits), distinct sources only.
    let mut found: BTreeMap<String, Vec<(String, String, Vec<Hit>)>> = BTreeMap::new();
    for (path, text) in chunks {
        if !CHUNK_ANCHORS.iter().any(|a| text.contains(a)) {
            continue;
        }
        let Some(factories) = chunk_factories(text) else {
            // A chunk that holds a gate but cannot be split is a plan failure;
            // others (shaped differently, no gate) do not matter.
            if RULES.iter().any(|r| text.contains(r.anchor)) && text.contains("__webpack_modules__")
            {
                return Err(no(format!("cannot split {path}")));
            }
            continue;
        };
        for f in factories {
            if !CHUNK_ANCHORS.iter().any(|a| f.source.contains(a)) {
                continue;
            }
            let variants = found.entry(f.id()).or_default();
            if variants.iter().any(|(_, original, _)| original == f.source) {
                continue;
            }
            let hits = factory_hits(f.source);
            variants.push((f.key.to_owned(), f.source.to_owned(), hits));
        }
    }
    // Modules without any gate in any copy are not ours.
    found.retain(|_, variants| variants.iter().any(|(_, _, hits)| !hits.is_empty()));
    // The first problem of each gate, and the modules holding it.
    let mut problems: BTreeMap<GateKind, String> = BTreeMap::new();
    let mut modules_of: BTreeMap<GateKind, Vec<String>> = BTreeMap::new();
    for (id, variants) in &found {
        let kinds = |hits: &[Hit]| {
            let mut k: Vec<GateKind> = hits.iter().map(|h| h.kind).collect();
            k.sort();
            k
        };
        let mut all: Vec<GateKind> = variants.iter().flat_map(|(_, _, h)| kinds(h)).collect();
        all.sort();
        all.dedup();
        let mut fail = |kinds: &[GateKind], why: String| {
            for k in kinds {
                problems.entry(*k).or_insert_with(|| why.clone());
            }
        };
        let first = kinds(&variants[0].2);
        if variants.iter().any(|(_, _, hits)| kinds(hits) != first) {
            fail(&all, format!("module {id}: copies differ in gates"));
            continue;
        }
        let uses_import = variants.iter().any(|(_, original, _)| {
            original.contains("import.meta") || IMPORT_CALL.is_match(original)
        });
        if uses_import {
            // The copies are compiled in a classic script at the document's
            // URL: module syntax or relative imports would break or resolve
            // elsewhere.
            fail(&all, format!("module {id} uses import"));
            continue;
        }
        let mixed = all.iter().any(|k| k.set() == PatchSet::SpeedMenu)
            && all.iter().any(|k| k.set() == PatchSet::Predictions);
        for kind in &all {
            let n = first.iter().filter(|k| *k == kind).count();
            if n != kind.hits_per_module() {
                fail(
                    &[*kind],
                    format!("module {id}: {} matches {n} times", kind.name()),
                );
            } else if mixed && kind.set() == PatchSet::Predictions {
                fail(
                    &[*kind],
                    format!("module {id}: {} shares it with the speed menu", kind.name()),
                );
            } else {
                modules_of.entry(*kind).or_default().push(id.clone());
            }
        }
    }
    for kind in GateKind::SPEED_MENU
        .into_iter()
        .chain(GateKind::PREDICTIONS)
    {
        let n = modules_of.get(&kind).map_or(0, Vec::len);
        if n != 1 && !problems.contains_key(&kind) {
            problems.insert(kind, format!("{}: {n} matches", kind.name()));
        }
    }
    for (a, b) in GateKind::PAIRED {
        for (x, y) in [(a, b), (b, a)] {
            if problems.contains_key(&x) && !problems.contains_key(&y) {
                problems.insert(y, format!("{}: needs {}", y.name(), x.name()));
            }
        }
    }
    let joined = |kinds: &[GateKind]| {
        let mut reasons: Vec<&str> = Vec::new();
        for k in kinds {
            if let Some(r) = problems.get(k)
                && !reasons.contains(&r.as_str())
            {
                reasons.push(r);
            }
        }
        reasons.join(", ")
    };
    // The gates that get patched.
    let mut patched: Vec<GateKind> = Vec::new();
    let mut skipped: BTreeMap<PatchSet, String> = BTreeMap::new();
    let speed = joined(&GateKind::SPEED_MENU);
    if speed.is_empty() {
        patched.extend(GateKind::SPEED_MENU);
    } else {
        skipped.insert(PatchSet::SpeedMenu, speed);
    }
    let predictions = joined(&GateKind::PREDICTIONS);
    patched.extend(
        GateKind::PREDICTIONS
            .into_iter()
            .filter(|k| !problems.contains_key(k)),
    );
    if !predictions.is_empty() {
        skipped.insert(PatchSet::Predictions, predictions);
    }
    let mut modules = Vec::new();
    for (id, variants) in found {
        for (key, original, hits) in variants {
            let hits: Vec<Hit> = hits
                .into_iter()
                .filter(|h| patched.contains(&h.kind))
                .collect();
            if hits.is_empty() {
                continue;
            }
            let source =
                apply(&original, &hits).ok_or_else(|| no(format!("overlapping gates in {id}")))?;
            let mut gates: Vec<GateKind> = hits.iter().map(|h| h.kind).collect();
            gates.sort();
            gates.dedup();
            modules.push(PatchedModule {
                key,
                id: id.clone(),
                original_len: original.encode_utf16().count(),
                original_fnv: fnv1a_utf16(&original),
                source,
                gates,
            });
        }
    }
    if modules.is_empty() {
        return Err(no(skipped.into_values().collect::<Vec<_>>().join(", ")));
    }
    Ok(Plan {
        version: PLAN_VERSION,
        modules,
        skipped,
    })
}

/// The plan for the renderer bundle in `asar` (blocking: reads every chunk
/// that may hold a gate, ~0.5-1 s for a 760 MB archive from the page cache).
pub fn plan_for_asar(asar: &Path) -> superba_core::Result<std::result::Result<Plan, NoPlan>> {
    let mut archive = Archive::open(asar)?;
    let entries: Vec<_> = archive
        .entries()
        .iter()
        .filter(|e| {
            e.path.starts_with("webview/assets/")
                && e.path.ends_with(".js")
                && e.size <= CHUNK_LIMIT
        })
        .cloned()
        .collect();
    let finders: Vec<memchr::memmem::Finder<'_>> = CHUNK_ANCHORS
        .iter()
        .map(|a| memchr::memmem::Finder::new(a.as_bytes()))
        .collect();
    let mut buf = Vec::new();
    let mut chunks: Vec<(String, String)> = Vec::new();
    for entry in entries {
        archive
            .read_into(&entry, &mut buf)
            .map_err(|e| superba_core::Error::io(asar, &e))?;
        if !finders.iter().any(|f| f.find(&buf).is_some()) {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(&buf) {
            chunks.push((entry.path.clone(), text.to_owned()));
        }
    }
    Ok(plan_from_chunks(
        chunks.iter().map(|(p, t)| (p.as_str(), t.as_str())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic chunks shaped like the 26.1106 renderer (identifiers renamed).
    const UI: &str = r#"aB12(e,t,a){var n=a("Q1");a.d(t,{},{U:function(e){let t,a,c,d=(0,n.c)(10),u=(0,s.Ul)(i.Dc),hh=e?.hostId??u,pp=(0,o.tn)(hh),g=pp?.authMethod==="chatgpt"||pp?.authMethod==="personalAccessToken"||pp?.authMethod==="amazonBedrock",m=pp?.authMethod??null;d[0]!==hh||d[1]!==m?(t={authMethod:m,hostId:hh},d[0]=hh,d[1]=m,d[2]=t):t=d[2];let{data:f,isPending:x}=(0,s._U)(l.b9,t),y=!!pp?.isLoading||g&&x;return{serviceTierAccess:g&&!y&&null!=f?f:null,isLoading:y}}})}"#;
    const REQ: &str = r#"cD34(e,t,n){n.d(t,{},{e:async function(e,t,n,u){try{let m=await async function(q,w){let z=await (0,l.s)(q,w);if("chatgpt"!==z&&"personalAccessToken"!==z&&"amazonBedrock"!==z)return null;let i=await (0,o.Wr)(q,w,{priority:"critical"});return q.query.setData(o.b9,{authMethod:z,hostId:w},i),(0,r.W6)(i)}(e,t);return m}catch(e){return null}}})}"#;
    const QUEUE: &str = r#"eF56(e,t,n){this.x=function(e,{runtime:t},n,r){let i=null;return{async readServiceTier(r){for(;null!=i;)await i.catch(()=>void 0);return n(),f(e.requestClient,e.logger,r,async()=>{if("local"===e.getHostId()&&await t.isUsingCopilotInference())return!1;let k=await e.getAccount({priority:"critical"});return k.account?.type==="chatgpt"||k.account?.type==="amazonBedrock"})},async writeServiceTier(t,a){return 1}}},this.y={prepare:function(e,{settings:t,storage:n,filesystem:r,runtime:i},a,s,o){return async(l,u)=>{let d=u.submissionOptions,c=u.context.workPermissions,h=d?.executionHostId??e.getHostId();let[E,I]=await Promise.all([(async()=>{if(null==d)return a(null);if(null==d.serviceTier)return null;let{account:z}=await e.getAccount({priority:"critical"});return z?.type==="chatgpt"||z?.type==="amazonBedrock"?d.serviceTier:null})(),1]);return E}}}}"#;

    /// The reasons of a bundle without any prediction check.
    const NO_PREDICTIONS: &str = "prediction_settings: 0 matches, prediction_live: 0 matches, \
         prediction_composer: 0 matches, prediction_services: 0 matches, prediction_client: 0 matches";

    fn chunk(modules: &[&str]) -> String {
        format!(
            "export const __rspack_esm_ids=[1];export const __webpack_modules__={{{}}};\n//# sourceMappingURL=1.js.map",
            modules.join(",")
        )
    }

    #[test]
    fn splits_chunk_factories() {
        let text = chunk(&[
            "aa(e,t){t.x=\"}\"}",
            "\"b-1\"(e){/}/.test(e)}",
            "12(e,t,n){}",
        ]);
        let f = chunk_factories(&text).unwrap();
        assert_eq!(
            f.iter().map(|f| (f.key, f.source)).collect::<Vec<_>>(),
            [
                ("aa", "aa(e,t){t.x=\"}\"}"),
                ("\"b-1\"", "\"b-1\"(e){/}/.test(e)}"),
                ("12", "12(e,t,n){}")
            ]
        );
        assert_eq!(f[1].id(), "b-1");
    }

    #[test]
    fn finds_and_patches_all_four_gates() {
        let one = chunk(&[UI, REQ]);
        let two = chunk(&[QUEUE, "zz(e){}"]);
        // The UI module ships in a second chunk too.
        let three = chunk(&[UI]);
        let plan = plan_from_chunks([
            ("a.js", one.as_str()),
            ("b.js", two.as_str()),
            ("c.js", three.as_str()),
        ])
        .unwrap();
        let by_id: BTreeMap<_, _> = plan.modules.iter().map(|m| (m.id.as_str(), m)).collect();
        assert_eq!(
            by_id.keys().copied().collect::<Vec<_>>(),
            ["aB12", "cD34", "eF56"]
        );
        assert_eq!(
            by_id["eF56"].gates,
            [GateKind::QueueDefault, GateKind::QueueExplicit]
        );
        let ui = &by_id["aB12"].source;
        assert!(ui.contains(r#"g=(pp?.authMethod==="chatgpt"||pp?.authMethod==="personalAccessToken"||pp?.authMethod==="amazonBedrock"||globalThis.__superbaSpeedMenu?.allow?.(hh,pp?.authMethod)===!0),m="#), "{ui}");
        let req = &by_id["cD34"].source;
        assert!(req.contains(r#"if("chatgpt"!==z&&"personalAccessToken"!==z&&"amazonBedrock"!==z&&!(globalThis.__superbaSpeedMenu?.allow?.(w,z)===!0))return null;"#), "{req}");
        let q = &by_id["eF56"].source;
        assert!(q.contains(r#"return k.account?.type==="chatgpt"||k.account?.type==="amazonBedrock"||globalThis.__superbaSpeedMenu?.allow?.(e.getHostId(),k.account?.type)===!0})},async writeServiceTier("#), "{q}");
        assert!(q.contains(r#"return (z?.type==="chatgpt"||z?.type==="amazonBedrock"||h==="local"&&globalThis.__superbaSpeedMenu?.allow?.(e.getHostId(),z?.type)===!0&&typeof i.isUsingCopilotInference=="function"&&!(await i.isUsingCopilotInference()))?d.serviceTier:null"#), "{q}");
        assert_eq!(by_id["aB12"].original_len, UI.encode_utf16().count());
        assert_eq!(by_id["aB12"].original_fnv, fnv1a_utf16(UI));
    }

    #[test]
    fn missing_or_duplicate_gates_give_no_plan() {
        let partial = chunk(&[UI, REQ]);
        let err = plan_from_chunks([("a.js", partial.as_str())]).unwrap_err();
        assert_eq!(
            err.reason,
            format!("queue_default: 0 matches, queue_explicit: 0 matches, {NO_PREDICTIONS}")
        );
        // The same gate in two different modules.
        let other_ui = UI.replacen("aB12", "zZ99", 1);
        let twice = chunk(&[UI, &other_ui, REQ, QUEUE]);
        let err = plan_from_chunks([("a.js", twice.as_str())]).unwrap_err();
        assert_eq!(
            err.reason,
            format!("ui_access: 2 matches, {NO_PREDICTIONS}")
        );
    }

    #[test]
    fn copies_minified_differently_are_variants() {
        let a = chunk(&[UI, REQ, QUEUE]);
        let renamed = UI.replace("hh", "kk");
        let b = chunk(&[&renamed]);
        let plan = plan_from_chunks([("a.js", a.as_str()), ("b.js", b.as_str())]).unwrap();
        let ui: Vec<_> = plan.modules.iter().filter(|m| m.id == "aB12").collect();
        assert_eq!(ui.len(), 2);
        assert!(
            ui[1].source.contains("allow?.(kk,pp?.authMethod)"),
            "{}",
            ui[1].source
        );
        assert_ne!(ui[0].original_fnv, ui[1].original_fnv);
        // A copy without the gate makes the module unusable.
        let broken = chunk(&[&UI.replace("\"personalAccessToken\"", "\"other\"")]);
        let err = plan_from_chunks([("a.js", a.as_str()), ("b.js", broken.as_str())]).unwrap_err();
        assert_eq!(
            err.reason,
            format!("module aB12: copies differ in gates, {NO_PREDICTIONS}")
        );
    }

    #[test]
    fn shadowed_or_undeclared_bindings_are_refused() {
        // The account binding shadows the execution host.
        let shadow = QUEUE.replace("let{account:z}", "let{account:h}").replace(
            "return z?.type===\"chatgpt\"||z?.type",
            "return h?.type===\"chatgpt\"||h?.type",
        );
        let text = chunk(&[UI, REQ, &shadow]);
        let err = plan_from_chunks([("a.js", text.as_str())]).unwrap_err();
        assert_eq!(
            err.reason,
            format!("queue_explicit: 0 matches, {NO_PREDICTIONS}")
        );
    }

    #[test]
    fn modules_using_import_are_refused() {
        let ui = UI.replace("var n=a(\"Q1\");", "var n=import(\"./x.js\");");
        let text = chunk(&[&ui, REQ, QUEUE]);
        let err = plan_from_chunks([("a.js", text.as_str())]).unwrap_err();
        assert_eq!(
            err.reason,
            format!("module aB12 uses import, {NO_PREDICTIONS}")
        );
    }

    // Excerpts of the three prediction checks in app 26.1106 (minified as
    // shipped, trimmed to the factory around them) and the cloud-task check
    // that stays as it is.
    const SETTINGS_1106: &str = r#"KM95(e,t,n){n.d(t,{},{$:function(){let e,t,a,c=(0,r.c)(17),u=(0,g.gh)(L.u);c[0]===Symbol.for("react.memo_cache_sentinel")?(e=(0,H.W)(),c[0]=e):e=c[0];let h=e.enabled,p=(0,F.iK)(h),m=(0,G.Zj)("4290031608"),f=Y.appServices.cumberbatch,y=(0,g.Ul)(q.chatGptAuthenticatedAccountId$),b=(0,g.Ul)(q.o8);c[1]!==y||c[2]!==b?(t=["cumberbatch-availability",y,b],c[1]=y,c[2]=b,c[3]=t):t=c[3],c[4]!==y||c[5]!==m||c[6]!==u||c[7]!==b?(a=m?async e=>{let{signal:t}=e;if(null!=f)return f.isEnabledByRemoteConfig();return!1}:V.hT,c[8]=a):a=c[8];return!m?null:a}})}"#;
    const LIVE_1106: &str = r#"J8Ro(e,t,n){n.d(t,{},{L:async function(e,t,n,a){let s=q.appServices?.cumberbatch;if(!(null==s||null!=n.automationKind||(0,i.ZJQ)(n.hostId))&&a()&&(0,e$.ie)(e,"4290031608"))try{let i={stack:[],error:void 0,hasError:!1};try{let o=(0,r.mS)(i,await t.getCachedConversationMetadata(n.conversationId),!1);if(!a()||!(0,e$.ie)(e,"4290031608")||o?.streamRole?.role!=="owner"||o.ephemeral||o.sideConversation&&!(0,e$.ie)(e,"1248574746")||(0,tf.N)(o.threadStartKind,o.threadSource)||e.get(tm.D0,n.conversationId)||o.latestTurnId!==n.turnId)return;let l={hostId:n.hostId,threadId:n.conversationId,latestTurnId:n.turnId},u=e.get(G.chatGptAuthenticatedAccountId$),d=e.get(G.o8),c={...l,generationTrigger:"live"},p=(0,r.mS)(i,new th.AX(t=>{a()&&(0,tg.wq)({scope:e,target:c,ineligibility:t})}),!1),h=e.get(t_.S8),f=await (0,ey.P)(s.requestUserMessagePrediction(c,p))}finally{}}catch(e){}}})}"#;
    const COMPOSER_1106: &str = r#"dlg1(e,t,n){var o=n("zTeN"),l=n("eEqo"),s=n("GaA7"),c=n("OWtl"),u=n("RxZZ"),m=n("OMIV");let p=(0,n("Nwp9").m3)("cumberbatch-has-accepted-prediction-1",!1),h=(0,o.un)(c.u,({get:e})=>e(m.JW,"4290031608")&&(0,u.CN)(e,(0,l.W)().enabled)&&null!=s.appServices.cumberbatch),f=(0,o.vP)(d.PR,()=>null);n.d(t,{},{k:h,p})}"#;
    // The module of `appServices` (two chunks ship it, minified differently)
    // and the renderer's app-server request client.
    const SERVICES_1106: &str = r#"GaA7(e,t,r){let n,i;r.r(t),r.d(t,{appHost:()=>n,appServices:()=>i},{setAppHost:function(e){n=e},setAppServices:function(e){i=e}})}"#;
    const SERVICES_1106_B: &str = r#"GaA7(e,p,s){let t,o;s.r(p),s.d(p,{appHost:()=>t,appServices:()=>o},{setAppHost:function(e){t=e},setAppServices:function(e){o=e}})}"#;
    const CLIENT_1106: &str = r#"XyZ1(e,t,n){var A=n("aa"),E=n("bb");class F{hostId;dispatchMessage;useHostRequestScheduler;getThreadAccess;getRequestRejection;requestClientId=(0,A.A)();appServerVersion=null;requestLifecycleListeners=new Set;requestPromises=new Map;sendConfigReadRequestMemoized=E()((e,t,n)=>this.enqueueRequest("config/read",t,n,void 0,e).finally(()=>{this.pendingConfigReadRequests.delete(e)}),{promise:!0,normalizer:([e])=>e});constructor(e,t=null,n=!1,r=()=>void 0,i){this.hostId=e,this.dispatchMessage=t,this.useHostRequestScheduler=n,this.getThreadAccess=r,this.getRequestRejection=i}setAppServerVersion(e){this.appServerVersion=e}}n.d(t,{},{F:()=>F})}"#;
    const CLOUD_1106: &str = r#"hXZ5(e,t,o){var g=o("OMIV");let y=(0,r.un)(h.u,({get:e})=>e(g.JW,"4290031608")&&(0,f.CN)(e,(0,u.W)().enabled)),b=1;o.d(t,{x:()=>y})}"#;

    /// The 26.1103 copies: the same code with other local names.
    fn v1103() -> [String; 3] {
        [
            SETTINGS_1106
                .replace("KM95", "pqJp")
                .replace("m=(0,G.Zj)", "f2=(0,G.Zj)")
                .replace("c[5]!==m", "c[5]!==f2")
                .replace("a=m?", "a=f2?")
                .replace("!m?", "!f2?"),
            LIVE_1106
                .replace("(0,e$.ie)", "(0,ej.ie)")
                .replace("q.appServices", "H.appServices")
                .replace("e.get(G.", "e.get(L."),
            COMPOSER_1106.to_owned(),
        ]
    }

    const FORCED: &str = r#"||globalThis.__superbaSpeedMenu?.gate?.("4290031608")===!0)"#;

    fn by_id(plan: &Plan) -> BTreeMap<&str, &PatchedModule> {
        plan.modules.iter().map(|m| (m.id.as_str(), m)).collect()
    }

    #[test]
    fn forces_the_prediction_gate_at_its_three_checks() {
        let a = chunk(&[UI, REQ, SETTINGS_1106, SERVICES_1106]);
        let b = chunk(&[QUEUE, LIVE_1106, CLOUD_1106, CLIENT_1106]);
        let c = chunk(&[COMPOSER_1106, SERVICES_1106_B]);
        let plan = plan_from_chunks([
            ("a.js", a.as_str()),
            ("b.js", b.as_str()),
            ("c.js", c.as_str()),
        ])
        .unwrap();
        assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
        let m = by_id(&plan);
        assert_eq!(
            m.keys().copied().collect::<Vec<_>>(),
            [
                "GaA7", "J8Ro", "KM95", "XyZ1", "aB12", "cD34", "dlg1", "eF56"
            ],
            "the cloud check (hXZ5) is not patched"
        );
        assert_eq!(m["KM95"].gates, [GateKind::PredictionSettings]);
        // Both copies of the services module are patched.
        let services: Vec<&str> = plan
            .modules
            .iter()
            .filter(|m| m.id == "GaA7")
            .map(|m| m.source.as_str())
            .collect();
        assert_eq!(services.len(), 2);
        assert!(
            services.iter().any(|s| s.contains(
                "setAppServices:function(e){i=globalThis.__superbaSpeedMenu?.services?.(e)??e}"
            )),
            "{services:?}"
        );
        assert!(
            services.iter().any(|s| s.contains(
                "setAppServices:function(e){o=globalThis.__superbaSpeedMenu?.services?.(e)??e}"
            )),
            "{services:?}"
        );
        assert!(m["XyZ1"].source.contains(
            "this.getRequestRejection=i,globalThis.__superbaSpeedMenu?.client?.(this)}setAppServerVersion("
        ), "{}", m["XyZ1"].source);
        assert!(
            m["KM95"]
                .source
                .contains(&format!("m=((0,G.Zj)(\"4290031608\"){FORCED},f=")),
            "{}",
            m["KM95"].source
        );
        let live = &m["J8Ro"].source;
        assert_eq!(m["J8Ro"].gates, [GateKind::PredictionLive]);
        assert!(
            live.contains(&format!(
                "&&a()&&((0,e$.ie)(e,\"4290031608\"){FORCED})try{{"
            )),
            "{live}"
        );
        assert!(
            live.contains(&format!(
                "if(!a()||!((0,e$.ie)(e,\"4290031608\"){FORCED}||o?."
            )),
            "{live}"
        );
        // Other gates in the same function stay.
        assert!(live.contains("!(0,e$.ie)(e,\"1248574746\")||"), "{live}");
        assert!(
            m["dlg1"].source.contains(&format!(
                "({{get:e}})=>(e(m.JW,\"4290031608\"){FORCED}&&(0,u.CN)(e,(0,l.W)().enabled)&&null!=s.appServices.cumberbatch)"
            )),
            "{}",
            m["dlg1"].source
        );
        // A launch gets only the sets it wants.
        let speed = plan
            .select(PatchSets {
                speed_menu: true,
                predictions: false,
            })
            .unwrap();
        assert_eq!(
            by_id(&speed).keys().copied().collect::<Vec<_>>(),
            ["aB12", "cD34", "eF56"]
        );
        let predictions = plan
            .select(PatchSets {
                speed_menu: false,
                predictions: true,
            })
            .unwrap();
        assert_eq!(
            by_id(&predictions).keys().copied().collect::<Vec<_>>(),
            ["GaA7", "J8Ro", "KM95", "XyZ1", "dlg1"]
        );
        assert_eq!(speed.settle_ids(), ["aB12", "cD34", "eF56"]);
        assert_eq!(predictions.settle_ids(), ["J8Ro"]);
        assert_eq!(
            plan.select(PatchSets::default()).unwrap_err().reason,
            "nothing to patch"
        );
    }

    #[test]
    fn the_1103_copies_match_too() {
        let [settings, live, composer] = v1103();
        let text = chunk(&[&settings, &live, &composer, SERVICES_1106, CLIENT_1106]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        let m = by_id(&plan);
        assert_eq!(
            m.keys().copied().collect::<Vec<_>>(),
            ["GaA7", "J8Ro", "XyZ1", "dlg1", "pqJp"]
        );
        assert!(
            m["pqJp"]
                .source
                .contains(&format!("f2=((0,G.Zj)(\"4290031608\"){FORCED},f=")),
            "{}",
            m["pqJp"].source
        );
        assert_eq!(m["J8Ro"].source.matches(FORCED).count(), 2);
        // Without the speed-menu gates that set is skipped, not the plan.
        assert_eq!(
            plan.skipped[&PatchSet::SpeedMenu],
            "ui_access: 0 matches, requirements: 0 matches, queue_default: 0 matches, queue_explicit: 0 matches"
        );
        assert!(!plan.skipped.contains_key(&PatchSet::Predictions));
        let err = plan
            .select(PatchSets {
                speed_menu: true,
                predictions: false,
            })
            .unwrap_err();
        assert!(
            err.reason.starts_with("ui_access: 0 matches"),
            "{}",
            err.reason
        );
    }

    #[test]
    fn each_prediction_check_stands_on_its_own() {
        // The live check is gone: the other two are still patched.
        let text = chunk(&[
            UI,
            REQ,
            QUEUE,
            SETTINGS_1106,
            COMPOSER_1106,
            SERVICES_1106,
            CLIENT_1106,
        ]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        let m = by_id(&plan);
        assert!(m.contains_key("KM95") && m.contains_key("dlg1"));
        assert_eq!(
            plan.skipped[&PatchSet::Predictions],
            "prediction_live: 0 matches"
        );
        // The live function with one of its two checks changed: not patched.
        let once = LIVE_1106.replacen("&&a()&&(0,e$.ie)(e,\"4290031608\")", "&&a()", 1);
        let text = chunk(&[SETTINGS_1106, &once]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        assert!(!by_id(&plan).contains_key("J8Ro"));
        assert_eq!(
            plan.skipped[&PatchSet::Predictions],
            "module J8Ro: prediction_live matches 1 times, prediction_composer: 0 matches, \
             prediction_services: 0 matches, prediction_client: 0 matches"
        );
        // A settings check without its context is not ours.
        let other = SETTINGS_1106.replace("\"cumberbatch-availability\"", "\"other\"");
        let text = chunk(&[&other, COMPOSER_1106]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        assert!(!by_id(&plan).contains_key("KM95"));
        // The composer check with another getter: refused.
        let getter = COMPOSER_1106.replace("=>e(m.JW", "=>x(m.JW");
        let text = chunk(&[SETTINGS_1106, &getter]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        assert!(!by_id(&plan).contains_key("dlg1"));
    }

    #[test]
    fn the_service_and_the_client_go_together() {
        // Without the client the service swap is left out too, and the other
        // way round.
        let text = chunk(&[SETTINGS_1106, SERVICES_1106]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        assert!(!by_id(&plan).contains_key("GaA7"));
        assert!(
            plan.skipped[&PatchSet::Predictions].ends_with(
                "prediction_services: needs prediction_client, prediction_client: 0 matches"
            ),
            "{:?}",
            plan.skipped
        );
        let text = chunk(&[SETTINGS_1106, CLIENT_1106]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        assert!(!by_id(&plan).contains_key("XyZ1"));
        assert!(
            plan.skipped[&PatchSet::Predictions].ends_with(
                "prediction_services: 0 matches, prediction_client: needs prediction_services"
            ),
            "{:?}",
            plan.skipped
        );
        // A constructor storing other fields is not the client.
        let other = CLIENT_1106.replace(
            "this.useHostRequestScheduler=n",
            "this.useHostRequestScheduler=r",
        );
        let text = chunk(&[SETTINGS_1106, SERVICES_1106, &other]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        assert!(!by_id(&plan).contains_key("XyZ1") && !by_id(&plan).contains_key("GaA7"));
    }

    #[test]
    fn a_prediction_check_in_two_modules_or_a_speed_module_is_skipped() {
        let twice = COMPOSER_1106.replacen("dlg1", "zz99", 1);
        let text = chunk(&[SETTINGS_1106, COMPOSER_1106, &twice]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        let m = by_id(&plan);
        assert!(!m.contains_key("dlg1") && !m.contains_key("zz99"));
        assert!(m.contains_key("KM95"));
        assert!(
            plan.skipped[&PatchSet::Predictions].contains("prediction_composer: 2 matches"),
            "{:?}",
            plan.skipped
        );
        // The settings check inside the speed menu's module: the speed menu
        // keeps it, the prediction check is left out.
        let ui = UI.replacen(
            "return{serviceTierAccess:",
            "let m=(0,G.Zj)(\"4290031608\"),f=Y.appServices.cumberbatch,t2=[\"cumberbatch-availability\"],a2=async()=>f.isEnabledByRemoteConfig();return{serviceTierAccess:",
            1,
        );
        let text = chunk(&[&ui, REQ, QUEUE]);
        let plan = plan_from_chunks([("a.js", text.as_str())]).unwrap();
        assert_eq!(by_id(&plan)["aB12"].gates, [GateKind::UiAccess]);
        assert!(!by_id(&plan)["aB12"].source.contains(FORCED));
        assert!(
            plan.skipped[&PatchSet::Predictions]
                .starts_with("module aB12: prediction_settings shares it with the speed menu"),
            "{:?}",
            plan.skipped
        );
    }

    #[test]
    fn fnv_matches_known_values() {
        assert_eq!(fnv1a_utf16(""), 0x811c_9dc5);
        assert_eq!(fnv1a_utf16("a"), 0xe40c_292c);
    }
}
