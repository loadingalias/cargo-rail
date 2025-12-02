//! `cargo rail sync` - Bidirectional sync between monorepo and split repositories.

use std::io::IsTerminal;

use crate::commands::common::{OutputFormat, SplitSyncConfigBuilder};
use crate::error::RailResult;
use crate::progress;
use crate::sync::{ConflictStrategy, SyncDirection, SyncEngine, SyncResult};
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;

/// Per-crate sync result for structured output
struct CrateSyncResult {
  crate_name: String,
  result: SyncResult,
  skipped: bool,
}

/// Run the sync command
#[allow(clippy::too_many_arguments)]
pub fn run_sync(
  ctx: &WorkspaceContext,
  crate_name: Option<String>,
  all: bool,
  remote: Option<String>,
  from_remote: bool,
  to_remote: bool,
  strategy: ConflictStrategy,
  _no_protected_branches: bool,
  check: bool,
  format: OutputFormat,
) -> RailResult<()> {
  let json = format.is_json();

  // JSON mode enables structured error output and suppresses progress
  if json {
    crate::output::set_json_mode(true);
  }

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
      let dir_str = match direction {
        SyncDirection::MonoToRemote => "to_remote",
        SyncDirection::RemoteToMono => "from_remote",
        SyncDirection::Both => "bidirectional",
        SyncDirection::None => "none",
      };

      let crates: Vec<_> = configs
        .iter()
        .map(|(sync_config, target_exists)| {
          serde_json::json!({
            "crate_name": sync_config.crate_name,
            "mode": format!("{:?}", sync_config.mode),
            "target_repo": sync_config.target_repo_path,
            "branch": sync_config.branch,
            "remote_url": sync_config.remote_url,
            "target_exists": target_exists,
          })
        })
        .collect();

      let output = serde_json::json!({
        "command": "sync",
        "check": true,
        "direction": dir_str,
        "strategy": format!("{:?}", strategy).to_lowercase(),
        "crates": crates,
        "count": configs.len()
      });
      println!("{}", serde_json::to_string_pretty(&output)?);
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
      println!("    strategy: {:?}", strategy);
      if !target_exists {
        println!("    warning: target repo missing (run split first)");
      }
    }

    println!("\nChanges detected. Run without --check to apply.");
    return Err(crate::error::RailError::CheckHasPendingChanges);
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

  // Execute syncs and collect per-crate results
  let crate_results: Vec<CrateSyncResult> = if config_count > 1 && all {
    progress!("syncing {} crates...", config_count);

    let ctx = ctx.clone();
    let results: Vec<RailResult<CrateSyncResult>> = configs
      .into_par_iter()
      .map(|(sync_config, target_exists)| {
        let crate_name = sync_config.crate_name.clone();

        if !target_exists {
          progress!("  {} skipped (run split first)", crate_name);
          return Ok(CrateSyncResult {
            crate_name,
            result: SyncResult::default(),
            skipped: true,
          });
        }

        progress!("  {}", crate_name);
        let mut engine = SyncEngine::new(&ctx, sync_config, strategy)?;

        let result = match direction {
          SyncDirection::MonoToRemote => engine.sync_to_remote()?,
          SyncDirection::RemoteToMono => engine.sync_from_remote()?,
          SyncDirection::Both => engine.sync_bidirectional()?,
          SyncDirection::None => SyncResult::default(),
        };

        Ok(CrateSyncResult {
          crate_name,
          result,
          skipped: false,
        })
      })
      .collect();

    results.into_iter().collect::<RailResult<Vec<_>>>()?
  } else {
    let mut results = Vec::new();
    for (sync_config, target_exists) in configs {
      let crate_name = sync_config.crate_name.clone();

      if !target_exists {
        progress!("{} skipped (run split first)", crate_name);
        results.push(CrateSyncResult {
          crate_name,
          result: SyncResult::default(),
          skipped: true,
        });
        continue;
      }

      progress!("syncing {}...", crate_name);
      let mut engine = SyncEngine::new(ctx, sync_config, strategy)?;

      let result = match direction {
        SyncDirection::MonoToRemote => engine.sync_to_remote()?,
        SyncDirection::RemoteToMono => engine.sync_from_remote()?,
        SyncDirection::Both => engine.sync_bidirectional()?,
        SyncDirection::None => SyncResult::default(),
      };

      results.push(CrateSyncResult {
        crate_name,
        result,
        skipped: false,
      });
    }
    results
  };

  // Print summary
  print_sync_summary(&crate_results, json)?;

  Ok(())
}

/// Print sync results summary
fn print_sync_summary(results: &[CrateSyncResult], json: bool) -> RailResult<()> {
  if json {
    let crates: Vec<_> = results
      .iter()
      .map(|r| {
        let conflicts: Vec<_> = r
          .result
          .conflicts
          .iter()
          .map(|c| c.file_path.display().to_string())
          .collect();

        serde_json::json!({
          "crate": r.crate_name,
          "commits_synced": r.result.commits_synced,
          "conflicts": conflicts,
          "skipped": r.skipped
        })
      })
      .collect();

    let total_commits: usize = results.iter().map(|r| r.result.commits_synced).sum();
    let total_conflicts: usize = results.iter().map(|r| r.result.conflicts.len()).sum();

    let output = serde_json::json!({
      "command": "sync",
      "crates": crates,
      "summary": {
        "total_commits": total_commits,
        "total_conflicts": total_conflicts,
        "crates_synced": results.iter().filter(|r| !r.skipped).count(),
        "crates_skipped": results.iter().filter(|r| r.skipped).count()
      }
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    return Ok(());
  }

  // Text output
  let active_results: Vec<_> = results.iter().filter(|r| !r.skipped).collect();
  let total_commits: usize = active_results.iter().map(|r| r.result.commits_synced).sum();
  let total_conflicts: usize = active_results.iter().map(|r| r.result.conflicts.len()).sum();

  // Per-crate details (only if multiple crates or conflicts)
  if active_results.len() > 1 || total_conflicts > 0 {
    println!();
    for r in &active_results {
      let commit_word = if r.result.commits_synced == 1 {
        "commit"
      } else {
        "commits"
      };
      if r.result.conflicts.is_empty() {
        println!("  {}: {} {}", r.crate_name, r.result.commits_synced, commit_word);
      } else {
        let conflict_word = if r.result.conflicts.len() == 1 {
          "conflict"
        } else {
          "conflicts"
        };
        println!(
          "  {}: {} {}, {} {}",
          r.crate_name,
          r.result.commits_synced,
          commit_word,
          r.result.conflicts.len(),
          conflict_word
        );
        for conflict in &r.result.conflicts {
          println!("    {}", conflict.file_path.display());
        }
      }
    }
    println!();
  }

  // Summary line
  let commit_word = if total_commits == 1 { "commit" } else { "commits" };
  if total_conflicts > 0 {
    let conflict_word = if total_conflicts == 1 { "conflict" } else { "conflicts" };
    println!(
      "sync complete: {} {}, {} {}",
      total_commits, commit_word, total_conflicts, conflict_word
    );
  } else {
    println!("sync complete: {} {}", total_commits, commit_word);
  }

  Ok(())
}
