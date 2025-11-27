use std::io::IsTerminal;
use std::str::FromStr;

use crate::commands::common::{OutputFormat, SplitSyncConfigBuilder};
use crate::error::RailResult;
use crate::sync::{ConflictStrategy, SyncDirection, SyncEngine};
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Run the sync command
#[allow(clippy::too_many_arguments)]
pub fn run_sync(
  ctx: &WorkspaceContext,
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  from_remote: bool,
  to_remote: bool,
  strategy_str: String,
  _no_protected_branches: bool,
  check: bool,
  format: String,
) -> RailResult<()> {
  let output_format: OutputFormat = format.parse()?;
  let json = output_format.is_json();

  let conflict_strategy = ConflictStrategy::from_str(&strategy_str)?;

  let builder = SplitSyncConfigBuilder::new(ctx)?
    .with_crate_or_all(crate_name.clone(), all)?
    .with_remote_override(remote);

  let config_count = builder.count();

  if config_count == 0 && all {
    return Err(crate::error::RailError::with_help(
      "no crates configured for sync",
      "run 'cargo rail split init' first",
    ));
  }

  let direction = match (from_remote, to_remote) {
    (true, true) => {
      return Err(crate::error::RailError::with_help(
        "cannot use both --from-remote and --to-remote",
        "choose one direction or neither for bidirectional",
      ));
    }
    (true, false) => SyncDirection::RemoteToMono,
    (false, true) => SyncDirection::MonoToRemote,
    (false, false) => SyncDirection::Both,
  };

  let configs = builder.build_sync_configs()?;

  // Check mode
  if check {
    if json {
      for (sync_config, target_exists) in &configs {
        let dir_str = match direction {
          SyncDirection::MonoToRemote => "to_remote",
          SyncDirection::RemoteToMono => "from_remote",
          SyncDirection::Both => "bidirectional",
          SyncDirection::None => "none",
        };

        println!(
          "{}",
          serde_json::to_string_pretty(&serde_json::json!({
            "crate_name": sync_config.crate_name,
            "mode": format!("{:?}", sync_config.mode),
            "target_repo": sync_config.target_repo_path,
            "branch": sync_config.branch,
            "remote_url": sync_config.remote_url,
            "direction": dir_str,
            "conflict_strategy": strategy_str,
            "target_exists": target_exists,
          }))?
        );
      }
      return Ok(());
    }

    let dir_display = match direction {
      SyncDirection::MonoToRemote => "mono -> remote",
      SyncDirection::RemoteToMono => "remote -> mono",
      SyncDirection::Both => "bidirectional",
      SyncDirection::None => "none",
    };

    println!("sync plan:\n");
    for (sync_config, target_exists) in &configs {
      println!("  {}", sync_config.crate_name);
      println!("    direction: {}", dir_display);
      println!("    target: {}", sync_config.target_repo_path.display());
      println!("    remote: {}", sync_config.remote_url);
      println!("    strategy: {}", strategy_str);
      if !target_exists {
        println!("    warning: target repo missing (run split first)");
      }
    }

    println!("\nrun without --check to execute");
    return Ok(());
  }

  // Interactive confirmation
  if std::io::stdin().is_terminal() && !json {
    let dir_sym = match direction {
      SyncDirection::MonoToRemote => "->",
      SyncDirection::RemoteToMono => "<-",
      SyncDirection::Both => "<->",
      SyncDirection::None => "-",
    };

    println!(
      "syncing {} crate(s) ({}):\n",
      config_count,
      match direction {
        SyncDirection::MonoToRemote => "mono -> remote",
        SyncDirection::RemoteToMono => "remote -> mono",
        SyncDirection::Both => "bidirectional",
        SyncDirection::None => "none",
      }
    );

    for (sync_config, target_exists) in &configs {
      let status = if !target_exists { " (missing)" } else { "" };
      println!("  {} {}{}", sync_config.crate_name, dir_sym, status);
    }

    if !utils::prompt_for_confirmation("\nproceed? [Enter/Ctrl+C]")? {
      println!("cancelled");
      return Ok(());
    }
  }

  // Execute syncs
  if config_count > 1 && all {
    eprintln!("syncing {} crates...", config_count);

    let ctx = ctx.clone();
    let results: Vec<RailResult<()>> = configs
      .into_par_iter()
      .map(|(sync_config, target_exists)| {
        if !target_exists {
          eprintln!("  {} skipped (run split first)", sync_config.crate_name);
          return Ok(());
        }

        eprintln!("  {}", sync_config.crate_name);
        let mut engine = SyncEngine::new(&ctx, sync_config, conflict_strategy)?;

        match direction {
          SyncDirection::MonoToRemote => engine.sync_to_remote()?,
          SyncDirection::RemoteToMono => engine.sync_from_remote()?,
          SyncDirection::Both => engine.sync_bidirectional()?,
          SyncDirection::None => return Ok(()),
        };

        Ok(())
      })
      .collect();

    for result in results {
      result?;
    }
  } else {
    for (sync_config, target_exists) in configs {
      if !target_exists {
        eprintln!("{} skipped (run split first)", sync_config.crate_name);
        continue;
      }

      eprintln!("syncing {}...", sync_config.crate_name);
      let mut engine = SyncEngine::new(ctx, sync_config, conflict_strategy)?;

      match direction {
        SyncDirection::MonoToRemote => engine.sync_to_remote()?,
        SyncDirection::RemoteToMono => engine.sync_from_remote()?,
        SyncDirection::Both => engine.sync_bidirectional()?,
        SyncDirection::None => continue,
      };
    }
  }

  println!("sync complete");
  Ok(())
}
