# Contributing to the Codex settings catalog

This directory describes which `config.toml` keys and `[features]` each Codex
release understands. Superba's settings page reads it to show official
descriptions, stages, defaults and per-version availability. It also holds the
signed data feeds (`feeds/`: model list, prices, Codex releases, desktop app
builds). This guide covers what you may edit, how to write Chinese (and, rarely,
English) explanations, the feeds, and how changes are reviewed and signed.

## Layout

| Path | Written by | Contents |
|---|---|---|
| `sources.toml` | people | Which `openai/codex` releases are included (floor, extra prereleases, `latest_prerelease`: the newest alpha while it is ahead of every stable release). |
| `curation.toml` | people | Settings categories, platform hints and the "Common" list (`[common]`); the schema has none of them. |
| `dist/codex-<version>.json` | generator | One Codex release: every config key, feature and legacy alias, with official upstream text only. |
| `dist/changes/codex-<version>.md` | generator | What changed against the previous stable release. |
| `dist/index.json` | generator | SHA-256 manifest of every file the app loads, with a serial number. |
| `dist/index.json.sig` | maintainer | ed25519 signature of the index. |
| `overlay/*.toml` | generator + people | Explanation text per entry (see below). |
| `COVERAGE.md` | generator | How many entries have official English and Chinese text. |
| `feeds/` | generator + people | Signed data feeds (see [Feeds](#feeds)). |
| `models-extra.json` | people | Hand-written model entries for third-party models, merged into `feeds/models.json`. |

The generator is `superba-catalog-gen` (crate `crates/superba-catalog-gen`):

```sh
cargo run -p superba-catalog-gen -- --help
```

It reads three files from an `openai/codex` release tag:
`codex-rs/core/config.schema.json`, `codex-rs/features/src/lib.rs` (the
`FEATURES` registry) and `codex-rs/features/src/legacy.rs` (legacy aliases).

## Where explanation text comes from

There are exactly three kinds of explanation text. Keep them apart.

1. **Official English** (`en`, `en_name`): copied verbatim by the generator from the
   schema description of a key, the description of an enum value, or the
   `/experimental` menu text of a feature. `en_source` names the release and file
   it came from. Never edit these fields by hand; the next `overlay-sync`
   overwrites them. When upstream has no text, `en` stays empty and the entry is
   counted as "English missing". Rust doc comments (`rustdoc` in `dist/`) are
   developer notes and are **not** used as explanations.
2. **Chinese** (`zh_name` for **every** entry, `zh`): written entry by entry by
   maintainers or agents from the codex source at the release tag: the code that
   reads the key or feature and what it does with the value, not only the doc
   comment. Never bulk machine translation, and never left empty "for later": a
   release ships with Chinese for every non-retired entry. When the behaviour is
   inferred from the code rather than documented, say so ("依据源码推断"). Entries
   for removed or deprecated keys say in one sentence what they did and what
   replaced them. The generator never writes these fields.
3. **English notes** (`en_note`): optional human text for entries where upstream
   has no English. Use it sparingly, only for facts you can point to (release
   notes, behaviour you verified), and never to paraphrase or "improve" an
   official text. The app shows it labelled as a contributor note, and only while
   the official text is missing.

## Overlay entries

Each table in `overlay/*.toml` is one entry. Ids:

- `feature:<key>` — a registry feature (`feature:fast_mode`). Dotted keys are
  nested tables in `config.toml` (`feature:guardianv2.thread_context`).
- `config:<path>` — a config key; map entries use `<name>`
  (`config:mcp_servers.<name>.command`).
- `config:<path>=<value>` — one value of an enum key (only values with official
  text get an entry).

Feature toggles (`features.<key>`) are explained by their `feature:` entry, not by
a `config:` entry. Files are grouped by category (`curation.toml`);
`features.toml` holds every feature.

```toml
["config:approval_policy=never"]
en = "Never ask the user to approve commands. Failures are ..."           # generator
en_source = "openai/codex rust-v0.155.0: codex-rs/core/config.schema.json"  # generator
en_hash = "1b9d767a51b68773"                                               # generator
zh = "<your Chinese text>"                                                 # you
zh_basis = "1b9d767a51b68773"                                              # you: copy en_hash
```

Fields you own: `zh`, `zh_name`, `zh_basis`, `en_note`. Everything else is
generator-owned. Comments inside overlay files are
not preserved; put remarks in the pull request.

### Staleness

`en_hash` fingerprints the official English. When you write or revise `zh`, set
`zh_basis` to the current `en_hash`. If upstream later rewords the English, the
hashes differ and the entry counts as **stale**: the app shows the Chinese text
with a "may be outdated" tag and `COVERAGE.md` lists it. Re-read the new English,
update `zh` if needed, then copy the new `en_hash` into `zh_basis`. If you leave
`zh_basis` out, `overlay-sync` fills it in with the hash of the English that was
current when it first saw your text.

### Retired entries

When a key or feature disappears from every included release, `overlay-sync`
removes its entry, unless it holds human text: then it stays with
`retired = "<last version>"` so the text is not lost. Delete retired entries in a
separate pull request once nobody needs them.

## Style guide for Chinese text

- Write from the codex source of the release (`git show <tag>:<path>` in a clone):
  find where the key is read and what changes for each value. Explain what it
  controls, then the effect for the user, then values, default and caveats (needs
  a restart, CLI only, one platform). Do not translate the English word by word.
- `zh_name`: a noun phrase of at most 12 Chinese characters. `zh`: one to three
  sentences.
- Simplified Chinese with full-width punctuation (，。：；（）); put a space between
  Chinese and Latin words or numbers (`使用 Codex 0.163 时`).
- Keep code identifiers, key names, values, commands and paths in backticks and
  untranslated: `approval_policy`, `on-request`, `~/.codex/history.jsonl`.
- Product names stay as they are: Codex, ChatGPT, MCP, OpenAI.
- When unsure, write what the code shows and mark it "依据源码推断"; never guess.
- Removed or deprecated entries: one sentence on what it did and what replaced it.
- Avoid marketing tone and second-person commands.

Terminology (use these words everywhere):

| English | 中文 |
|---|---|
| feature flag | 功能开关 |
| under development / experimental / stable / deprecated / removed | 开发中 / 实验性 / 稳定 / 已弃用 / 已移除 |
| sandbox / sandbox mode | 沙箱 / 沙箱模式 |
| approval policy | 审批策略 |
| workspace / workspace-write | 工作区 / 工作区可写 |
| MCP server | MCP 服务器 |
| tool / skill / plugin / connector (app) | 工具 / 技能 / 插件 / 应用连接器 |
| model provider | 模型供应商 |
| reasoning effort / reasoning summary / verbosity | 推理强度 / 推理摘要 / 输出详细度 |
| context window / auto compaction / compact | 上下文窗口 / 自动压缩 / 压缩上下文 |
| thread / turn / rollout | 会话 / 轮次 / 会话记录（rollout 文件） |
| hook / notify | 钩子 / 通知 |
| telemetry / analytics / OTEL | 遥测 / 使用统计 / OpenTelemetry |
| TUI / app-server / exec | 终端界面（TUI）/ app-server / `codex exec` |
| profile (`[profiles.*]`) | 配置档（profile） |
| shell environment policy | Shell 环境变量策略 |
| web search | 联网搜索 |
| realtime | 实时语音 |
| prompt / instructions / developer instructions | 提示词 / 指令 / 开发者指令 |
| subagent / multi-agent | 子代理 / 多代理 |
| desktop app | 桌面 app |

## Workflow

New upstream releases arrive through the scheduled workflow
(`.github/workflows/catalog-update.yml`), which opens a pull request with the
regenerated `dist/`, synchronized overlays (empty Chinese for new entries) and a
change summary. New entries arrive with empty Chinese fields; maintainers write
them (per the style guide) before the next release, and a release is not cut
while `COVERAGE.md` shows Chinese missing for a non-retired entry. To do the same
locally with a clone of `openai/codex`:

```sh
superba-catalog-gen build --git ../codex --only-new
superba-catalog-gen overlay-sync
superba-catalog-gen coverage --write
superba-catalog-gen index
superba-catalog-gen check
```

After editing overlays or `curation.toml` by hand:

```sh
superba-catalog-gen overlay-sync      # normalizes the file and fills zh_basis
superba-catalog-gen coverage --write
superba-catalog-gen index             # new manifest; drops the old signature
superba-catalog-gen check             # what CI runs
```

`superba-catalog-gen coverage --list-missing` lists entries without official
English, a good queue for `en_note` work. `superba-catalog-gen features-check`
compares a `codex features list` capture with a catalog version.

### Review

- Chinese text: reviewed by a second Chinese-speaking maintainer, who checks it
  against the source; small pull requests per category.
- `curation.toml`, `sources.toml`, `models-extra.json`, `feeds/prices.json`: any
  maintainer.
- `dist/` and generator-owned overlay fields: must come from the generator; CI
  runs `superba-catalog-gen check`, and maintainers may run
  `check --git <codex clone>` to re-extract every version byte for byte.
- `overlay/` is protected with CODEOWNERS (`.github/CODEOWNERS`).

## Feeds

`feeds/` holds data Superba uses besides the settings catalog. The app compiles
the directory in and can refresh it from mirrors of a public location (GitHub raw
and jsDelivr-style CDNs, raced; see `crates/superba-catalog/src/feeds/`), so
everything is signed:

| File | Written by | Contents |
|---|---|---|
| `models.json` | generator | Model metadata (no instructions) from each selected release's `codex-rs/models-manager/models.json`, newest first, plus `models-extra.json`. Used when a provider does not list its models and for generic catalog entries. |
| `prompts.json`, `prompts/<sha8>.md` | generator | `codex-rs/models-manager/prompt.md` per Codex version (deduplicated): the instructions of models Codex does not know. |
| `codex-versions.json` | generator | Codex releases (tag, date, prerelease, `codex-package_SHA256SUMS` per target, the app builds that bundle them). |
| `app-versions.json` | generator | Desktop app builds (APT `Packages`, RPM `primary.xml.gz`, Sparkle appcast, Windows Store JSON; bundled Codex from the package). Known builds are kept. |
| `prices.json` | people | Official API list prices from each model vendor's own public price page (OpenAI, Anthropic, Google, DeepSeek, Alibaba Qwen, Zhipu GLM, Moonshot Kimi, xAI and others), USD per 1M tokens: `input`, `cached_input`, `cache_write` (only where OpenAI publishes one; otherwise writes cost `input`), `output`, `long_context` bands (`above_input_tokens` with their own prices), `tier_prices` (absolute `priority` = Fast, `flex` and `ultrafast` prices copied from each tier's own table, long-context bands and an optional per-tier `source`), `tiers` (multipliers, for hand-written entries only), and per entry the `source` page (https) and `rev` (the date the page was read, `YYYY-MM-DD`). **Only from the vendor's own official price pages** (for OpenAI: `developers.openai.com/api/docs/pricing`, the per-model pages under `developers.openai.com/api/docs/models/`, `openai.com/api-fast-mode/`), never from resellers or third-party aggregators, and never extrapolated; `windows` hold time-of-day prices (e.g. DeepSeek's off-peak) in the vendor's stated timezone: a model without a published price is absent, and its costs show "—" with a hint to set a price on the Models page. Costs are estimates at these prices × the provider's ratio. |
| `index.json`, `index.json.sig` | generator, maintainer | Manifest with a serial and the signature. |

The feed index is signed with the same key files as the catalog index but over a
different context (`superba-feeds-index-v1\n` instead of
`superba-catalog-index-v1\n`), so one index can never pass as the other. Clients
never accept a serial lower than the one they use. Until the public feeds
location exists (`FEEDS_REPO`, currently a placeholder) the app uses the
compiled-in copy only.

```sh
superba-catalog-gen feeds models --git ../codex
superba-catalog-gen feeds app-versions --apt Packages-amd64 --rpm x86_64=primary.xml.gz \
    --appcast appcast.xml --windows-store windows-store-update.json --bundled <app>=<codex>
superba-catalog-gen feeds codex-versions --git ../codex --sums-dir sums/
superba-catalog-gen feeds index && superba-catalog-gen feeds check
superba-catalog-gen feeds sign --key .secrets/catalog-signing.key   # maintainers
```

The generator makes no network requests; the workflow downloads the index files.

## Integrity, signing and key rotation

The app embeds this directory at build time and verifies `dist/index.json`
against every file's SHA-256 before use. Remote updates (off by default) also
require a valid ed25519 signature in `dist/index.json.sig` from a key listed in
`TRUSTED_KEYS` (`crates/superba-catalog/src/integrity.rs`) and an index serial
not lower than the one in use. Only public keys are in the repository.

Signing (maintainers, after the index is final):

```sh
superba-catalog-gen sign --key .secrets/catalog-signing.key
superba-catalog-gen verify --require-signature
superba-catalog-gen feeds sign --key .secrets/catalog-signing.key
superba-catalog-gen feeds verify --require-signature
```

Automation never signs: the catalog-update workflow builds and runs third-party
code, and the signing key is the trust root of remote updates, so keep it out of
CI secrets and sign on a maintainer machine after the update is reviewed and
merged. (`--key env:NAME` reads the key file's hex line from an environment
variable when a file is not practical.) Key files are created with
`superba-catalog-gen keygen --out <path>` (mode 0600, never overwritten) and must
stay out of git (`.secrets/` is ignored).

Rotation:

1. `superba-catalog-gen keygen --out .secrets/catalog-signing-2.key`; add the
   printed public key to `TRUSTED_KEYS` next to the old one and ship a release.
2. Once most users run that release, sign with both keys
   (`sign --key old --key new`), then with the new key only.
3. Remove the old key from `TRUSTED_KEYS` in a later release. If a key leaks,
   add its id to `REVOKED_KEY_IDS`, ship a release, and re-sign with a new key;
   revoked keys are rejected even if still listed as trusted.
