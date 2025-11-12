//! Git tagging and post-release sync

use crate::commands::release::{plan, tags};
use crate::core::error::RailResult;
use std::env;
use std::path::Path;
use std::process::Command;

/// Run release finalize command
pub fn run_release_finalize(crate_name: Option<&str>, apply: bool, _no_github: bool, no_sync: bool) -> RailResult<()> {
  // Generate release plan to determine what needs to be finalized
  println!("📊 Analyzing workspace for release plan...");
  let mut full_plan = plan::generate_release_plan(true)?;
  println!();

  // Filter to specific crate if requested
  if let Some(name) = crate_name {
    full_plan.crates.retain(|c| c.name == name);
    full_plan.publish_order.retain(|n| n == name);
  }

  // Only finalize crates with changes
  let plan = full_plan.only_changed();

  if plan.crates.is_empty() {
    if crate_name.is_some() {
      println!("ℹ️  No changes detected for the specified crate");
    } else {
      println!("ℹ️  No crates need to be finalized");
    }
    return Ok(());
  }

  // Get current directory (workspace root)
  let workspace_root = env::current_dir().map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;

  // Open git repository
  let repo = gix::open(&workspace_root).map_err(|e| anyhow::anyhow!("Failed to open git repository: {}", e))?;

  println!("🏷️  Finalizing {} crate(s)", plan.crates.len());
  println!();

  // Get current HEAD commit
  let head = repo.head().map_err(|e| anyhow::anyhow!("Failed to get HEAD: {}", e))?;
  let commit = head
    .into_peeled_id()
    .map_err(|e| anyhow::anyhow!("Failed to resolve HEAD to commit: {}", e))?;
  let commit_id = commit.detach();

  // Create tags for each crate
  for crate_plan in &plan.crates {
    let tag_name = tags::format_tag(&crate_plan.name, &crate_plan.next_version);

    println!(
      "📌 {} (v{} → v{})",
      crate_plan.name, crate_plan.current_version, crate_plan.next_version
    );

    // Generate tag message from changelog
    let tag_message = format!(
      "Release {} v{}\n\n{}",
      crate_plan.name, crate_plan.next_version, crate_plan.reason
    );

    if !apply {
      println!("   💡 Would create tag: {}", tag_name);
      println!("   💡 Would push to origin");
      if !no_sync {
        println!("   💡 Would sync to split repo");
      }
    } else {
      // Create annotated tag
      println!("   🏷️  Creating tag: {}", tag_name);

      // Check if tag already exists
      if repo.find_reference(&format!("refs/tags/{}", tag_name)).is_ok() {
        eprintln!("   ⚠️  Tag {} already exists, skipping", tag_name);
        continue;
      }

      // Create the tag using git command (gix doesn't have a simple tag creation API)
      let tag_result = Command::new("git")
        .arg("tag")
        .arg("-a")
        .arg(&tag_name)
        .arg("-m")
        .arg(&tag_message)
        .arg(commit_id.to_string())
        .current_dir(&workspace_root)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run git tag: {}", e))?;

      if !tag_result.status.success() {
        let stderr = String::from_utf8_lossy(&tag_result.stderr);
        return Err(anyhow::anyhow!("Failed to create tag {}:\n{}", tag_name, stderr).into());
      }

      println!("   ✅ Created tag: {}", tag_name);

      // Push tag to origin
      println!("   📤 Pushing tag to origin...");
      let push_result = Command::new("git")
        .arg("push")
        .arg("origin")
        .arg(format!("refs/tags/{}", tag_name))
        .current_dir(&workspace_root)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run git push: {}", e))?;

      if !push_result.status.success() {
        let stderr = String::from_utf8_lossy(&push_result.stderr);
        eprintln!("   ⚠️  Warning: Failed to push tag to origin:\n{}", stderr);
      } else {
        println!("   ✅ Pushed to origin");
      }

      // Sync to split repo (if configured and not disabled)
      if !no_sync {
        println!("   🔄 Syncing to split repo...");
        let sync_result = sync_to_split_repo(&workspace_root, &crate_plan.name)?;
        if sync_result {
          println!("   ✅ Synced to split repo");
        } else {
          println!("   ℹ️  No split repo configured");
        }
      }
    }

    println!();
  }

  // Summary
  if !apply {
    println!("💡 This was a dry-run. Use --apply to create and push tags.");
  } else {
    println!("🎉 Successfully finalized {} crate(s)!", plan.crates.len());
    println!();
    println!("Tags created:");
    for crate_plan in &plan.crates {
      let tag_name = tags::format_tag(&crate_plan.name, &crate_plan.next_version);
      println!("  - {}", tag_name);
    }
  }

  Ok(())
}

/// Sync a crate to its split repository
fn sync_to_split_repo(workspace_root: &Path, crate_name: &str) -> RailResult<bool> {
  // Check if rail.toml exists and has split repo config for this crate
  let rail_config = workspace_root.join(".rail").join("rail.toml");
  if !rail_config.exists() {
    return Ok(false);
  }

  // Try to sync using cargo rail sync command
  let sync_result = Command::new("cargo")
    .arg("rail")
    .arg("sync")
    .arg(crate_name)
    .arg("--apply")
    .current_dir(workspace_root)
    .output()
    .map_err(|e| anyhow::anyhow!("Failed to run cargo rail sync: {}", e))?;

  if !sync_result.status.success() {
    let stderr = String::from_utf8_lossy(&sync_result.stderr);
    eprintln!("   ⚠️  Warning: Failed to sync to split repo:\n{}", stderr);
    return Ok(false);
  }

  Ok(true)
}
