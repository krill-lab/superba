//! Find the four speed-tier gates in the app's renderer bundle and build
//! patched copies of the module factories that hold them.
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
//! gates stay as they are. A rule must match exactly once across the bundle,
//! otherwise there is no plan and the app runs unchanged.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};

use super::asar::Archive;
use super::jslex;

/// Version of the plan format and rules; a cached plan of another version is
/// computed again.
pub const PLAN_VERSION: u32 = 1;

/// Chunks larger than this are not considered (the largest app chunk is ~9 MB).
const CHUNK_LIMIT: u64 = 48 * 1024 * 1024;

/// Literal text every chunk holding a gate contains; chunks without any of
/// them are not lexed.
const CHUNK_ANCHORS: &[&str] = &[
    "serviceTierAccess",
    "personalAccessToken",
    "writeServiceTier",
    "submissionOptions",
];

/// The page policy call: `true` only for the local host signed in with an API
/// key (see `hook.js`).
fn allow(host: &str, method: &str) -> String {
    format!("globalThis.__superbaSpeedMenu?.allow?.({host},{method})===!0")
}

/// One of the four gates.
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
}

impl GateKind {
    /// All gates.
    pub const ALL: [GateKind; 4] = [
        GateKind::UiAccess,
        GateKind::Requirements,
        GateKind::QueueDefault,
        GateKind::QueueExplicit,
    ];
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
    /// The patched modules.
    pub modules: Vec<PatchedModule>,
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
/// gate must be in exactly one module.
pub fn plan_from_chunks<'a>(
    chunks: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> std::result::Result<Plan, NoPlan> {
    let no = |reason: String| NoPlan {
        version: PLAN_VERSION,
        reason,
    };
    let kind_name = |kind: GateKind| {
        serde_json::to_value(kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
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
    let mut modules_of: BTreeMap<GateKind, Vec<String>> = BTreeMap::new();
    for (id, variants) in &found {
        let kinds = |hits: &[Hit]| {
            let mut k: Vec<GateKind> = hits.iter().map(|h| h.kind).collect();
            k.sort();
            k
        };
        let first = kinds(&variants[0].2);
        if variants.iter().any(|(_, _, hits)| kinds(hits) != first) {
            return Err(no(format!("module {id}: copies differ in gates")));
        }
        let mut unique = first.clone();
        unique.dedup();
        if unique.len() != first.len() {
            return Err(no(format!("module {id}: a gate matches twice")));
        }
        for kind in first {
            modules_of.entry(kind).or_default().push(id.clone());
        }
    }
    let problems: Vec<String> = GateKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let n = modules_of.get(&kind).map_or(0, Vec::len);
            (n != 1).then(|| format!("{}: {n} matches", kind_name(kind)))
        })
        .collect();
    if !problems.is_empty() {
        return Err(no(problems.join(", ")));
    }
    let mut modules = Vec::new();
    for (id, variants) in found {
        for (key, original, hits) in variants {
            // The copies are compiled in a classic script at the document's
            // URL: module syntax or relative imports would break or resolve
            // elsewhere.
            if original.contains("import.meta") || IMPORT_CALL.is_match(&original) {
                return Err(no(format!("module {id} uses import")));
            }
            let source =
                apply(&original, &hits).ok_or_else(|| no(format!("overlapping gates in {id}")))?;
            let mut gates: Vec<GateKind> = hits.iter().map(|h| h.kind).collect();
            gates.sort();
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
    Ok(Plan {
        version: PLAN_VERSION,
        modules,
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
            "queue_default: 0 matches, queue_explicit: 0 matches"
        );
        // The same gate in two different modules.
        let other_ui = UI.replacen("aB12", "zZ99", 1);
        let twice = chunk(&[UI, &other_ui, REQ, QUEUE]);
        let err = plan_from_chunks([("a.js", twice.as_str())]).unwrap_err();
        assert_eq!(err.reason, "ui_access: 2 matches");
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
        assert_eq!(err.reason, "module aB12: copies differ in gates");
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
        assert_eq!(err.reason, "queue_explicit: 0 matches");
    }

    #[test]
    fn modules_using_import_are_refused() {
        let ui = UI.replace("var n=a(\"Q1\");", "var n=import(\"./x.js\");");
        let text = chunk(&[&ui, REQ, QUEUE]);
        let err = plan_from_chunks([("a.js", text.as_str())]).unwrap_err();
        assert_eq!(err.reason, "module aB12 uses import");
    }

    #[test]
    fn fnv_matches_known_values() {
        assert_eq!(fnv1a_utf16(""), 0x811c_9dc5);
        assert_eq!(fnv1a_utf16("a"), 0xe40c_292c);
    }
}
