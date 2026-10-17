//! View model of the catalog-driven Codex settings page (UX §3.6). Pure data and
//! logic (rail filters, Chinese-first titles, search ranking, pending edits, the
//! change list handed to the switch flow), filled by
//! [`super::source`] from the settings catalog and the observed configuration, so the
//! drawing code and the tests never need a real Codex home.

use std::collections::BTreeMap;
use std::fmt;

pub use superba_core::settings::ReservedOwner;

/// Lifecycle stage of a catalog entry. Config keys that are not features are
/// [`Stage::Stable`] unless upstream marks them deprecated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// Generally available.
    Stable,
    /// Offered in Codex's experimental menu.
    Experimental,
    /// Not finished upstream; hidden unless Advanced is on.
    UnderDevelopment,
    /// Still accepted, scheduled to go away.
    Deprecated,
    /// No longer has any effect.
    Removed,
}

impl Stage {
    /// Shown without the Advanced switch (decision 4: stable + experimental).
    pub fn in_default_view(self) -> bool {
        matches!(self, Stage::Stable | Stage::Experimental)
    }

    /// i18n key of the badge.
    pub fn label_key(self) -> &'static str {
        match self {
            Stage::Stable => "codex_settings.stage.stable",
            Stage::Experimental => "codex_settings.stage.experimental",
            Stage::UnderDevelopment => "codex_settings.stage.under_development",
            Stage::Deprecated => "codex_settings.stage.deprecated",
            Stage::Removed => "codex_settings.stage.removed",
        }
    }
}

/// A setting value as written to `config.toml`. Composite values (tables, arrays)
/// are carried as their TOML text (normalized by [`superba_core::settings::SettingValue`]);
/// lists of scalars and maps of scalars are edited row by row ([`Kind::List`],
/// [`Kind::Map`]), other composites are read-only on this page.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `true` / `false`.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
    /// String (also enum values and paths).
    Str(String),
    /// Anything else, rendered as TOML.
    Other(String),
}

impl Value {
    /// The bool, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    /// TOML-style rendering: strings quoted, everything else verbatim.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Other(s) => f.write_str(s),
        }
    }
}

/// One allowed value of an enum setting.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumOption {
    /// The value written to the file.
    pub value: String,
    /// Official English description of the value, if upstream has one.
    pub en: Option<String>,
    /// Human Chinese description, if a contributor wrote one.
    pub zh: Option<String>,
}

/// How a setting is edited.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// On / off switch.
    Bool,
    /// One of a fixed set of strings. `table_form` means upstream also accepts a
    /// table (only the string forms are offered here).
    Enum {
        /// Allowed values.
        options: Vec<EnumOption>,
        /// A table form exists too.
        table_form: bool,
    },
    /// Integer with optional bounds.
    Integer {
        /// Minimum.
        min: Option<i64>,
        /// Maximum.
        max: Option<i64>,
    },
    /// Floating point number with optional bounds.
    Number {
        /// Minimum.
        min: Option<f64>,
        /// Maximum.
        max: Option<f64>,
    },
    /// Free text; `path` asks for a file system path.
    Text {
        /// The value is a path.
        path: bool,
    },
    /// An array of scalars (`notify`, `sandbox_workspace_write.writable_roots`),
    /// edited one row per item.
    List {
        /// Item type.
        item: Item,
    },
    /// A table with free-form keys and scalar values
    /// (`shell_environment_policy.set`), edited as key-value rows.
    Map {
        /// Value type.
        value: Item,
    },
    /// Tables of fixed shape, arrays of tables and unions: shown, never edited here.
    ReadOnly,
}

/// The scalar type of a [`Kind::List`] item or a [`Kind::Map`] value.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// String; `path` asks for a file system path.
    Text {
        /// The item is a path.
        path: bool,
    },
    /// Integer.
    Integer,
    /// Number.
    Number,
    /// `true` / `false`.
    Bool,
    /// One of these strings.
    Enum(Vec<String>),
}

impl Item {
    /// Parse one item as typed (strings exactly as typed; numbers and booleans
    /// trimmed).
    pub fn parse(&self, input: &str) -> Result<superba_core::settings::SettingValue, ParseError> {
        use superba_core::settings::SettingValue;
        let t = input.trim();
        match self {
            Item::Text { .. } => Ok(SettingValue::String(input.to_owned())),
            Item::Integer => t
                .parse()
                .map(SettingValue::Integer)
                .map_err(|_| ParseError::NotInteger),
            Item::Number => t
                .parse::<f64>()
                .ok()
                .and_then(SettingValue::float)
                .ok_or(ParseError::NotNumber),
            Item::Bool => match t {
                "true" => Ok(SettingValue::Bool(true)),
                "false" => Ok(SettingValue::Bool(false)),
                _ => Err(ParseError::NotBool),
            },
            Item::Enum(options) => options
                .iter()
                .find(|o| o.as_str() == t)
                .map(|o| SettingValue::String(o.clone()))
                .ok_or(ParseError::NotAnOption),
        }
    }

    /// The item is a path (file picker, mono).
    pub fn is_path(&self) -> bool {
        matches!(self, Item::Text { path: true })
    }
}

/// The text of one list item or map value in its editor row.
pub fn item_text(v: &superba_core::settings::SettingValue) -> String {
    match v {
        superba_core::settings::SettingValue::String(s) => s.clone(),
        other => other.to_toml_repr(),
    }
}

/// The rows of a list value (`None` when `v` is not an array of scalars).
pub fn list_items(v: &Value) -> Option<Vec<String>> {
    use superba_core::settings::SettingValue;
    let Value::Other(repr) = v else {
        return None;
    };
    match SettingValue::from_toml_repr(repr)? {
        SettingValue::Array(items) => items
            .iter()
            .map(|i| match i {
                SettingValue::Array(_) | SettingValue::Table(_) => None,
                other => Some(item_text(other)),
            })
            .collect(),
        _ => None,
    }
}

/// The list value of editor rows: one item per non-empty row, in order, typed by
/// `item`, written as a TOML array (`[]` when no row is left).
pub fn list_value(rows: &[String], item: &Item) -> Result<Value, ParseError> {
    use superba_core::settings::SettingValue;
    let items = rows
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| item.parse(r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Other(SettingValue::Array(items).to_toml_repr()))
}

/// The rows of a map value, in key order (`None` when `v` is not a table of
/// scalars).
pub fn map_rows(v: &Value) -> Option<Vec<(String, String)>> {
    use superba_core::settings::SettingValue;
    let Value::Other(repr) = v else {
        return None;
    };
    match SettingValue::from_toml_repr(repr)? {
        SettingValue::Table(map) => map
            .iter()
            .map(|(k, v)| match v {
                SettingValue::Array(_) | SettingValue::Table(_) => None,
                other => Some((k.clone(), item_text(other))),
            })
            .collect(),
        _ => None,
    }
}

/// The map value of key-value rows (rows with neither key nor value are
/// skipped), written as an inline TOML table. Keys are taken as typed; an empty
/// key with a value, or a key given twice, is refused.
pub fn map_value(rows: &[(String, String)], value: &Item) -> Result<Value, ParseError> {
    use superba_core::settings::SettingValue;
    let mut out = BTreeMap::new();
    for (k, v) in rows {
        if k.is_empty() && v.is_empty() {
            continue;
        }
        if k.trim().is_empty() {
            return Err(ParseError::EmptyKey);
        }
        if out.insert(k.clone(), value.parse(v)?).is_some() {
            return Err(ParseError::DuplicateKey);
        }
    }
    Ok(Value::Other(SettingValue::Table(out).to_toml_repr()))
}

/// Move row `index` by `delta` (-1 up, +1 down); false when it cannot move.
pub fn move_row<T>(rows: &mut [T], index: usize, delta: isize) -> bool {
    let Some(to) = index.checked_add_signed(delta) else {
        return false;
    };
    if index >= rows.len() || to >= rows.len() {
        return false;
    }
    rows.swap(index, to);
    true
}

impl Kind {
    /// True when the page offers an editor for this kind.
    pub fn editable(&self) -> bool {
        !matches!(self, Kind::ReadOnly)
    }

    /// Parse user input for this kind (text, integer and number editors).
    pub fn parse(&self, input: &str) -> Result<Value, ParseError> {
        let input = input.trim();
        match self {
            Kind::Bool => match input {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(ParseError::NotBool),
            },
            Kind::Enum { options, .. } => options
                .iter()
                .find(|o| o.value == input)
                .map(|o| Value::Str(o.value.clone()))
                .ok_or(ParseError::NotAnOption),
            Kind::Integer { min, max } => {
                let n: i64 = input.parse().map_err(|_| ParseError::NotInteger)?;
                check_range(n, *min, *max)?;
                Ok(Value::Int(n))
            }
            Kind::Number { min, max } => {
                let n: f64 = input.parse().map_err(|_| ParseError::NotNumber)?;
                if !n.is_finite() {
                    return Err(ParseError::NotNumber);
                }
                check_range(n, *min, *max)?;
                Ok(Value::Float(n))
            }
            Kind::Text { .. } => {
                if input.is_empty() {
                    Err(ParseError::Empty)
                } else {
                    Ok(Value::Str(input.to_owned()))
                }
            }
            Kind::List { item } => {
                match superba_core::settings::SettingValue::from_toml_repr(input) {
                    Some(superba_core::settings::SettingValue::Array(items)) => {
                        let rows: Vec<String> = items.iter().map(item_text).collect();
                        list_value(&rows, item)
                    }
                    _ => Err(ParseError::NotList),
                }
            }
            Kind::Map { value } => {
                match superba_core::settings::SettingValue::from_toml_repr(input) {
                    Some(superba_core::settings::SettingValue::Table(map)) => {
                        let rows: Vec<(String, String)> =
                            map.iter().map(|(k, v)| (k.clone(), item_text(v))).collect();
                        map_value(&rows, value)
                    }
                    _ => Err(ParseError::NotTable),
                }
            }
            Kind::ReadOnly => Err(ParseError::ReadOnly),
        }
    }
}

fn check_range<T: PartialOrd>(n: T, min: Option<T>, max: Option<T>) -> Result<(), ParseError> {
    if min.is_some_and(|m| n < m) {
        return Err(ParseError::BelowMinimum);
    }
    if max.is_some_and(|m| n > m) {
        return Err(ParseError::AboveMaximum);
    }
    Ok(())
}

/// Why an edited value was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Not `true` / `false`.
    NotBool,
    /// Not one of the enum values.
    NotAnOption,
    /// Not an integer.
    NotInteger,
    /// Not a finite number.
    NotNumber,
    /// Below the minimum.
    BelowMinimum,
    /// Above the maximum.
    AboveMaximum,
    /// Empty text.
    Empty,
    /// Not a TOML array.
    NotList,
    /// Not a TOML table.
    NotTable,
    /// A map row has a value but no key.
    EmptyKey,
    /// A map key is given twice.
    DuplicateKey,
    /// Not editable here.
    ReadOnly,
}

impl ParseError {
    /// i18n key of the message.
    pub fn key(self) -> &'static str {
        match self {
            ParseError::NotBool => "codex_settings.invalid.bool",
            ParseError::NotAnOption => "codex_settings.invalid.option",
            ParseError::NotInteger => "codex_settings.invalid.integer",
            ParseError::NotNumber => "codex_settings.invalid.number",
            ParseError::BelowMinimum => "codex_settings.invalid.minimum",
            ParseError::AboveMaximum => "codex_settings.invalid.maximum",
            ParseError::Empty => "codex_settings.invalid.empty",
            ParseError::NotList => "codex_settings.invalid.list",
            ParseError::NotTable => "codex_settings.invalid.table",
            ParseError::EmptyKey => "codex_settings.invalid.empty_key",
            ParseError::DuplicateKey => "codex_settings.invalid.duplicate_key",
            ParseError::ReadOnly => "codex_settings.invalid.read_only",
        }
    }
}

/// Explanation texts. English is official upstream text only; Chinese is written by
/// people (decision 2). Superba never invents either.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Explanation {
    /// Official English text (schema description or experimental menu text).
    pub en: Option<String>,
    /// English note written by a contributor where upstream has no text (shown
    /// labelled as such, never as official text).
    pub en_note: Option<String>,
    /// Human Chinese text from the overlay.
    pub zh: Option<String>,
    /// The Chinese text was written for an older English text.
    pub zh_stale: bool,
}

/// A Codex install whose version decides which keys exist.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TargetKey {
    /// The Codex bundled with the desktop app.
    App,
    /// The Codex CLI first on PATH.
    Cli,
    /// A remote host (target id).
    Remote(String),
}

/// A target shown as an availability chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetInfo {
    /// Key.
    pub key: TargetKey,
    /// Display label ("App", "CLI", host alias).
    pub label: String,
    /// `codex --version`, when known.
    pub version: Option<String>,
    /// The catalog has no exact entry for this version; the nearest older one is used.
    pub approximate: bool,
    /// This Codex is newer than the catalog.
    pub catalog_outdated: bool,
}

/// Whether an entry exists for a target's Codex version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The key exists in this version.
    Yes,
    /// The key does not exist in this version (writing it has no effect; with
    /// `--strict-config` it is an error).
    No,
    /// The target's version is unknown.
    Unknown,
}

/// A legacy key and what replaced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Legacy {
    /// Canonical config path that replaces this key, when there is one.
    pub canonical: Option<String>,
    /// Value to write at the canonical path (semantic migrations such as
    /// `web_search_request = true` -> `web_search = "live"`); `None` = same value.
    pub canonical_value: Option<Value>,
    /// The key is ignored by current Codex versions.
    pub ignored: bool,
}

/// One setting row.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Stable id: `feature:<key>` or `config:<path>`.
    pub id: String,
    /// Config path (`features.fast_mode`, `history.persistence`).
    pub path: String,
    /// Official display name (experimental features), else `None` (the path is shown).
    pub name: Option<String>,
    /// Chinese display name written by a contributor.
    pub name_zh: Option<String>,
    /// Category id.
    pub category: String,
    /// Stage.
    pub stage: Stage,
    /// Editor kind.
    pub kind: Kind,
    /// Explanation.
    pub text: Explanation,
    /// Codex default (per OS for features), when known.
    pub default: Option<Value>,
    /// Value the edited profile wants (its managed entry), else the value in the
    /// local `config.toml`; `None` when unset.
    pub current: Option<Value>,
    /// Value in the local `config.toml` right now.
    pub file_value: Option<Value>,
    /// Effective value reported by `codex features list` (features only).
    pub effective: Option<bool>,
    /// First Codex version that knows the key.
    pub since: Option<String>,
    /// Availability per target, in target order.
    pub availability: Vec<(TargetKey, Availability)>,
    /// Set when this is a legacy key.
    pub legacy: Option<Legacy>,
    /// Superba recommends a value for Krill (shown as a hint, also outside Advanced).
    pub recommended: Option<Value>,
    /// Upstream description mentions an operating system (`windows.*`, ...).
    pub platform: Option<String>,
    /// The edited profile manages this key (Superba writes it on every switch).
    pub managed: bool,
    /// Shown only with Advanced (large families such as key bindings).
    pub advanced_only: bool,
    /// Owned by the connection, the Models page / speed control or Superba
    /// itself: read-only here, with a link to where it is edited.
    pub owner: Option<ReservedOwner>,
    /// Position in the curated "Common" list (`None` = not common).
    pub common: Option<usize>,
    /// Availability per remote host the app is connected to (what a settings
    /// sync to that host would skip), in host order.
    pub host_support: Vec<(TargetKey, Availability)>,
}

impl Entry {
    /// Read-only here because another page owns the key.
    pub fn reserved(&self) -> bool {
        self.owner.is_some()
    }

    /// The page offers an editor (an editable kind not owned by another page).
    pub fn editable(&self) -> bool {
        self.kind.editable() && !self.reserved()
    }

    /// The list title: in Chinese the Chinese name (`missing` when there is none
    /// and the path stands in), otherwise the official English name, else the
    /// path.
    pub fn title_for(&self, zh: bool) -> Title<'_> {
        if zh {
            return match &self.name_zh {
                Some(n) => Title {
                    text: n,
                    is_path: false,
                    zh_missing: false,
                },
                None => Title {
                    text: &self.path,
                    is_path: true,
                    zh_missing: true,
                },
            };
        }
        match &self.name {
            Some(n) => Title {
                text: n,
                is_path: false,
                zh_missing: false,
            },
            None => Title {
                text: &self.path,
                is_path: true,
                zh_missing: false,
            },
        }
    }

    /// The second line of a list row: the path when the title is a name,
    /// otherwise the first sentence of the explanation (English, or Chinese in
    /// Chinese).
    pub fn subtitle_for(&self, zh: bool) -> Option<String> {
        let title = self.title_for(zh);
        if !title.is_path {
            return Some(self.path.clone());
        }
        let text = if zh {
            self.text.zh.as_deref().or(self.text.en.as_deref())
        } else {
            self.text.en.as_deref().or(self.text.en_note.as_deref())
        }?;
        Some(super::detail::strip_code_ticks(&first_sentence(text)))
    }

    /// The first Codex version that knows the key, when a local Codex is older
    /// (the "needs >= x" badge).
    pub fn needs_version(&self) -> Option<&str> {
        self.availability
            .iter()
            .any(|(_, a)| *a == Availability::No)
            .then_some(self.since.as_deref())
            .flatten()
    }

    /// Search rank (lower first): 0 exact path, 1 path prefix, 2 name match,
    /// 3 path contains, 4 explanation only.
    pub fn rank(&self, query: &str) -> u8 {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return 0;
        }
        let as_path = q.split_whitespace().collect::<Vec<_>>().join("_");
        let path = self.path.to_lowercase();
        let names = [self.name.as_deref(), self.name_zh.as_deref()];
        if path == q || path == as_path {
            0
        } else if path.starts_with(&q) || path.starts_with(&as_path) {
            1
        } else if names
            .iter()
            .flatten()
            .any(|n| n.to_lowercase().contains(&q))
        {
            2
        } else if path.contains(&q) || path.contains(&as_path) {
            3
        } else {
            4
        }
    }

    /// A managed value of the applied profile that `config.toml` does not hold yet
    /// (saved, waiting for the next switch).
    pub fn unapplied(&self, profile_applied: bool) -> bool {
        profile_applied && self.managed && self.current != self.file_value
    }

    /// No Codex reading this home knows the key, so a switch skips it (only
    /// an update of Codex helps), as opposed to a saved value that simply has
    /// not been applied yet.
    pub fn version_blocked(&self) -> bool {
        !self.availability.is_empty()
            && self
                .availability
                .iter()
                .all(|(_, a)| *a == Availability::No)
    }

    /// True for `[features]` entries.
    pub fn is_feature(&self) -> bool {
        self.id.starts_with("feature:")
    }

    /// Title shown in the list: the official name, else the path.
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.path)
    }

    /// The value this entry has after `pending` is applied (`None` = unset).
    pub fn value_with<'a>(&'a self, pending: &'a Pending) -> Option<&'a Value> {
        match pending.get(&self.path) {
            Some(Change::Set(v)) => Some(v),
            Some(Change::Unset) => None,
            None => self.current.as_ref(),
        }
    }

    /// The value Codex will use: the configured one, else the default.
    pub fn shown_value<'a>(&'a self, pending: &'a Pending) -> Option<&'a Value> {
        self.value_with(pending).or(self.default.as_ref())
    }

    /// Configured to something other than the default.
    pub fn differs_from_default(&self, pending: &Pending) -> bool {
        match (self.value_with(pending), &self.default) {
            (None, _) => false,
            (Some(v), Some(d)) => v != d,
            (Some(_), None) => true,
        }
    }

    /// The runtime value disagrees with what the catalog predicts (feature only):
    /// the catalog may be older than this Codex.
    pub fn runtime_mismatch(&self) -> bool {
        let Some(effective) = self.effective else {
            return false;
        };
        // What Codex read: the file (a saved but unapplied profile value is not
        // in it yet), else the default.
        let predicted = self
            .file_value
            .as_ref()
            .and_then(Value::as_bool)
            .or_else(|| self.default.as_ref().and_then(Value::as_bool));
        predicted.is_some_and(|p| p != effective)
    }

    /// Text matched by the search box.
    fn haystack(&self) -> impl Iterator<Item = &str> {
        [
            Some(self.path.as_str()),
            self.name.as_deref(),
            self.text.en.as_deref(),
            self.text.en_note.as_deref(),
            self.text.zh.as_deref(),
            self.name_zh.as_deref(),
        ]
        .into_iter()
        .flatten()
    }

    /// Availability for one target.
    pub fn availability_for(&self, target: &TargetKey) -> Availability {
        self.availability
            .iter()
            .find(|(t, _)| t == target)
            .map(|(_, a)| *a)
            .unwrap_or(Availability::Unknown)
    }
}

/// What a list row shows as its first line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Title<'a> {
    /// Text.
    pub text: &'a str,
    /// The text is the config path (mono).
    pub is_path: bool,
    /// Chinese UI and no Chinese name: show the `暂缺中文` badge.
    pub zh_missing: bool,
}

/// The first sentence of an explanation (up to `.`, `。`, a line break or 140
/// characters).
pub fn first_sentence(text: &str) -> String {
    let t = text.trim();
    let mut end = t.len();
    for (i, c) in t.char_indices() {
        if c == '\n' {
            end = i;
            break;
        }
        if c == '。' {
            end = i + c.len_utf8();
            break;
        }
        if c == '.' && t[i + 1..].starts_with([' ', '\n']) {
            end = i + 1;
            break;
        }
    }
    let s = &t[..end];
    if s.chars().count() > 140 {
        let cut: String = s.chars().take(139).collect();
        format!("{cut}…")
    } else {
        s.to_owned()
    }
}

/// A category of the left rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Category {
    /// Id (`features`, `model`, `sandbox`, ...).
    pub id: String,
    /// English label from the catalog.
    pub label: String,
    /// Chinese label from the overlay, when present.
    pub label_zh: Option<String>,
}

/// Everything the Codex settings tab shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    /// Profile whose Codex settings are edited.
    pub profile_id: Option<superba_core::ProfileId>,
    /// That profile is the one applied to the local configuration.
    pub profile_applied: bool,
    /// i18n key of why nothing can be edited (no key-based profile, ...).
    pub read_only_reason: Option<&'static str>,
    /// Catalog notices (version match, cross-check with `codex features list`).
    pub notes: Vec<superba_core::LocalizedText>,
    /// Local consumers in chip order (the app's Codex, the CLI first on PATH).
    pub targets: Vec<TargetInfo>,
    /// Remote hosts the app is connected to (availability when syncing).
    pub hosts: Vec<TargetInfo>,
    /// Categories in rail order.
    pub categories: Vec<Category>,
    /// All entries known for any target, in display order.
    pub entries: Vec<Entry>,
    /// Version of the catalog snapshot (newest Codex it knows).
    pub catalog_version: Option<String>,
    /// The profile whose Codex settings are edited (`None` = the live config).
    pub profile: Option<String>,
    /// `config.toml` could not be parsed: the page is read-only.
    pub config_error: Option<String>,
}

impl Model {
    /// Look up an entry by config path.
    pub fn entry(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Entries passing `filter`, in catalog order.
    pub fn visible<'a>(
        &'a self,
        filter: &'a Filter,
        pending: &'a Pending,
    ) -> impl Iterator<Item = &'a Entry> + 'a {
        self.entries
            .iter()
            .filter(move |e| filter.accepts(e, pending))
    }

    /// Entries passing `filter` in list order: "Common" in its curated order,
    /// search results by rank (exact path, path prefix, names, path, text), else
    /// catalog order.
    pub fn listed<'a>(&'a self, filter: &'a Filter, pending: &'a Pending) -> Vec<&'a Entry> {
        let mut rows: Vec<&Entry> = self.visible(filter, pending).collect();
        if filter.searching() {
            rows.sort_by_key(|e| e.rank(&filter.query));
        } else if filter.rail == Rail::Common {
            rows.sort_by_key(|e| e.common.unwrap_or(usize::MAX));
        }
        rows
    }

    /// Counts for the rail: Common, Changed, and per category (search hits while
    /// searching, else the entries the stage rules show).
    pub fn counts(&self, filter: &Filter, pending: &Pending) -> RailCounts {
        let mut out = RailCounts::default();
        let common = Filter {
            rail: Rail::Common,
            ..Filter::default()
        };
        let changed = Filter {
            rail: Rail::Changed,
            ..Filter::default()
        };
        let any = Filter {
            rail: Rail::All,
            ..filter.clone()
        };
        for e in &self.entries {
            if common.accepts(e, pending) {
                out.common += 1;
            }
            if changed.accepts(e, pending) {
                out.changed += 1;
            }
            if any.accepts(e, pending) {
                *out.categories.entry(e.category.clone()).or_insert(0) += 1;
                out.results += 1;
            }
        }
        out
    }

    /// Legacy keys set in the current configuration that have a replacement.
    pub fn migrations(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| {
            e.current.is_some() && e.legacy.as_ref().is_some_and(|l| l.canonical.is_some())
        })
    }

    /// The edits that migrate a legacy entry: unset it and set the canonical path
    /// (unless the canonical path is already configured).
    pub fn migration_changes(&self, entry: &Entry) -> Vec<(String, Change)> {
        let Some(legacy) = &entry.legacy else {
            return Vec::new();
        };
        let mut out = vec![(entry.path.clone(), Change::Unset)];
        if let Some(canonical) = &legacy.canonical {
            let already = self.entry(canonical).is_some_and(|c| c.current.is_some());
            let value = legacy.canonical_value.clone().or(entry.current.clone());
            if let (false, Some(value)) = (already, value) {
                out.push((canonical.clone(), Change::Set(value)));
            }
        }
        out
    }
}

/// What the rail selects (UX §3.6): the curated common settings, the changed
/// ones, a category, or everything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rail {
    /// Every entry (search results, palette focus).
    #[default]
    All,
    /// The curated "Common" list (people-maintained, `[common]` in curation.toml).
    Common,
    /// Keys the provider manages plus pending edits.
    Changed,
    /// One catalog category.
    Category(String),
}

/// List filter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    /// Search text (case-insensitive, every word must match; searches every
    /// entry, a selected category narrows the results).
    pub query: String,
    /// Rail selection.
    pub rail: Rail,
    /// Show under-development, deprecated and removed entries and legacy keys.
    pub advanced: bool,
}

impl Filter {
    /// Searching (the rail shows "Search results").
    pub fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// The entry passes the stage rules (Advanced shows everything; configured
    /// keys, pending edits and recommendations are never hidden).
    pub fn staged(&self, entry: &Entry, pending: &Pending) -> bool {
        let touched = entry.current.is_some() || pending.get(&entry.path).is_some();
        self.advanced
            || touched
            || (entry.stage.in_default_view() && entry.legacy.is_none() && !entry.advanced_only)
            || entry.recommended.is_some()
    }

    /// The entry matches the search text (every word in some field).
    pub fn matches(&self, entry: &Entry) -> bool {
        let query = self.query.trim().to_lowercase();
        query.split_whitespace().all(|word| {
            entry
                .haystack()
                .any(|text| text.to_lowercase().contains(word))
        })
    }

    /// True if `entry` is listed.
    pub fn accepts(&self, entry: &Entry, pending: &Pending) -> bool {
        if self.searching() {
            if !self.staged(entry, pending) || !self.matches(entry) {
                return false;
            }
            return match &self.rail {
                Rail::Category(c) => &entry.category == c,
                _ => true,
            };
        }
        match &self.rail {
            Rail::Changed => entry.managed || pending.get(&entry.path).is_some(),
            // Common entries a local Codex does not know are left out.
            Rail::Common => {
                entry.common.is_some()
                    && !entry.reserved()
                    && !entry
                        .availability
                        .iter()
                        .any(|(_, a)| *a == Availability::No)
            }
            Rail::Category(c) => &entry.category == c && self.staged(entry, pending),
            Rail::All => self.staged(entry, pending),
        }
    }
}

/// Counts shown in the rail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RailCounts {
    /// Entries in "Common".
    pub common: usize,
    /// Entries in "Changed".
    pub changed: usize,
    /// Visible entries (search hits while searching) per category.
    pub categories: BTreeMap<String, usize>,
    /// All search hits (while searching).
    pub results: usize,
}

/// One pending edit.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Write this value.
    Set(Value),
    /// Remove the key (back to Codex's default).
    Unset,
}

/// Edits not yet handed to the switch flow, keyed by config path.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    changes: BTreeMap<String, Change>,
}

impl Pending {
    /// The edit for `path`.
    pub fn get(&self, path: &str) -> Option<&Change> {
        self.changes.get(path)
    }

    /// Record an edit; an edit back to the configured value is dropped.
    pub fn set(&mut self, entry: &Entry, change: Change) {
        let same = match (&change, &entry.current) {
            (Change::Set(v), Some(c)) => v == c,
            (Change::Unset, None) => true,
            _ => false,
        };
        if same {
            self.changes.remove(&entry.path);
        } else {
            self.changes.insert(entry.path.clone(), change);
        }
    }

    /// Record an edit for a path without an entry (legacy migrations).
    pub fn set_path(&mut self, model: &Model, path: &str, change: Change) {
        match model.entry(path) {
            Some(entry) => self.set(entry, change),
            None => {
                self.changes.insert(path.to_owned(), change);
            }
        }
    }

    /// Reset to default = remove the key.
    pub fn reset(&mut self, entry: &Entry) {
        self.set(entry, Change::Unset);
    }

    /// Drop the edit of one path.
    pub fn discard(&mut self, path: &str) {
        self.changes.remove(path);
    }

    /// Drop every edit.
    pub fn clear(&mut self) {
        self.changes.clear();
    }

    /// Number of edits.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// No edits.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Edits in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Change)> {
        self.changes.iter()
    }

    /// Drop edits that no longer differ from a reloaded model.
    pub fn rebase(&mut self, model: &Model) {
        self.changes.retain(|path, change| match model.entry(path) {
            Some(e) => match (change, &e.current) {
                (Change::Set(v), Some(c)) => v != c,
                (Change::Unset, None) => false,
                _ => true,
            },
            None => true,
        });
    }
}

/// One line of the pending-changes panel.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffLine {
    /// Config path.
    pub path: String,
    /// Value now (`None` = unset).
    pub before: Option<Value>,
    /// Value after applying (`None` = unset).
    pub after: Option<Value>,
    /// Targets whose Codex version does not know the key.
    pub unsupported: Vec<TargetKey>,
}

/// The change list shown before handing off to the switch flow.
pub fn diff(model: &Model, pending: &Pending) -> Vec<DiffLine> {
    pending
        .iter()
        .map(|(path, change)| {
            let entry = model.entry(path);
            DiffLine {
                path: path.clone(),
                before: entry.and_then(|e| e.current.clone()),
                after: match change {
                    Change::Set(v) => Some(v.clone()),
                    Change::Unset => None,
                },
                unsupported: entry
                    .map(|e| {
                        e.availability
                            .iter()
                            .filter(|(_, a)| *a == Availability::No)
                            .map(|(t, _)| t.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}
