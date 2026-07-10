//! Bidirectional sync engine between monorepo and split repositories.
//!
//! Coordinates commit mapping, conflict detection/resolution, and Cargo.toml
//! transforms while preserving deterministic sync behavior.

use crate::cargo::{CargoTransform, TransformContext};
use crate::config::{SplitMode, WorkspaceMode};
use crate::error::RailResult;
use crate::git::SystemGit;
use crate::git::mappings::MappingStore;
use crate::progress;
use crate::split::SplitPathCapabilities;
use crate::sync::conflict::{ConflictClass, ConflictInfo, ConflictResolver, ConflictStrategy};
use crate::utils;
use crate::workspace::WorkspaceContext;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Configuration for sync operation
#[derive(Clone)]
pub struct SyncConfig {
  /// Name of the crate being synced
  pub crate_name: String,
  /// Paths to crate directories
  pub crate_paths: Vec<PathBuf>,
  /// Split mode (single or combined)
  pub mode: SplitMode,
  /// Workspace mode (standalone or workspace)
  pub workspace_mode: WorkspaceMode,
  /// Path to target repository
  pub target_repo_path: PathBuf,
  /// Branch name
  pub branch: String,
  /// Remote repository URL
  pub remote_url: String,
  /// Validated source/target/temporary mutation authority.
  pub path_capabilities: SplitPathCapabilities,
}

/// Result of a sync operation
pub struct SyncResult {
  /// Number of commits synced
  pub commits_synced: usize,
  /// Conflicts encountered during sync
  pub conflicts: Vec<ConflictInfo>,
  /// Terminal status for this invocation.
  pub status: SyncStatus,
  /// Durable recovery receipt when manual resolution is required.
  pub conflict_receipt: Option<PathBuf>,
}

impl Default for SyncResult {
  fn default() -> Self {
    Self {
      commits_synced: 0,
      conflicts: Vec::new(),
      status: SyncStatus::Complete,
      conflict_receipt: None,
    }
  }
}

/// Operator-visible sync terminal status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
  /// All selected work was committed.
  Complete,
  /// Work is preserved on a branch but requires manual resolution.
  Conflicted,
}

/// Direction of synchronization
#[derive(Debug, Clone)]
pub enum SyncDirection {
  /// Monorepo to remote
  MonoToRemote,
  /// Remote to monorepo
  RemoteToMono,
  /// Both directions
  Both,
  /// No sync needed
  None,
}

/// Result of conflict resolution containing both conflict info and changed files
/// Changed files are cached for reuse in the apply step to avoid redundant git calls
pub struct ConflictResolutionResult {
  /// Conflict information for files that had merge conflicts
  pub conflicts: Vec<ConflictInfo>,
  /// Paths already materialized by a merge strategy and not to overwrite.
  pub resolved_files: Vec<PathBuf>,
  /// Changed files from the commit (cached to avoid redundant git calls)
  pub changed_files: Vec<(PathBuf, char)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SyncConflictReceipt {
  schema_version: u32,
  status: String,
  crate_name: String,
  branch: String,
  expected_head: String,
  remote_commit: String,
  message: String,
  author: String,
  author_email: String,
  timestamp: i64,
  commit_paths: Vec<PathBuf>,
  conflicts: Vec<ConflictInfo>,
  resulting_commit: Option<String>,
}

/// Bidirectional sync engine
pub struct SyncEngine<'a> {
  /// Workspace context
  ctx: &'a WorkspaceContext,
  /// Sync configuration
  config: SyncConfig,
  /// Commit mapping store
  mapping_store: MappingStore,
  /// Cargo.toml transformer
  transform: CargoTransform,
  /// Conflict resolver
  conflict_resolver: ConflictResolver,
  /// Track which repos we've loaded mappings from (to avoid redundant loads)
  loaded_repos: std::collections::HashSet<PathBuf>,
}

impl<'a> SyncEngine<'a> {
  /// Create a new sync engine
  pub fn new(ctx: &'a WorkspaceContext, config: SyncConfig, conflict_strategy: ConflictStrategy) -> RailResult<Self> {
    let target = config.path_capabilities.authorize_target(&config.target_repo_path)?;
    if target != config.path_capabilities.target_root() {
      return Err(crate::error::RailError::message(
        "runtime sync target does not match the validated path capability",
      ));
    }
    config.path_capabilities.validate_crate_paths(&config.crate_paths)?;
    config.path_capabilities.validate_target_repository()?;
    let mapping_store = MappingStore::new(config.crate_name.clone());
    let transformer = CargoTransform::new(ctx.cargo.metadata().clone());

    // Create unique temporary directory for conflict resolution (avoid conflicts in parallel tests)
    let temp_dir = config.path_capabilities.authorize_temporary(Path::new(&format!(
      "cargo-rail-conflicts-{}-{}-{}",
      config.crate_name,
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_nanos()
    )))?;
    std::fs::create_dir_all(&temp_dir)?;
    let conflict_resolver = ConflictResolver::new(conflict_strategy, temp_dir);

    Ok(Self {
      ctx,
      config,
      mapping_store,
      transform: transformer,

      conflict_resolver,
      loaded_repos: std::collections::HashSet::new(),
    })
  }

  /// Load mappings from a repo if not already loaded (avoids redundant subprocess calls)
  fn ensure_mappings_loaded(&mut self, repo_path: &Path) -> RailResult<()> {
    if !self.loaded_repos.contains(repo_path) {
      self.mapping_store.load(repo_path)?;
      self.loaded_repos.insert(repo_path.to_path_buf());
    }
    Ok(())
  }

  /// Commit operator-resolved work from a durable conflict receipt, then
  /// continue with any remaining remote commits.
  pub fn resume_from_receipt(&mut self, receipt_path: &Path) -> RailResult<SyncResult> {
    let receipt_path = self.validate_conflict_receipt_path(receipt_path)?;
    let bytes = std::fs::read(&receipt_path)?;
    let mut receipt: SyncConflictReceipt = serde_json::from_slice(&bytes)
      .map_err(|error| crate::error::RailError::message(format!("invalid sync conflict receipt: {}", error)))?;
    if receipt.schema_version != 1 || receipt.status != "conflicted" {
      return Err(crate::error::RailError::message(
        "sync conflict receipt is not an active version-1 conflict",
      ));
    }
    if receipt.crate_name != self.config.crate_name {
      return Err(crate::error::RailError::message(format!(
        "sync receipt is for crate '{}', not '{}'",
        receipt.crate_name, self.config.crate_name
      )));
    }

    let git = self.ctx.git()?.git();
    let branch = git.current_branch()?;
    if branch != receipt.branch {
      return Err(crate::error::RailError::with_help(
        format!(
          "sync resume requires branch '{}'; current branch is '{}'",
          receipt.branch, branch
        ),
        format!("git switch {}", receipt.branch),
      ));
    }
    let head = git.head_commit()?;
    if head != receipt.expected_head {
      return Err(crate::error::RailError::with_help(
        "sync recovery branch moved after the conflict was recorded",
        "inspect the branch history and restart sync; cargo-rail will not commit against an unverified parent",
      ));
    }

    for conflict in &receipt.conflicts {
      let path = self
        .config
        .path_capabilities
        .authorize_source_mutation(&conflict.file_path)?;
      let content = std::fs::read(&path)?;
      if contains_conflict_markers(&content) {
        return Err(crate::error::RailError::with_help(
          format!(
            "unresolved conflict markers remain in '{}'",
            conflict.file_path.display()
          ),
          "resolve every marker in the receipt before resuming",
        ));
      }
    }

    let expected = receipt
      .commit_paths
      .iter()
      .cloned()
      .collect::<std::collections::BTreeSet<_>>();
    let unexpected = git
      .changed_paths()?
      .into_iter()
      .filter(|path| !expected.contains(path))
      .collect::<Vec<_>>();
    if !unexpected.is_empty() {
      return Err(crate::error::RailError::with_help(
        format!(
          "sync resume found unrelated changes: {}",
          unexpected
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
        ),
        "commit or restore unrelated work before resuming",
      ));
    }

    let commit = git.create_commit_with_metadata(
      &receipt.message,
      &receipt.author,
      &receipt.author_email,
      receipt.timestamp,
      std::slice::from_ref(&receipt.expected_head),
      &receipt.commit_paths,
    )?;
    self.mapping_store.record_mapping(&commit, &receipt.remote_commit)?;
    self.mapping_store.save(self.ctx.workspace_root())?;
    receipt.status = "resolved".to_string();
    receipt.resulting_commit = Some(commit);
    write_json_atomic(&receipt_path, &receipt)?;

    let mut remaining = self.sync_from_remote()?;
    remaining.commits_synced += 1;
    Ok(remaining)
  }

  fn write_conflict_receipt(&self, receipt: SyncConflictReceipt) -> RailResult<PathBuf> {
    let dir = self.conflict_receipt_dir();
    std::fs::create_dir_all(&dir)?;
    let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let path = dir.join(format!("sync-conflict-{}-{}.json", self.config.crate_name, nonce));
    write_json_atomic(&path, &receipt)?;
    Ok(path)
  }

  fn conflict_receipt_dir(&self) -> PathBuf {
    self.ctx.workspace_root().join("target/cargo-rail/receipts")
  }

  fn validate_conflict_receipt_path(&self, receipt_path: &Path) -> RailResult<PathBuf> {
    let path = utils::canonicalize_existing(receipt_path)?;
    let dir = utils::canonicalize_existing(&self.conflict_receipt_dir())?;
    if path.parent().is_none_or(|parent| parent != dir) {
      return Err(crate::error::RailError::message(format!(
        "sync receipt '{}' is outside the workspace receipt directory",
        receipt_path.display()
      )));
    }
    Ok(path)
  }

  /// Get the appropriate branch reference (origin/branch for remotes, just branch for local)
  fn get_branch_ref(&self) -> String {
    if utils::is_local_path(&self.config.remote_url) {
      self.config.branch.clone()
    } else {
      format!("origin/{}", self.config.branch)
    }
  }

  /// Check whether a monorepo path belongs to this sync scope.
  ///
  /// - `single`: only files under the single configured crate path
  /// - `combined`: files under any configured crate path
  fn mono_path_in_scope(&self, path: &Path) -> bool {
    match self.config.mode {
      SplitMode::Single => self
        .config
        .crate_paths
        .first()
        .is_some_and(|crate_path| path.starts_with(crate_path)),
      SplitMode::Combined => self
        .config
        .crate_paths
        .iter()
        .any(|crate_path| path.starts_with(crate_path)),
    }
  }

  /// Sync changes from monorepo to remote repository
  pub fn sync_to_remote(&mut self) -> RailResult<SyncResult> {
    progress!("   Syncing monorepo → remote...");

    // Load mappings (cached - only loads if not already loaded)

    self.ensure_mappings_loaded(self.ctx.workspace_root())?;

    // Open remote repo
    let target_repo_path = self.config.target_repo_path.clone();
    let remote_git = SystemGit::open(&target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !utils::is_local_path(&self.config.remote_url) {
      remote_git.fetch_from_remote("origin")?;
      self.mapping_store.fetch_notes(&target_repo_path, "origin")?;
    } else {
      progress!("   Skipping fetch (local testing mode)");
    }
    // Fetch updates mapping notes, so we need to reload from target repo
    // Clear the loaded flag and reload
    self.loaded_repos.remove(&target_repo_path);
    self.ensure_mappings_loaded(&target_repo_path)?;

    // Find last synced commit in mono
    let last_synced_mono = self.find_last_synced_mono_commit()?;

    // Get new commits in mono that touch any of the crate paths (handles both single and combined modes)
    let new_commits = self.ctx.git()?.git().get_commits_touching_paths(
      &self.config.crate_paths,
      last_synced_mono.as_deref(),
      "HEAD",
    )?;

    if new_commits.is_empty() {
      progress!("   No new commits to sync");
    } else {
      progress!("   Syncing {} commits to remote...", new_commits.len());

      let mut synced_count = 0;
      let mut current_remote_head = remote_git.head_commit()?; // Cache HEAD, update after each commit

      for commit in &new_commits {
        // Skip if already synced
        if self.mapping_store.has_mapping(&commit.sha) {
          continue;
        }

        // Skip if this commit came from remote (check trailer)
        if commit.message.contains("Rail-Origin: remote@") {
          continue;
        }

        // Apply commit to remote
        let remote_sha = self.apply_mono_commit_to_remote(commit, &remote_git, &current_remote_head)?;

        // Record mapping
        self.mapping_store.record_mapping(&commit.sha, &remote_sha)?;
        synced_count += 1;
        current_remote_head = remote_sha; // Update cached HEAD (move, not clone)
      }

      // Save mappings after processing commits
      self.mapping_store.save(self.ctx.workspace_root())?;
      self.mapping_store.save(&self.config.target_repo_path)?;

      // Push to remote (skip for local paths)
      if synced_count > 0 && !utils::is_local_path(&self.config.remote_url) {
        remote_git.push_to_remote("origin", &self.config.branch)?;
        self.mapping_store.push_notes(&self.config.target_repo_path, "origin")?;
      }

      return Ok(SyncResult {
        commits_synced: synced_count,
        conflicts: Vec::new(),
        ..SyncResult::default()
      });
    }

    let synced_count = 0;

    // Save mappings
    self.mapping_store.save(self.ctx.workspace_root())?;
    self.mapping_store.save(&self.config.target_repo_path)?;

    // Push to remote (skip for local paths)
    if synced_count > 0 {
      if !utils::is_local_path(&self.config.remote_url) {
        remote_git.push_to_remote("origin", &self.config.branch)?;
        self.mapping_store.push_notes(&self.config.target_repo_path, "origin")?;
      } else {
        progress!("   Skipping push (local testing mode)");
      }
    }

    Ok(SyncResult {
      commits_synced: synced_count,
      conflicts: Vec::new(),
      ..SyncResult::default()
    })
  }

  /// Sync changes from remote repository to monorepo
  pub fn sync_from_remote(&mut self) -> RailResult<SyncResult> {
    progress!("   Syncing remote → monorepo...");

    // Check current branch - NEVER commit directly to protected branches
    let _current_branch = self.ctx.git()?.git().current_branch()?;

    // Load mappings (cached - only loads if not already loaded)

    self.ensure_mappings_loaded(self.ctx.workspace_root())?;

    // Open remote repo
    let target_repo_path = self.config.target_repo_path.clone();
    let remote_git = SystemGit::open(&target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !utils::is_local_path(&self.config.remote_url) {
      remote_git.fetch_from_remote("origin")?;
      self.mapping_store.fetch_notes(&target_repo_path, "origin")?;
    } else {
      progress!("   Skipping fetch (local testing mode)");
    }
    // Fetch updates mapping notes, so we need to reload from target repo
    // Clear the loaded flag and reload
    self.loaded_repos.remove(&target_repo_path);
    self.ensure_mappings_loaded(&target_repo_path)?;

    // Find last synced commit in remote
    let last_synced_remote = self.find_last_synced_remote_commit(&remote_git)?;

    // Get new commits in remote
    let branch_ref = self.get_branch_ref();
    let new_commits = if let Some(ref last) = last_synced_remote {
      remote_git.get_commits_touching_path(Path::new("."), Some(last), &branch_ref)?
    } else {
      remote_git.get_commits_touching_path(Path::new("."), None, &branch_ref)?
    };

    // Filter to only commits that need syncing (not already mapped)
    let commits_to_sync: Vec<_> = new_commits
      .iter()
      .filter(|c| {
        // Skip if this commit came from mono (check trailer)
        if c.message.contains("Rail-Origin: mono@") {
          return false;
        }
        // Skip if already synced (O(1) reverse mapping lookup)
        if self.mapping_store.has_reverse_mapping(&c.sha) {
          return false;
        }
        true
      })
      .collect();

    // If nothing to sync, report up-to-date
    if commits_to_sync.is_empty() {
      progress!("   No new commits to sync (already up-to-date)");
      return Ok(SyncResult {
        commits_synced: 0,
        conflicts: Vec::new(),
        ..SyncResult::default()
      });
    }

    // Always create a PR branch for safety when syncing from remote
    // This prevents direct commits to protected branches (main, master, develop)
    // Use deterministic branch name (without timestamp) for idempotency
    let branch_name = format!("cargo-rail-sync-{}", self.config.crate_name);

    // Check if branch already exists
    let branch_exists = self.ctx.git()?.git().branch_exists(&branch_name)?;

    let pr_branch = if branch_exists {
      // Branch exists - switch to it and check if commits are already there
      progress!("   PR branch '{}' already exists, checking state...", branch_name);
      self.ctx.git()?.git().checkout_branch(&branch_name)?;
      Some(branch_name)
    } else {
      // Create new branch
      progress!("   Creating PR branch: {}", branch_name);
      self.ctx.git()?.git().create_and_checkout_branch(&branch_name)?;
      Some(branch_name)
    };

    // Pre-allocate for expected conflicts (typically a small fraction of commits)
    let mut conflicts = Vec::with_capacity(commits_to_sync.len().min(16));

    // Process commits (we already filtered to only those needing sync above)
    progress!("   Syncing {} commits from remote...", commits_to_sync.len());

    let mut count = 0;
    let mut current_mono_head = self.ctx.git()?.git().head_commit()?; // Cache HEAD, update after each commit

    for commit in &commits_to_sync {
      // Resolve conflicts using 3-way merge (returns conflicts + changed_files for caching)
      let resolution = self.resolve_conflicts_for_commit(commit, &remote_git)?;

      // Collect paths of resolved files (don't overwrite these in apply_remote_commit_to_mono)
      // Using HashSet<&Path> for O(1) membership testing - borrows from resolution.conflicts, no clones
      let resolved_files: HashSet<&Path> = resolution.resolved_files.iter().map(PathBuf::as_path).collect();

      let mut commit_paths = resolution
        .changed_files
        .iter()
        .filter_map(|(remote_path, _)| self.map_remote_path_to_mono(remote_path).ok())
        .chain(resolution.resolved_files.iter().cloned())
        .collect::<Vec<_>>();
      commit_paths.sort();
      commit_paths.dedup();

      // Apply commit to mono (skipping already-resolved files, reusing cached changed_files)
      let mono_sha = self.apply_remote_commit_to_mono(
        commit,
        &remote_git,
        &resolved_files,
        &current_mono_head,
        &resolution.changed_files,
        resolution.conflicts.is_empty(),
      )?;

      if !resolution.conflicts.is_empty() {
        let branch = pr_branch
          .as_deref()
          .ok_or_else(|| crate::error::RailError::message("conflicted sync has no recovery branch"))?;
        let receipt = self.write_conflict_receipt(SyncConflictReceipt {
          schema_version: 1,
          status: "conflicted".to_string(),
          crate_name: self.config.crate_name.clone(),
          branch: branch.to_string(),
          expected_head: current_mono_head.clone(),
          remote_commit: commit.sha.clone(),
          message: format!("{}\n\nRail-Origin: remote@{}", commit.message.trim(), commit.sha),
          author: commit.author.clone(),
          author_email: commit.author_email.clone(),
          timestamp: commit.timestamp,
          commit_paths,
          conflicts: resolution.conflicts.clone(),
          resulting_commit: None,
        })?;
        progress!("   sync paused with unresolved conflicts on branch '{}'", branch);
        progress!("   resolve the listed paths, then run:");
        progress!("   cargo rail sync --resume {}", receipt.display());
        return Ok(SyncResult {
          commits_synced: count,
          conflicts: resolution.conflicts,
          status: SyncStatus::Conflicted,
          conflict_receipt: Some(receipt),
        });
      }
      let mono_sha = mono_sha.ok_or_else(|| crate::error::RailError::message("clean sync did not create a commit"))?;

      // Extend conflicts AFTER apply (resolved_files borrows from resolution.conflicts)
      if !resolution.conflicts.is_empty() {
        conflicts.extend(resolution.conflicts);
      }

      // Record mapping (remote -> mono)
      self.mapping_store.record_mapping(&mono_sha, &commit.sha)?;
      count += 1;
      current_mono_head = mono_sha; // Update cached HEAD (move, not clone)
    }

    let synced_count = count;

    // Save mappings
    self.mapping_store.save(self.ctx.workspace_root())?;

    // Print PR creation instructions if we created a branch with synced commits
    if let Some(branch_name) = pr_branch
      && synced_count > 0
    {
      progress!(
        "\n✅ Synced {} commit{} to branch: {}",
        synced_count,
        if synced_count == 1 { "" } else { "s" },
        branch_name
      );
      progress!("\n📋 To create a pull request:");
      progress!("   git push origin {}", branch_name);

      // Try to detect GitHub URL and suggest gh CLI command
      if let Ok(Some(url)) = self.ctx.git()?.git().get_config("remote.origin.url")
        && url.contains("github.com")
      {
        progress!(
          "   gh pr create --title \"Sync {} from remote\"",
          self.config.crate_name
        );
      }
      progress!();
    }

    Ok(SyncResult {
      commits_synced: synced_count,
      conflicts,
      ..SyncResult::default()
    })
  }

  /// Sync changes bidirectionally between monorepo and remote
  pub fn sync_bidirectional(&mut self) -> RailResult<SyncResult> {
    progress!("   Detecting changes...");

    // Check both directions
    let mono_has_changes = self.check_mono_has_changes()?;
    let remote_has_changes = self.check_remote_has_changes()?;

    match (mono_has_changes, remote_has_changes) {
      (true, false) => {
        progress!("   Only monorepo has changes");
        self.sync_to_remote()
      }
      (false, true) => {
        progress!("   Only remote has changes");
        self.sync_from_remote()
      }
      (true, true) => {
        progress!("   Both sides have changes, syncing both directions");
        let to_remote = self.sync_to_remote()?;
        let from_remote = self.sync_from_remote()?;

        Ok(SyncResult {
          commits_synced: to_remote.commits_synced + from_remote.commits_synced,
          conflicts: from_remote.conflicts,
          status: from_remote.status,
          conflict_receipt: from_remote.conflict_receipt,
        })
      }
      (false, false) => {
        progress!("   No changes on either side");
        Ok(SyncResult {
          commits_synced: 0,
          conflicts: Vec::new(),
          ..SyncResult::default()
        })
      }
    }
  }

  // Helper methods

  fn find_last_synced_mono_commit(&self) -> RailResult<Option<String>> {
    // Find the most recent mono commit that has a mapping
    let commits = self.ctx.git()?.git().commit_history(Some(100))?;

    for commit in commits {
      if self.mapping_store.has_mapping(&commit.sha) {
        return Ok(Some(commit.sha));
      }
    }

    Ok(None)
  }

  fn find_last_synced_remote_commit(&self, remote_git: &SystemGit) -> RailResult<Option<String>> {
    // Find the most recent remote commit that has a reverse mapping (O(1) lookups)
    let commits = remote_git.commit_history(Some(100))?;

    for commit in commits {
      // Check if this remote commit has been mapped (O(1) reverse lookup)
      if self.mapping_store.has_reverse_mapping(&commit.sha) {
        return Ok(Some(commit.sha));
      }
    }

    Ok(None)
  }

  fn apply_mono_commit_to_remote(
    &self,
    commit: &crate::git::CommitInfo,
    remote_git: &SystemGit,
    current_remote_head: &str,
  ) -> RailResult<String> {
    // Get changed files in mono
    let changed_files = self.ctx.git()?.git().get_changed_files(&commit.sha)?;

    // Filter to only files in configured crate path scope.
    let relevant_files: Vec<_> = changed_files
      .into_iter()
      .filter(|(path, _)| {
        // Skip files that shouldn't be synced (target dir, etc.)
        let path_str = path.to_string_lossy();
        let should_exclude = path_str.contains("/target/") || path_str.contains("\\target\\");
        self.mono_path_in_scope(path) && !should_exclude
      })
      .collect();

    // Separate deletions from additions/modifications
    let (deletions, modifications): (Vec<_>, Vec<_>) =
      relevant_files.iter().partition(|(_, change_type)| *change_type == 'D');

    // Handle deletions
    for (mono_path, _) in &deletions {
      let remote_path = self.map_mono_path_to_remote(mono_path)?;
      let full_remote_path = self.config.path_capabilities.authorize_target(&remote_path)?;
      if full_remote_path.exists() {
        std::fs::remove_file(&full_remote_path)?;
      }
    }

    // Bulk read all files that need to be added/modified (single git call instead of N calls)
    // Uses references to avoid cloning SHA and paths for each file
    let bulk_items: Vec<(&str, &Path)> = modifications
      .iter()
      .map(|(path, _)| (commit.sha.as_str(), path.as_path()))
      .collect();

    let file_contents = if !bulk_items.is_empty() {
      self.ctx.git()?.git().read_files_bulk(&bulk_items)?
    } else {
      vec![]
    };

    // Apply each file to remote
    for (idx, (mono_path, _)) in modifications.iter().enumerate() {
      let content = &file_contents[idx];
      let remote_path = self.map_mono_path_to_remote(mono_path)?;
      let full_remote_path = self.config.path_capabilities.authorize_target(&remote_path)?;

      // Create parent directories
      if let Some(parent) = full_remote_path.parent() {
        std::fs::create_dir_all(parent)?;
      }

      // Write file first, then transform manifest if applicable
      std::fs::write(&full_remote_path, content)?;

      // Transform Cargo.toml manifest
      if mono_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content = std::fs::read_to_string(&full_remote_path)?;
        // Determine if target has workspace based on split mode
        let target_has_workspace =
          self.config.mode == SplitMode::Combined && self.config.workspace_mode == WorkspaceMode::Workspace;
        let context = TransformContext {
          crate_name: self.config.crate_name.clone(),
          workspace_root: self.ctx.workspace_root().to_path_buf(),
          target_has_workspace,
        };
        let transformed = self.transform.transform_to_split(&content, &context)?;
        std::fs::write(&full_remote_path, transformed)?;
      }
    }

    // Create commit with trailer
    let message = format!("{}\n\nRail-Origin: mono@{}", commit.message.trim(), commit.sha);

    let parent_shas = vec![current_remote_head.to_string()];
    let mut commit_paths = relevant_files
      .iter()
      .map(|(mono_path, _)| self.map_mono_path_to_remote(mono_path))
      .collect::<RailResult<Vec<_>>>()?;
    commit_paths.sort();
    commit_paths.dedup();

    let new_commit_sha = remote_git.create_commit_with_metadata(
      &message,
      &commit.author,
      &commit.author_email,
      commit.timestamp,
      &parent_shas,
      &commit_paths,
    )?;

    Ok(new_commit_sha)
  }

  fn apply_remote_commit_to_mono(
    &self,
    commit: &crate::git::CommitInfo,
    remote_git: &SystemGit,
    resolved_files: &HashSet<&Path>,
    current_mono_head: &str,
    changed_files: &[(PathBuf, char)], // Pre-fetched from resolve_conflicts to avoid duplicate subprocess call
    create_commit: bool,
  ) -> RailResult<Option<String>> {
    // Use pre-fetched changed_files (already retrieved in resolve_conflicts_for_commit)

    // Filter and separate files by operation type
    let relevant_files: Vec<_> = changed_files
      .iter()
      .filter_map(|(remote_path, change_type)| {
        let mono_path = self.map_remote_path_to_mono(remote_path).ok()?;

        // Skip files excluded by Cargo (target, etc.)
        let path_str = mono_path.to_string_lossy();
        let should_exclude = path_str.contains("/target/") || path_str.contains("\\target\\");
        if should_exclude {
          return None;
        }

        // Skip files that were already resolved by conflict resolution (O(1) HashSet lookup)
        if resolved_files.contains(mono_path.as_path()) {
          progress!("      Skipping {} (already resolved)", mono_path.display());
          return None;
        }

        Some((remote_path, mono_path, change_type))
      })
      .collect();

    // Separate deletions from additions/modifications
    let (deletions, modifications): (Vec<_>, Vec<_>) = relevant_files
      .iter()
      .partition(|(_, _, change_type)| **change_type == 'D');

    // Handle deletions
    for (_, mono_path, _) in &deletions {
      let full_mono_path = self.config.path_capabilities.authorize_source_mutation(mono_path)?;
      if full_mono_path.exists() {
        std::fs::remove_file(&full_mono_path)?;
      }
    }

    // Bulk read all files that need to be added/modified (single git call instead of N calls)
    // Uses references to avoid cloning SHA and paths for each file
    let bulk_items: Vec<(&str, &Path)> = modifications
      .iter()
      .map(|(remote_path, _, _)| (commit.sha.as_str(), (*remote_path).as_path()))
      .collect();

    let file_contents = if !bulk_items.is_empty() {
      remote_git.read_files_bulk(&bulk_items)?
    } else {
      vec![]
    };

    // Apply files to mono
    for (idx, (remote_path, mono_path, _)) in modifications.iter().enumerate() {
      let content = &file_contents[idx];
      let full_mono_path = self.config.path_capabilities.authorize_source_mutation(mono_path)?;

      // Create parent directories
      if let Some(parent) = full_mono_path.parent() {
        std::fs::create_dir_all(parent)?;
      }

      // Write file first, then transform manifest if applicable
      std::fs::write(&full_mono_path, content)?;

      // Transform Cargo.toml manifest
      if remote_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content = std::fs::read_to_string(&full_mono_path)?;
        // Monorepo always has a workspace
        let context = TransformContext {
          crate_name: self.config.crate_name.clone(),
          workspace_root: self.ctx.workspace_root().to_path_buf(),
          target_has_workspace: true,
        };
        let transformed = self.transform.transform_to_mono(&content, &context)?;
        std::fs::write(&full_mono_path, transformed)?;
      }
    }

    // Create commit with trailer
    let message = format!("{}\n\nRail-Origin: remote@{}", commit.message.trim(), commit.sha);

    let parent_shas = vec![current_mono_head.to_string()];
    let mut commit_paths = relevant_files
      .iter()
      .map(|(_, mono_path, _)| mono_path.clone())
      .chain(resolved_files.iter().map(|path| (*path).to_path_buf()))
      .collect::<Vec<_>>();
    commit_paths.sort();
    commit_paths.dedup();

    if !create_commit {
      return Ok(None);
    }

    let new_commit_sha = self.ctx.git()?.git().create_commit_with_metadata(
      &message,
      &commit.author,
      &commit.author_email,
      commit.timestamp,
      &parent_shas,
      &commit_paths,
    )?;

    Ok(Some(new_commit_sha))
  }

  fn map_mono_path_to_remote(&self, mono_path: &Path) -> RailResult<PathBuf> {
    match self.config.mode {
      SplitMode::Single => {
        let crate_path = self
          .config
          .crate_paths
          .first()
          .ok_or_else(|| crate::error::RailError::message("single-mode sync requires exactly one crate path"))?;
        // Strip crate path prefix
        Ok(mono_path.strip_prefix(crate_path)?.to_path_buf())
      }
      SplitMode::Combined => {
        // Keep full path
        Ok(mono_path.to_path_buf())
      }
    }
  }

  fn map_remote_path_to_mono(&self, remote_path: &Path) -> RailResult<PathBuf> {
    match self.config.mode {
      SplitMode::Single => {
        let crate_path = self
          .config
          .crate_paths
          .first()
          .ok_or_else(|| crate::error::RailError::message("single-mode sync requires exactly one crate path"))?;
        // Prepend crate path
        Ok(crate_path.join(remote_path))
      }
      SplitMode::Combined => {
        // Keep full path
        Ok(remote_path.to_path_buf())
      }
    }
  }

  /// Resolve conflicts for a commit using 3-way merge
  /// Returns: (conflicts, changed_files) - the changed_files are cached for reuse in apply step
  fn resolve_conflicts_for_commit(
    &self,
    remote_commit: &crate::git::CommitInfo,
    remote_git: &SystemGit,
  ) -> RailResult<ConflictResolutionResult> {
    // Get files changed in this remote commit
    let changed_files = remote_git.get_changed_files(&remote_commit.sha)?;

    // Find the base commit (common ancestor)
    let last_synced = self.find_last_synced_mono_commit()?;

    // Build cache of all files modified in mono since last sync
    // Single git call instead of N calls (one per remote file)
    let mono_changed_paths: std::collections::HashSet<PathBuf> = if let Some(ref last) = last_synced {
      self
        .ctx
        .git()?
        .git()
        .get_changed_files_between(last, Some("HEAD"))?
        .into_iter()
        .map(|(path, _)| path)
        .collect()
    } else {
      std::collections::HashSet::new()
    };

    // Identify conflicting files (files modified on both sides)
    // Pre-allocate for worst case (all files conflict) - typically much smaller
    let mut conflicting_files = Vec::with_capacity(changed_files.len());
    for (remote_path, _) in &changed_files {
      let mono_path = self.map_remote_path_to_mono(remote_path)?;
      let full_mono_path = self.config.path_capabilities.authorize_source_mutation(&mono_path)?;

      // Skip if file doesn't exist in monorepo (new file, no conflict)
      if !full_mono_path.exists() {
        continue;
      }

      // Check if file was modified in mono since last sync (O(1) HashSet lookup)
      let mono_modified = mono_changed_paths.contains(&mono_path);

      // If not modified in mono, no conflict - will be cleanly applied
      if !mono_modified {
        continue;
      }

      // Both sides modified - this is a conflict
      conflicting_files.push((remote_path.clone(), mono_path, full_mono_path));
    }

    // Pre-allocate conflicts vec now that we know the size
    let mut conflicts = Vec::with_capacity(conflicting_files.len());
    let mut resolved_files = Vec::with_capacity(conflicting_files.len());

    // Bulk read base and incoming versions for all conflicting files
    // Uses references to avoid cloning SHA and paths for each file
    let base_items: Vec<(&str, &Path)> = if let Some(ref sha) = last_synced {
      conflicting_files
        .iter()
        .map(|(_, mono_path, _)| (sha.as_str(), mono_path.as_path()))
        .collect()
    } else {
      vec![]
    };

    let incoming_items: Vec<(&str, &Path)> = conflicting_files
      .iter()
      .map(|(remote_path, _, _)| (remote_commit.sha.as_str(), remote_path.as_path()))
      .collect();

    let base_contents = if !base_items.is_empty() {
      self.ctx.git()?.git().read_files_bulk(&base_items)?
    } else {
      vec![Vec::new(); conflicting_files.len()]
    };

    let incoming_contents = if !incoming_items.is_empty() {
      remote_git.read_files_bulk(&incoming_items)?
    } else {
      vec![]
    };

    // Resolve conflicts with bulk-loaded content
    for (idx, (_, mono_path, full_mono_path)) in conflicting_files.iter().enumerate() {
      let base_content = if idx < base_contents.len() {
        &base_contents[idx]
      } else {
        &Vec::new()
      };
      let incoming_content = &incoming_contents[idx];

      // Perform 3-way merge
      match self
        .conflict_resolver
        .resolve_file(full_mono_path, base_content, incoming_content)
      {
        Ok(crate::sync::conflict::MergeResult::Success) => {
          // Merged successfully - add to resolved files to prevent overwriting
          progress!("      ✅ Auto-merged {}", mono_path.display());
          resolved_files.push(mono_path.clone());
        }
        Ok(crate::sync::conflict::MergeResult::Conflicts(_paths)) => {
          conflicts.push(ConflictInfo {
            file_path: mono_path.clone(),
            class: ConflictClass::Content,
          });
          resolved_files.push(mono_path.clone());
        }
        Ok(crate::sync::conflict::MergeResult::Failed(message)) => {
          return Err(crate::error::RailError::with_help(
            format!("failed to merge '{}': {}", mono_path.display(), message),
            "the sync branch and worktree were preserved; correct the underlying Git merge failure and retry",
          ));
        }
        Err(error) => return Err(error.context(format!("merging '{}'", mono_path.display()))),
      }
    }

    Ok(ConflictResolutionResult {
      conflicts,
      resolved_files,
      changed_files,
    })
  }

  fn check_mono_has_changes(&self) -> RailResult<bool> {
    let last_synced = self.find_last_synced_mono_commit()?;
    let new_commits =
      self
        .ctx
        .git()?
        .git()
        .get_commits_touching_paths(&self.config.crate_paths, last_synced.as_deref(), "HEAD")?;

    Ok(
      new_commits
        .into_iter()
        .any(|commit| !commit.message.contains("Rail-Origin: remote@")),
    )
  }

  fn check_remote_has_changes(&self) -> RailResult<bool> {
    let remote_git = SystemGit::open(&self.config.target_repo_path)?;

    // Fetch from remote (skip for local paths)
    if !utils::is_local_path(&self.config.remote_url) {
      remote_git.fetch_from_remote("origin")?;
    }

    let last_synced = self.find_last_synced_remote_commit(&remote_git)?;

    let branch_ref = self.get_branch_ref();
    let new_commits = if let Some(ref last) = last_synced {
      remote_git.get_commits_touching_path(Path::new("."), Some(last), &branch_ref)?
    } else {
      remote_git.get_commits_touching_path(Path::new("."), None, &branch_ref)?
    };

    // Filter out commits from mono
    let relevant_commits: Vec<_> = new_commits
      .into_iter()
      .filter(|c| !c.message.contains("Rail-Origin: mono@"))
      .collect();

    Ok(!relevant_commits.is_empty())
  }
}

fn contains_conflict_markers(content: &[u8]) -> bool {
  content
    .split(|byte| *byte == b'\n')
    .any(|line| line.starts_with(b"<<<<<<<") || line.starts_with(b"=======") || line.starts_with(b">>>>>>>"))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> RailResult<()> {
  let bytes = serde_json::to_vec_pretty(value)
    .map_err(|error| crate::error::RailError::message(format!("failed to serialize sync receipt: {}", error)))?;
  let temporary = path.with_extension("json.tmp");
  std::fs::write(&temporary, bytes)?;
  std::fs::rename(&temporary, path)?;
  Ok(())
}

/// Read the crate identity from a workspace-owned conflict receipt.
pub fn conflict_receipt_crate(workspace_root: &Path, receipt_path: &Path) -> RailResult<String> {
  let path = utils::canonicalize_existing(receipt_path)?;
  let dir = utils::canonicalize_existing(&workspace_root.join("target/cargo-rail/receipts"))?;
  if path.parent().is_none_or(|parent| parent != dir) {
    return Err(crate::error::RailError::message(format!(
      "sync receipt '{}' is outside the workspace receipt directory",
      receipt_path.display()
    )));
  }
  let receipt: SyncConflictReceipt = serde_json::from_slice(&std::fs::read(path)?)
    .map_err(|error| crate::error::RailError::message(format!("invalid sync conflict receipt: {}", error)))?;
  Ok(receipt.crate_name)
}
