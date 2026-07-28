//! usagenometer — AI usage meters in the terminal.
//!
//! Binaries: `usagenometer` and short alias `usg`.

use std::io;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossterm::style::Stylize;
use crossterm::terminal::{Clear, ClearType};
use crossterm::{ExecutableCommand, cursor};

use usagenometer::cli::{Cli, Command, ProviderArg};
use usagenometer::providers::{self, resolve_providers};
use usagenometer::ui::{
    WHITE, banner, bin_name, flush_stdout, print_error, print_info, print_status, print_success,
    print_warn,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("{} {err}", "error:".with(WHITE));
        for cause in err.chain().skip(1) {
            eprintln!("  {} {cause}", "↳".with(usagenometer::ui::DIM));
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let quiet = cli.quiet;
    let _bin = bin_name();

    match cli.command {
        None | Some(Command::Status) => {
            cmd_status(&cli, quiet)?;
        }
        Some(Command::Watch { interval }) => {
            cmd_watch(&cli, interval.max(5), quiet)?;
        }
        Some(Command::Test { provider }) => {
            cmd_test(&cli, provider, quiet)?;
        }
        Some(Command::Providers) => {
            cmd_providers(quiet);
        }
        Some(Command::Json) => {
            cmd_json(&cli)?;
        }
        Some(Command::Version) => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}

fn cmd_status(cli: &Cli, quiet: bool) -> Result<()> {
    if cli.json {
        return cmd_json(cli);
    }
    if !quiet {
        banner();
    }
    let snaps = providers::fetch_all(&cli.providers);
    print_status(&snaps, cli.display);
    Ok(())
}

fn cmd_watch(cli: &Cli, interval: u64, quiet: bool) -> Result<()> {
    if cli.json {
        loop {
            let snaps = providers::fetch_all(&cli.providers);
            emit_json(&snaps, cli.pretty)?;
            thread::sleep(Duration::from_secs(interval));
        }
    }

    loop {
        let mut stdout = io::stdout();
        let _ = stdout.execute(cursor::MoveTo(0, 0));
        let _ = stdout.execute(Clear(ClearType::All));
        if !quiet {
            banner();
            print_info(&format!("refresh every {interval}s · Ctrl-C to quit"));
            println!();
        }
        let snaps = providers::fetch_all(&cli.providers);
        print_status(&snaps, cli.display);
        flush_stdout();
        thread::sleep(Duration::from_secs(interval));
    }
}

fn cmd_test(cli: &Cli, provider: Option<ProviderArg>, quiet: bool) -> Result<()> {
    if !quiet {
        banner();
    }
    let ids: Vec<&str> = match provider {
        Some(p) => vec![p.id()],
        None => resolve_providers(&cli.providers),
    };

    let mut failures = 0usize;
    for id in ids {
        let (ok, message, _) = providers::test_provider(id);
        if ok {
            print_success(&format!("{:<12} {message}", providers::provider_label(id)));
        } else {
            failures += 1;
            print_warn(&format!("{:<12} {message}", providers::provider_label(id)));
        }
    }
    println!();
    if failures > 0 {
        print_error(&format!("{failures} provider(s) failed"));
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_providers(quiet: bool) {
    if !quiet {
        banner();
    }
    for p in ProviderArg::all() {
        println!(
            "  {}  {}",
            p.id().with(WHITE),
            providers::provider_label(p.id()).with(usagenometer::ui::GRAY)
        );
    }
    println!();
}

fn cmd_json(cli: &Cli) -> Result<()> {
    let snaps = providers::fetch_all(&cli.providers);
    emit_json(&snaps, cli.pretty)
}

fn emit_json(snaps: &[usagenometer::providers::types::ProviderSnapshot], pretty: bool) -> Result<()> {
    if pretty {
        serde_json::to_writer_pretty(io::stdout().lock(), snaps)?;
    } else {
        serde_json::to_writer(io::stdout().lock(), snaps)?;
    }
    println!();
    Ok(())
}
