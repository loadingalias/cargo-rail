mod adapters;
mod cargo;
mod checks;
mod commands;
mod core;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Split Rust crates from monorepos, keep them in sync
#[derive(Parser)]
#[command(name = "cargo")]
#[command(bin_name = "cargo")]
#[command(styles = get_styles())]
enum CargoCli {
  Rail(RailCli),
}

#[derive(Parser)]
#[command(name = "rail")]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(styles = get_styles())]
struct RailCli {
  #[command(subcommand)]
  command: Commands,
}

#[derive(Subcommand)]
enum Commands {
  /// Initialize cargo-rail configuration for a workspace
  Init {
    /// Process all crates in the workspace
    #[arg(short, long)]
    all: bool,
  },
  /// Split a crate from monorepo to separate repo with history
  Split {
    /// Name of the crate to split
    crate_name: Option<String>,
    /// Split all configured crates
    #[arg(short, long)]
    all: bool,
    /// Actually perform the split (default: dry-run mode showing plan)
    #[arg(long)]
    apply: bool,
    /// Output plan in JSON format (useful for CI/automation)
    #[arg(long)]
    json: bool,
  },
  /// Sync changes between monorepo and split repos
  Sync {
    /// Name of the crate to sync
    crate_name: Option<String>,
    /// Sync all configured crates
    #[arg(short, long)]
    all: bool,
    /// Only sync from remote to monorepo
    #[arg(long)]
    from_remote: bool,
    /// Only sync from monorepo to remote
    #[arg(long)]
    to_remote: bool,
    /// Conflict resolution strategy: ours (use monorepo), theirs (use remote), manual (create markers), union (combine both)
    #[arg(long, default_value = "manual")]
    strategy: String,
    /// Actually perform the sync (default: dry-run mode showing plan)
    #[arg(long)]
    apply: bool,
    /// Output plan in JSON format (useful for CI/automation)
    #[arg(long)]
    json: bool,
  },
  /// Run health checks and diagnostics
  Doctor {
    /// Run thorough checks (includes network tests)
    #[arg(long)]
    thorough: bool,
    /// Output results in JSON format
    #[arg(long)]
    json: bool,
  },
}

fn get_styles() -> clap::builder::Styles {
  clap::builder::Styles::styled()
    .usage(
      anstyle::Style::new()
        .bold()
        .underline()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
    )
    .header(
      anstyle::Style::new()
        .bold()
        .underline()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow))),
    )
    .literal(anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Green))))
    .invalid(
      anstyle::Style::new()
        .bold()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red))),
    )
    .error(
      anstyle::Style::new()
        .bold()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red))),
    )
    .valid(
      anstyle::Style::new()
        .bold()
        .underline()
        .fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Green))),
    )
    .placeholder(anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::White))))
}

fn main() -> Result<()> {
  let CargoCli::Rail(cli) = CargoCli::parse();

  match cli.command {
    Commands::Init { all } => commands::run_init(all)?,
    Commands::Split {
      crate_name,
      all,
      apply,
      json,
    } => commands::run_split(crate_name, all, apply, json)?,
    Commands::Sync {
      crate_name,
      all,
      from_remote,
      to_remote,
      strategy,
      apply,
      json,
    } => commands::run_sync(crate_name, all, from_remote, to_remote, strategy, apply, json)?,
    Commands::Doctor { thorough, json } => commands::run_doctor(thorough, json)?,
  }

  Ok(())
}
