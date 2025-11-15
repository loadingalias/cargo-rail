use serde::{Deserialize, Serialize};
use std::env;
use std::process::Command;

use crate::core::config::RailConfig;
use crate::core::error::{ConfigError, RailError, RailResult};
use crate::core::mapping::MappingStore;
use crate::ui::progress::FileProgress;
use crate::utils;

/// A single SHA mapping
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mapping {
  /// Monorepo commit SHA
  pub mono_sha: String,

  /// Remote/split repo commit SHA
  pub remote_sha: String,

  /// Whether both commits still exist
  #[serde(skip_serializing_if = "Option::is_none")]
  pub valid: Option<bool>,
}

/// Mappings for a crate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateMappings {
  /// Crate name
  pub crate_name: String,

  /// Git notes ref
  pub notes_ref: String,

  /// Total number of mappings
  pub count: usize,

  /// Individual mappings
  pub mappings: Vec<Mapping>,

  /// Integrity check results (if --check was used)
  #[serde(skip_serializing_if = "Option::is_none")]
  pub integrity: Option<IntegrityCheck>,
}

/// Results of integrity checking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityCheck {
  /// Number of valid mappings
  pub valid_count: usize,

  /// Number of invalid mappings (commits missing)
  pub invalid_count: usize,

  /// List of missing commits
  pub missing_commits: Vec<String>,
}

/// Run the mappings command
pub fn run_mappings(crate_name: String, check: bool, json: bool) -> RailResult<()> {
  let current_dir = env::current_dir()?;

  // Load configuration
  if !RailConfig::exists(&current_dir) {
    return Err(RailError::Config(ConfigError::NotFound {
      workspace_root: current_dir,
    }));
  }

  let config = RailConfig::load(&current_dir)?;

  // Find crate configuration
  let split_config = config.splits.iter().find(|s| s.name == crate_name).ok_or_else(|| {
    RailError::Config(ConfigError::CrateNotFound {
      name: crate_name.clone(),
    })
  })?;

  // Load mappings
  let notes_ref = format!("refs/notes/rail/{}", crate_name);
  let mut mapping_store = MappingStore::new(crate_name.clone());
  mapping_store.load(&current_dir)?;

  let raw_mappings = mapping_store.all_mappings();

  // Convert to our structure
  let mut mappings = Vec::new();

  // Show progress bar if checking validity and there are many mappings
  let mut progress = if check && !raw_mappings.is_empty() {
    Some(FileProgress::new(
      raw_mappings.len(),
      format!("Validating {} commit mappings", raw_mappings.len()),
    ))
  } else {
    None
  };

  for (mono_sha, remote_sha) in raw_mappings {
    let mut mapping = Mapping {
      mono_sha: mono_sha.clone(),
      remote_sha: remote_sha.clone(),
      valid: None,
    };

    if check {
      // Verify both commits exist
      let mono_exists = commit_exists(&current_dir, mono_sha)?;
      let remote_exists = if let Some(target_path) = get_target_path(&current_dir, split_config) {
        commit_exists(&target_path, remote_sha)?
      } else {
        false
      };

      mapping.valid = Some(mono_exists && remote_exists);

      if let Some(ref mut p) = progress {
        p.inc();
      }
    }

    mappings.push(mapping);
  }

  // Compute integrity check results
  let integrity = if check {
    let valid_count = mappings.iter().filter(|m| m.valid == Some(true)).count();
    let invalid_count = mappings.iter().filter(|m| m.valid == Some(false)).count();
    let missing_commits: Vec<String> = mappings
      .iter()
      .filter(|m| m.valid == Some(false))
      .flat_map(|m| vec![m.mono_sha.clone(), m.remote_sha.clone()])
      .collect();

    Some(IntegrityCheck {
      valid_count,
      invalid_count,
      missing_commits,
    })
  } else {
    None
  };

  let crate_mappings = CrateMappings {
    crate_name: crate_name.clone(),
    notes_ref,
    count: mappings.len(),
    mappings,
    integrity,
  };

  // Output
  if json {
    println!(
      "{}",
      serde_json::to_string_pretty(&crate_mappings)
        .map_err(|e| RailError::message(format!("Serialization error: {}", e)))?
    );
  } else {
    print_mappings_table(&crate_mappings, check);
  }

  Ok(())
}

/// Check if a commit exists in a repository
fn commit_exists(repo_path: &std::path::Path, sha: &str) -> RailResult<bool> {
  let output = Command::new("git")
    .current_dir(repo_path)
    .args(["cat-file", "-e", &format!("{}^{{commit}}", sha)])
    .output()?;

  Ok(output.status.success())
}

/// Get target repository path for a split config
fn get_target_path(
  current_dir: &std::path::Path,
  split_config: &crate::core::config::SplitConfig,
) -> Option<std::path::PathBuf> {
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

  if target_repo_path.exists() {
    Some(target_repo_path)
  } else {
    None
  }
}

/// Print mappings as a formatted table
fn print_mappings_table(crate_mappings: &CrateMappings, show_check: bool) {
  println!("\n🗺️  Git-Notes Mappings for '{}'", crate_mappings.crate_name);
  println!("   Notes ref: {}", crate_mappings.notes_ref);
  println!("   Total mappings: {}\n", crate_mappings.count);

  if crate_mappings.mappings.is_empty() {
    println!(
      "   No mappings found. Split this crate first with `cargo rail split {}`",
      crate_mappings.crate_name
    );
    return;
  }

  // Header
  if show_check {
    println!("{:<12} {:<42} {:<42} STATUS", "VALID", "MONOREPO SHA", "REMOTE SHA");
  } else {
    println!("{:<42} {:<42}", "MONOREPO SHA", "REMOTE SHA");
  }
  println!("{:-<80}", "");

  // Limit display to first 20 mappings (or show all with --verbose in the future)
  let display_limit = 20;
  let mappings_to_show = if crate_mappings.mappings.len() > display_limit {
    &crate_mappings.mappings[..display_limit]
  } else {
    &crate_mappings.mappings
  };

  for mapping in mappings_to_show {
    if show_check {
      let valid_str = match mapping.valid {
        Some(true) => "✓",
        Some(false) => "✗",
        None => "-",
      };

      let status_str = match mapping.valid {
        Some(true) => "both exist",
        Some(false) => "missing",
        None => "",
      };

      println!(
        "{:<12} {:<42} {:<42} {}",
        valid_str, mapping.mono_sha, mapping.remote_sha, status_str
      );
    } else {
      println!("{:<42} {:<42}", mapping.mono_sha, mapping.remote_sha);
    }
  }

  if crate_mappings.mappings.len() > display_limit {
    println!(
      "\n   ... and {} more mappings",
      crate_mappings.mappings.len() - display_limit
    );
  }

  // Show integrity summary
  if let Some(ref integrity) = crate_mappings.integrity {
    println!("\n📋 Integrity Check:");
    println!(
      "   Valid mappings:   {} / {}",
      integrity.valid_count, crate_mappings.count
    );
    println!("   Invalid mappings: {}", integrity.invalid_count);

    if integrity.invalid_count > 0 {
      println!("\n   ⚠️  Some commits are missing. This may indicate:");
      println!("      - Commits were rewritten (rebase, amend)");
      println!("      - Target repository was force-pushed");
      println!("      - Git-notes are out of sync");
      println!("\n   Run `cargo rail doctor` to diagnose further.");
    } else {
      println!("\n   ✅ All mappings are valid!");
    }
  }

  println!();
}
