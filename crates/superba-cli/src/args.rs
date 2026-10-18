//! Command syntax. Help text comes from the CLI catalog.

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use superba_core::{AuthMode, ProviderRestore, TargetId, TargetSelection, catalog};

use crate::text::english;

/// The headless command line, also embedded by the GUI binary.
#[derive(Debug, Parser)]
#[command(name = "spb", version, about = english("cli.help.about"))]
pub struct Cli {
    #[arg(long, global = true, help = english("cli.help.json"))]
    pub json: bool,
    #[arg(long, global = true, help = english("cli.help.verbose"))]
    pub verbose: bool,
    #[arg(long, global = true, value_parser = ["en", "zh-CN"], help = english("cli.help.locale"))]
    pub locale: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = english("cli.help.status"))]
    Status,
    #[command(about = english("cli.help.lines"))]
    Lines {
        #[arg(long, default_value = catalog::KRILL_SITE_ID, help = english("cli.help.site"))]
        site: String,
        #[arg(long, help = english("cli.help.probe"))]
        probe: bool,
    },
    #[command(about = english("cli.help.key"))]
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    #[command(about = english("cli.help.profile_group"))]
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    #[command(about = english("cli.help.switch"))]
    Switch(SwitchArgs),
    #[command(about = english("cli.help.restore"))]
    Restore(RestoreArgs),
    #[command(visible_alias = "open-app", about = english("cli.help.launch"))]
    Launch {
        #[arg(long, help = english("cli.help.restart"))]
        restart: bool,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.sessions"))]
    Sessions {
        #[command(subcommand)]
        command: Option<SessionsCommand>,
        #[arg(long, global = true, value_name = "TARGET", help = english("cli.help.sessions_host"))]
        host: Option<String>,
    },
    #[command(about = english("cli.help.site_group"))]
    Site {
        #[command(subcommand)]
        command: SiteCommand,
    },
    #[command(about = english("cli.help.account"))]
    Account(AccountArgs),
    #[command(about = english("cli.help.settings_group"))]
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    #[command(about = english("cli.help.router_group"))]
    Router {
        #[command(subcommand)]
        command: RouterCommand,
    },
    #[command(about = english("cli.help.usage_group"))]
    Usage(UsageArgs),
    #[command(about = english("cli.help.remote_group"))]
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    #[command(about = english("cli.help.wsl_group"))]
    Wsl {
        #[command(subcommand)]
        command: WslCommand,
    },
    #[command(about = english("cli.help.models_group"))]
    Models {
        #[command(subcommand)]
        command: Option<ModelsCommand>,
        #[arg(long, global = true, help = english("cli.help.models_profile"))]
        profile: Option<String>,
        #[arg(long, help = english("cli.help.models_refresh"))]
        refresh: bool,
    },
    #[command(about = english("cli.help.speed"))]
    Speed(SpeedArgs),
    #[command(about = english("cli.help.codex_group"))]
    Codex {
        #[command(subcommand)]
        command: CodexCommand,
    },
    #[command(about = english("cli.help.app_group"))]
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
    #[command(name = "self-update", about = english("cli.help.self_update_group"))]
    SelfUpdate {
        #[command(subcommand)]
        command: SelfUpdateCommand,
    },
    #[command(about = english("cli.help.feeds_group"))]
    Feeds {
        #[command(subcommand)]
        command: FeedsCommand,
    },
    #[command(about = english("cli.import.help.about"))]
    Import(ImportArgs),
    #[command(about = english("cli.help.doctor"))]
    Doctor,
    #[command(about = english("cli.help.recover"))]
    Recover,
    #[command(about = english("cli.help.uninstall"))]
    Uninstall {
        #[arg(long, help = english("cli.help.delete_backups"))]
        delete_backups: bool,
        #[arg(long, value_enum, help = english("cli.help.provider"))]
        provider: Option<ProviderArg>,
        #[arg(long, help = english("cli.help.restore_take_over"))]
        take_over: bool,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(name = "hand-off", about = english("cli.help.hand_off"))]
    HandOff(HandOffArgs),
}

/// A profile's auth choice (Auto unless changed under Advanced).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AuthChoiceArg {
    Auto,
    Pat,
    Apikey,
}

impl From<AuthChoiceArg> for superba_core::AuthChoice {
    fn from(value: AuthChoiceArg) -> Self {
        match value {
            AuthChoiceArg::Auto => Self::Auto,
            AuthChoiceArg::Pat => Self::Pat,
            AuthChoiceArg::Apikey => Self::ApiKey,
        }
    }
}

/// Token authentication values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AuthArg {
    Pat,
    Apikey,
}

impl From<AuthArg> for AuthMode {
    fn from(value: AuthArg) -> Self {
        match value {
            AuthArg::Pat => Self::Pat,
            AuthArg::Apikey => Self::ApiKey,
        }
    }
}

/// Options shared by all commands that execute a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
pub struct ExecArgs {
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=30), help = english("cli.help.countdown"))]
    pub countdown: u32,
    #[arg(short = 'y', long, help = english("cli.help.yes"))]
    pub yes: bool,
    #[arg(long, help = english("cli.help.allow_full_restart"))]
    pub allow_full_restart: bool,
    #[arg(long, conflicts_with = "force", help = english("cli.help.wait_turn"))]
    pub wait_turn: bool,
    #[arg(long, help = english("cli.help.force"))]
    pub force: bool,
    #[arg(long, help = english("cli.help.stop_codex"))]
    pub stop_codex: bool,
    #[arg(long, help = english("cli.help.write_only"))]
    pub write_only: bool,
    #[arg(long = "remote", value_name = "TARGET", help = english("cli.help.remote"))]
    pub remotes: Vec<String>,
    #[arg(long, help = english("cli.help.no_local"))]
    pub no_local: bool,
    #[arg(long, help = english("cli.help.dry_run"))]
    pub dry_run: bool,
}

impl ExecArgs {
    pub(crate) fn targets(&self) -> TargetSelection {
        TargetSelection {
            local: !self.no_local,
            remotes: self.remotes.iter().map(TargetId::new).collect(),
        }
    }
}

/// Plan answers and profile overrides shared by switch and profile use.
#[derive(Debug, Clone, Default, Args)]
pub struct SwitchAnswers {
    #[arg(long, help = english("cli.help.line"))]
    pub line: Option<String>,
    #[arg(long, value_enum, help = english("cli.help.auth"))]
    pub auth: Option<AuthArg>,
    #[arg(long, help = english("cli.help.model"))]
    pub model: Option<String>,
    #[arg(long, conflicts_with = "skip_legacy_sessions", help = english("cli.help.migrate_legacy_sessions"))]
    pub migrate_legacy_sessions: bool,
    #[arg(long, help = english("cli.help.skip_legacy_sessions"))]
    pub skip_legacy_sessions: bool,
    #[arg(long, conflicts_with = "keep_env", help = english("cli.help.take_over_env"))]
    pub take_over_env: bool,
    #[arg(long, help = english("cli.help.keep_env"))]
    pub keep_env: bool,
    #[arg(long, help = english("cli.help.accept_unknown_version"))]
    pub accept_unknown_version: bool,
    #[arg(long, help = english("cli.help.accept_shared_home"))]
    pub accept_shared_home: bool,
    #[arg(long, value_name = "PATH", help = english("cli.help.keep_key"))]
    pub keep_key: Vec<String>,
    #[arg(long, value_name = "PATH", help = english("cli.help.overwrite_key"))]
    pub overwrite_key: Vec<String>,
    #[arg(long, help = english("cli.help.take_over"))]
    pub take_over: bool,
    #[arg(long, value_name = "ID", help = english("cli.help.exclude_thread"))]
    pub exclude_thread: Vec<String>,
}

/// A profile switch.
#[derive(Debug, Clone, Args)]
pub struct SwitchArgs {
    #[arg(long, help = english("cli.help.profile"))]
    pub profile: Option<String>,
    #[arg(long, help = english("cli.help.site"))]
    pub site: Option<String>,
    #[command(flatten)]
    pub answers: SwitchAnswers,
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// Account and usage data of a profile's key.
#[derive(Debug, Clone, Args)]
pub struct AccountArgs {
    #[arg(long, help = english("cli.help.account_profile"))]
    pub profile: Option<String>,
    #[arg(long, help = english("cli.help.line"))]
    pub line: Option<String>,
    #[arg(long, help = english("cli.help.account_usage"))]
    pub usage: bool,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=31), help = english("cli.help.account_days"))]
    pub days: Option<u32>,
    #[arg(long, help = english("cli.help.account_refresh"))]
    pub refresh: bool,
}

impl AccountArgs {
    /// The usage query, when usage was asked for (`--usage`, `--days` or
    /// `--refresh`).
    pub fn usage_query(&self) -> Option<superba_core::UsageQuery> {
        (self.usage || self.days.is_some() || self.refresh).then(|| superba_core::UsageQuery {
            days: self.days.unwrap_or(superba_core::UsageQuery::DEFAULT_DAYS),
            refresh: self.refresh,
        })
    }
}

/// Restore destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RestoreTo {
    Original,
    Official,
    Generation,
}

/// Provider handling for an original configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderArg {
    KeepOpenai,
    ReverseMigrate,
    NoMigrate,
}

impl From<ProviderArg> for ProviderRestore {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::KeepOpenai => Self::KeepOpenai,
            ProviderArg::ReverseMigrate => Self::Baseline {
                reverse_migrate: true,
            },
            ProviderArg::NoMigrate => Self::Baseline {
                reverse_migrate: false,
            },
        }
    }
}

/// Restore syntax.
#[derive(Debug, Clone, Args)]
pub struct RestoreArgs {
    #[arg(value_enum, default_value_t = RestoreTo::Original, help = english("cli.help.restore_to"))]
    pub to: RestoreTo,
    #[arg(long, value_enum, help = english("cli.help.provider"))]
    pub provider: Option<ProviderArg>,
    #[arg(long, required_if_eq("to", "generation"), help = english("cli.help.generation"))]
    pub generation: Option<u64>,
    #[arg(long, help = english("cli.help.restore_take_over"))]
    pub take_over: bool,
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// Who a hand-off names (the attribution Superba made, by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HandOffTo {
    #[value(name = "cc-switch")]
    CcSwitch,
    #[value(name = "ai-toolbox")]
    AiToolbox,
    Other,
}

/// `spb hand-off`.
#[derive(Debug, Clone, Args)]
pub struct HandOffArgs {
    #[arg(long, value_enum, help = english("cli.help.hand_off_to"))]
    pub to: Option<HandOffTo>,
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// Secret management; raw secrets are never accepted on argv.
#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    #[command(about = english("cli.help.key_set"))]
    Set(KeySetArgs),
    #[command(about = english("cli.help.key_check"))]
    Check(KeyCheckArgs),
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("key_source").required(true).args(["key_env", "stdin"])))]
pub struct KeySetArgs {
    #[arg(long, help = english("cli.help.profile"))]
    pub profile: Option<String>,
    #[arg(long, value_name = "VAR", help = english("cli.help.key_env"))]
    pub key_env: Option<String>,
    #[arg(long, visible_alias = "key-stdin", help = english("cli.help.stdin"))]
    pub stdin: bool,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("key_source").args(["key_env", "stdin"])))]
pub struct KeyCheckArgs {
    #[arg(long, help = english("cli.help.profile"))]
    pub profile: Option<String>,
    #[arg(long, help = english("cli.help.site"))]
    pub site: Option<String>,
    #[arg(long, help = english("cli.help.line"))]
    pub line: Option<String>,
    #[arg(long, value_enum, help = english("cli.help.auth"))]
    pub auth: Option<AuthArg>,
    #[arg(long, value_name = "VAR", help = english("cli.help.key_env_read"))]
    pub key_env: Option<String>,
    #[arg(long, visible_alias = "key-stdin", help = english("cli.help.stdin"))]
    pub stdin: bool,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    #[command(about = english("cli.help.profile_list"))]
    List,
    #[command(about = english("cli.help.profile_add"))]
    Add(ProfileAddArgs),
    #[command(about = english("cli.help.profile_edit"))]
    Edit(ProfileEditArgs),
    #[command(about = english("cli.help.profile_remove"))]
    Remove {
        #[arg(help = english("cli.help.id"))]
        id: String,
    },
    #[command(about = english("cli.help.profile_compaction"))]
    Compaction {
        #[arg(help = english("cli.help.id"))]
        id: String,
        #[arg(long, help = english("cli.help.compaction_check"))]
        check: bool,
    },
    #[command(about = english("cli.help.profile_use"))]
    Use {
        #[arg(help = english("cli.help.id"))]
        id: String,
        #[command(flatten)]
        answers: SwitchAnswers,
        #[command(flatten)]
        exec: ExecArgs,
    },
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("key_source").args(["key_env", "key_stdin"])))]
pub struct ProfileAddArgs {
    #[arg(help = english("cli.help.id"))]
    pub id: String,
    #[arg(long, help = english("cli.help.name"))]
    pub name: Option<String>,
    #[arg(long, default_value = catalog::KRILL_SITE_ID, help = english("cli.help.site"))]
    pub site: String,
    #[arg(long = "line", value_name = "LINE", help = english("cli.help.profile_lines"))]
    pub lines: Vec<String>,
    #[arg(long, value_enum, help = english("cli.help.auth_choice"))]
    pub auth: Option<AuthChoiceArg>,
    #[arg(long, value_name = "VAR", help = english("cli.help.key_env"))]
    pub key_env: Option<String>,
    #[arg(long, visible_alias = "stdin", help = english("cli.help.stdin"))]
    pub key_stdin: bool,
    #[arg(long, value_name = "URL", help = english("cli.help.base_url"))]
    pub base_url: Option<String>,
    #[arg(long, help = english("cli.help.model"))]
    pub model: Option<String>,
    #[arg(long, help = english("cli.help.effort"))]
    pub effort: Option<String>,
    #[arg(long, help = english("cli.help.no_recommended_keys"))]
    pub no_recommended_keys: bool,
    #[arg(long, help = english("cli.help.replace"))]
    pub replace: bool,
}

/// `spb profile edit`.
#[derive(Debug, Args)]
pub struct ProfileEditArgs {
    #[arg(help = english("cli.help.id"))]
    pub id: String,
    #[arg(long, help = english("cli.help.name"))]
    pub name: Option<String>,
    #[arg(long = "line", value_name = "LINE", conflicts_with = "auto_line", help = english("cli.help.profile_lines"))]
    pub lines: Vec<String>,
    #[arg(long, help = english("cli.help.auto_line"))]
    pub auto_line: bool,
    #[arg(long, value_enum, help = english("cli.help.auth_choice"))]
    pub auth: Option<AuthChoiceArg>,
    #[arg(long, help = english("cli.help.model"))]
    pub model: Option<String>,
    #[arg(long, help = english("cli.help.effort"))]
    pub effort: Option<String>,
    #[arg(long, value_name = "URL", conflicts_with = "clear_base_url", help = english("cli.help.base_url"))]
    pub base_url: Option<String>,
    #[arg(long, help = english("cli.help.clear_base_url"))]
    pub clear_base_url: bool,
    #[arg(long, value_enum, help = english("cli.help.compaction_choice"))]
    pub compaction: Option<CompactionArg>,
}

/// A provider's remote compaction (`--compaction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompactionArg {
    Auto,
    Native,
    Unsupported,
}

impl From<CompactionArg> for superba_core::CompactionChoice {
    fn from(value: CompactionArg) -> Self {
        match value {
            CompactionArg::Auto => Self::Auto,
            CompactionArg::Native => Self::Native,
            CompactionArg::Unsupported => Self::Unsupported,
        }
    }
}

/// `spb settings`.
#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    #[command(about = english("cli.help.settings_list"))]
    List {
        #[arg(value_name = "PATH", help = english("cli.help.settings_paths"))]
        paths: Vec<String>,
        #[arg(long, help = english("cli.help.settings_profile"))]
        profile: Option<String>,
        #[arg(long, help = english("cli.help.settings_advanced"))]
        advanced: bool,
    },
    #[command(about = english("cli.help.settings_get"))]
    Get {
        #[arg(value_name = "PATH", help = english("cli.help.settings_path"))]
        path: String,
        #[arg(long, help = english("cli.help.settings_profile"))]
        profile: Option<String>,
    },
    #[command(about = english("cli.help.settings_set"))]
    Set {
        #[arg(value_name = "PATH", help = english("cli.help.settings_path"))]
        path: String,
        #[arg(value_name = "VALUE", allow_hyphen_values = true, help = english("cli.help.settings_value"))]
        value: String,
        #[arg(long, help = english("cli.help.settings_profile"))]
        profile: Option<String>,
        #[arg(long, help = english("cli.help.settings_force"))]
        force: bool,
    },
    #[command(about = english("cli.help.settings_reset"))]
    Reset {
        #[arg(required = true, value_name = "PATH", help = english("cli.help.settings_path"))]
        paths: Vec<String>,
        #[arg(long, help = english("cli.help.settings_profile"))]
        profile: Option<String>,
    },
    #[command(about = english("cli.help.settings_forget"))]
    Forget {
        #[arg(required = true, value_name = "PATH", help = english("cli.help.settings_path"))]
        paths: Vec<String>,
        #[arg(long, help = english("cli.help.settings_profile"))]
        profile: Option<String>,
    },
    #[command(about = english("cli.help.settings_migrate"))]
    Migrate {
        #[arg(required = true, value_name = "LEGACY_PATH", help = english("cli.help.settings_legacy"))]
        legacy: Vec<String>,
        #[arg(long, help = english("cli.help.settings_profile"))]
        profile: Option<String>,
    },
    #[command(about = english("cli.help.settings_diff"))]
    Diff {
        #[arg(long, help = english("cli.help.settings_profile"))]
        profile: Option<String>,
        #[arg(long = "remote", value_name = "TARGET", help = english("cli.help.remote"))]
        remotes: Vec<String>,
    },
}

/// `spb router`.
#[derive(Debug, Subcommand)]
pub enum RouterCommand {
    #[command(about = english("cli.help.router_status"))]
    Status,
    #[command(about = english("cli.help.router_install"))]
    Install {
        #[arg(long = "member", value_name = "PROFILE", help = english("cli.help.router_member"))]
        members: Vec<String>,
    },
    #[command(about = english("cli.help.router_enable"))]
    Enable {
        #[arg(long = "member", value_name = "PROFILE", help = english("cli.help.router_member"))]
        members: Vec<String>,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.router_disable"))]
    Disable {
        #[arg(long, help = english("cli.help.router_disable_uninstall"))]
        uninstall: bool,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.router_uninstall"))]
    Uninstall {
        /// Used by the Windows installer: no output, never fails; also switches Codex back to
        /// direct, and stops the router processes of this install.
        #[arg(long, hide = true)]
        quiet: bool,
        /// With `--quiet`: only stop the router processes of this install (an upgrade).
        #[arg(long, hide = true, requires = "quiet")]
        keep_service: bool,
    },
    #[command(about = english("cli.help.router_doctor"))]
    Doctor {
        #[arg(long, help = english("cli.help.router_doctor_foreground"))]
        foreground: bool,
    },
    #[command(about = english("cli.help.router_watch"))]
    Watch {
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=600), help = english("cli.help.router_interval"))]
        interval: u64,
        #[arg(long, help = english("cli.help.router_once"))]
        once: bool,
    },
    #[command(about = english("cli.help.router_stats_line"))]
    StatsLine {
        #[arg(value_enum, help = english("cli.help.stats_line_state"))]
        state: Option<OnOff>,
        #[arg(long, value_enum, value_delimiter = ',', help = english("cli.help.stats_line_clients"))]
        clients: Vec<StatsClientArg>,
        #[arg(long, help = english("cli.help.stats_line_template"))]
        template: Option<String>,
        #[arg(long, help = english("cli.help.stats_line_italic"))]
        italic: bool,
    },
    #[command(about = english("cli.help.router_record"))]
    Record {
        #[arg(value_enum, help = english("cli.help.on_off"))]
        state: OnOff,
    },
    #[command(about = english("cli.help.router_clear"))]
    Clear {
        #[arg(long, value_parser = parse_age, value_name = "AGE", help = english("cli.help.router_older_than"))]
        older_than: Option<u64>,
        #[arg(short = 'y', long, help = english("cli.help.router_clear_yes"))]
        yes: bool,
    },
}

/// `on` / `off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

impl OnOff {
    /// True for `on`.
    pub fn is_on(self) -> bool {
        self == OnOff::On
    }
}

/// `spb usage`: the request log (`spb usage` / `spb usage log`) and its
/// statistics (`spb usage stats`), from the local router's database merged with
/// Codex's session files, so it works without the router.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct UsageArgs {
    #[command(subcommand)]
    pub command: Option<UsageCommand>,
    /// Filters of the log when no subcommand is given.
    #[command(flatten)]
    pub log: RouterLogArgs,
}

/// `spb usage` subcommands.
#[derive(Debug, Subcommand)]
pub enum UsageCommand {
    #[command(about = english("cli.help.usage_log"))]
    Log(RouterLogArgs),
    #[command(about = english("cli.help.usage_stats"))]
    Stats {
        #[arg(long, value_enum, default_value_t = StatsByArg::Model, help = english("cli.help.router_stats_by"))]
        by: StatsByArg,
        #[arg(long, default_value = "7d", value_parser = parse_age, help = english("cli.help.router_since"))]
        since: u64,
        #[arg(long, value_enum, help = english("cli.help.usage_source"))]
        source: Option<UsageSourceArg>,
    },
}

/// `spb usage log` filters.
#[derive(Debug, Clone, Args)]
pub struct RouterLogArgs {
    #[arg(long, default_value = "1h", value_parser = parse_age, help = english("cli.help.router_since"))]
    pub since: u64,
    #[arg(long, value_name = "PROFILE", help = english("cli.help.router_log_provider"))]
    pub provider: Option<String>,
    #[arg(long, help = english("cli.help.router_log_model"))]
    pub model: Option<String>,
    #[arg(long, help = english("cli.help.router_log_errors"))]
    pub errors: bool,
    #[arg(long, conflicts_with = "errors", help = english("cli.help.router_log_failover"))]
    pub failover: bool,
    #[arg(long, value_enum, help = english("cli.help.router_log_client"))]
    pub client: Option<LogClientArg>,
    #[arg(long, value_enum, help = english("cli.help.usage_source"))]
    pub source: Option<UsageSourceArg>,
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000), help = english("cli.help.router_log_limit"))]
    pub limit: u32,
}

/// Where usage rows come from (`--source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum UsageSourceArg {
    Router,
    Rollout,
    #[value(alias = "cc-switch")]
    Ccs,
    #[value(name = "ai-toolbox", alias = "aitoolbox")]
    AiToolbox,
}

impl From<UsageSourceArg> for superba_core::RequestSource {
    fn from(value: UsageSourceArg) -> Self {
        match value {
            UsageSourceArg::Router => Self::Router,
            UsageSourceArg::Rollout => Self::Rollout,
            UsageSourceArg::Ccs => Self::CcSwitch,
            UsageSourceArg::AiToolbox => Self::AiToolbox,
        }
    }
}

/// Request log clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogClientArg {
    App,
    Cli,
    Exec,
}

/// Stats line clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatsClientArg {
    App,
    Tui,
}

/// `spb usage stats --by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatsByArg {
    Model,
    Provider,
    Day,
    Session,
}

/// An age such as `90s`, `30m`, `1h`, `7d`, `2w` in seconds.
pub fn parse_age(value: &str) -> Result<u64, String> {
    let v = value.trim();
    let (num, unit) = v.split_at(v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| english("cli.error.age").to_owned())?;
    let scale = match unit {
        "s" => 1,
        "m" | "min" => 60,
        "h" | "" => 3_600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return Err(english("cli.error.age").to_owned()),
    };
    n.checked_mul(scale)
        .filter(|s| *s > 0)
        .ok_or_else(|| english("cli.error.age").to_owned())
}

/// `spb models`.
#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    #[command(about = english("cli.help.models_list"))]
    List,
    #[command(about = english("cli.help.models_default"))]
    Default(ModelsDefaultArgs),
    #[command(about = english("cli.help.models_set"))]
    Set(ModelsSetArgs),
    #[command(about = english("cli.help.models_reset"))]
    Reset(ModelsResetArgs),
    #[command(about = english("cli.help.models_picker"))]
    Picker {
        #[arg(value_enum, help = english("cli.help.on_off"))]
        state: OnOff,
        #[command(flatten)]
        exec: ExecArgs,
    },
}

/// `spb models default`.
#[derive(Debug, Args)]
pub struct ModelsDefaultArgs {
    #[arg(value_name = "MODEL", help = english("cli.help.models_slug"))]
    pub slug: Option<String>,
    #[arg(long, help = english("cli.help.effort"))]
    pub effort: Option<String>,
    #[arg(long, value_name = "TOKENS", help = english("cli.help.models_all_context"))]
    pub context: Option<String>,
    #[arg(long, value_name = "TOKENS|PERCENT", help = english("cli.help.models_all_compact"))]
    pub compact: Option<String>,
    #[arg(long, help = english("cli.help.models_clear_all"))]
    pub clear: bool,
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// `spb models set`.
#[derive(Debug, Args)]
pub struct ModelsSetArgs {
    #[arg(value_name = "MODEL", help = english("cli.help.models_slug"))]
    pub slug: String,
    #[arg(long, value_name = "TOKENS", help = english("cli.help.models_context"))]
    pub context: Option<String>,
    #[arg(long, value_name = "TOKENS|PERCENT", help = english("cli.help.models_compact"))]
    pub compact: Option<String>,
    #[arg(long, help = english("cli.help.models_name"))]
    pub name: Option<String>,
    #[arg(long, conflicts_with = "show", help = english("cli.help.models_hide"))]
    pub hide: bool,
    #[arg(long, help = english("cli.help.models_show"))]
    pub show: bool,
    #[arg(long, value_name = "NAME", help = english("cli.help.models_upstream_name"))]
    pub upstream_name: Option<String>,
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// `spb models reset`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("which").required(true).args(["slug", "all"])))]
pub struct ModelsResetArgs {
    #[arg(value_name = "MODEL", help = english("cli.help.models_slug"))]
    pub slug: Option<String>,
    #[arg(long, help = english("cli.help.models_reset_all"))]
    pub all: bool,
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// `spb speed`.
#[derive(Debug, Args)]
pub struct SpeedArgs {
    #[arg(value_enum, help = english("cli.help.speed_tier"))]
    pub tier: Option<SpeedArg>,
    #[arg(long, help = english("cli.help.models_profile"))]
    pub profile: Option<String>,
    #[command(flatten)]
    pub exec: ExecArgs,
}

/// Speed tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SpeedArg {
    Standard,
    Fast,
    Ultrafast,
}

impl From<SpeedArg> for superba_core::SpeedTier {
    fn from(value: SpeedArg) -> Self {
        match value {
            SpeedArg::Standard => Self::Standard,
            SpeedArg::Fast => Self::Fast,
            SpeedArg::Ultrafast => Self::Ultrafast,
        }
    }
}

/// `spb codex`.
#[derive(Debug, Subcommand)]
pub enum CodexCommand {
    #[command(about = english("cli.help.codex_list"))]
    List {
        #[arg(long, help = english("cli.help.codex_list_all"))]
        all: bool,
        #[arg(long, help = english("cli.help.versions_refresh"))]
        refresh: bool,
    },
    #[command(about = english("cli.help.codex_install"))]
    Install {
        #[arg(value_name = "VERSION", help = english("cli.help.codex_version"))]
        version: String,
        #[arg(long, help = english("cli.help.codex_install_local"))]
        local: bool,
    },
    #[command(about = english("cli.help.codex_pin"))]
    Pin {
        #[arg(value_name = "VERSION|PATH|bundled", help = english("cli.help.codex_pin_target"))]
        target: String,
        #[arg(long, help = english("cli.help.codex_persist_env"))]
        persist_env: bool,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.codex_unpin"))]
    Unpin {
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.codex_rollback"))]
    Rollback,
}

/// `spb app`.
#[derive(Debug, Subcommand)]
pub enum AppCommand {
    #[command(about = english("cli.help.app_check"))]
    Check {
        #[arg(long, help = english("cli.help.versions_refresh"))]
        refresh: bool,
    },
    #[command(about = english("cli.help.app_download"))]
    Download {
        #[arg(long, help = english("cli.help.app_download_open"))]
        open: bool,
        #[arg(long, value_enum, help = english("cli.help.app_download_source"))]
        source: Option<AppSourceArg>,
    },
}

/// `spb app download --source`: where the Windows package comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AppSourceArg {
    /// OpenAI's download server only.
    Official,
    /// The Microsoft Store CDN only (links resolved by store.rg-adguard.net).
    Store,
}

impl From<AppSourceArg> for superba_core::WindowsAppSource {
    fn from(value: AppSourceArg) -> Self {
        match value {
            AppSourceArg::Official => Self::OfficialOnly,
            AppSourceArg::Store => Self::StoreMirrorOnly,
        }
    }
}

/// `spb import`: providers (or, with `usage`, the usage history) from another
/// tool.
#[derive(Debug, Clone, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct ImportArgs {
    #[command(subcommand)]
    pub what: Option<ImportCommand>,
    #[arg(value_enum, required = true, help = english("cli.import.help.source"))]
    pub source: Option<ImportSourceArg>,
    #[arg(long, help = english("cli.import.help.dry_run"))]
    pub dry_run: bool,
    #[arg(long, value_name = "NAMES", value_delimiter = ',', help = english("cli.import.help.select"))]
    pub select: Vec<String>,
    #[arg(long, value_enum, default_value_t = ConflictArg::Skip, help = english("cli.import.help.on_conflict"))]
    pub on_conflict: ConflictArg,
}

/// `spb import usage`.
#[derive(Debug, Clone, Subcommand)]
pub enum ImportCommand {
    #[command(about = english("cli.import_usage.help.about"))]
    Usage(ImportUsageArgs),
}

/// `spb import usage ccs|ai-toolbox`.
#[derive(Debug, Clone, Args)]
pub struct ImportUsageArgs {
    #[arg(value_enum, help = english("cli.import.help.source"))]
    pub source: ImportSourceArg,
    #[arg(long, help = english("cli.import_usage.help.dry_run"))]
    pub dry_run: bool,
    #[arg(long, help = english("cli.import_usage.help.remove"))]
    pub remove: bool,
}

/// Tools `spb import` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ImportSourceArg {
    #[value(alias = "cc-switch")]
    Ccs,
    #[value(name = "ai-toolbox", alias = "aitoolbox")]
    AiToolbox,
}

impl From<ImportSourceArg> for superba_core::ImportSource {
    fn from(value: ImportSourceArg) -> Self {
        match value {
            ImportSourceArg::Ccs => Self::Ccs,
            ImportSourceArg::AiToolbox => Self::AiToolbox,
        }
    }
}

/// What `spb import` does with a provider that collides with an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConflictArg {
    Skip,
    Rename,
    Update,
}

impl From<ConflictArg> for superba_core::ConflictAction {
    fn from(value: ConflictArg) -> Self {
        match value {
            ConflictArg::Skip => Self::Skip,
            ConflictArg::Rename => Self::Rename,
            ConflictArg::Update => Self::Update,
        }
    }
}

/// `spb self-update`.
#[derive(Debug, Subcommand)]
pub enum SelfUpdateCommand {
    #[command(about = english("cli.help.self_update_check"))]
    Check,
    #[command(about = english("cli.help.self_update_install"))]
    Install {
        #[arg(short = 'y', long, help = english("cli.help.self_update_yes"))]
        yes: bool,
    },
    /// Restart the router service onto the new binary (the Windows hand-off runs it).
    #[command(hide = true)]
    RestartRouter,
}

/// `spb feeds`.
#[derive(Debug, Subcommand)]
pub enum FeedsCommand {
    #[command(about = english("cli.help.feeds_status"))]
    Status,
    #[command(about = english("cli.help.feeds_refresh"))]
    Refresh,
}

#[derive(Debug, Subcommand)]
pub enum SessionsCommand {
    #[command(about = english("cli.help.scan"))]
    Scan,
    #[command(about = english("cli.help.migrate"))]
    Migrate {
        #[arg(long, value_name = "PROVIDER", help = english("cli.help.from"))]
        from: String,
        #[arg(long, default_value = superba_core::codex::PROVIDER_OPENAI, value_name = "PROVIDER", help = english("cli.help.to"))]
        to: String,
        #[arg(long, help = english("cli.help.no_archived"))]
        no_archived: bool,
        #[arg(long, value_name = "ID", help = english("cli.help.exclude_thread"))]
        exclude_thread: Vec<String>,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.rollback_migration"))]
    Rollback {
        #[arg(help = english("cli.help.migration_id"))]
        migration_id: String,
        #[command(flatten)]
        exec: ExecArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum SiteCommand {
    #[command(about = english("cli.help.site_list"))]
    List,
    #[command(about = english("cli.help.site_add"))]
    Add {
        #[arg(help = english("cli.help.site_id"))]
        id: String,
        #[arg(long, help = english("cli.help.name"))]
        name: Option<String>,
        #[arg(long, value_name = "URL", help = english("cli.help.site_base_url"))]
        base_url: String,
        #[arg(long = "map", value_name = "CODEX=SITE", value_parser = parse_model_mapping, help = english("cli.help.site_map"))]
        map: Vec<(String, String)>,
    },
    #[command(about = english("cli.help.site_edit"))]
    Edit(SiteEditArgs),
    #[command(about = english("cli.help.site_remove"))]
    Remove {
        #[arg(help = english("cli.help.site_id"))]
        id: String,
    },
    #[command(about = english("cli.help.site_probe"))]
    Probe(SiteProbeArgs),
}

/// `spb site edit`.
#[derive(Debug, Args)]
pub struct SiteEditArgs {
    #[arg(help = english("cli.help.site_id"))]
    pub id: String,
    #[arg(long, help = english("cli.help.name"))]
    pub name: Option<String>,
    #[arg(long, value_name = "URL", help = english("cli.help.site_base_url"))]
    pub base_url: Option<String>,
    #[arg(long = "map", value_name = "CODEX=SITE", value_parser = parse_model_mapping, help = english("cli.help.site_map"))]
    pub map: Vec<(String, String)>,
    #[arg(long = "unmap", value_name = "CODEX", help = english("cli.help.site_unmap"))]
    pub unmap: Vec<String>,
    #[arg(long, help = english("cli.help.site_clear_map"))]
    pub clear_map: bool,
}

/// `spb site probe`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("key_source").args(["key_env", "stdin", "profile"])))]
pub struct SiteProbeArgs {
    #[arg(help = english("cli.help.site_id"))]
    pub id: String,
    #[arg(long, help = english("cli.help.line"))]
    pub line: Option<String>,
    #[arg(long, value_name = "VAR", help = english("cli.help.key_env_read"))]
    pub key_env: Option<String>,
    #[arg(long, visible_alias = "key-stdin", help = english("cli.help.stdin"))]
    pub stdin: bool,
    #[arg(long, help = english("cli.help.site_probe_profile"))]
    pub profile: Option<String>,
    #[arg(long, help = english("cli.help.site_probe_model"))]
    pub model: Option<String>,
    #[arg(long, help = english("cli.help.site_probe_responses"))]
    pub responses: bool,
}

/// `codex-name=site-name` for `--map`.
fn parse_model_mapping(value: &str) -> Result<(String, String), String> {
    let (codex, site) = value
        .split_once('=')
        .ok_or_else(|| english("cli.error.model_mapping").to_owned())?;
    let (codex, site) = (codex.trim(), site.trim());
    if codex.is_empty() || site.is_empty() {
        return Err(english("cli.error.model_mapping").to_owned());
    }
    Ok((codex.to_owned(), site.to_owned()))
}

#[derive(Debug, Subcommand)]
pub enum RemoteCommand {
    #[command(about = english("cli.help.remote_list"))]
    List,
    #[command(about = english("cli.help.remote_apply"))]
    Apply {
        #[arg(required = true, num_args = 1.., value_name = "TARGET", help = english("cli.help.target"))]
        targets: Vec<String>,
        #[arg(long, help = english("cli.help.profile"))]
        profile: Option<String>,
        #[arg(long, help = english("cli.help.line"))]
        line: Option<String>,
        #[arg(long, value_enum, help = english("cli.help.auth"))]
        auth: Option<AuthArg>,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.remote_rollback"))]
    Rollback {
        #[arg(help = english("cli.help.target"))]
        target: String,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.remote_probe"))]
    Probe {
        #[arg(help = english("cli.help.target"))]
        target: String,
    },
    #[command(about = english("cli.help.remote_follow"))]
    Follow {
        #[arg(help = english("cli.help.target"))]
        target: String,
        #[arg(value_enum, help = english("cli.help.on_off"))]
        state: OnOff,
    },
    #[command(about = english("cli.help.remote_sync"))]
    Sync {
        #[arg(help = english("cli.help.target"))]
        target: String,
        #[arg(long, value_delimiter = ',', value_name = "PATH", help = english("cli.help.remote_sync_only"))]
        only: Vec<String>,
        #[command(flatten)]
        exec: ExecArgs,
    },
    #[command(about = english("cli.help.remote_codex"))]
    Codex {
        #[arg(help = english("cli.help.target"))]
        target: String,
        #[command(subcommand)]
        action: RemoteCodexCommand,
    },
}

/// `spb wsl ...` (user bug 45).
#[derive(Debug, Subcommand)]
pub enum WslCommand {
    #[command(about = english("cli.help.wsl_list"))]
    List,
    #[command(name = "router-access", about = english("cli.help.wsl_router_access"))]
    RouterAccess {
        #[arg(value_name = "DISTRO", help = english("cli.help.wsl_distro"))]
        distro: String,
    },
}

/// `spb remote codex <host> ...`.
#[derive(Debug, Subcommand)]
pub enum RemoteCodexCommand {
    #[command(about = english("cli.help.remote_codex_install"))]
    Install {
        #[arg(value_name = "VERSION", help = english("cli.help.codex_version"))]
        version: String,
    },
    #[command(about = english("cli.help.remote_codex_rollback"))]
    Rollback {
        #[arg(long, value_name = "VERSION", help = english("cli.help.remote_codex_rollback_to"))]
        to: Option<String>,
    },
}
