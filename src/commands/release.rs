//! Release command implementation
//!
//! Commands for release planning and execution following the invariants:
//! 1. Releases driven from monorepo
//! 2. Changelogs per-thing
//! 3. Every release has name + version + last_sha

use crate::core::error::{RailError, RailResult};
use crate::release::{ReleasePlan, ReleaseTracker, VersionBump};
use std::env;

/// Run the release plan command
pub fn run_release_plan(name: Option<String>, all: bool, json: bool) -> RailResult<()> {
  let workspace_root = env::current_dir()?;

  // Load release tracker
  let tracker = ReleaseTracker::load(&workspace_root)?;

  // Determine which releases to plan
  let releases: Vec<_> = if all {
    tracker.releases().to_vec()
  } else if let Some(name) = name {
    let release = tracker
      .find_release(&name)
      .ok_or_else(|| RailError::message(format!("Release '{}' not found in rail.toml", name)))?;
    vec![release.clone()]
  } else {
    return Err(RailError::message("Must specify release name or use --all flag"));
  };

  if releases.is_empty() {
    if json {
      println!("[]");
    } else {
      println!("⚠️  No releases configured in rail.toml");
      println!();
      println!("Add a release configuration:");
      println!("  [[releases]]");
      println!("  name = \"my-crate\"");
      println!("  crate = \"crates/my-crate\"");
    }
    return Ok(());
  }

  // Analyze each release
  let mut plans = Vec::new();
  for release in &releases {
    let plan = ReleasePlan::analyze(&workspace_root, release)?;
    plans.push(plan);
  }

  // Output results
  if json {
    println!("{}", serde_json::to_string_pretty(&plans)?);
  } else {
    print_release_plans(&plans);
  }

  Ok(())
}

/// Run the release apply command
pub fn run_release_apply(
  name: String,
  dry_run: bool,
  _skip_sync: bool, // TODO: Implement split sync
) -> RailResult<()> {
  let workspace_root = env::current_dir()?;

  // Load release tracker
  let mut tracker = ReleaseTracker::load(&workspace_root)?;

  // Find the release
  let release = tracker
    .find_release(&name)
    .ok_or_else(|| RailError::message(format!("Release '{}' not found in rail.toml", name)))?
    .clone();

  // Generate plan
  let plan = ReleasePlan::analyze(&workspace_root, &release)?;

  if !plan.has_changes {
    println!("⚠️  No changes detected for '{}'", name);
    println!("   Current version: {}", plan.current_version);
    return Ok(());
  }

  // Show what will happen
  println!("📦 Release Plan for '{}'", name);
  println!();
  println!("  Current:  {}", plan.current_version);
  println!(
    "  Proposed: {} ({})",
    plan.proposed_version,
    format_bump(plan.bump_type)
  );
  println!();
  println!("  Changes:");
  for commit in &plan.commits {
    let icon = match commit.commit_type {
      crate::release::plan::CommitType::Feat => "✨",
      crate::release::plan::CommitType::Fix => "🐛",
      crate::release::plan::CommitType::Perf => "⚡",
      _ => "  ",
    };
    let breaking = if commit.is_breaking { " [BREAKING]" } else { "" };
    println!(
      "    {} {}{}",
      icon,
      commit.message.lines().next().unwrap_or(""),
      breaking
    );
  }
  println!();

  if dry_run {
    println!("🔍 Dry-run mode (no changes applied)");
    return Ok(());
  }

  // Apply the release
  println!("✅ Applying release...");

  // 1. Update Cargo.toml version
  update_crate_version(&workspace_root, &release.crate_path, &plan.proposed_version.to_string())?;
  println!("   Updated Cargo.toml version");

  // 2. Get current HEAD SHA
  let head_sha = get_head_sha(&workspace_root)?;

  // 3. Update rail.toml metadata
  tracker.update_release(&name, &plan.proposed_version.to_string(), &head_sha)?;
  tracker.save()?;
  println!("   Updated rail.toml metadata");

  // 4. Create git tag
  create_git_tag(&workspace_root, &name, &plan.proposed_version.to_string())?;
  println!("   Created tag: {}-v{}", name, plan.proposed_version);

  println!();
  println!("✅ Release {} completed!", plan.proposed_version);
  println!();
  println!("Next steps:");
  println!("  git push origin {}-v{}", name, plan.proposed_version);
  if release.has_split() {
    println!("  cargo rail sync {} --to-remote --apply  # Sync to split repo", name);
  }

  Ok(())
}

/// Update version in Cargo.toml
fn update_crate_version(
  workspace_root: &std::path::Path,
  crate_path: &std::path::Path,
  version: &str,
) -> RailResult<()> {
  let cargo_toml_path = workspace_root.join(crate_path).join("Cargo.toml");

  let content = std::fs::read_to_string(&cargo_toml_path)
    .map_err(|e| RailError::message(format!("Failed to read Cargo.toml: {}", e)))?;

  let mut doc: toml_edit::DocumentMut = content
    .parse()
    .map_err(|e| RailError::message(format!("Failed to parse Cargo.toml: {}", e)))?;

  if let Some(package) = doc.get_mut("package").and_then(|p| p.as_table_mut()) {
    package["version"] = toml_edit::value(version);
  } else {
    return Err(RailError::message("No [package] section in Cargo.toml"));
  }

  std::fs::write(&cargo_toml_path, doc.to_string())
    .map_err(|e| RailError::message(format!("Failed to write Cargo.toml: {}", e)))?;

  Ok(())
}

/// Get current HEAD SHA
fn get_head_sha(workspace_root: &std::path::Path) -> RailResult<String> {
  let output = std::process::Command::new("git")
    .current_dir(workspace_root)
    .args(["rev-parse", "HEAD"])
    .output()
    .map_err(|e| RailError::message(format!("Failed to run git: {}", e)))?;

  if !output.status.success() {
    return Err(RailError::message(format!(
      "git rev-parse failed: {}",
      String::from_utf8_lossy(&output.stderr)
    )));
  }

  Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Create git tag
fn create_git_tag(workspace_root: &std::path::Path, name: &str, version: &str) -> RailResult<()> {
  let tag = format!("{}-v{}", name, version);

  let output = std::process::Command::new("git")
    .current_dir(workspace_root)
    .args(["tag", "-a", &tag, "-m", &format!("Release {} v{}", name, version)])
    .output()
    .map_err(|e| RailError::message(format!("Failed to run git: {}", e)))?;

  if !output.status.success() {
    return Err(RailError::message(format!(
      "git tag failed: {}",
      String::from_utf8_lossy(&output.stderr)
    )));
  }

  Ok(())
}

fn print_release_plans(plans: &[ReleasePlan]) {
  if plans.is_empty() {
    return;
  }

  println!("📋 Release Plans");
  println!();

  for plan in plans {
    let status_icon = if !plan.has_changes {
      "✅"
    } else {
      match plan.bump_type {
        VersionBump::Major => "🔴",
        VersionBump::Minor => "🟡",
        VersionBump::Patch => "🟢",
        VersionBump::None => "⚪",
      }
    };

    println!("{} {}", status_icon, plan.name);
    println!("   Current:  {}", plan.current_version);

    if plan.has_changes {
      println!(
        "   Proposed: {} ({})",
        plan.proposed_version,
        format_bump(plan.bump_type)
      );
      println!("   Changes:  {} commit(s)", plan.commits.len());

      // Show summary of commit types
      let feats = plan
        .commits
        .iter()
        .filter(|c| matches!(c.commit_type, crate::release::plan::CommitType::Feat))
        .count();
      let fixes = plan
        .commits
        .iter()
        .filter(|c| matches!(c.commit_type, crate::release::plan::CommitType::Fix))
        .count();
      let breaking = plan.commits.iter().filter(|c| c.is_breaking).count();

      if breaking > 0 {
        println!("             {} breaking change(s)", breaking);
      }
      if feats > 0 {
        println!("             {} feature(s)", feats);
      }
      if fixes > 0 {
        println!("             {} fix(es)", fixes);
      }
    } else {
      println!("   Status:   No changes since last release");
    }

    println!();
  }

  let needs_release: Vec<_> = plans.iter().filter(|p| p.has_changes).collect();

  if needs_release.is_empty() {
    println!("✅ All releases are up to date");
  } else {
    println!("To release:");
    for plan in needs_release {
      println!("  cargo rail release apply {}", plan.name);
    }
  }
}

fn format_bump(bump: VersionBump) -> &'static str {
  match bump {
    VersionBump::Major => "major",
    VersionBump::Minor => "minor",
    VersionBump::Patch => "patch",
    VersionBump::None => "none",
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn test_module_exists() {
    // Ensure module compiles
  }
}
