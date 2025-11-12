//! Publishing crates to crates.io in topological order

use crate::commands::release::plan;
use crate::core::error::RailResult;
use std::env;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Run release publish command
pub fn run_release_publish(
  crate_name: Option<&str>,
  apply: bool,
  yes: bool,
  delay: u64,
  dry_run: bool,
) -> RailResult<()> {
  // Generate release plan to determine what needs to be published
  println!("📊 Analyzing workspace for release plan...");
  let mut full_plan = plan::generate_release_plan(true)?;
  println!();

  // Filter to specific crate if requested
  if let Some(name) = crate_name {
    full_plan.crates.retain(|c| c.name == name);
    full_plan.publish_order.retain(|n| n == name);
  }

  // Only publish crates with changes
  let plan = full_plan.only_changed();

  if plan.crates.is_empty() {
    if crate_name.is_some() {
      println!("ℹ️  No changes detected for the specified crate");
    } else {
      println!("ℹ️  No crates need to be published");
    }
    return Ok(());
  }

  // Get current directory (workspace root)
  let workspace_root = env::current_dir().map_err(|e| anyhow::anyhow!("Failed to get current directory: {}", e))?;

  println!("📦 Publishing {} crate(s) in topological order", plan.crates.len());
  println!("   Order: {}", plan.publish_order.join(" → "));
  println!();

  // Check for CARGO_REGISTRY_TOKEN
  if apply && !dry_run && env::var("CARGO_REGISTRY_TOKEN").is_err() {
    return Err(
      anyhow::anyhow!(
        "CARGO_REGISTRY_TOKEN not found in environment.\n\
       \n\
       To publish to crates.io, you need to set your token:\n\
       export CARGO_REGISTRY_TOKEN=<your-token>\n\
       \n\
       Get your token from: https://crates.io/me"
      )
      .into(),
    );
  }

  // Publish each crate in topological order
  for (idx, crate_name_to_publish) in plan.publish_order.iter().enumerate() {
    // Find the crate plan
    let crate_plan = plan
      .crates
      .iter()
      .find(|c| &c.name == crate_name_to_publish)
      .ok_or_else(|| anyhow::anyhow!("Crate {} not found in plan", crate_name_to_publish))?;

    println!(
      "📌 [{}/{}] {} ({})",
      idx + 1,
      plan.publish_order.len(),
      crate_plan.name,
      crate_plan.next_version
    );

    // Find the crate directory
    let crate_dir = find_crate_dir(&workspace_root, &crate_plan.name)?;

    // Step 1: Run pre-publish validation (cargo package --dry-run)
    println!("   🔍 Running pre-publish validation...");
    let package_result = Command::new("cargo")
      .arg("package")
      .arg("--manifest-path")
      .arg(crate_dir.join("Cargo.toml"))
      .arg("--allow-dirty")
      .output()
      .map_err(|e| anyhow::anyhow!("Failed to run cargo package: {}", e))?;

    if !package_result.status.success() {
      let stderr = String::from_utf8_lossy(&package_result.stderr);
      return Err(anyhow::anyhow!("Pre-publish validation failed for {}:\n{}", crate_plan.name, stderr).into());
    }
    println!("   ✅ Validation passed");

    // Step 2: Publish (or dry-run)
    if dry_run {
      println!("   🔍 Dry-run mode - skipping actual publish");
    } else if !apply {
      println!(
        "   💡 Would publish {} v{} (use --apply to publish)",
        crate_plan.name, crate_plan.next_version
      );
    } else {
      // Confirm before publishing (unless --yes)
      if !yes {
        println!(
          "   ⚠️  About to publish {} v{} to crates.io",
          crate_plan.name, crate_plan.next_version
        );
        println!("   Continue? [y/N] ");

        // Read user input
        let mut input = String::new();
        std::io::stdin()
          .read_line(&mut input)
          .map_err(|e| anyhow::anyhow!("Failed to read input: {}", e))?;

        let input = input.trim().to_lowercase();
        if input != "y" && input != "yes" {
          println!("   ⏭️  Skipped");
          continue;
        }
      }

      println!("   🚀 Publishing to crates.io...");
      let publish_result = Command::new("cargo")
        .arg("publish")
        .arg("--manifest-path")
        .arg(crate_dir.join("Cargo.toml"))
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run cargo publish: {}", e))?;

      if !publish_result.status.success() {
        let stderr = String::from_utf8_lossy(&publish_result.stderr);
        return Err(
          anyhow::anyhow!(
            "Failed to publish {}:\n{}\n\n\
             To rollback or continue:\n\
             1. Check crates.io to verify what was published\n\
             2. Fix the issue and re-run publish\n\
             3. Or use --crate {} to skip already-published crates",
            crate_plan.name,
            stderr,
            crate_plan.name
          )
          .into(),
        );
      }

      println!("   ✅ Published {} v{}", crate_plan.name, crate_plan.next_version);

      // Step 3: Wait for crates.io propagation (if not last crate)
      if idx < plan.publish_order.len() - 1 && delay > 0 {
        println!("   ⏳ Waiting {}s for crates.io propagation...", delay);
        thread::sleep(Duration::from_secs(delay));
      }
    }

    println!();
  }

  // Summary
  if dry_run {
    println!(
      "✅ Dry-run complete. All {} crate(s) passed validation.",
      plan.crates.len()
    );
    println!();
    println!("Next steps:");
    println!("  1. Review validation output above");
    println!("  2. Publish: cargo rail release publish --apply");
  } else if !apply {
    println!("💡 This was a dry-run. Use --apply to actually publish to crates.io.");
  } else {
    println!("🎉 Successfully published {} crate(s)!", plan.crates.len());
    println!();
    println!("Next steps:");
    println!("  1. Create git tags: cargo rail release finalize --apply");
    println!("  2. Verify on crates.io: https://crates.io/crates/<crate-name>");
  }

  Ok(())
}

/// Find the directory for a crate by name
fn find_crate_dir(workspace_root: &Path, crate_name: &str) -> RailResult<std::path::PathBuf> {
  use cargo_metadata::MetadataCommand;

  let metadata = MetadataCommand::new()
    .current_dir(workspace_root)
    .exec()
    .map_err(|e| anyhow::anyhow!("Failed to load workspace metadata: {}", e))?;

  for pkg in metadata.workspace_packages() {
    if pkg.name == crate_name {
      return Ok(
        pkg
          .manifest_path
          .parent()
          .ok_or_else(|| anyhow::anyhow!("Failed to get parent directory of manifest"))?
          .as_std_path()
          .to_path_buf(),
      );
    }
  }

  Err(anyhow::anyhow!("Crate {} not found in workspace", crate_name).into())
}
