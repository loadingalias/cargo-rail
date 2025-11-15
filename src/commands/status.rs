use serde::{Deserialize, Serialize};
use std::env;
use std::path::PathBuf;
use std::process::Command;

use crate::core::config::RailConfig;
use crate::core::error::{ConfigError, RailError, RailResult};
use crate::utils;

/// Status of a crate
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitStatus {
  /// Crate has not been split yet
  NotSplit,
  /// Crate has been split to remote
  Split,
  /// Crate is split and in sync
  Synced,
}

/// Sync status showing commits ahead/behind
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
  /// Up to date
  UpToDate,
  /// Ahead of remote (N commits)
  Ahead { commits: u64 },
  /// Behind remote (N commits)
  Behind { commits: u64 },
  /// Diverged (ahead and behind)
  Diverged { ahead: u64, behind: u64 },
}

/// Status information for a single crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateStatus {
  /// Crate name
  pub name: String,

  /// Split status
  pub split_status: SplitStatus,

  /// Sync status
  pub sync_status: Option<SyncStatus>,

  /// Whether there are uncommitted changes
  pub dirty: bool,

  /// Remote URL
  pub remote: String,

  /// Target repository path (if it exists)
  pub target_path: Option<PathBuf>,
}

/// Run the status command
pub fn run_status(json: bool) -> RailResult<()> {
  let current_dir = env::current_dir()?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    return Err(RailError::Config(ConfigError::NotFound {
      workspace_root: current_dir,
    }));
  }

  let config = RailConfig::load(&current_dir)?;

  // Gather status for all crates
  let mut statuses = Vec::new();

  for split_config in &config.splits {
    let target_repo_path = if utils::is_local_path(&split_config.remote) {
      std::path::PathBuf::from(&split_config.remote)
    } else {
      let remote_name = split_config
        .remote
        .rsplit('/')
        .next()
        .unwrap_or(&split_config.name)
        .trim_end_matches(".git");
      current_dir.join("..").join(remote_name)
    };

    let target_exists = target_repo_path.exists();

    // Check if split exists by checking git-notes (more reliable than directory check)
    let has_git_notes = check_git_notes_exist(&current_dir, &split_config.name)?;

    let split_status = if has_git_notes {
      SplitStatus::Split
    } else if target_exists {
      // Fallback: local directory exists but no git-notes yet
      SplitStatus::Split
    } else {
      SplitStatus::NotSplit
    };

    // Check sync status if target exists
    let sync_status = if target_exists {
      Some(check_sync_status(
        &current_dir,
        &target_repo_path,
        &split_config.branch,
      )?)
    } else {
      None
    };

    // Check for dirty state in monorepo paths
    let dirty = check_dirty_state(&current_dir, split_config.get_paths())?;

    statuses.push(CrateStatus {
      name: split_config.name.clone(),
      split_status,
      sync_status,
      dirty,
      remote: split_config.remote.clone(),
      target_path: if target_exists { Some(target_repo_path) } else { None },
    });
  }

  // Output
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&statuses).map_err(|e| RailError::message(format!("Serialization error: {}", e)))?
    );
  } else {
    print_status_table(&statuses);
  }

  Ok(())
}

/// Check sync status between monorepo and target repo
fn check_sync_status(
  _monorepo_path: &std::path::Path,
  target_path: &std::path::Path,
  _branch: &str,
) -> RailResult<SyncStatus> {
  // For now, we'll use a simple git status check
  // In the future, we can use git-notes mappings to be more precise

  // Check if target repo has uncommitted changes or is ahead/behind
  let output = Command::new("git")
    .current_dir(target_path)
    .args(["status", "--porcelain", "--branch"])
    .output()?;

  if !output.status.success() {
    // If git status fails, assume up to date
    return Ok(SyncStatus::UpToDate);
  }

  let status_output = String::from_utf8_lossy(&output.stdout);

  // Parse branch status line (## branch...origin/branch [ahead N, behind M])
  for line in status_output.lines() {
    if line.starts_with("##") {
      if line.contains("[ahead") && line.contains("behind") {
        // Diverged
        let ahead = extract_number_after(line, "ahead ");
        let behind = extract_number_after(line, "behind ");
        return Ok(SyncStatus::Diverged { ahead, behind });
      } else if line.contains("[ahead") {
        let commits = extract_number_after(line, "ahead ");
        return Ok(SyncStatus::Ahead { commits });
      } else if line.contains("[behind") {
        let commits = extract_number_after(line, "behind ");
        return Ok(SyncStatus::Behind { commits });
      }
    }
  }

  Ok(SyncStatus::UpToDate)
}

/// Extract number after a given prefix in a string
fn extract_number_after(text: &str, prefix: &str) -> u64 {
  text
    .split_once(prefix)
    .and_then(|(_, rest)| rest.split(&[',', ']'][..]).next())
    .and_then(|num_str| num_str.trim().parse().ok())
    .unwrap_or(0)
}

/// Check if git-notes exist for a split (indicates split has been performed)
fn check_git_notes_exist(repo_path: &std::path::Path, split_name: &str) -> RailResult<bool> {
  let notes_ref = format!("refs/notes/rail/{}", split_name);

  let output = Command::new("git")
    .current_dir(repo_path)
    .args(["notes", "--ref", &notes_ref, "list"])
    .output()?;

  // If git notes list succeeds and has output, notes exist
  Ok(output.status.success() && !output.stdout.is_empty())
}

/// Check if paths have uncommitted changes
fn check_dirty_state(repo_path: &std::path::Path, paths: Vec<&PathBuf>) -> RailResult<bool> {
  for path in paths {
    let full_path = repo_path.join(path);
    if !full_path.exists() {
      continue;
    }

    let output = Command::new("git")
      .current_dir(repo_path)
      .args(["status", "--porcelain", "--"])
      .arg(path)
      .output()?;

    if !output.status.success() {
      continue;
    }

    let status_output = String::from_utf8_lossy(&output.stdout);
    if !status_output.trim().is_empty() {
      return Ok(true);
    }
  }

  Ok(false)
}

/// Print status as a formatted table
fn print_status_table(statuses: &[CrateStatus]) {
  println!("\n📊 Crate Status\n");

  // Header
  println!("{:<20} {:<12} {:<20} {:<10} REMOTE", "CRATE", "SPLIT", "SYNC", "DIRTY");
  println!("{:-<120}", "");

  for status in statuses {
    let split_str = match status.split_status {
      SplitStatus::NotSplit => "not split",
      SplitStatus::Split => "split",
      SplitStatus::Synced => "synced",
    };

    let sync_str = match &status.sync_status {
      None => "-".to_string(),
      Some(SyncStatus::UpToDate) => "up-to-date".to_string(),
      Some(SyncStatus::Ahead { commits }) => format!("ahead {}", commits),
      Some(SyncStatus::Behind { commits }) => format!("behind {}", commits),
      Some(SyncStatus::Diverged { ahead, behind }) => format!("diverged +{} -{}", ahead, behind),
    };

    let dirty_str = if status.dirty { "yes" } else { "no" };

    // Intelligently truncate remote URL for display (preserve end which has repo name)
    let remote_display = if status.remote.len() > 75 {
      // Truncate from middle to preserve repo name at end
      let start = &status.remote[..35];
      let end = &status.remote[status.remote.len().saturating_sub(35)..];
      format!("{}...{}", start, end)
    } else {
      status.remote.clone()
    };

    println!(
      "{:<20} {:<12} {:<20} {:<10} {}",
      status.name, split_str, sync_str, dirty_str, remote_display
    );
  }

  println!();
}
