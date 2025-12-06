use crate::cargo::{CargoTransform, TransformContext};
use crate::config::{SplitMode, WorkspaceMode};
use crate::error::{GitError, RailError, RailResult, ResultExt};
use crate::git::mappings::MappingStore;
use crate::git::{CommitInfo, SystemGit};
use crate::utils;
use crate::workspace::WorkspaceContext;
use crate::workspace::files::{AuxiliaryFiles, ProjectFiles};
use glob::Pattern;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Configuration for a split operation
pub struct SplitConfig {
  /// Name of the crate being split
  pub crate_name: String,
  /// Paths to crate directories in monorepo
  pub crate_paths: Vec<PathBuf>,
  /// Split mode (single or combined)
  pub mode: SplitMode,
  /// Workspace mode (standalone or workspace)
  pub workspace_mode: WorkspaceMode,
  /// Target repository path
  pub target_repo_path: PathBuf,
  /// Branch name for split repo
  pub branch: String,
  /// Remote repository URL
  pub remote_url: Option<String>,
  /// Additional files/directories to include (glob patterns)
  pub include: Vec<String>,
  /// Files/directories to exclude (glob patterns)
  pub exclude: Vec<String>,
}

/// Pre-fetched files for a commit: (file_path, content)
type PrefetchedFiles = Vec<(PathBuf, Vec<u8>)>;

/// Parameters for recreating a commit in the target repository
struct RecreateCommitParams<'a> {
  commit: &'a CommitInfo,
  crate_paths: &'a [PathBuf],
  target_repo_path: &'a Path,
  crate_name: &'a str,
  mode: &'a SplitMode,
  workspace_mode: &'a WorkspaceMode,
  mapping_store: &'a MappingStore,
  last_recreated_sha: Option<&'a str>,
  /// Pre-fetched files (if available from parallel prefetch)
  prefetched_files: Option<&'a PrefetchedFiles>,
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
    let transformer = CargoTransform::new(ctx.cargo.metadata().clone());

    Ok(Self {
      ctx,
      transform: transformer,
    })
  }

  /// Check if a file path should be excluded based on glob patterns (Issue #26)
  fn should_exclude(path: &str, exclude_patterns: &[Pattern]) -> bool {
    for pattern in exclude_patterns {
      if pattern.matches(path) {
        return true;
      }
    }
    false
  }

  /// Compile glob patterns from string slices
  fn compile_patterns(patterns: &[String]) -> Vec<Pattern> {
    patterns.iter().filter_map(|p| Pattern::new(p).ok()).collect()
  }

  /// Find additional files to include based on include patterns (Issue #25)
  fn find_included_files(workspace_root: &Path, include_patterns: &[String]) -> RailResult<Vec<PathBuf>> {
    use std::collections::HashSet;
    let mut included = HashSet::new();

    if include_patterns.is_empty() {
      return Ok(Vec::new());
    }

    // Use glob to find files matching include patterns
    for pattern_str in include_patterns {
      let full_pattern = workspace_root.join(pattern_str);
      let glob_pattern = full_pattern.to_string_lossy();

      if let Ok(paths) = glob::glob(&glob_pattern) {
        for path_result in paths.flatten() {
          if path_result.is_file() {
            // Skip .git directory contents
            let path_str = path_result.to_string_lossy();
            if path_str.contains("/.git/") || path_str.contains("\\.git\\") {
              continue;
            }

            // Get relative path
            if let Ok(rel) = path_result.strip_prefix(workspace_root) {
              included.insert(rel.to_path_buf());
            }
          }
        }
      }
    }

    Ok(included.into_iter().collect())
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

  /// Prefetch files for multiple commits in parallel
  ///
  /// This significantly speeds up split operations on large repositories by
  /// reading file contents for many commits concurrently while the sequential
  /// commit recreation happens.
  ///
  /// Returns a HashMap from commit SHA to its prefetched files.
  fn prefetch_commit_files(&self, commits: &[CommitInfo], crate_paths: &[PathBuf]) -> HashMap<String, PrefetchedFiles> {
    // Use rayon to prefetch files in parallel
    // Each commit's file collection is independent, so this is safe
    let git = self.ctx.git.git();
    let paths_arc = Arc::new(crate_paths.to_vec());

    commits
      .par_iter()
      .filter_map(|commit| {
        let paths = Arc::clone(&paths_arc);
        let mut all_files = Vec::new();

        for crate_path in paths.iter() {
          match git.collect_tree_files(&commit.sha, crate_path) {
            Ok(files) => all_files.extend(files),
            Err(_) => {
              // If we can't collect files, skip this commit in prefetch
              // The main loop will handle it appropriately
              return None;
            }
          }
        }

        Some((commit.sha.clone(), all_files))
      })
      .collect()
  }

  /// Apply Cargo.toml transformation to a manifest file
  /// Returns Ok(()) if transform succeeded or file doesn't exist
  ///
  /// # Arguments
  /// * `manifest_path` - Path to the Cargo.toml to transform
  /// * `crate_name` - Name of the crate being transformed
  /// * `target_has_workspace` - Whether target repo will have a workspace structure
  ///   - true: keep `[lints] workspace = true` (Combined + Workspace mode)
  ///   - false: resolve `[lints]` to actual values (Single or Combined + Standalone mode)
  fn apply_manifest_transform(
    &self,
    manifest_path: &Path,
    crate_name: &str,
    target_has_workspace: bool,
  ) -> RailResult<()> {
    if !manifest_path.exists() {
      return Ok(());
    }

    let content = std::fs::read_to_string(manifest_path)?;
    let context = TransformContext {
      crate_name: crate_name.to_string(),
      workspace_root: self.ctx.workspace_root().to_path_buf(),
      target_has_workspace,
    };
    let transformed = self.transform.transform_to_split(&content, &context)?;
    std::fs::write(manifest_path, transformed)?;
    Ok(())
  }

  /// Recreate a commit in the target repository with transforms applied
  /// Returns the new commit SHA, or None if the commit should be skipped
  /// (e.g., when files were deleted at this commit - "dirty history")
  fn recreate_commit_in_target(&self, params: &RecreateCommitParams) -> RailResult<Option<String>> {
    // Use pre-fetched files if available, otherwise collect them now
    let all_files: Vec<(PathBuf, Vec<u8>)> = if let Some(prefetched) = params.prefetched_files {
      prefetched.clone()
    } else {
      let mut files = Vec::new();
      for crate_path in params.crate_paths {
        let collected = self.ctx.git.git().collect_tree_files(&params.commit.sha, crate_path)?;
        files.extend(collected);
      }
      files
    };

    // Handle "dirty history" - commits where the path was deleted or didn't exist yet
    // This commonly happens when:
    // - A crate was temporarily removed and later restored
    // - Files were moved/renamed in a way that deleted the old path
    // - The crate didn't exist at the start of the filtered history
    if all_files.is_empty() {
      return Ok(None);
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
        // Determine if target will have a workspace structure:
        // - Single mode: always standalone (no workspace)
        // - Combined + Standalone: no workspace
        // - Combined + Workspace: has workspace
        let target_has_workspace =
          *params.mode == SplitMode::Combined && *params.workspace_mode == WorkspaceMode::Workspace;
        self.apply_manifest_transform(&target_path, params.crate_name, target_has_workspace)?;
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

    let sha = self.create_git_commit(&CommitParams {
      repo_path: params.target_repo_path,
      message: &params.commit.message,
      author_name: &params.commit.author,
      author_email: &params.commit.author_email,
      committer_name: &params.commit.committer,
      committer_email: &params.commit.committer_email,
      timestamp: params.commit.timestamp,
      parent_shas: &mapped_parents,
    })?;
    Ok(Some(sha))
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

    // Issue #25/#26: Compile exclude patterns (include uses glob directly)
    let exclude_patterns = Self::compile_patterns(&config.exclude);

    if !config.include.is_empty() {
      println!("   Include patterns: {} configured", config.include.len());
    }
    if !config.exclude.is_empty() {
      println!("   Exclude patterns: {} configured", config.exclude.len());
    }

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

    // Issue #25: Find additional files to include based on include patterns
    let additional_files = Self::find_included_files(self.ctx.workspace_root(), &config.include)?;
    if !additional_files.is_empty() {
      println!(
        "   Found {} additional files from include patterns",
        additional_files.len()
      );
    }

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
            &config.workspace_mode,
          )?;
        }
      }
    } else {
      // Recreate history in target repo
      println!("   Processing {} commits...", filtered_commits.len());

      // Prefetch file contents in parallel for significant speedup on large repos
      // This reads all files for all commits concurrently while we process sequentially
      let use_parallel = filtered_commits.len() > 5; // Only parallelize if worthwhile
      let prefetched_files = if use_parallel {
        println!("   Prefetching file contents in parallel...");
        self.prefetch_commit_files(&filtered_commits, &config.crate_paths)
      } else {
        HashMap::new()
      };

      let mut last_recreated_sha: Option<String> = None;

      let mut skipped_commits = 0usize;

      for (idx, commit) in filtered_commits.iter().enumerate() {
        if (idx + 1) % 10 == 0 || idx + 1 == filtered_commits.len() {
          println!("   Progress: {}/{} commits", idx + 1, filtered_commits.len());
        }

        // Use prefetched files if available
        let prefetched = prefetched_files.get(&commit.sha);

        let maybe_sha = self.recreate_commit_in_target(&RecreateCommitParams {
          commit,
          crate_paths: &config.crate_paths,
          target_repo_path: &config.target_repo_path,
          crate_name: &config.crate_name,
          mode: &config.mode,
          workspace_mode: &config.workspace_mode,
          mapping_store: &mapping_store,
          last_recreated_sha: last_recreated_sha.as_deref(),
          prefetched_files: prefetched,
        })?;

        // Handle skipped commits (dirty history - path didn't exist at this commit)
        let Some(new_sha) = maybe_sha else {
          skipped_commits += 1;
          continue;
        };

        // Record mapping
        mapping_store.record_mapping(&commit.sha, &new_sha)?;

        // Track last recreated commit
        last_recreated_sha = Some(new_sha);
      }

      if skipped_commits > 0 {
        println!(
          "   Skipped {} commits where path didn't exist (dirty history)",
          skipped_commits
        );
      }

      // Create workspace Cargo.toml if in workspace mode
      if config.mode == SplitMode::Combined && config.workspace_mode == WorkspaceMode::Workspace {
        println!("   Creating workspace Cargo.toml...");
        self.create_workspace_cargo_toml(&config.crate_paths, &config.target_repo_path)?;
      }

      // Copy workspace config files and project files to the final state
      let has_files = !aux_files.is_empty() || project_files.count() > 0 || !additional_files.is_empty();
      if has_files {
        println!("   Copying workspace configs and project files...");
        aux_files.copy_to_split(self.ctx.workspace_root(), &config.target_repo_path)?;
        project_files.copy_to_split(self.ctx.workspace_root(), &config.target_repo_path)?;

        // Issue #25: Copy additional files from include patterns
        if !additional_files.is_empty() {
          println!(
            "   Copying {} additional files from include patterns...",
            additional_files.len()
          );
          for rel_path in &additional_files {
            let source = self.ctx.workspace_root().join(rel_path);
            let target = config.target_repo_path.join(rel_path);

            // Skip files that match exclude patterns
            let path_str = rel_path.to_string_lossy();
            if Self::should_exclude(&path_str, &exclude_patterns) {
              continue;
            }

            // Create parent directories and copy
            if let Some(parent) = target.parent() {
              std::fs::create_dir_all(parent)?;
            }
            if source.exists() && source.is_file() {
              std::fs::copy(&source, &target)?;
            }
          }
        }

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
    // Single mode is always standalone (no workspace)
    println!("   Transforming Cargo.toml");
    let manifest_path = target_repo_path.join("Cargo.toml");
    self.apply_manifest_transform(&manifest_path, crate_name, false)?;

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
    workspace_mode: &WorkspaceMode,
  ) -> RailResult<()> {
    // Determine if target will have a workspace structure
    let target_has_workspace = *workspace_mode == WorkspaceMode::Workspace;

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
      self.apply_manifest_transform(&manifest_path, crate_name, target_has_workspace)?;
    }

    // Copy auxiliary files
    if !aux_files.is_empty() {
      println!("   Copying auxiliary files");
      aux_files.copy_to_split(self.ctx.workspace_root(), target_repo_path)?;
    }

    Ok(())
  }

  /// Create a workspace Cargo.toml for combined mode with workspace_mode = Workspace
  fn create_workspace_cargo_toml(&self, crate_paths: &[PathBuf], target_repo_path: &Path) -> RailResult<()> {
    // Extract workspace members from crate paths
    let members: Vec<String> = crate_paths.iter().map(|p| p.to_string_lossy().to_string()).collect();

    // Read workspace Cargo.toml from source monorepo
    let source_workspace_toml = self.ctx.workspace_root().join("Cargo.toml");
    let source_content = std::fs::read_to_string(&source_workspace_toml).with_context(|| {
      format!(
        "Failed to read workspace Cargo.toml from {}",
        source_workspace_toml.display()
      )
    })?;

    // Parse the source Cargo.toml
    let mut doc: toml_edit::DocumentMut = source_content
      .parse()
      .map_err(|e| RailError::message(format!("Failed to parse workspace Cargo.toml: {}", e)))?;

    // Update workspace members
    if let Some(workspace) = doc.get_mut("workspace")
      && let Some(table) = workspace.as_table_mut()
    {
      // Set members to only the split crates
      let mut members_array = toml_edit::Array::new();
      for member in &members {
        members_array.push(member.as_str());
      }
      table.insert("members", toml_edit::value(members_array));

      // Remove exclude if present (not needed for split repo)
      table.remove("exclude");

      // Filter default-members to only include split crates (Issue #2)
      let members_set: std::collections::HashSet<&str> = members.iter().map(|s| s.as_str()).collect();
      if let Some(default_members) = table.get_mut("default-members")
        && let Some(arr) = default_members.as_array_mut()
      {
        arr.retain(|item| item.as_str().map(|s| members_set.contains(s)).unwrap_or(false));
      }
      // Remove default-members if empty
      if table
        .get("default-members")
        .and_then(|d| d.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(false)
      {
        table.remove("default-members");
      }

      // Remove workspace.dependencies - split crates have inlined deps (Issue #4)
      table.remove("dependencies");
    }

    // Filter profile package specs to only include split crates (Issue #3)
    let members_set: std::collections::HashSet<&str> = members.iter().map(|s| s.as_str()).collect();
    if let Some(profile) = doc.get_mut("profile").and_then(|p| p.as_table_mut()) {
      for (_, profile_section) in profile.iter_mut() {
        if let Some(profile_table) = profile_section.as_table_mut() {
          if let Some(pkg) = profile_table.get_mut("package").and_then(|p| p.as_table_mut()) {
            let pkg_names: Vec<String> = pkg.iter().map(|(k, _)| k.to_string()).collect();
            for pkg_name in pkg_names {
              if !members_set.contains(pkg_name.as_str()) {
                pkg.remove(&pkg_name);
              }
            }
          }
          // Remove empty package table
          if profile_table
            .get("package")
            .and_then(|p| p.as_table())
            .map(|t| t.is_empty())
            .unwrap_or(false)
          {
            profile_table.remove("package");
          }
        }
      }
    }

    // Remove package section if present (virtual workspace)
    doc.remove("package");
    doc.remove("dependencies");
    doc.remove("dev-dependencies");
    doc.remove("build-dependencies");

    // Write to target repo
    let target_toml = target_repo_path.join("Cargo.toml");
    std::fs::write(&target_toml, doc.to_string())?;

    println!("   Created workspace Cargo.toml with {} members", members.len());

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
      Ok(git) => git.worktree_root.clone(),
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
