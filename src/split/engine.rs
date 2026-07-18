//! Split engine for deterministic crate extraction.
//!
//! Rebuilds crate history into a target repository while preserving stable commit
//! metadata and applying manifest transformations for split modes.

use crate::cargo::{CargoTransform, TransformContext};
use crate::config::{SplitMode, WorkspaceMode};
use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::git::git_cmd_for_path;
use crate::git::mappings::MappingStore;
use crate::git::{CommitInfo, SystemGit};
use crate::progress;
use crate::split::SplitPathCapabilities;
use crate::utils;
use crate::workspace::WorkspaceContext;
use crate::workspace::files::{AuxiliaryFiles, ProjectFiles};
use glob::Pattern;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

/// Runtime parameters for a split operation
///
/// Distinct from `config::SplitConfig` which is the deserialized config schema.
/// This struct holds computed/resolved values needed to execute the split.
pub struct SplitParams {
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
  /// Validated source/target path authority for all filesystem mutations.
  pub path_capabilities: SplitPathCapabilities,
}

/// Pre-fetched exact Git tree entries for a commit.
type PrefetchedFiles = Vec<crate::git::ops::GitTreeEntry>;

/// Maximum number of commits to prefetch at once
/// This bounds memory usage to O(window_size × avg_commit_size) instead of O(total_commits × avg_commit_size)
/// For a typical crate with ~1-2MB of files, 50 commits uses ~50-100MB max
const PREFETCH_WINDOW_SIZE: usize = 50;

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
  path_capabilities: &'a SplitPathCapabilities,
}

/// Parameters for creating a git commit
struct CommitParams<'a> {
  repo_path: &'a Path,
  tree_sha: &'a str,
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
    let transformer = CargoTransform::new(ctx.cargo().metadata().clone());

    Ok(Self {
      ctx,
      transform: transformer,
    })
  }

  /// Check if a file path should be excluded based on glob patterns
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

  /// Find additional files to include based on include patterns
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
    progress!("   Walking commit history to find commits touching crate...");

    // Use batched git command for all paths at once (much faster than N separate calls)
    let filtered_commits = self.ctx.git()?.git().get_commits_touching_paths(paths, None, "HEAD")?;

    progress!(
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
  /// Returns a FxHashMap from commit SHA to its prefetched files (faster String hashing).
  /// Accepts references to avoid cloning CommitInfo structs.
  fn prefetch_commit_files(
    &self,
    commits: &[&CommitInfo],
    crate_paths: &[PathBuf],
  ) -> FxHashMap<String, PrefetchedFiles> {
    // Use rayon to prefetch files in parallel
    // Each commit's file collection is independent, so this is safe
    let Ok(git_state) = self.ctx.git() else {
      return FxHashMap::default();
    };
    let git = git_state.git();
    let paths_arc = Arc::new(crate_paths.to_vec());

    commits
      .par_iter()
      .filter_map(|commit| {
        let paths = Arc::clone(&paths_arc);
        // Pre-allocate for expected files per commit (typically 10-50 files per crate)
        let mut all_files = Vec::with_capacity(32);

        for crate_path in paths.iter() {
          match git.collect_tree_entries(&commit.sha, crate_path) {
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

  /// Apply Cargo.toml transformation for split output.
  ///
  /// If the manifest does not exist, this is a no-op. Workspace inheritance is
  /// preserved only when the split target will remain a workspace.
  fn apply_manifest_transform(
    &self,
    manifest_path: &Path,
    crate_name: &str,
    target_has_workspace: bool,
    path_capabilities: &SplitPathCapabilities,
  ) -> RailResult<()> {
    let manifest_path = path_capabilities.authorize_target(manifest_path)?;
    if !manifest_path.exists() {
      return Ok(());
    }

    let content = std::fs::read_to_string(&manifest_path)?;
    let context = TransformContext {
      crate_name: crate_name.to_string(),
      workspace_root: self.ctx.workspace_root().to_path_buf(),
      target_has_workspace,
    };
    let transformed = self.transform.transform_to_split(&content, &context)?;
    std::fs::write(manifest_path, transformed)?;
    Ok(())
  }

  /// Recreate one commit in the target repository with split transforms applied.
  ///
  /// Returns `Some(new_sha)` when a commit is materialized, or `None` when the
  /// source commit should be skipped (for example, path was deleted at that point).
  fn recreate_commit_in_target(&self, params: &RecreateCommitParams) -> RailResult<Option<String>> {
    // Use pre-fetched files if available, otherwise collect them now
    // Use Cow to avoid cloning the prefetched Vec when it's already available
    let all_files: std::borrow::Cow<'_, PrefetchedFiles> = if let Some(prefetched) = params.prefetched_files {
      std::borrow::Cow::Borrowed(prefetched)
    } else {
      let mut files = Vec::with_capacity(params.crate_paths.len() * 32);
      for crate_path in params.crate_paths {
        let collected = self
          .ctx
          .git()?
          .git()
          .collect_tree_entries(&params.commit.sha, crate_path)?;
        files.extend(collected);
      }
      std::borrow::Cow::Owned(files)
    };

    let mut target_entries = Vec::with_capacity(all_files.len());
    for entry in all_files.iter() {
      let target_path = match params.mode {
        SplitMode::Single => params
          .crate_paths
          .iter()
          .find_map(|crate_path| entry.path.strip_prefix(crate_path).ok().map(Path::to_path_buf))
          .unwrap_or_else(|| entry.path.clone()),
        SplitMode::Combined => entry.path.clone(),
      };
      params.path_capabilities.authorize_target(&target_path)?;

      let object_id = if entry.path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content = self
          .ctx
          .git()?
          .git()
          .read_files_bulk(&[(&params.commit.sha, entry.path.as_path())])?
          .into_iter()
          .next()
          .ok_or_else(|| RailError::message(format!("manifest '{}' has no blob", entry.path.display())))?;
        let content = String::from_utf8(content)
          .map_err(|_| RailError::message(format!("manifest '{}' is not valid UTF-8", entry.path.display())))?;
        let target_has_workspace =
          *params.mode == SplitMode::Combined && *params.workspace_mode == WorkspaceMode::Workspace;
        let context = TransformContext {
          crate_name: params.crate_name.to_string(),
          workspace_root: self.ctx.workspace_root().to_path_buf(),
          target_has_workspace,
        };
        let transformed = self.transform.transform_to_split(&content, &context)?;
        self.write_target_blob(params.target_repo_path, transformed.as_bytes())?
      } else {
        entry.object_id.clone()
      };
      target_entries.push((entry.mode.clone(), object_id, target_path));
    }

    // A fresh index per snapshot makes absence authoritative: deleted and
    // renamed files cannot leak forward from the previous worktree.
    let tree_sha = self.write_exact_tree(params.target_repo_path, &target_entries)?;

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

    params.path_capabilities.validate_target_repository()?;
    let sha = self.create_git_commit(&CommitParams {
      repo_path: params.target_repo_path,
      tree_sha: &tree_sha,
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
    // Prepare environment for deterministic commit
    let author_date = format!("{} +0000", params.timestamp);
    let commit_date = format!("{} +0000", params.timestamp);

    // Build commit-tree command
    let mut cmd = git_cmd_for_path(params.repo_path);
    cmd
      .env("GIT_AUTHOR_NAME", params.author_name)
      .env("GIT_AUTHOR_EMAIL", params.author_email)
      .env("GIT_AUTHOR_DATE", &author_date)
      .env("GIT_COMMITTER_NAME", params.committer_name)
      .env("GIT_COMMITTER_EMAIL", params.committer_email)
      .env("GIT_COMMITTER_DATE", &commit_date)
      .arg("commit-tree")
      .arg(params.tree_sha)
      .arg("-m")
      .arg(params.message);

    // Add parent arguments
    for parent in params.parent_shas {
      cmd.arg("-p").arg(parent);
    }

    // Execute commit-tree
    let output = cmd.output().context("Failed to run git commit-tree")?;

    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git commit-tree".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }

    let commit_sha = String::from_utf8(output.stdout)?.trim().to_string();

    // Update the branch reference
    let update_output = Self::run_git_in_repo(params.repo_path, &["update-ref", "HEAD", &commit_sha])?;
    if !update_output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git update-ref HEAD".to_string(),
        stderr: git_command_diagnostics(&update_output.stdout, &update_output.stderr),
      }));
    }

    let reset_output = Self::run_git_in_repo(params.repo_path, &["reset", "--hard", &commit_sha])?;
    if !reset_output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git reset --hard".to_string(),
        stderr: git_command_diagnostics(&reset_output.stdout, &reset_output.stderr),
      }));
    }

    Ok(commit_sha)
  }

  fn write_target_blob(&self, repo_path: &Path, content: &[u8]) -> RailResult<String> {
    let mut child = git_cmd_for_path(repo_path)
      .args(["hash-object", "-w", "--stdin"])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .context("Failed to start git hash-object")?;
    child
      .stdin
      .take()
      .ok_or_else(|| RailError::message("git hash-object stdin was unavailable"))?
      .write_all(content)
      .context("Failed to write transformed blob")?;
    let output = child.wait_with_output().context("Failed to finish git hash-object")?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git hash-object -w --stdin".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
  }

  fn write_exact_tree(&self, repo_path: &Path, entries: &[(String, String, PathBuf)]) -> RailResult<String> {
    let index_path = repo_path
      .join(".git")
      .join(format!("cargo-rail-index-{}", std::process::id()));
    let _ = std::fs::remove_file(&index_path);
    let mut read_tree = git_cmd_for_path(repo_path);
    let output = read_tree
      .env("GIT_INDEX_FILE", &index_path)
      .args(["read-tree", "--empty"])
      .output()
      .context("Failed to initialize exact-tree index")?;
    if !output.status.success() {
      return Err(RailError::message(format!(
        "git read-tree --empty failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      )));
    }

    if !entries.is_empty() {
      let mut child = git_cmd_for_path(repo_path)
        .env("GIT_INDEX_FILE", &index_path)
        .args(["update-index", "-z", "--index-info"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start git update-index")?;
      let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| RailError::message("git update-index stdin was unavailable"))?;
      for (mode, object_id, path) in entries {
        let path = path
          .to_str()
          .ok_or_else(|| RailError::message(format!("split path '{}' is not valid UTF-8", path.display())))?;
        write!(stdin, "{} {}\t{}\0", mode, object_id, path).context("Failed to populate exact-tree index")?;
      }
      drop(stdin);
      let output = child.wait_with_output().context("Failed to finish git update-index")?;
      if !output.status.success() {
        let _ = std::fs::remove_file(&index_path);
        return Err(RailError::message(format!(
          "git update-index --index-info failed: {}",
          String::from_utf8_lossy(&output.stderr).trim()
        )));
      }
    }

    let output = git_cmd_for_path(repo_path)
      .env("GIT_INDEX_FILE", &index_path)
      .arg("write-tree")
      .output()
      .context("Failed to write exact split tree")?;
    let _ = std::fs::remove_file(&index_path);
    if !output.status.success() {
      return Err(RailError::message(format!(
        "git write-tree failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
      )));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
  }

  fn run_git_in_repo(repo_path: &Path, args: &[&str]) -> RailResult<std::process::Output> {
    let mut cmd = git_cmd_for_path(repo_path);
    cmd.args(args);
    cmd
      .output()
      .with_context(|| format!("Failed to execute git {}", args.join(" ")))
  }

  /// Check if remote repository exists and has content
  fn check_remote_exists(&self, remote_url: &str) -> RailResult<bool> {
    self.ctx.git()?.git().ls_remote_has_content(remote_url)
  }

  /// Execute a split operation (idempotent - re-runs sync new commits only)
  pub fn split(&self, config: &SplitParams) -> RailResult<()> {
    let target = config.path_capabilities.authorize_target(&config.target_repo_path)?;
    if target != config.path_capabilities.target_root() {
      return Err(RailError::message(
        "runtime split target does not match the validated path capability",
      ));
    }
    config.path_capabilities.validate_crate_paths(&config.crate_paths)?;
    config.path_capabilities.validate_target_repository()?;
    progress!("🚂 Splitting crate: {}", config.crate_name);
    progress!("   Mode: {:?}", config.mode);
    progress!("   Target: {}", config.target_repo_path.display());

    // Compile exclude patterns (include uses glob directly)
    let exclude_patterns = Self::compile_patterns(&config.exclude);

    if !config.include.is_empty() {
      progress!("   Include patterns: {} configured", config.include.len());
    }
    if !config.exclude.is_empty() {
      progress!("   Exclude patterns: {} configured", config.exclude.len());
    }

    // Check if target repo already exists (for idempotency)
    let target_exists = config.target_repo_path.join(".git").exists();

    // Check if remote already exists - warn but allow re-run for idempotency
    if let Some(ref remote_url) = config.remote_url {
      let remote_exists = self.check_remote_exists(remote_url)?;
      if remote_exists && !target_exists {
        // Remote exists but no local target - user probably wants to use sync instead
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
      // If both remote and target exist, we'll check mappings below for idempotency
    }

    // Create or reuse target repo
    self.ensure_target_repo(&config.path_capabilities)?;
    self.import_source_objects(&config.target_repo_path)?;

    // Discover workspace-level auxiliary files from workspace
    let aux_files = AuxiliaryFiles::discover(self.ctx.workspace_root())?;
    progress!("   Found {} workspace config files", aux_files.count());

    // Discover project files (README, LICENSE) with crate-first fallback
    let project_files = ProjectFiles::discover(self.ctx.workspace_root(), &config.crate_paths)?;
    progress!("   Found {} project files (README, LICENSE)", project_files.count());

    // Find additional files to include based on include patterns
    let additional_files = Self::find_included_files(self.ctx.workspace_root(), &config.include)?;
    if !additional_files.is_empty() {
      progress!(
        "   Found {} additional files from include patterns",
        additional_files.len()
      );
    }

    // Create mapping store and load existing mappings (from both workspace and target)
    let mut mapping_store = MappingStore::new(config.crate_name.clone());
    mapping_store.load(self.ctx.workspace_root())?;
    if target_exists {
      mapping_store.load(&config.target_repo_path)?;
    }

    // Walk filtered history to find commits touching the crate
    let filtered_commits = self.walk_filtered_history(&config.crate_paths)?;

    // Count how many commits are already mapped (for idempotency)
    let already_mapped_count = filtered_commits
      .iter()
      .filter(|c| mapping_store.has_mapping(&c.sha))
      .count();

    if already_mapped_count > 0 {
      progress!("   Found {} commits already split (will skip)", already_mapped_count);
    }

    // Check if all commits are already mapped - nothing to do
    if already_mapped_count == filtered_commits.len() && !filtered_commits.is_empty() {
      progress!("\n✅ Split already up-to-date!");
      progress!("   All {} commits have been split previously.", filtered_commits.len());
      progress!("   Target repo: {}", config.target_repo_path.display());
      return Ok(());
    }

    if filtered_commits.is_empty() {
      progress!("   No commits found that touch the crate paths");
      progress!("   Falling back to current state copy...");

      // Fallback to snapshot copy if no history found
      match config.mode {
        SplitMode::Single => {
          let crate_path = &config.crate_paths[0];
          self.split_single_crate(
            crate_path,
            &config.target_repo_path,
            &aux_files,
            &config.crate_name,
            &config.path_capabilities,
          )?;
        }
        SplitMode::Combined => {
          self.split_combined_crates(
            &config.crate_paths,
            &config.target_repo_path,
            &aux_files,
            &config.crate_name,
            &config.workspace_mode,
            &config.path_capabilities,
          )?;
        }
      }
    } else {
      // Recreate history in target repo
      progress!("   Processing {} commits...", filtered_commits.len());

      let mut last_recreated_sha: Option<String> = None;
      let mut skipped_commits = 0usize;
      let skipped_already_mapped = already_mapped_count;

      // For incremental splits, find the last mapped commit's SHA in target repo
      // to use as parent for new commits
      if target_exists && already_mapped_count > 0 {
        // Find the most recent mapped commit and use its target SHA as last_recreated_sha
        for commit in filtered_commits.iter().rev() {
          if let Ok(Some(target_sha)) = mapping_store.get_mapping(&commit.sha) {
            last_recreated_sha = Some(target_sha);
            break;
          }
        }
      }

      // Filter out already-mapped commits upfront for accurate counting and windowing
      let commits_to_process: Vec<&CommitInfo> = filtered_commits
        .iter()
        .filter(|c| !mapping_store.has_mapping(&c.sha))
        .collect();

      let total_new = commits_to_process.len();

      // Process commits in windows to bound memory usage
      // Each window prefetches files for up to PREFETCH_WINDOW_SIZE commits,
      // processes them, then drops the prefetch cache before the next window.
      // This limits memory to O(window_size × avg_commit_size) instead of O(total × avg_commit_size)
      let use_parallel = total_new > 5;

      for (window_idx, window) in commits_to_process.chunks(PREFETCH_WINDOW_SIZE).enumerate() {
        // Prefetch this window's files in parallel
        let prefetched_files: FxHashMap<String, PrefetchedFiles> = if use_parallel {
          if window_idx == 0 {
            if total_new > PREFETCH_WINDOW_SIZE {
              progress!(
                "   Prefetching in windows of {} commits to bound memory...",
                PREFETCH_WINDOW_SIZE
              );
            } else {
              progress!("   Prefetching file contents in parallel...");
            }
          }
          // Pass references directly - no cloning needed
          self.prefetch_commit_files(window, &config.crate_paths)
        } else {
          FxHashMap::default()
        };

        // Process this window's commits
        for (idx_in_window, commit) in window.iter().enumerate() {
          let overall_idx = window_idx * PREFETCH_WINDOW_SIZE + idx_in_window + 1;

          if overall_idx.is_multiple_of(10) || overall_idx == total_new {
            progress!("   Progress: {}/{} new commits", overall_idx, total_new);
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
            path_capabilities: &config.path_capabilities,
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

        // prefetched_files is dropped here at end of window iteration,
        // freeing memory before the next window is prefetched
      }

      if skipped_commits > 0 || skipped_already_mapped > 0 {
        if skipped_commits > 0 {
          progress!(
            "   Skipped {} commits where path didn't exist (dirty history)",
            skipped_commits
          );
        }
        if skipped_already_mapped > 0 {
          progress!(
            "   Skipped {} commits already split (idempotent)",
            skipped_already_mapped
          );
        }
      }

      // Create workspace Cargo.toml if in workspace mode
      if config.mode == SplitMode::Combined && config.workspace_mode == WorkspaceMode::Workspace {
        progress!("   Creating workspace Cargo.toml...");
        self.create_workspace_cargo_toml(&config.crate_paths, &config.target_repo_path, &config.path_capabilities)?;
      }

      // Copy workspace config files and project files to the final state
      let has_files = !aux_files.is_empty() || project_files.count() > 0 || !additional_files.is_empty();
      if has_files {
        progress!("   Copying workspace configs and project files...");
        aux_files.copy_to_split(&config.path_capabilities)?;
        project_files.copy_to_split(&config.path_capabilities)?;

        // Copy additional files from include patterns
        if !additional_files.is_empty() {
          progress!(
            "   Copying {} additional files from include patterns...",
            additional_files.len()
          );
          for rel_path in &additional_files {
            let source = config.path_capabilities.authorize_source(rel_path)?;
            let target = config.path_capabilities.authorize_target(rel_path)?;

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
        config.path_capabilities.validate_target_repository()?;
        let target_git = SystemGit::open(&config.target_repo_path)?;
        target_git.stage_all()?;

        // Check if there are staged changes before committing
        if target_git.has_staged_changes()? {
          progress!("   Creating commit for auxiliary files");
          target_git.commit("Add workspace configs and project files")?;
        }
      }
    }

    // Save mappings to both workspace and target repo
    mapping_store.save(self.ctx.workspace_root())?;
    mapping_store.save(&config.target_repo_path)?;

    // Push to remote if URL is configured and is not a local file path
    if let Some(ref remote_url) = config.remote_url {
      if !remote_url.is_empty() && !utils::is_local_path(remote_url) {
        progress!("\n🚀 Pushing to remote...");

        // Open the target repo
        let target_git = SystemGit::open(&config.target_repo_path)?;

        // Add or update remote
        if !target_git.has_remote("origin")? {
          progress!("   Adding remote 'origin': {}", remote_url);
          target_git.add_remote("origin", remote_url)?;
        } else {
          progress!("   Remote 'origin' already exists");
        }

        // Push to remote
        target_git.push_to_remote("origin", &config.branch)?;

        // Push git-notes
        mapping_store.push_notes(&config.target_repo_path, "origin")?;

        progress!("   ✅ Pushed to {}", remote_url);
      } else {
        progress!("\n💾 Split repository created locally");
        if utils::is_local_path(remote_url) {
          progress!("   Note: Remote is a local path, skipping push");
          progress!(
            "   Local testing mode - split repo at: {}",
            config.target_repo_path.display()
          );
        } else {
          progress!("   No remote URL configured");
        }
        progress!("\n   To push to a real remote later:");
        progress!("   cd {}", config.target_repo_path.display());
        progress!("   git remote add origin <url>");
        progress!("   git push -u origin {}", config.branch);
      }
    } else {
      progress!("\n⚠️  No remote URL configured - repository created locally only");
      progress!("   To push manually:");
      progress!("   cd {}", config.target_repo_path.display());
      progress!("   git remote add origin <url>");
      progress!("   git push -u origin {}", config.branch);
    }

    progress!("\n✅ Split complete!");
    progress!("   Target repo: {}", config.target_repo_path.display());

    Ok(())
  }

  /// Ensure target repository exists and is initialized
  fn ensure_target_repo(&self, paths: &SplitPathCapabilities) -> RailResult<()> {
    let target_path = paths.authorize_target(paths.target_root())?;
    if !target_path.exists() {
      std::fs::create_dir_all(&target_path)
        .with_context(|| format!("Failed to create target directory: {}", target_path.display()))?;
    }

    // Check if it's already a git repo
    let git_dir = target_path.join(".git");
    if !git_dir.exists() {
      progress!("   Initializing git repository at {}", target_path.display());

      // Initialize using system git with main as default branch
      paths.validate_target_repository()?;
      crate::git::init_repo(&target_path, "main")?;

      // Configure git identity from source repository
      paths.validate_target_repository()?;
      self.configure_git_identity(&target_path)?;
    }

    Ok(())
  }

  /// Import the source object graph once so reconstructed trees can reuse blob
  /// object IDs directly. This replaces per-file copy/hash subprocesses.
  fn import_source_objects(&self, target_path: &Path) -> RailResult<()> {
    let source = self
      .ctx
      .workspace_root()
      .to_str()
      .ok_or_else(|| RailError::message("source repository path is not valid UTF-8"))?;
    let output = Self::run_git_in_repo(target_path, &["fetch", "--quiet", "--no-tags", source, "HEAD"])?;
    if !output.status.success() {
      return Err(RailError::Git(GitError::CommandFailed {
        command: "git fetch source objects".to_string(),
        stderr: git_command_diagnostics(&output.stdout, &output.stderr),
      }));
    }
    Ok(())
  }

  /// Configure git identity in the target repository by copying from source
  fn configure_git_identity(&self, target_path: &Path) -> RailResult<()> {
    // Get identity from source repository
    let user_name = self.ctx.git()?.git().get_config("user.name")?.unwrap_or_default();
    let user_email = self.ctx.git()?.git().get_config("user.email")?.unwrap_or_default();

    // Set identity in target repository
    // Use a fallback if source doesn't have identity configured
    let name = if user_name.is_empty() { "Cargo Rail" } else { &user_name };
    let email = if user_email.is_empty() {
      "cargo-rail@localhost"
    } else {
      &user_email
    };

    // Open target repo and set config
    let target_git = SystemGit::open(target_path)?;
    target_git.set_config("user.name", name)?;
    target_git.set_config("user.email", email)?;

    Ok(())
  }

  /// Split a single crate (move to root of target repo)
  fn split_single_crate(
    &self,
    crate_path: &Path,
    target_repo_path: &Path,
    aux_files: &AuxiliaryFiles,
    crate_name: &str,
    path_capabilities: &SplitPathCapabilities,
  ) -> RailResult<()> {
    let source_path = self.ctx.workspace_root().join(crate_path);

    // Copy source files
    progress!("   Copying source files from {}", crate_path.display());
    self.copy_directory_recursive(&source_path, target_repo_path, path_capabilities)?;

    // Transform Cargo.toml manifest
    // Single mode is always standalone (no workspace)
    progress!("   Transforming Cargo.toml");
    let manifest_path = target_repo_path.join("Cargo.toml");
    self.apply_manifest_transform(&manifest_path, crate_name, false, path_capabilities)?;

    // Copy auxiliary files
    if !aux_files.is_empty() {
      progress!("   Copying auxiliary files");
      aux_files.copy_to_split(path_capabilities)?;
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
    path_capabilities: &SplitPathCapabilities,
  ) -> RailResult<()> {
    // Determine if target will have a workspace structure
    let target_has_workspace = *workspace_mode == WorkspaceMode::Workspace;

    for crate_path in crate_paths {
      let source_path = self.ctx.workspace_root().join(crate_path);
      let target_path = target_repo_path.join(crate_path);

      progress!("   Copying {} to {}", crate_path.display(), crate_path.display());

      self.copy_directory_recursive(&source_path, &target_path, path_capabilities)?;

      // Transform Cargo.toml manifest
      let manifest_path = target_path.join("Cargo.toml");
      self.apply_manifest_transform(&manifest_path, crate_name, target_has_workspace, path_capabilities)?;
    }

    // Copy auxiliary files
    if !aux_files.is_empty() {
      progress!("   Copying auxiliary files");
      aux_files.copy_to_split(path_capabilities)?;
    }

    Ok(())
  }

  /// Create a workspace Cargo.toml for combined mode with workspace_mode = Workspace
  fn create_workspace_cargo_toml(
    &self,
    crate_paths: &[PathBuf],
    target_repo_path: &Path,
    path_capabilities: &SplitPathCapabilities,
  ) -> RailResult<()> {
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

      // Filter default-members to only include split crates
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

      // Remove workspace.dependencies - split crates have inlined deps
      table.remove("dependencies");
    }

    // Filter profile package specs to only include split crates
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
    let target_toml = path_capabilities.authorize_target(&target_repo_path.join("Cargo.toml"))?;
    std::fs::write(&target_toml, doc.to_string())?;

    progress!("   Created workspace Cargo.toml with {} members", members.len());

    Ok(())
  }

  /// Recursively copy a directory, excluding .git
  fn copy_directory_recursive(
    &self,
    source: &Path,
    target: &Path,
    path_capabilities: &SplitPathCapabilities,
  ) -> RailResult<()> {
    copy_directory_recursive_impl(source, target, path_capabilities)
  }
}

/// Helper function to recursively copy a directory, excluding .git
fn copy_directory_recursive_impl(
  source: &Path,
  target: &Path,
  path_capabilities: &SplitPathCapabilities,
) -> RailResult<()> {
  let source = path_capabilities.authorize_source(source)?;
  let target = path_capabilities.authorize_target(target)?;
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

  std::fs::create_dir_all(&target)?;

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
      copy_directory_recursive_impl(&source_path, &target_path, path_capabilities)?;
    } else {
      let source_path = path_capabilities.authorize_source(&source_path)?;
      let target_path = path_capabilities.authorize_target(&target_path)?;
      std::fs::copy(source_path, target_path)?;
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

    let paths = SplitPathCapabilities::new(&source, &source, &[PathBuf::from(".")], &target).unwrap();
    engine.copy_directory_recursive(&source, &target, &paths).unwrap();

    // Verify files copied
    assert!(target.join("Cargo.toml").exists());
    assert!(target.join("src/lib.rs").exists());

    // Verify .git excluded
    assert!(!target.join(".git").exists());
  }
}
