use crate::cargo::{CargoTransform, TransformContext};
use crate::config::SplitMode;
use crate::error::{GitError, RailError, RailResult, ResultExt};
use crate::git::mappings::MappingStore;
use crate::git::{CommitInfo, SystemGit};
use crate::utils;
use crate::workspace::WorkspaceContext;
use crate::workspace::files::{AuxiliaryFiles, ProjectFiles};
use std::path::{Path, PathBuf};

/// Configuration for a split operation
pub struct SplitConfig {
  /// Name of the crate being split
  pub crate_name: String,
  /// Paths to crate directories in monorepo
  pub crate_paths: Vec<PathBuf>,
  /// Split mode (single or combined)
  pub mode: SplitMode,
  /// Target repository path
  pub target_repo_path: PathBuf,
  /// Branch name for split repo
  pub branch: String,
  /// Remote repository URL
  pub remote_url: Option<String>,
}

/// Parameters for recreating a commit in the target repository
struct RecreateCommitParams<'a> {
  commit: &'a CommitInfo,
  crate_paths: &'a [PathBuf],
  target_repo_path: &'a Path,
  crate_name: &'a str,
  mode: &'a SplitMode,
  mapping_store: &'a MappingStore,
  last_recreated_sha: Option<&'a str>,
}

/// Parameters for creating a git commit
struct CommitParams<'a> {
  repo_path: &'a Path,
  message: &'a str,
  author_name: &'a str,
  author_email: &'a str,
  committer_name: &'a str,
  committer_email: &'a str,
  timestamp: i64,
  parent_shas: &'a [String],
}

/// Split engine - extracts crates with full history
///
/// Deterministic git splitting: same input = same commit SHAs
/// Uses WorkspaceContext for git and cargo operations - no duplicate loads.
pub struct SplitEngine<'a> {
  ctx: &'a WorkspaceContext,
  transform: CargoTransform,
}

impl<'a> SplitEngine<'a> {
  /// Create a new split engine from workspace context
  pub fn new(ctx: &'a WorkspaceContext) -> RailResult<Self> {
    // Build CargoTransform from context's metadata
    let transform = CargoTransform::new(ctx.cargo.metadata().clone());

    Ok(Self { ctx, transform })
  }

  /// Walk commit history and filter commits that touch the given paths
  /// Returns commits in chronological order (oldest first)
  fn walk_filtered_history(&self, paths: &[PathBuf]) -> RailResult<Vec<CommitInfo>> {
    println!("   Walking commit history to find commits touching crate...");

    // Use batched git command for all paths at once (much faster than N separate calls)
    let filtered_commits = self.ctx.git.git().get_commits_touching_paths(paths, None, "HEAD")?;

    println!(
      "   Found {} total commits that touch the crate paths",
      filtered_commits.len()
    );

    Ok(filtered_commits)
  }

  /// Apply Cargo.toml transformation to a manifest file
  /// Returns Ok(()) if transform succeeded or file doesn't exist
  fn apply_manifest_transform(&self, manifest_path: &Path, crate_name: &str) -> RailResult<()> {
    if !manifest_path.exists() {
      return Ok(());
    }

    let content = std::fs::read_to_string(manifest_path)?;
    let context = TransformContext {
      crate_name: crate_name.to_string(),
      workspace_root: self.ctx.workspace_root().to_path_buf(),
    };
    let transformed = self.transform.transform_to_split(&content, &context)?;
    std::fs::write(manifest_path, transformed)?;
    Ok(())
  }

  /// Recreate a commit in the target repository with transforms applied
  /// Returns the new commit SHA
  fn recreate_commit_in_target(&self, params: &RecreateCommitParams) -> RailResult<String> {
    // Collect all files for the crate at this commit
    let mut all_files = Vec::new();
    for crate_path in params.crate_paths {
      let files = self.ctx.git.git().collect_tree_files(&params.commit.sha, crate_path)?;
      all_files.extend(files);
    }

    if all_files.is_empty() {
      return Err(RailError::message(format!(
        "No files found for commit {} at paths {:?}",
        params.commit.sha, params.crate_paths
      )));
    }

    // Write files to target repo, applying transforms
    for (file_path, content_bytes) in &all_files {
      let target_path = match params.mode {
        SplitMode::Single => {
          // For single mode, move files to root (strip crate path prefix)
          let mut relative = file_path.clone();
          for crate_path in params.crate_paths {
            if let Ok(stripped) = file_path.strip_prefix(crate_path) {
              relative = stripped.to_path_buf();
              break;
            }
          }
          params.target_repo_path.join(relative)
        }
        SplitMode::Combined => {
          // For combined mode, preserve paths
          params.target_repo_path.join(file_path)
        }
      };

      // Create parent directories
      if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
      }

      // Write file content
      std::fs::write(&target_path, content_bytes)?;

      // Apply Cargo.toml transformation if applicable
      if file_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        self.apply_manifest_transform(&target_path, params.crate_name)?;
      }
    }

    // Auxiliary files are handled separately via AuxiliaryFiles::discover
    // This section has been removed as it was redundant

    // Create commit using git command for determinism
    // Map parent SHAs from monorepo to split repo
    let mut mapped_parents: Vec<String> = params
      .commit
      .parent_shas
      .iter()
      .filter_map(|parent_sha| params.mapping_store.get_mapping(parent_sha).ok().flatten())
      .collect();

    // If no mapped parents (because original parents were filtered out),
    // use the last recreated commit as parent to maintain linear history
    if mapped_parents.is_empty()
      && let Some(ref sha) = params.last_recreated_sha
    {
      mapped_parents.push(sha.to_string());
    }

    self.create_git_commit(&CommitParams {
      repo_path: params.target_repo_path,
      message: &params.commit.message,
      author_name: &params.commit.author,
      author_email: &params.commit.author_email,
      committer_name: &params.commit.committer,
      committer_email: &params.commit.committer_email,
      timestamp: params.commit.timestamp,
      parent_shas: &mapped_parents,
    })
  }

  /// Create a git commit using git commands for determinism
  /// Uses git commit-tree for full control over parents
  fn create_git_commit(&self, params: &CommitParams) -> RailResult<String> {
    use std::process::Command;

    // Stage all files
    let status = Command::new("git")
      .current_dir(params.repo_path)
      .args(["add", "-A"])
      .status()
      .context("Failed to run git add")?;

    if !status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git add".to_string(),
        stderr: "git add failed".to_string(),
      }));
    }

    // Write the tree
    let output = Command::new("git")
      .current_dir(params.repo_path)
      .args(["write-tree"])
      .output()
      .context("Failed to write tree")?;

    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git write-tree".to_string(),
        stderr: "git write-tree failed".to_string(),
      }));
    }

    let tree_sha = String::from_utf8(output.stdout)?.trim().to_string();

    // Prepare environment for deterministic commit
    let author_date = format!("{} +0000", params.timestamp);
    let commit_date = format!("{} +0000", params.timestamp);

    // Build commit-tree command
    let mut cmd = Command::new("git");
    cmd
      .current_dir(params.repo_path)
      .env("GIT_AUTHOR_NAME", params.author_name)
      .env("GIT_AUTHOR_EMAIL", params.author_email)
      .env("GIT_AUTHOR_DATE", &author_date)
      .env("GIT_COMMITTER_NAME", params.committer_name)
      .env("GIT_COMMITTER_EMAIL", params.committer_email)
      .env("GIT_COMMITTER_DATE", &commit_date)
      .arg("commit-tree")
      .arg(&tree_sha)
      .arg("-m")
      .arg(params.message);

    // Add parent arguments
    for parent in params.parent_shas {
      cmd.arg("-p").arg(parent);
    }

    // Execute commit-tree
    let output = cmd.output().context("Failed to run git commit-tree")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git commit-tree".to_string(),
        stderr: stderr.to_string(),
      }));
    }

    let commit_sha = String::from_utf8(output.stdout)?.trim().to_string();

    // Update the branch reference
    Command::new("git")
      .current_dir(params.repo_path)
      .args(["update-ref", "HEAD", &commit_sha])
      .status()
      .context("Failed to update HEAD")?;

    Ok(commit_sha)
  }

  /// Check if remote repository exists and has content
  fn check_remote_exists(&self, remote_url: &str) -> RailResult<bool> {
    use std::process::Command;

    let output = Command::new("git")
      .args(["ls-remote", "--heads", remote_url])
      .output()
      .context("Failed to check remote")?;

    // If command succeeds and has output, remote exists with content
    Ok(output.status.success() && !output.stdout.is_empty())
  }

  /// Execute a split operation (ONE-TIME ONLY - use sync for updates)
  pub fn split(&self, config: &SplitConfig) -> RailResult<()> {
    println!("🚂 Splitting crate: {}", config.crate_name);
    println!("   Mode: {:?}", config.mode);
    println!("   Target: {}", config.target_repo_path.display());

    // Check if remote already exists - if so, error with helpful message
    if let Some(ref remote_url) = config.remote_url {
      let remote_exists = self.check_remote_exists(remote_url)?;
      if remote_exists {
        return Err(RailError::with_help(
          format!("Split already exists at {}", remote_url),
          format!(
            "Split is a one-time operation. To update the split repo, use:\n  \
             cargo rail sync {}\n\n\
             This will sync new commits from the monorepo to the split repo.",
            config.crate_name
          ),
        ));
      }
    }

    // Create fresh target repo
    self.ensure_target_repo(&config.target_repo_path)?;

    // Discover workspace-level auxiliary files from workspace
    let aux_files = AuxiliaryFiles::discover(self.ctx.workspace_root())?;
    println!("   Found {} workspace config files", aux_files.count());

    // Discover project files (README, LICENSE) with crate-first fallback
    let crate_path = &config.crate_paths[0]; // Use first crate path for project files
    let project_files = ProjectFiles::discover(self.ctx.workspace_root(), crate_path)?;
    println!("   Found {} project files (README, LICENSE)", project_files.count());

    // Create mapping store
    let mut mapping_store = MappingStore::new(config.crate_name.clone());
    mapping_store.load(self.ctx.workspace_root())?;

    // Walk filtered history to find commits touching the crate
    let filtered_commits = self.walk_filtered_history(&config.crate_paths)?;

    if filtered_commits.is_empty() {
      println!("   No commits found that touch the crate paths");
      println!("   Falling back to current state copy...");

      // Fallback to snapshot copy if no history found
      match config.mode {
        SplitMode::Single => {
          let crate_path = &config.crate_paths[0];
          self.split_single_crate(crate_path, &config.target_repo_path, &aux_files, &config.crate_name)?;
        }
        SplitMode::Combined => {
          self.split_combined_crates(
            &config.crate_paths,
            &config.target_repo_path,
            &aux_files,
            &config.crate_name,
          )?;
        }
      }
    } else {
      // Recreate history in target repo
      println!("   Processing {} commits...", filtered_commits.len());

      let mut last_recreated_sha: Option<String> = None;

      for (idx, commit) in filtered_commits.iter().enumerate() {
        if (idx + 1) % 10 == 0 || idx + 1 == filtered_commits.len() {
          println!("   Progress: {}/{} commits", idx + 1, filtered_commits.len());
        }
        let new_sha = self.recreate_commit_in_target(&RecreateCommitParams {
          commit,
          crate_paths: &config.crate_paths,
          target_repo_path: &config.target_repo_path,
          crate_name: &config.crate_name,
          mode: &config.mode,
          mapping_store: &mapping_store,
          last_recreated_sha: last_recreated_sha.as_deref(),
        })?;

        // Record mapping
        mapping_store.record_mapping(&commit.sha, &new_sha)?;

        // Track last recreated commit
        last_recreated_sha = Some(new_sha);
      }

      // Copy workspace config files and project files to the final state
      let has_files = !aux_files.is_empty() || project_files.count() > 0;
      if has_files {
        println!("   Copying workspace configs and project files...");
        aux_files.copy_to_split(self.ctx.workspace_root(), &config.target_repo_path)?;
        project_files.copy_to_split(self.ctx.workspace_root(), &config.target_repo_path)?;

        // Create a final commit if any files were added
        // git add -A is safe to run unconditionally (no-op if no changes)
        std::process::Command::new("git")
          .current_dir(&config.target_repo_path)
          .args(["add", "-A"])
          .status()?;

        // Check if there are staged changes before committing
        let diff_cached = std::process::Command::new("git")
          .current_dir(&config.target_repo_path)
          .args(["diff", "--cached", "--quiet"])
          .status()?;

        if !diff_cached.success() {
          // Exit code 1 means there are differences (i.e., staged changes)
          println!("   Creating commit for auxiliary files");
          std::process::Command::new("git")
            .current_dir(&config.target_repo_path)
            .args(["commit", "-m", "Add workspace configs and project files"])
            .status()?;
        }
      }
    }

    // Save mappings to both workspace and target repo
    mapping_store.save(self.ctx.workspace_root())?;
    mapping_store.save(&config.target_repo_path)?;

    // Push to remote if URL is configured and is not a local file path
    if let Some(ref remote_url) = config.remote_url {
      if !remote_url.is_empty() && !utils::is_local_path(remote_url) {
        println!("\n🚀 Pushing to remote...");

        // Open the target repo
        let target_git = SystemGit::open(&config.target_repo_path)?;

        // Add or update remote
        if !target_git.has_remote("origin")? {
          println!("   Adding remote 'origin': {}", remote_url);
          target_git.add_remote("origin", remote_url)?;
        } else {
          println!("   Remote 'origin' already exists");
        }

        // Push to remote
        target_git.push_to_remote("origin", &config.branch)?;

        // Push git-notes
        mapping_store.push_notes(&config.target_repo_path, "origin")?;

        println!("   ✅ Pushed to {}", remote_url);
      } else {
        println!("\n💾 Split repository created locally");
        if utils::is_local_path(remote_url) {
          println!("   Note: Remote is a local path, skipping push");
          println!(
            "   Local testing mode - split repo at: {}",
            config.target_repo_path.display()
          );
        } else {
          println!("   No remote URL configured");
        }
        println!("\n   To push to a real remote later:");
        println!("   cd {}", config.target_repo_path.display());
        println!("   git remote add origin <url>");
        println!("   git push -u origin {}", config.branch);
      }
    } else {
      println!("\n⚠️  No remote URL configured - repository created locally only");
      println!("   To push manually:");
      println!("   cd {}", config.target_repo_path.display());
      println!("   git remote add origin <url>");
      println!("   git push -u origin {}", config.branch);
    }

    println!("\n✅ Split complete!");
    println!("   Target repo: {}", config.target_repo_path.display());

    Ok(())
  }

  /// Ensure target repository exists and is initialized
  fn ensure_target_repo(&self, target_path: &Path) -> RailResult<()> {
    if !target_path.exists() {
      std::fs::create_dir_all(target_path)
        .with_context(|| format!("Failed to create target directory: {}", target_path.display()))?;
    }

    // Check if it's already a git repo
    let git_dir = target_path.join(".git");
    if !git_dir.exists() {
      println!("   Initializing git repository at {}", target_path.display());

      // Initialize using system git with main as default branch
      std::process::Command::new("git")
        .arg("init")
        .arg("--initial-branch=main")
        .arg(target_path)
        .output()
        .with_context(|| format!("Failed to initialize git repository at {}", target_path.display()))?;

      // Configure git identity from source repository
      self.configure_git_identity(target_path)?;
    }

    Ok(())
  }

  /// Configure git identity in the target repository by copying from source
  fn configure_git_identity(&self, target_path: &Path) -> RailResult<()> {
    use std::process::Command;

    // Get identity from source repository
    let user_name = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["config", "user.name"])
      .output()
      .ok()
      .and_then(|o| {
        if o.status.success() {
          Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
          None
        }
      });

    let user_email = Command::new("git")
      .current_dir(self.ctx.workspace_root())
      .args(["config", "user.email"])
      .output()
      .ok()
      .and_then(|o| {
        if o.status.success() {
          Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
          None
        }
      });

    // Set identity in target repository
    // Use a fallback if source doesn't have identity configured
    let name = user_name.as_deref().unwrap_or("Cargo Rail");
    let email = user_email.as_deref().unwrap_or("cargo-rail@localhost");

    let output = Command::new("git")
      .current_dir(target_path)
      .args(["config", "user.name", name])
      .output()
      .context("Failed to configure git user.name")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git config user.name".to_string(),
        stderr: stderr.to_string(),
      }));
    }

    let output = Command::new("git")
      .current_dir(target_path)
      .args(["config", "user.email", email])
      .output()
      .context("Failed to configure git user.email")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git config user.email".to_string(),
        stderr: stderr.to_string(),
      }));
    }

    Ok(())
  }

  /// Split a single crate (move to root of target repo)
  fn split_single_crate(
    &self,
    crate_path: &Path,
    target_repo_path: &Path,
    aux_files: &AuxiliaryFiles,
    crate_name: &str,
  ) -> RailResult<()> {
    let source_path = self.ctx.workspace_root().join(crate_path);

    // Copy source files
    println!("   Copying source files from {}", crate_path.display());
    self.copy_directory_recursive(&source_path, target_repo_path)?;

    // Transform Cargo.toml manifest
    println!("   Transforming Cargo.toml");
    let manifest_path = target_repo_path.join("Cargo.toml");
    self.apply_manifest_transform(&manifest_path, crate_name)?;

    // Copy auxiliary files
    if !aux_files.is_empty() {
      println!("   Copying auxiliary files");
      aux_files.copy_to_split(self.ctx.workspace_root(), target_repo_path)?;
    }

    Ok(())
  }

  /// Split multiple crates (preserve structure in target repo)
  fn split_combined_crates(
    &self,
    crate_paths: &[PathBuf],
    target_repo_path: &Path,
    aux_files: &AuxiliaryFiles,
    crate_name: &str,
  ) -> RailResult<()> {
    for crate_path in crate_paths {
      let source_path = self.ctx.workspace_root().join(crate_path);
      let target_path = target_repo_path.join(crate_path);

      println!("   Copying {} to {}", crate_path.display(), crate_path.display());

      // Create parent directories
      if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
      }

      self.copy_directory_recursive(&source_path, &target_path)?;

      // Transform Cargo.toml manifest
      let manifest_path = target_path.join("Cargo.toml");
      self.apply_manifest_transform(&manifest_path, crate_name)?;
    }

    // Copy auxiliary files
    if !aux_files.is_empty() {
      println!("   Copying auxiliary files");
      aux_files.copy_to_split(self.ctx.workspace_root(), target_repo_path)?;
    }

    Ok(())
  }

  /// Recursively copy a directory, excluding .git
  fn copy_directory_recursive(&self, source: &Path, target: &Path) -> RailResult<()> {
    copy_directory_recursive_impl(source, target)
  }
}

/// Helper function to recursively copy a directory, excluding .git
fn copy_directory_recursive_impl(source: &Path, target: &Path) -> RailResult<()> {
  if !source.exists() {
    return Err(RailError::message(format!(
      "Source path does not exist: {}",
      source.display()
    )));
  }

  if source.is_file() {
    if let Some(parent) = target.parent() {
      std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, target)?;
    return Ok(());
  }

  std::fs::create_dir_all(target)?;

  for entry in std::fs::read_dir(source)? {
    let entry = entry?;
    let file_type = entry.file_type()?;
    let file_name = entry.file_name();

    // Skip .git directory
    if file_name == ".git" {
      continue;
    }

    let source_path = entry.path();
    let target_path = target.join(&file_name);

    if file_type.is_dir() {
      copy_directory_recursive_impl(&source_path, &target_path)?;
    } else {
      std::fs::copy(&source_path, &target_path)?;
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

  /// Helper to find the git repository root from the current directory.
  /// This is needed because tests run from the crate directory, but the
  /// git repository may be at the workspace root.
  fn find_git_root() -> PathBuf {
    let current_dir = std::env::current_dir().unwrap();
    match SystemGit::open(&current_dir) {
      Ok(git) => git.work_tree.clone(),
      Err(_) => current_dir,
    }
  }

  #[test]
  fn test_copy_directory_recursive() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");

    // Create source structure
    fs::create_dir_all(source.join("src")).unwrap();
    fs::write(source.join("Cargo.toml"), "test").unwrap();
    fs::write(source.join("src/lib.rs"), "pub fn test() {}").unwrap();
    fs::create_dir(source.join(".git")).unwrap(); // Should be excluded

    let workspace_root = find_git_root();
    let ctx = WorkspaceContext::build(&workspace_root).unwrap();
    let engine = SplitEngine::new(&ctx).unwrap();

    engine.copy_directory_recursive(&source, &target).unwrap();

    // Verify files copied
    assert!(target.join("Cargo.toml").exists());
    assert!(target.join("src/lib.rs").exists());

    // Verify .git excluded
    assert!(!target.join(".git").exists());
  }
}
