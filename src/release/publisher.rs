//! Release execution (publishing to crates.io and GitHub)

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::release::changelog::ChangelogGenerator;
use crate::release::planner::{CrateReleasePlan, ReleasePlan};
use crate::release::version::VersionBumper;
use crate::workspace::WorkspaceContext;
use std::fs;
use std::process::Command;
use std::thread;
use std::time::Duration;

/// Release publisher
pub struct ReleasePublisher<'a> {
  ctx: &'a WorkspaceContext,
  release_config: &'a ReleaseConfig,
}

impl<'a> ReleasePublisher<'a> {
  pub fn new(ctx: &'a WorkspaceContext, release_config: &'a ReleaseConfig) -> Self {
    Self { ctx, release_config }
  }

  /// Execute a release plan
  ///
  /// # Arguments
  /// * `plan` - The release plan to execute
  /// * `skip_publish` - Skip publishing to crates.io (only tag)
  /// * `skip_tag` - Skip git tag creation
  pub fn execute(&self, plan: &ReleasePlan, skip_publish: bool, skip_tag: bool) -> RailResult<()> {
    println!("🚀 Executing release plan...\n");

    for (i, crate_plan) in plan.crates.iter().enumerate() {
      println!("[{}/{}] Processing {}...", i + 1, plan.crates.len(), crate_plan.name);

      // 1. Bump version in Cargo.toml
      println!(
        "   📝 Bumping version {} → {}",
        crate_plan.current_version, crate_plan.new_version
      );
      self.bump_crate_version(crate_plan)?;

      // 2. Update dependent crates
      if !crate_plan.affected_dependents.is_empty() {
        println!("   🔗 Updating {} dependent(s)", crate_plan.affected_dependents.len());
        self.update_dependents(crate_plan)?;
      }

      // 3. Generate/update changelog
      println!("   📜 Updating changelog");
      self.update_changelog(crate_plan)?;

      // 4. Commit changes
      println!("   💾 Committing changes");
      self.commit_version_bump(crate_plan)?;

      // 5. Create git tag
      if !skip_tag {
        println!("   🏷️  Creating tag {}", crate_plan.tag_name);
        self.create_tag(crate_plan)?;
      }

      // 6. Publish to crates.io
      if !skip_publish && crate_plan.publish {
        println!("   📤 Publishing to crates.io");
        self.publish_crate(crate_plan)?;

        // Delay between publishes to avoid registry race conditions
        if i + 1 < plan.crates.len() {
          let delay = self.release_config.publish_delay;
          println!("   ⏳ Waiting {}s before next publish", delay);
          thread::sleep(Duration::from_secs(delay));
        }
      } else if !crate_plan.publish {
        println!("   ⏭️  Skipping publish (publish = false)");
      }

      // 7. Create GitHub release
      if self.release_config.create_github_release && !skip_tag {
        println!("   🌐 Creating GitHub release");
        self.create_github_release(crate_plan)?;
      }

      println!("   ✅ Completed\n");
    }

    println!("🎉 All releases completed successfully!\n");

    // Print next steps
    if !skip_tag {
      println!("Next steps:");
      println!("  git push origin main");
      println!("  git push origin --tags");
    }

    Ok(())
  }

  /// Bump version in Cargo.toml
  fn bump_crate_version(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    use crate::release::version::BumpType;
    let bump = BumpType::Exact(plan.new_version.clone());
    VersionBumper::bump_version(&plan.manifest_path, bump)?;
    Ok(())
  }

  /// Update dependent crates to use new version
  fn update_dependents(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let metadata = self.ctx.cargo.metadata();

    for dependent_name in &plan.affected_dependents {
      if let Some(pkg) = metadata.get_package(dependent_name) {
        let manifest_path = pkg.manifest_path.clone().into_std_path_buf();
        VersionBumper::update_dependency_version(&manifest_path, &plan.name, &plan.new_version)?;
      }
    }

    Ok(())
  }

  /// Update or create CHANGELOG.md
  fn update_changelog(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    if !plan.generate_changelog {
      println!("   🧹 Skipping changelog (disabled for {})", plan.name);
      return Ok(());
    }

    // Find previous tag for this crate
    let previous_tag = self.find_previous_tag(plan)?;

    // Get crate directory for path filtering
    let crate_dir = plan
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message("Invalid manifest path"))?;

    // Generate changelog
    let generator = ChangelogGenerator::new(self.ctx.workspace_root());
    let github_repo = generator.github_repo().cloned();
    let new_entries = generator.generate(previous_tag.as_deref(), "HEAD", Some(&[crate_dir]))?;

    // Read existing changelog or create new
    let existing = if plan.changelog_path.exists() {
      fs::read_to_string(&plan.changelog_path).unwrap_or_default()
    } else {
      format!(
        "# Changelog\n\nAll notable changes to {} will be documented in this file.\n\n",
        plan.name
      )
    };

    // Prepend new version section
    let mut updated = String::new();
    let lines: Vec<&str> = existing.lines().collect();

    // Add header
    if let Some(header) = lines.first() {
      updated.push_str(header);
      updated.push_str("\n\n");
    }

    // Add new version with current date (get from git commit date or system)
    let date = self.get_current_date()?;
    updated.push_str(&self.format_version_header(plan, previous_tag.as_deref(), &date, github_repo.as_ref()));
    updated.push_str(&new_entries);
    updated.push('\n');

    if new_entries.trim().is_empty() {
      if self.release_config.require_changelog_entries {
        return Err(RailError::message(format!(
          "No changelog entries found for {} (enable commits or disable changelog generation)",
          plan.name
        )));
      } else {
        println!(
          "   ℹ️  No changelog entries for {}, skipping changelog write",
          plan.name
        );
        return Ok(());
      }
    }

    // Add rest of existing changelog
    if lines.len() > 1 {
      for line in &lines[1..] {
        updated.push_str(line);
        updated.push('\n');
      }
    }

    // Write updated changelog
    fs::write(&plan.changelog_path, updated)
      .map_err(|e| RailError::message(format!("Failed to write {}: {}", plan.changelog_path.display(), e)))?;

    Ok(())
  }

  /// Commit version bump and changelog
  fn commit_version_bump(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let message = format!("chore(release): {} v{}", plan.name, plan.new_version);

    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["add", "."])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git add: {}", e)))?;

    if !output.status.success() {
      return Err(RailError::message("git add failed"));
    }

    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["commit", "-m", &message])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git commit: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!("git commit failed: {}", stderr)));
    }

    Ok(())
  }

  /// Create git tag
  fn create_tag(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let mut cmd = Command::new("git");
    cmd.current_dir(self.ctx.workspace_root()).args(["tag"]);

    if self.release_config.sign_tags {
      cmd.arg("-s");
    } else {
      cmd.arg("-a");
    }

    cmd.args([
      &plan.tag_name,
      "-m",
      &format!("Release {} v{}", plan.name, plan.new_version),
    ]);

    let output = cmd
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git tag: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!("git tag failed: {}", stderr)));
    }

    Ok(())
  }

  /// Publish crate to crates.io
  fn publish_crate(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    let crate_dir = plan
      .manifest_path
      .parent()
      .ok_or_else(|| RailError::message("Invalid manifest path"))?;

    let output = Command::new("cargo")
      .current_dir(crate_dir)
      .args(["publish", "--allow-dirty"]) // dirty because we just committed
      .output()
      .map_err(|e| RailError::message(format!("Failed to run cargo publish: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::message(format!(
        "cargo publish failed for {}: {}",
        plan.name, stderr
      )));
    }

    Ok(())
  }

  /// Create GitHub release using gh CLI
  fn create_github_release(&self, plan: &CrateReleasePlan) -> RailResult<()> {
    // Check if gh CLI is available
    let check = Command::new("gh").args(["--version"]).output();

    if check.is_err() || !check.unwrap().status.success() {
      println!("   ⚠️  gh CLI not found, skipping GitHub release");
      return Ok(());
    }

    // Read changelog for release notes
    let notes = if plan.changelog_path.exists() {
      fs::read_to_string(&plan.changelog_path)
        .unwrap_or_else(|_| format!("Release {} v{}", plan.name, plan.new_version))
    } else {
      format!("Release {} v{}", plan.name, plan.new_version)
    };

    let output = Command::new("gh")
      .current_dir(self.ctx.workspace_root())
      .args([
        "release",
        "create",
        &plan.tag_name,
        "--title",
        &format!("{} v{}", plan.name, plan.new_version),
        "--notes",
        &notes,
      ])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run gh release: {}", e)))?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      println!("   ⚠️  GitHub release creation failed: {}", stderr);
      // Don't error out - this is optional
    }

    Ok(())
  }

  /// Get current date in YYYY-MM-DD format (using system git)
  fn get_current_date(&self) -> RailResult<String> {
    // Use git to get current date (portable across platforms)
    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["log", "-1", "--format=%cd", "--date=short"])
      .output()
      .map_err(|e| RailError::message(format!("Failed to get date: {}", e)))?;

    if output.status.success() {
      let date = String::from_utf8_lossy(&output.stdout).trim().to_string();
      if !date.is_empty() {
        return Ok(date);
      }
    }

    // Fallback: use system date command
    let output = Command::new("date")
      .args(["+%Y-%m-%d"])
      .output()
      .map_err(|e| RailError::message(format!("Failed to get system date: {}", e)))?;

    let date = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(date)
  }

  fn format_version_header(
    &self,
    plan: &CrateReleasePlan,
    previous_tag: Option<&str>,
    date: &str,
    github_repo: Option<&(String, String)>,
  ) -> String {
    if let Some((org, repo)) = github_repo {
      let url = if let Some(prev) = previous_tag {
        format!(
          "https://github.com/{}/{}/compare/{}...{}",
          org, repo, prev, plan.tag_name
        )
      } else {
        format!("https://github.com/{}/{}/releases/tag/{}", org, repo, plan.tag_name)
      };

      return format!("## [{}]({}) - {}\n\n", plan.new_version, url, date);
    }

    format!("## [{}] - {}\n\n", plan.new_version, date)
  }

  /// Find previous tag for a crate
  fn find_previous_tag(&self, plan: &CrateReleasePlan) -> RailResult<Option<String>> {
    let workspace_members = self.ctx.graph.workspace_members();
    let is_single_crate = workspace_members.len() == 1;

    let pattern = if is_single_crate {
      format!("{}*", self.release_config.tag_prefix)
    } else {
      self
        .release_config
        .tag_format
        .replace("{crate}", &plan.name)
        .replace("{version}", "*")
    };

    let output = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["tag", "--list", &pattern, "--sort=-version:refname"])
      .output()
      .map_err(|e| RailError::message(format!("Failed to run git tag: {}", e)))?;

    if !output.status.success() {
      return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let tags: Vec<&str> = stdout.lines().collect();

    Ok(tags.first().map(|s| s.to_string()))
  }
}
