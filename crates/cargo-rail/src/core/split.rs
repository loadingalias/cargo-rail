#![allow(dead_code)]

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::cargo::files::{AuxiliaryFiles, ProjectFiles};
use crate::cargo::metadata::WorkspaceMetadata;
use crate::cargo::transform::CargoTransform;
use crate::core::config::SplitMode;
use crate::core::mapping::MappingStore;
use crate::core::transform::{Transform, TransformContext};
use crate::core::vcs::git::GitBackend;
use crate::core::vcs::{CommitInfo, Vcs};

/// Configuration for a split operation
pub struct SplitConfig {
  pub crate_name: String,
  pub crate_paths: Vec<PathBuf>,
  pub mode: SplitMode,
  pub target_repo_path: PathBuf,
  pub branch: String,
  pub remote_url: Option<String>,
}

/// Parameters for recreating a commit in the target repository
struct RecreateCommitParams<'a> {
  commit: &'a CommitInfo,
  crate_paths: &'a [PathBuf],
  target_repo_path: &'a Path,
  transformer: &'a CargoTransform,
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

/// Deterministic Git splitter
/// Extracts crates with full history, ensuring same input = same commit SHAs
pub struct Splitter {
  workspace_root: PathBuf,
  git: GitBackend,
  metadata: WorkspaceMetadata,
}

impl Splitter {
  /// Create a new splitter for a workspace
  pub fn new(workspace_root: PathBuf) -> Result<Self> {
    let git = GitBackend::open(&workspace_root)?;
    let metadata = WorkspaceMetadata::load(&workspace_root)?;

    Ok(Self {
      workspace_root,
      git,
      metadata,
    })
  }

  /// Walk commit history and filter commits that touch the given paths
  /// Returns commits in chronological order (oldest first)
  fn walk_filtered_history(&self, paths: &[PathBuf]) -> Result<Vec<CommitInfo>> {
    println!("   Walking commit history to find commits touching crate...");

    // Collect commits that touch any of the paths
    // Use IndexMap to deduplicate while preserving insertion order from git log
    let mut commits_by_sha = indexmap::IndexMap::new();

    for path in paths {
      // Get all commits that touch this path (already in chronological order from git log --reverse)
      let path_commits = self.git.get_commits_touching_path(path, None, "HEAD")?;

      // Add to our deduplication map (preserves first insertion order)
      for commit in path_commits {
        commits_by_sha.entry(commit.sha.clone()).or_insert(commit);
      }
    }

    // Convert to vec - order is already chronological from git log
    let filtered_commits: Vec<_> = commits_by_sha.into_values().collect();

    println!(
      "   Found {} total commits that touch the crate paths",
      filtered_commits.len()
    );

    Ok(filtered_commits)
  }

  /// Recreate a commit in the target repository with transforms applied
  /// Returns the new commit SHA
  fn recreate_commit_in_target(&self, params: &RecreateCommitParams) -> Result<String> {
    // Collect all files for the crate at this commit
    let mut all_files = Vec::new();
    for crate_path in params.crate_paths {
      let files = self.git.collect_tree_files(&params.commit.sha, crate_path)?;
      all_files.extend(files);
    }

    if all_files.is_empty() {
      anyhow::bail!(
        "No files found for commit {} at paths {:?}",
        params.commit.sha,
        params.crate_paths
      );
    }

    // Write files to target repo, applying transforms
    for (file_path, content) in &all_files {
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

      // Apply Cargo.toml transformation if applicable
      if file_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content_str = String::from_utf8(content.clone())
          .with_context(|| format!("Cargo.toml is not valid UTF-8 at {}", file_path.display()))?;
        let transformed = params.transformer.transform_to_split(
          &content_str,
          &TransformContext {
            crate_name: params.crate_name.to_string(),
            workspace_root: self.workspace_root.clone(),
          },
        )?;
        std::fs::write(&target_path, transformed)?;
      } else {
        std::fs::write(&target_path, content)?;
      }
    }

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
    if mapped_parents.is_empty() && params.last_recreated_sha.is_some() {
      mapped_parents.push(params.last_recreated_sha.unwrap().to_string());
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
  fn create_git_commit(&self, params: &CommitParams) -> Result<String> {
    use std::process::Command;

    // Stage all files
    let status = Command::new("git")
      .current_dir(params.repo_path)
      .args(["add", "-A"])
      .status()
      .context("Failed to run git add")?;

    if !status.success() {
      anyhow::bail!("git add failed");
    }

    // Write the tree
    let output = Command::new("git")
      .current_dir(params.repo_path)
      .args(["write-tree"])
      .output()
      .context("Failed to write tree")?;

    if !output.status.success() {
      anyhow::bail!("git write-tree failed");
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
      anyhow::bail!("git commit-tree failed: {}", stderr);
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
  fn check_remote_exists(&self, remote_url: &str) -> Result<bool> {
    use std::process::Command;

    let output = Command::new("git")
      .args(["ls-remote", "--heads", remote_url])
      .output()
      .context("Failed to check remote")?;

    // If command succeeds and has output, remote exists with content
    Ok(output.status.success() && !output.stdout.is_empty())
  }

  /// Clone remote repository to local path
  fn clone_remote(&self, remote_url: &str, target_path: &Path) -> Result<()> {
    use std::process::Command;

    println!("   Cloning existing remote repository...");

    let output = Command::new("git")
      .args(["clone", remote_url, target_path.to_str().unwrap()])
      .output()
      .context("Failed to clone remote")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      anyhow::bail!("git clone failed: {}", stderr);
    }

    println!("   ✅ Cloned remote repository");
    Ok(())
  }

  /// Execute a split operation (ONE-TIME ONLY - use sync for updates)
  pub fn split(&self, config: &SplitConfig) -> Result<()> {
    println!("🚂 Splitting crate: {}", config.crate_name);
    println!("   Mode: {:?}", config.mode);
    println!("   Target: {}", config.target_repo_path.display());

    // Check if remote already exists - if so, error with helpful message
    if let Some(ref remote_url) = config.remote_url {
      let remote_exists = self.check_remote_exists(remote_url)?;
      if remote_exists {
        anyhow::bail!(
          "Split already exists at {}\n\n\
           Split is a one-time operation. To update the split repo, use:\n  \
           cargo rail sync {}\n\n\
           This will sync new commits from the monorepo to the split repo.",
          remote_url,
          config.crate_name
        );
      }
    }

    // Create fresh target repo
    self.ensure_target_repo(&config.target_repo_path)?;

    // Create transformer for Cargo.toml
    let transformer = CargoTransform::new(WorkspaceMetadata::load(&self.workspace_root)?);

    // Discover workspace-level auxiliary files (rust-toolchain, rustfmt, etc.)
    let aux_files = AuxiliaryFiles::discover(&self.workspace_root)?;
    println!("   Found {} workspace config files", aux_files.count());

    // Discover project files (README, LICENSE) with crate-first fallback
    let crate_path = &config.crate_paths[0]; // Use first crate path for project files
    let project_files = ProjectFiles::discover(&self.workspace_root, crate_path)?;
    println!("   Found {} project files (README, LICENSE)", project_files.count());

    // Create mapping store
    let mut mapping_store = MappingStore::new(config.crate_name.clone());
    mapping_store.load(&self.workspace_root)?;

    // Walk filtered history to find commits touching the crate
    let filtered_commits = self.walk_filtered_history(&config.crate_paths)?;

    if filtered_commits.is_empty() {
      println!("   No commits found that touch the crate paths");
      println!("   Falling back to current state copy...");

      // Fallback to snapshot copy if no history found
      match config.mode {
        SplitMode::Single => {
          let crate_path = &config.crate_paths[0];
          self.split_single_crate(
            crate_path,
            &config.target_repo_path,
            &transformer,
            &aux_files,
            &config.crate_name,
          )?;
        }
        SplitMode::Combined => {
          self.split_combined_crates(
            &config.crate_paths,
            &config.target_repo_path,
            &transformer,
            &aux_files,
            &config.crate_name,
          )?;
        }
      }
    } else {
      // Recreate history in target repo
      println!(
        "   Recreating {} commits in target repository...",
        filtered_commits.len()
      );

      let mut last_recreated_sha: Option<String> = None;

      for (idx, commit) in filtered_commits.iter().enumerate() {
        println!("   [{}/{}] {}", idx + 1, filtered_commits.len(), commit.summary());

        let new_sha = self.recreate_commit_in_target(&RecreateCommitParams {
          commit,
          crate_paths: &config.crate_paths,
          target_repo_path: &config.target_repo_path,
          transformer: &transformer,
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
        aux_files.copy_to_split(&self.workspace_root, &config.target_repo_path)?;
        project_files.copy_to_split(&self.workspace_root, &config.target_repo_path)?;

        // Create a final commit if any files were added
        let status = std::process::Command::new("git")
          .current_dir(&config.target_repo_path)
          .args(["status", "--porcelain"])
          .output()?;

        if !status.stdout.is_empty() {
          println!("   Creating commit for auxiliary files");
          std::process::Command::new("git")
            .current_dir(&config.target_repo_path)
            .args(["add", "-A"])
            .status()?;
          std::process::Command::new("git")
            .current_dir(&config.target_repo_path)
            .args(["commit", "-m", "Add workspace configs and project files"])
            .status()?;
        }
      }
    }

    // Save mappings to both workspace and target repo
    mapping_store.save(&self.workspace_root)?;
    mapping_store.save(&config.target_repo_path)?;

    // Push to remote if URL is configured and is not a local file path
    if let Some(ref remote_url) = config.remote_url {
      // Check if this is a local file path (absolute path or relative)
      let is_local_path = remote_url.starts_with('/') || remote_url.starts_with("./") || remote_url.starts_with("../");

      if !remote_url.is_empty() && !is_local_path {
        println!("\n🚀 Pushing to remote...");

        // Open the target repo
        let target_git = GitBackend::open(&config.target_repo_path)?;

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
        if is_local_path {
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
  fn ensure_target_repo(&self, target_path: &Path) -> Result<()> {
    if !target_path.exists() {
      std::fs::create_dir_all(target_path)
        .with_context(|| format!("Failed to create target directory: {}", target_path.display()))?;
    }

    // Check if it's already a git repo
    let git_dir = target_path.join(".git");
    if !git_dir.exists() {
      println!("   Initializing git repository at {}", target_path.display());

      // Initialize using gix
      gix::init(target_path)
        .with_context(|| format!("Failed to initialize git repository at {}", target_path.display()))?;
    }

    Ok(())
  }

  /// Split a single crate (move to root of target repo)
  fn split_single_crate(
    &self,
    crate_path: &Path,
    target_repo_path: &Path,
    transformer: &CargoTransform,
    aux_files: &AuxiliaryFiles,
    crate_name: &str,
  ) -> Result<()> {
    let source_path = self.workspace_root.join(crate_path);

    // Copy source files
    println!("   Copying source files from {}", crate_path.display());
    self.copy_directory_recursive(&source_path, target_repo_path)?;

    // Transform Cargo.toml
    let cargo_toml_path = target_repo_path.join("Cargo.toml");
    if cargo_toml_path.exists() {
      println!("   Transforming Cargo.toml");
      let content = std::fs::read_to_string(&cargo_toml_path)?;
      let transformed = transformer.transform_to_split(
        &content,
        &TransformContext {
          crate_name: crate_name.to_string(),
          workspace_root: self.workspace_root.clone(),
        },
      )?;
      std::fs::write(&cargo_toml_path, transformed)?;
    }

    // Copy auxiliary files
    if !aux_files.is_empty() {
      println!("   Copying auxiliary files");
      aux_files.copy_to_split(&self.workspace_root, target_repo_path)?;
    }

    Ok(())
  }

  /// Split multiple crates (preserve structure in target repo)
  fn split_combined_crates(
    &self,
    crate_paths: &[PathBuf],
    target_repo_path: &Path,
    transformer: &CargoTransform,
    aux_files: &AuxiliaryFiles,
    crate_name: &str,
  ) -> Result<()> {
    for crate_path in crate_paths {
      let source_path = self.workspace_root.join(crate_path);
      let target_path = target_repo_path.join(crate_path);

      println!("   Copying {} to {}", crate_path.display(), crate_path.display());

      // Create parent directories
      if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
      }

      self.copy_directory_recursive(&source_path, &target_path)?;

      // Transform Cargo.toml
      let cargo_toml_path = target_path.join("Cargo.toml");
      if cargo_toml_path.exists() {
        let content = std::fs::read_to_string(&cargo_toml_path)?;
        let transformed = transformer.transform_to_split(
          &content,
          &TransformContext {
            crate_name: crate_name.to_string(),
            workspace_root: self.workspace_root.clone(),
          },
        )?;
        std::fs::write(&cargo_toml_path, transformed)?;
      }
    }

    // Copy auxiliary files
    if !aux_files.is_empty() {
      println!("   Copying auxiliary files");
      aux_files.copy_to_split(&self.workspace_root, target_repo_path)?;
    }

    Ok(())
  }

  /// Recursively copy a directory, excluding .git
  fn copy_directory_recursive(&self, source: &Path, target: &Path) -> Result<()> {
    copy_directory_recursive_impl(source, target)
  }
}

/// Helper function to recursively copy a directory, excluding .git
fn copy_directory_recursive_impl(source: &Path, target: &Path) -> Result<()> {
  if !source.exists() {
    anyhow::bail!("Source path does not exist: {}", source.display());
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
    match gix::discover(&current_dir) {
      Ok(repo) => repo.workdir().unwrap().to_path_buf(),
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
    let splitter = Splitter::new(workspace_root).unwrap();

    splitter.copy_directory_recursive(&source, &target).unwrap();

    // Verify files copied
    assert!(target.join("Cargo.toml").exists());
    assert!(target.join("src/lib.rs").exists());

    // Verify .git excluded
    assert!(!target.join(".git").exists());
  }
}
