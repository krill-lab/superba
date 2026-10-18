//! Models in Claude Code: the slots (`/model` "Opus", "Sonnet", ...), what
//! each model supports, the picker rows and the per-provider configuration.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::ClaudeRoute;
use crate::ids::ProfileId;
use crate::models::ModelSpec;

/// A capability Claude Code may use with a model it does not know
/// (`*_SUPPORTED_CAPABILITIES`, a comma list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeCapability {
    /// Effort levels (`/effort`, `CLAUDE_CODE_EFFORT_LEVEL`).
    Effort,
    /// The `xhigh` effort level.
    XhighEffort,
    /// The `max` effort level.
    MaxEffort,
    /// Extended thinking with a token budget.
    Thinking,
    /// Adaptive thinking (the model decides how long to think).
    AdaptiveThinking,
    /// Thinking between tool calls.
    InterleavedThinking,
    /// A `temperature` parameter.
    Temperature,
    /// System messages in the middle of a conversation.
    MidConversationSystem,
}

impl ClaudeCapability {
    /// Every capability, in the order Superba writes them.
    pub const ALL: [ClaudeCapability; 8] = [
        ClaudeCapability::Effort,
        ClaudeCapability::XhighEffort,
        ClaudeCapability::MaxEffort,
        ClaudeCapability::Thinking,
        ClaudeCapability::AdaptiveThinking,
        ClaudeCapability::InterleavedThinking,
        ClaudeCapability::Temperature,
        ClaudeCapability::MidConversationSystem,
    ];

    /// The name Claude Code reads.
    pub fn as_str(self) -> &'static str {
        match self {
            ClaudeCapability::Effort => "effort",
            ClaudeCapability::XhighEffort => "xhigh_effort",
            ClaudeCapability::MaxEffort => "max_effort",
            ClaudeCapability::Thinking => "thinking",
            ClaudeCapability::AdaptiveThinking => "adaptive_thinking",
            ClaudeCapability::InterleavedThinking => "interleaved_thinking",
            ClaudeCapability::Temperature => "temperature",
            ClaudeCapability::MidConversationSystem => "mid_conversation_system",
        }
    }

    /// Parse a name (case-insensitive, `-` for `_` accepted).
    pub fn parse(s: &str) -> Option<Self> {
        let n = s.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL.into_iter().find(|c| c.as_str() == n)
    }
}

/// The comma list Claude Code reads (`effort,thinking`), in
/// [`ClaudeCapability::ALL`] order.
pub fn capabilities_text(caps: &BTreeSet<ClaudeCapability>) -> String {
    ClaudeCapability::ALL
        .into_iter()
        .filter(|c| caps.contains(c))
        .map(ClaudeCapability::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a comma list; unknown names are returned separately.
pub fn parse_capabilities(text: &str) -> (BTreeSet<ClaudeCapability>, Vec<String>) {
    let mut caps = BTreeSet::new();
    let mut unknown = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match ClaudeCapability::parse(part) {
            Some(c) => {
                caps.insert(c);
            }
            None => unknown.push(part.to_owned()),
        }
    }
    (caps, unknown)
}

/// The family a model id belongs to, for capability presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    /// `claude-*`: Claude Code knows these itself.
    Claude,
    /// OpenAI GPT / o-series / Codex models.
    Gpt,
    /// DeepSeek.
    DeepSeek,
    /// Kimi (Moonshot).
    Kimi,
    /// GLM (Zhipu).
    Glm,
    /// Qwen.
    Qwen,
    /// MiniMax.
    MiniMax,
    /// Anything else.
    Other,
}

impl ModelFamily {
    /// The family of `model` (by name).
    pub fn of(model: &str) -> Self {
        let m = model.trim().to_ascii_lowercase();
        let m = m.rsplit('/').next().unwrap_or(&m);
        let starts = |p: &str| m.starts_with(p);
        if starts("claude") || m.contains("anthropic") {
            ModelFamily::Claude
        } else if starts("gpt")
            || starts("o1")
            || starts("o3")
            || starts("o4")
            || starts("codex")
            || starts("chatgpt")
        {
            ModelFamily::Gpt
        } else if starts("deepseek") {
            ModelFamily::DeepSeek
        } else if starts("kimi") || starts("moonshot") {
            ModelFamily::Kimi
        } else if starts("glm") || starts("chatglm") {
            ModelFamily::Glm
        } else if starts("qwen") || starts("qwq") {
            ModelFamily::Qwen
        } else if starts("minimax") || starts("abab") {
            ModelFamily::MiniMax
        } else {
            ModelFamily::Other
        }
    }
}

/// The capabilities Superba suggests for `model`: from its Codex metadata
/// when there is some (reasoning levels -> `effort`, `xhigh_effort`,
/// `max_effort`, `thinking`; no `temperature` with reasoning), else by
/// family. A hint the user can change, never a fact.
pub fn suggested_capabilities(model: &str, spec: Option<&ModelSpec>) -> BTreeSet<ClaudeCapability> {
    use ClaudeCapability as C;
    let mut out = BTreeSet::new();
    if let Some(spec) = spec.filter(|s| !s.reasoning_levels.is_empty()) {
        out.insert(C::Effort);
        out.insert(C::Thinking);
        for level in &spec.reasoning_levels {
            match level.effort.to_ascii_lowercase().as_str() {
                "xhigh" => {
                    out.insert(C::XhighEffort);
                }
                "max" => {
                    out.insert(C::MaxEffort);
                }
                _ => {}
            }
        }
        return out;
    }
    let m = model.to_ascii_lowercase();
    match ModelFamily::of(model) {
        ModelFamily::Claude | ModelFamily::Other => {}
        ModelFamily::Gpt => {
            let reasoning =
                !m.contains("chat") && !m.starts_with("gpt-4") && !m.starts_with("gpt-3");
            if reasoning {
                out.extend([C::Effort, C::XhighEffort, C::Thinking]);
            } else {
                out.insert(C::Temperature);
            }
        }
        ModelFamily::DeepSeek => {
            if m.contains("chat") && !m.contains("v4") {
                out.insert(C::Temperature);
            } else {
                out.extend([C::Thinking, C::Effort, C::MaxEffort]);
            }
        }
        ModelFamily::Kimi => out.extend([C::Thinking, C::Effort, C::MaxEffort]),
        ModelFamily::Glm | ModelFamily::Qwen | ModelFamily::MiniMax => {
            out.insert(C::Thinking);
        }
    }
    out
}

/// A `/model` slot Claude Code offers by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeSlot {
    /// "Opus" (`ANTHROPIC_DEFAULT_OPUS_MODEL`).
    Opus,
    /// "Sonnet".
    Sonnet,
    /// "Haiku": also the background / small-fast model.
    Haiku,
    /// "Fable".
    Fable,
    /// One extra row (`ANTHROPIC_CUSTOM_MODEL_OPTION`).
    Custom,
}

impl ClaudeSlot {
    /// Every slot.
    pub const ALL: [ClaudeSlot; 5] = [
        ClaudeSlot::Opus,
        ClaudeSlot::Sonnet,
        ClaudeSlot::Haiku,
        ClaudeSlot::Fable,
        ClaudeSlot::Custom,
    ];

    /// `opus`, `sonnet`, `haiku`, `fable`, `custom`.
    pub fn as_str(self) -> &'static str {
        match self {
            ClaudeSlot::Opus => "opus",
            ClaudeSlot::Sonnet => "sonnet",
            ClaudeSlot::Haiku => "haiku",
            ClaudeSlot::Fable => "fable",
            ClaudeSlot::Custom => "custom",
        }
    }

    /// Parse [`ClaudeSlot::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        let n = s.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|x| x.as_str() == n)
    }

    /// The variable with the model id.
    pub fn env_model(self) -> &'static str {
        match self {
            ClaudeSlot::Opus => "ANTHROPIC_DEFAULT_OPUS_MODEL",
            ClaudeSlot::Sonnet => "ANTHROPIC_DEFAULT_SONNET_MODEL",
            ClaudeSlot::Haiku => "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            ClaudeSlot::Fable => "ANTHROPIC_DEFAULT_FABLE_MODEL",
            ClaudeSlot::Custom => "ANTHROPIC_CUSTOM_MODEL_OPTION",
        }
    }

    /// The variable with the row's display name.
    pub fn env_name(self) -> String {
        format!("{}_NAME", self.env_model())
    }

    /// The variable with the row's description.
    pub fn env_description(self) -> String {
        format!("{}_DESCRIPTION", self.env_model())
    }

    /// The variable with the model's capabilities.
    pub fn env_capabilities(self) -> String {
        format!("{}_SUPPORTED_CAPABILITIES", self.env_model())
    }

    /// The four variables of the slot.
    pub fn env_vars(self) -> [String; 4] {
        [
            self.env_model().to_owned(),
            self.env_name(),
            self.env_description(),
            self.env_capabilities(),
        ]
    }

    /// A Claude model this version of Claude Code knows, for the "behaves as"
    /// of a picker row in this slot (its prompt profile and effort defaults).
    pub fn known_model(self) -> Option<&'static str> {
        match self {
            ClaudeSlot::Opus => Some("claude-opus-5-5"),
            ClaudeSlot::Sonnet => Some("claude-sonnet-5-5"),
            ClaudeSlot::Haiku => Some("claude-haiku-5-5"),
            ClaudeSlot::Fable => Some("claude-fable-5-1"),
            ClaudeSlot::Custom => None,
        }
    }
}

/// The model of a slot with its row text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotModel {
    /// Model id sent to the provider.
    pub model: String,
    /// Display name in `/model` (`None` = Claude Code's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Description in `/model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl SlotModel {
    /// A slot with `model` and no row text.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            name: None,
            description: None,
        }
    }
}

/// A row of the `/model` picker (`modelPicker.options[]`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRow {
    /// Model id.
    pub model: String,
    /// Row title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Row subtitle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A Claude model this Claude Code knows whose client-side handling
    /// applies (`behavesAs`): rows for unknown models are not offered
    /// without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behaves_as: Option<String>,
}

/// What Claude Code uses from one provider.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeModels {
    /// Slot -> model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<ClaudeSlot, SlotModel>,
    /// The default model (`model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    /// The model of subagents (`CLAUDE_CODE_SUBAGENT_MODEL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    /// The background model (`ANTHROPIC_SMALL_FAST_MODEL`; the Haiku slot is
    /// used when this is unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_fast: Option<String>,
    /// Capabilities per model the user set (models without an entry use
    /// [`suggested_capabilities`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capabilities: BTreeMap<String, BTreeSet<ClaudeCapability>>,
    /// `/model` rows, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub picker: Vec<PickerRow>,
    /// Hide Claude Code's own rows (`replaceBuiltInOptions`).
    #[serde(default)]
    pub replace_builtin: bool,
    /// Model ids added by hand (providers without a model list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_models: Vec<String>,
}

impl ClaudeModels {
    /// Nothing chosen.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// The capabilities of `model`: the user's choice, else the suggestion.
    pub fn capabilities_of(
        &self,
        model: &str,
        spec: Option<&ModelSpec>,
    ) -> BTreeSet<ClaudeCapability> {
        self.capabilities
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(model))
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| suggested_capabilities(model, spec))
    }

    /// Every model this configuration names (slots, main, subagent,
    /// background, picker rows, added by hand), deduplicated, in that order.
    pub fn named_models(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |m: &str| {
            let m = m.trim();
            if !m.is_empty() && !out.iter().any(|x| x == m) {
                out.push(m.to_owned());
            }
        };
        for s in self.slots.values() {
            push(&s.model);
        }
        for m in [&self.main, &self.subagent, &self.small_fast]
            .into_iter()
            .flatten()
        {
            push(m);
        }
        for r in &self.picker {
            push(&r.model);
        }
        for m in &self.extra_models {
            push(m);
        }
        out
    }

    /// The slots `model` is in.
    pub fn slots_of(&self, model: &str) -> Vec<ClaudeSlot> {
        self.slots
            .iter()
            .filter(|(_, s)| s.model == model)
            .map(|(k, _)| *k)
            .collect()
    }

    /// Add `model` to the picker (no-op when it is there); the row behaves as
    /// the known model of the first slot it is in.
    pub fn add_to_picker(&mut self, model: &str) {
        if model.trim().is_empty() || self.picker.iter().any(|r| r.model == model) {
            return;
        }
        let behaves_as = self
            .slots_of(model)
            .into_iter()
            .find_map(ClaudeSlot::known_model)
            .or(ClaudeSlot::Sonnet.known_model())
            .map(str::to_owned);
        self.picker.push(PickerRow {
            model: model.to_owned(),
            label: None,
            description: None,
            behaves_as,
        });
    }
}

/// Claude Code's configuration in Superba (`<data>/claude-code.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeConfig {
    /// The provider Claude Code uses (`None` = the one Codex uses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProfileId>,
    /// Direct or through the local router.
    #[serde(default)]
    pub route: ClaudeRoute,
    /// Models per provider.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<ProfileId, ClaudeModels>,
}

impl ClaudeConfig {
    /// The models of `provider` (empty when none were chosen).
    pub fn models_of(&self, provider: &ProfileId) -> ClaudeModels {
        self.models.get(provider).cloned().unwrap_or_default()
    }
}
