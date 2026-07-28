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
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum DisplayMode {
    /// Remaining quota (default)
    #[default]
    Left,
    /// Used quota
    Used,
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
  usg status -p codex -p cursor\n\
  usg watch --interval 60\n\
  usg test antigravity\n\
  usg json --pretty\n\
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
        default_value_t = DisplayMode::Left,
        value_name = "MODE",
        help = "Meter emphasis: left (default) or used"
    )]
    pub display: DisplayMode,

    /// Only these providers (repeatable). Default: all.
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

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show current AI usage meters (default)
    #[command(visible_aliases = ["st", "s"])]
    Status,

    /// Refresh meters on an interval
    #[command(visible_alias = "w")]
    Watch {
        /// Seconds between refreshes
        #[arg(short, long, default_value_t = 60, value_name = "SECS")]
        interval: u64,
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
            Some(Command::Watch { interval }) => assert_eq!(interval, 30),
            _ => panic!("expected watch"),
        }
    }

    #[test]
    fn parses_providers_filter() {
        let cli = Cli::parse_from(["usg", "status", "-p", "codex", "-p", "cursor"]);
        assert_eq!(cli.providers.len(), 2);
        assert_eq!(cli.providers[0], ProviderArg::Codex);
    }
}
