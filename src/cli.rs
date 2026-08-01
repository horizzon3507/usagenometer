//! Command-line interface for usagenometer.

use clap::{
    Parser, Subcommand, ValueEnum,
    builder::styling::{AnsiColor, Effects, Styles},
};

fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::White.on_default() | Effects::BOLD)
        .usage(AnsiColor::White.on_default() | Effects::BOLD)
        .literal(AnsiColor::BrightWhite.on_default())
        .placeholder(AnsiColor::BrightBlack.on_default())
        .error(AnsiColor::BrightRed.on_default() | Effects::BOLD)
        .valid(AnsiColor::BrightWhite.on_default())
        .invalid(AnsiColor::BrightRed.on_default())
}

/// Which meter value to emphasize in the panel.
#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq, Eq)]
pub enum DisplayMode {
    /// Remaining quota (default)
    #[default]
    Left,
    /// Used quota
    Used,
}

/// Output format for status / json / check.
#[derive(Debug, Clone, Copy, ValueEnum, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Prometheus,
}

/// Shell for completions.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ShellArg {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

/// Known AI usage providers.
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum ProviderArg {
    Codex,
    Cursor,
    Antigravity,
    Claude,
    Grok,
}

impl ProviderArg {
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Antigravity => "antigravity",
            Self::Claude => "claude",
            Self::Grok => "grok",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Codex,
            Self::Cursor,
            Self::Antigravity,
            Self::Claude,
            Self::Grok,
        ]
    }
}

/// usagenometer — AI usage meters in the terminal
///
/// Short for “usage meter”. Invoke as `usagenometer` or `usg`.
#[derive(Debug, Parser)]
#[command(
    name = "usagenometer",
    version,
    about = "◈ usagenometer — AI usage meters in the terminal",
    long_about = "usagenometer — show Codex, Cursor, Antigravity (and related) AI quotas.\n\
\n\
  binaries   usagenometer · usg\n\
  sources    local auth (Codex CLI, Cursor, secret store) — no secrets stored\n\
  config     ~/.config/usagenometer/config.toml\n\
  companion  GNOME Shell extension (beta)",
    after_help = "Shortcuts:\n\
  usg                  same as  usg status\n\
  usg st               same as  usg status\n\
  usg w                same as  usg watch\n\
  usg t                same as  usg test\n\
  usg ls               same as  usg providers\n\
  usg j                same as  usg json\n\
\n\
Examples:\n\
  usg\n\
  usg -c\n\
  usg status -p codex -p cursor\n\
  usg watch --interval 60 --alert 80 --diff\n\
  usg check --fail-under 10\n\
  usg doctor\n\
  usg explain codex\n\
  usg history\n\
  usg tui\n\
  usg completions zsh\n\
  usg --help",
    styles = cli_styles(),
    propagate_version = true,
    arg_required_else_help = false,
    disable_help_subcommand = false,
)]
pub struct Cli {
    /// Emphasize left (remaining) or used percent
    #[arg(
        long = "display",
        global = true,
        value_enum,
        value_name = "MODE",
        help = "Meter emphasis: left (default) or used"
    )]
    pub display: Option<DisplayMode>,

    /// Only these providers (repeatable). Default: all / config.
    #[arg(
        short = 'p',
        long = "provider",
        global = true,
        value_enum,
        value_name = "NAME",
        help = "Limit to provider(s); repeatable"
    )]
    pub providers: Vec<ProviderArg>,

    /// Suppress the startup banner
    #[arg(
        short = 'q',
        long = "quiet",
        global = true,
        help = "Quiet mode (less stdout noise)"
    )]
    pub quiet: bool,

    /// Compact one-liner (statuslines)
    #[arg(
        short = 'c',
        long = "compact",
        global = true,
        help = "Compact one-liner output"
    )]
    pub compact: bool,

    /// Hide/redact account emails
    #[arg(
        long = "privacy",
        global = true,
        help = "Redact account identifiers"
    )]
    pub privacy: bool,

    /// Alert when used % reaches this threshold (0–100)
    #[arg(
        long = "alert",
        global = true,
        value_name = "PCT",
        help = "Warn when used %% >= PCT (overrides config)"
    )]
    pub alert: Option<f64>,

    /// Alert when exhaustion ETA is within this many hours (needs history samples)
    #[arg(
        long = "alert-eta",
        global = true,
        value_name = "HOURS",
        help = "Warn when ETA <= HOURS (needs history)"
    )]
    pub alert_eta: Option<f64>,

    /// Enable desktop notify-send for alerts
    #[arg(
        long = "notify",
        global = true,
        help = "Desktop notification on alerts (notify-send)"
    )]
    pub notify: bool,

    /// Machine-readable JSON (applies to status / watch)
    #[arg(
        long = "json",
        global = true,
        help = "Emit JSON instead of the text panel"
    )]
    pub json: bool,

    /// Pretty-print JSON when --json / json command
    #[arg(
        long = "pretty",
        global = true,
        help = "Pretty-print JSON output"
    )]
    pub pretty: bool,

    /// Output format (text / json / prometheus)
    #[arg(
        long = "format",
        global = true,
        value_enum,
        value_name = "FMT",
        help = "Output format: text, json, prometheus"
    )]
    pub format: Option<OutputFormat>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show current AI usage meters (default)
    #[command(visible_aliases = ["st", "s"])]
    Status {
        /// Compact one-liner
        #[arg(short = 'c', long = "compact")]
        compact: bool,
    },

    /// Refresh meters on an interval
    #[command(visible_alias = "w")]
    Watch {
        /// Seconds between refreshes
        #[arg(short, long, value_name = "SECS")]
        interval: Option<u64>,
        /// Alert threshold (used %)
        #[arg(long, value_name = "PCT")]
        alert: Option<f64>,
        /// Only print meters that changed
        #[arg(long)]
        diff: bool,
    },

    /// Test provider auth + API connectivity
    #[command(visible_alias = "t")]
    Test {
        /// Optional single provider (default: all selected / all)
        #[arg(value_enum, value_name = "PROVIDER")]
        provider: Option<ProviderArg>,
    },

    /// List known providers
    #[command(visible_aliases = ["ls", "list"])]
    Providers,

    /// Dump snapshots as JSON
    #[command(visible_alias = "j")]
    Json,

    /// Exit non-zero if remaining % below threshold
    Check {
        /// Fail when any meter remaining % is below this (0–100)
        #[arg(long = "fail-under", value_name = "PCT", default_value_t = 10.0)]
        fail_under: f64,
    },

    /// Diagnose auth paths / expiry (no secrets)
    Doctor,

    /// Explain what provider meters mean
    Explain {
        #[arg(value_enum, value_name = "PROVIDER")]
        provider: Option<ProviderArg>,
    },

    /// Show recent local history snapshots
    History {
        /// Max rows
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Filter provider
        #[arg(value_enum, value_name = "PROVIDER")]
        provider: Option<ProviderArg>,
        /// Show sparkline for burn rate
        #[arg(long)]
        spark: bool,
    },

    /// Show config path / effective settings
    Config {
        /// Dump effective config as TOML
        #[arg(long)]
        dump: bool,
    },

    /// Interactive TUI
    Tui,

    /// Generate shell completions to stdout
    Completions {
        #[arg(value_enum, value_name = "SHELL")]
        shell: ShellArg,
    },

    /// Print version
    #[command(visible_alias = "ver")]
    Version,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_default_status() {
        let cli = Cli::parse_from(["usg"]);
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_watch_interval() {
        let cli = Cli::parse_from(["usg", "watch", "-i", "30"]);
        match cli.command {
            Some(Command::Watch { interval, .. }) => assert_eq!(interval, Some(30)),
            _ => panic!("expected watch"),
        }
    }

    #[test]
    fn parses_providers_filter() {
        let cli = Cli::parse_from(["usg", "status", "-p", "codex", "-p", "cursor"]);
        assert_eq!(cli.providers.len(), 2);
        assert_eq!(cli.providers[0], ProviderArg::Codex);
    }

    #[test]
    fn parses_compact_and_alert() {
        let cli = Cli::parse_from(["usg", "--compact", "--alert", "80"]);
        assert!(cli.compact);
        assert_eq!(cli.alert, Some(80.0));
    }

    #[test]
    fn parses_check() {
        let cli = Cli::parse_from(["usg", "check", "--fail-under", "5"]);
        match cli.command {
            Some(Command::Check { fail_under }) => assert_eq!(fail_under, 5.0),
            _ => panic!("expected check"),
        }
    }

    #[test]
    fn parses_completions() {
        let cli = Cli::parse_from(["usg", "completions", "zsh"]);
        match cli.command {
            Some(Command::Completions { shell }) => {
                assert!(matches!(shell, ShellArg::Zsh));
            }
            _ => panic!("expected completions"),
        }
    }
}
