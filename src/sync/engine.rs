//! Bidirectional sync engine between monorepo and split repositories.
//!
//! Coordinates commit mapping, conflict detection/resolution, and Cargo.toml
//! transforms while preserving deterministic sync behavior.

use crate::cargo::{CargoTransform, TransformContext};
use crate::config::{SplitMode, WorkspaceMode};
use crate::error::RailResult;
use crate::git::mappings::{HistorySide, MappingStore, OriginContext, append_origin_trailers};
use crate::git::ops::{GitIndexChange, GitTreeEntry};
use crate::git::{CommitMetadata, SystemGit};
use crate::progress;
use crate::split::{SplitOwnership, SplitPathCapabilities};
use crate::sync::conflict::{ConflictClass, ConflictInfo, ConflictResolver, ConflictStrategy};
use crate::utils;
use crate::workspace::WorkspaceContext;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Configuration for sync operation
#[derive(Clone)]
pub struct SyncConfig {
  /// Name of the crate being synced
  pub crate_name: String,
  /// Paths to crate directories
  pub crate_paths: Vec<PathBuf>,
  /// Explicit non-Cargo assets resolved from the ownership snapshot.
  pub asset_paths: Vec<PathBuf>,
  /// Snapshot-derived ownership and dependency/release evidence.
  pub ownership: SplitOwnership,
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
  author_timestamp: i64,
  author_timezone: String,
  committer: String,
  committer_email: String,
  committer_timestamp: i64,
  committer_timezone: String,
  commit_paths: Vec<PathBuf>,
  conflicts: Vec<ConflictInfo>,
  resulting_commit: Option<String>,
}

impl SyncConflictReceipt {
  fn commit_metadata(&self) -> CommitMetadata {
    CommitMetadata {
      author: self.author.clone(),
      author_email: self.author_email.clone(),
      author_timestamp: self.author_timestamp,
      author_timezone: self.author_timezone.clone(),
      committer: self.committer.clone(),
      committer_email: self.committer_email.clone(),
      committer_timestamp: self.committer_timestamp,
      committer_timezone: self.committer_timezone.clone(),
    }
  }
}

/// Bidirectional sync engine
pub struct SyncEngine<'a> {
  /// Workspace context
  ctx: &'a WorkspaceContext,
  /// Sync configuration
  config: SyncConfig,
  /// Commit mapping store
  mapping_store: MappingStore,
  /// Origin evidence for monorepo commits synthesized into the target.
  source_origin: OriginContext,
  /// Origin evidence for target commits synthesized into the monorepo.
  target_origin: OriginContext,
  /// Cargo.toml transformer
  transform: CargoTransform,
  /// Conflict resolver
  conflict_resolver: ConflictResolver,
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
    for asset in &config.asset_paths {
      config.path_capabilities.authorize_source(asset)?;
      if config
        .crate_paths
        .iter()
        .any(|crate_root| asset.starts_with(crate_root))
      {
        return Err(crate::error::RailError::message(format!(
          "explicit asset '{}' overlaps a Cargo-owned member root",
          asset.display()
        )));
      }
    }
    config.path_capabilities.validate_target_repository()?;
    let mapping_store = MappingStore::new(config.crate_name.clone());
    let source_origin =
      OriginContext::discover(ctx.workspace_root(), &config.crate_name, &config.ownership.snapshot_id)?;
    let target_origin = OriginContext::discover(
      &config.target_repo_path,
      &config.crate_name,
      &config.ownership.snapshot_id,
    )?;
    let transformer = CargoTransform::new(ctx.cargo().metadata().clone());

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
    let conflict_resolver = ConflictResolver::new(conflict_strategy, temp_dir);

    Ok(Self {
      ctx,
      config,
      mapping_store,
      source_origin,
      target_origin,
      transform: transformer,

      conflict_resolver,
    })
  }

  fn load_mappings(&mut self) -> RailResult<()> {
    self.load_mapping_evidence()?;
    if self
      .mapping_store
      .migrate_legacy_mappings(&self.config.target_repo_path, &self.source_origin)?
      .is_some()
    {
      progress!("   Migrated legacy mappings into ordinary Git history");
    }
    Ok(())
  }

  fn load_mapping_evidence(&mut self) -> RailResult<()> {
    self.mapping_store.load_history(
      self.ctx.workspace_root(),
      HistorySide::Source,
      self.target_origin.source_repository(),
    )?;
    self.mapping_store.load_history(
      &self.config.target_repo_path,
      HistorySide::Target,
      self.source_origin.source_repository(),
    )?;
    self.mapping_store.load_legacy_notes(self.ctx.workspace_root())?;
    self.mapping_store.load_legacy_notes(&self.config.target_repo_path)?;
    self.config.path_capabilities.validate_target_repository()?;
    Ok(())
  }

  /// Classify whether the selected direction has mapped commits pending.
  pub fn has_pending_changes(&mut self, direction: &SyncDirection) -> RailResult<bool> {
    self.load_mapping_evidence()?;
    match direction {
      SyncDirection::MonoToRemote => self.check_mono_has_changes(),
      SyncDirection::RemoteToMono => self.check_remote_has_changes(),
      SyncDirection::Both => Ok(self.check_mono_has_changes()? || self.check_remote_has_changes()?),
      SyncDirection::None => Ok(false),
    }
  }

  /// Commit operator-resolved work from a durable conflict receipt, then
  /// continue with any remaining remote commits.
  pub fn resume_from_receipt(&mut self, receipt_path: &Path) -> RailResult<SyncResult> {
    let receipt_path = self.validate_conflict_receipt_path(receipt_path)?;
    let bytes = std::fs::read(&receipt_path)?;
    let mut receipt: SyncConflictReceipt = serde_json::from_slice(&bytes)
      .map_err(|error| crate::error::RailError::message(format!("invalid sync conflict receipt: {}", error)))?;
    if receipt.schema_version != 2 || receipt.status != "conflicted" {
      return Err(crate::error::RailError::message(
        "sync conflict receipt is not an active version-2 conflict",
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
    let unexpected = self
      .ctx
      .changed_source_paths()?
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

    let metadata = receipt.commit_metadata();
    let commit = git.create_commit_with_metadata(
      &receipt.message,
      &metadata,
      std::slice::from_ref(&receipt.expected_head),
      &receipt.commit_paths,
    )?;
    self.mapping_store.record_mapping(&commit, &receipt.remote_commit)?;
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
    crate::workspace::cargo_rail_state_root(self.ctx.workspace_root()).join("receipts")
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
    if self.config.asset_paths.iter().any(|asset| asset == path) {
      return true;
    }
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

  fn owned_paths(&self) -> Vec<PathBuf> {
    let mut paths = self.config.crate_paths.clone();
    paths.extend(self.config.asset_paths.iter().cloned());
    paths.sort();
    paths.dedup();
    paths
  }

  /// Sync changes from monorepo to remote repository
  pub fn sync_to_remote(&mut self) -> RailResult<SyncResult> {
    progress!("   Syncing monorepo → remote...");

    self.load_mappings()?;

    // Open remote repo
    let target_repo_path = self.config.target_repo_path.clone();
    let remote_git = SystemGit::open(&target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !utils::is_local_path(&self.config.remote_url) {
      remote_git.fetch_from_remote("origin")?;
    } else {
      progress!("   Skipping fetch (local testing mode)");
    }

    // Find last synced commit in mono
    let last_synced_mono = self.find_last_synced_mono_commit()?;

    // Get new commits in mono that touch any of the crate paths (handles both single and combined modes)
    let new_commits =
      self
        .ctx
        .git()?
        .git()
        .get_commits_touching_paths(&self.owned_paths(), last_synced_mono.as_deref(), "HEAD")?;

    if new_commits.is_empty() {
      progress!("   No new commits to sync");
    } else {
      progress!("   Syncing {} commits to remote...", new_commits.len());

      let mut synced_count = 0;
      let mut current_remote_head = remote_git.head_commit()?; // Cache HEAD, update after each commit
      let commit_shas = new_commits.iter().map(|commit| commit.sha.clone()).collect::<Vec<_>>();
      let changed_files = self.ctx.git()?.git().get_changed_files_bulk(&commit_shas)?;

      for (commit, changed_files) in new_commits.iter().zip(&changed_files) {
        // Skip if already synced
        if self.mapping_store.has_mapping(&commit.sha) {
          continue;
        }

        // Apply commit to remote
        let remote_sha = self.apply_mono_commit_to_remote(commit, changed_files, &remote_git, &current_remote_head)?;

        // Record mapping
        self.mapping_store.record_mapping(&commit.sha, &remote_sha)?;
        synced_count += 1;
        current_remote_head = remote_sha; // Update cached HEAD (move, not clone)
      }

      // Push to remote (skip for local paths)
      if synced_count > 0 && !utils::is_local_path(&self.config.remote_url) {
        remote_git.push_to_remote("origin", &self.config.branch)?;
      }

      return Ok(SyncResult {
        commits_synced: synced_count,
        conflicts: Vec::new(),
        ..SyncResult::default()
      });
    }

    let synced_count = 0;

    // Push to remote (skip for local paths)
    if synced_count > 0 {
      if !utils::is_local_path(&self.config.remote_url) {
        remote_git.push_to_remote("origin", &self.config.branch)?;
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

    self.load_mappings()?;

    // Open remote repo
    let target_repo_path = self.config.target_repo_path.clone();
    let remote_git = SystemGit::open(&target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !utils::is_local_path(&self.config.remote_url) {
      remote_git.fetch_from_remote("origin")?;
    } else {
      progress!("   Skipping fetch (local testing mode)");
    }

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
    let commit_shas = commits_to_sync
      .iter()
      .map(|commit| commit.sha.clone())
      .collect::<Vec<_>>();
    let changed_files = remote_git.get_changed_files_bulk(&commit_shas)?;

    for (commit, changed_files) in commits_to_sync.iter().zip(&changed_files) {
      // Resolve conflicts using 3-way merge (returns conflicts + changed_files for caching)
      let resolution = self.resolve_conflicts_for_commit(commit, &remote_git, changed_files)?;

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
          schema_version: 2,
          status: "conflicted".to_string(),
          crate_name: self.config.crate_name.clone(),
          branch: branch.to_string(),
          expected_head: current_mono_head.clone(),
          remote_commit: commit.sha.clone(),
          message: append_origin_trailers(&commit.message, &[self.target_origin.trailer(&commit.sha)?]),
          author: commit.author.clone(),
          author_email: commit.author_email.clone(),
          author_timestamp: commit.timestamp,
          author_timezone: commit.author_timezone.clone(),
          committer: commit.committer.clone(),
          committer_email: commit.committer_email.clone(),
          committer_timestamp: commit.committer_timestamp,
          committer_timezone: commit.committer_timezone.clone(),
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
    self.load_mappings()?;

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
    changed_files: &[(PathBuf, char)],
    remote_git: &SystemGit,
    current_remote_head: &str,
  ) -> RailResult<String> {
    let source_git = self.ctx.git()?.git();

    // Filter to only files in configured crate path scope.
    let relevant_files: Vec<_> = changed_files
      .iter()
      .filter(|(path, _)| self.mono_path_in_scope(path))
      .cloned()
      .collect();

    let modification_paths = relevant_files
      .iter()
      .filter(|(_, change_type)| *change_type != 'D')
      .map(|(path, _)| path.clone())
      .collect::<Vec<_>>();
    let entries = source_git
      .collect_tree_entries_for_paths(&commit.sha, &modification_paths)?
      .into_iter()
      .map(|entry| (entry.path.clone(), entry))
      .collect::<HashMap<_, _>>();

    remote_git.import_objects(self.ctx.workspace_root(), &commit.sha)?;
    let mut changes = Vec::with_capacity(relevant_files.len());
    for (mono_path, change_type) in &relevant_files {
      let remote_path = self.map_mono_path_to_remote(mono_path)?;
      self.config.path_capabilities.authorize_target(&remote_path)?;
      if *change_type == 'D' {
        changes.push(GitIndexChange::Delete(remote_path));
        continue;
      }
      let entry = entries.get(mono_path).ok_or_else(|| {
        crate::error::RailError::message(format!(
          "commit '{}' has no exact tree entry for '{}'",
          commit.sha,
          mono_path.display()
        ))
      })?;
      let object_id = if mono_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content = source_git.read_blobs_bulk(&[entry.object_id.as_str()])?;
        remote_git.write_blob(&self.transform_manifest_to_split(&content[0])?)?
      } else {
        entry.object_id.clone()
      };
      changes.push(GitIndexChange::Upsert(GitTreeEntry {
        mode: entry.mode.clone(),
        object_id,
        path: remote_path,
      }));
    }

    // Create commit with trailer
    let message = append_origin_trailers(&commit.message, &[self.source_origin.trailer(&commit.sha)?]);

    let parent_shas = self.mapped_target_parents(commit, current_remote_head);
    let new_commit_sha =
      remote_git.create_commit_with_index_changes(&message, &commit.metadata(), &parent_shas, &changes)?;

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
    let relevant_files = changed_files
      .iter()
      .map(|(remote_path, change_type)| {
        self
          .map_remote_path_to_mono(remote_path)
          .map(|mono_path| (remote_path, mono_path, change_type))
      })
      .collect::<RailResult<Vec<_>>>()?;

    if !create_commit {
      self.materialize_remote_changes(commit, remote_git, resolved_files, &relevant_files)?;
      return Ok(None);
    }

    let incoming = relevant_files
      .iter()
      .filter(|(_, mono_path, change_type)| *change_type != &'D' && !resolved_files.contains(mono_path.as_path()))
      .map(|(remote_path, _, _)| (*remote_path).clone())
      .collect::<Vec<_>>();
    let entries = remote_git
      .collect_tree_entries_for_paths(&commit.sha, &incoming)?
      .into_iter()
      .map(|entry| (entry.path.clone(), entry))
      .collect::<HashMap<_, _>>();
    let mono_git = self.ctx.git()?.git();
    mono_git.import_objects(&self.config.target_repo_path, &commit.sha)?;
    let mut changes = Vec::with_capacity(relevant_files.len() + resolved_files.len());
    for (remote_path, mono_path, change_type) in &relevant_files {
      self.config.path_capabilities.authorize_source_mutation(mono_path)?;
      if resolved_files.contains(mono_path.as_path()) {
        continue;
      }
      if **change_type == 'D' {
        changes.push(GitIndexChange::Delete(mono_path.clone()));
        continue;
      }
      let entry = entries.get(*remote_path).ok_or_else(|| {
        crate::error::RailError::message(format!(
          "commit '{}' has no exact tree entry for '{}'",
          commit.sha,
          remote_path.display()
        ))
      })?;
      let object_id = if remote_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content = remote_git.read_blobs_bulk(&[entry.object_id.as_str()])?;
        mono_git.write_blob(&self.transform_manifest_to_mono(&content[0])?)?
      } else {
        entry.object_id.clone()
      };
      changes.push(GitIndexChange::Upsert(GitTreeEntry {
        mode: entry.mode.clone(),
        object_id,
        path: mono_path.clone(),
      }));
    }

    if !resolved_files.is_empty() {
      let resolved = resolved_files
        .iter()
        .map(|path| (*path).to_path_buf())
        .collect::<Vec<_>>();
      let modes = mono_git
        .collect_tree_entries_for_paths(current_mono_head, &resolved)?
        .into_iter()
        .map(|entry| (entry.path, entry.mode))
        .collect::<HashMap<_, _>>();
      for mono_path in resolved {
        let full_path = self.config.path_capabilities.authorize_source_mutation(&mono_path)?;
        let (mut content, mode) = read_worktree_blob(&full_path, modes.get(&mono_path).map(String::as_str))?;
        if mode != "120000" && mono_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
          content = self.transform_manifest_to_mono(&content)?;
        }
        changes.push(GitIndexChange::Upsert(GitTreeEntry {
          mode,
          object_id: mono_git.write_blob(&content)?,
          path: mono_path,
        }));
      }
    }

    // Create commit with trailer
    let message = append_origin_trailers(&commit.message, &[self.target_origin.trailer(&commit.sha)?]);

    let parent_shas = self.mapped_source_parents(commit, current_mono_head);
    let new_commit_sha =
      mono_git.create_commit_with_index_changes(&message, &commit.metadata(), &parent_shas, &changes)?;

    Ok(Some(new_commit_sha))
  }

  fn materialize_remote_changes(
    &self,
    commit: &crate::git::CommitInfo,
    remote_git: &SystemGit,
    resolved_files: &HashSet<&Path>,
    relevant_files: &[(&PathBuf, PathBuf, &char)],
  ) -> RailResult<()> {
    let (deletions, modifications): (Vec<_>, Vec<_>) = relevant_files
      .iter()
      .filter(|(_, mono_path, _)| !resolved_files.contains(mono_path.as_path()))
      .partition(|entry| *entry.2 == 'D');
    for (_, mono_path, _) in deletions {
      let path = self.config.path_capabilities.authorize_source_mutation(mono_path)?;
      remove_worktree_file(&path)?;
    }
    let items = modifications
      .iter()
      .map(|(remote_path, _, _)| (commit.sha.as_str(), remote_path.as_path()))
      .collect::<Vec<_>>();
    let contents = remote_git.read_files_bulk(&items)?;
    for ((remote_path, mono_path, _), mut content) in modifications.into_iter().zip(contents) {
      let path = self.config.path_capabilities.authorize_source_mutation(mono_path)?;
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
      }
      if remote_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        content = self.transform_manifest_to_mono(&content)?;
      }
      write_worktree_file(&path, &content)?;
    }
    Ok(())
  }

  fn transform_manifest_to_split(&self, content: &[u8]) -> RailResult<Vec<u8>> {
    let content = std::str::from_utf8(content)
      .map_err(|error| crate::error::RailError::message(format!("Cargo.toml is not UTF-8: {error}")))?;
    let target_has_workspace =
      self.config.mode == SplitMode::Combined && self.config.workspace_mode == WorkspaceMode::Workspace;
    let context = TransformContext {
      crate_name: self.config.crate_name.clone(),
      workspace_root: self.ctx.workspace_root().to_path_buf(),
      target_has_workspace,
    };
    Ok(self.transform.transform_to_split(content, &context)?.into_bytes())
  }

  fn transform_manifest_to_mono(&self, content: &[u8]) -> RailResult<Vec<u8>> {
    let content = std::str::from_utf8(content)
      .map_err(|error| crate::error::RailError::message(format!("Cargo.toml is not UTF-8: {error}")))?;
    let context = TransformContext {
      crate_name: self.config.crate_name.clone(),
      workspace_root: self.ctx.workspace_root().to_path_buf(),
      target_has_workspace: true,
    };
    Ok(self.transform.transform_to_mono(content, &context)?.into_bytes())
  }

  fn mapped_target_parents(&self, commit: &crate::git::CommitInfo, current_head: &str) -> Vec<String> {
    let mut parents = commit
      .parent_shas
      .iter()
      .filter_map(|parent| self.mapping_store.get_mapping(parent))
      .collect::<Vec<_>>();
    if !parents.iter().any(|parent| parent == current_head) {
      parents.insert(0, current_head.to_string());
    }
    parents.dedup();
    parents
  }

  fn mapped_source_parents(&self, commit: &crate::git::CommitInfo, current_head: &str) -> Vec<String> {
    let mut parents = commit
      .parent_shas
      .iter()
      .filter_map(|parent| self.mapping_store.get_reverse_mapping(parent))
      .collect::<Vec<_>>();
    if !parents.iter().any(|parent| parent == current_head) {
      parents.insert(0, current_head.to_string());
    }
    parents.dedup();
    parents
  }

  fn map_mono_path_to_remote(&self, mono_path: &Path) -> RailResult<PathBuf> {
    if self.config.asset_paths.iter().any(|asset| asset == mono_path) {
      return Ok(mono_path.to_path_buf());
    }
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
    if self.config.asset_paths.iter().any(|asset| asset == remote_path) {
      return Ok(remote_path.to_path_buf());
    }
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
        if self.mono_path_in_scope(remote_path) {
          Ok(remote_path.to_path_buf())
        } else {
          Err(crate::error::RailError::message(format!(
            "remote path '{}' is outside combined split ownership",
            remote_path.display()
          )))
        }
      }
    }
  }

  /// Resolve conflicts for a commit using 3-way merge
  /// Returns: (conflicts, changed_files) - the changed_files are cached for reuse in apply step
  fn resolve_conflicts_for_commit(
    &self,
    remote_commit: &crate::git::CommitInfo,
    remote_git: &SystemGit,
    changed_files: &[(PathBuf, char)],
  ) -> RailResult<ConflictResolutionResult> {
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
    for (remote_path, _) in changed_files {
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
      changed_files: changed_files.to_vec(),
    })
  }

  fn check_mono_has_changes(&self) -> RailResult<bool> {
    let last_synced = self.find_last_synced_mono_commit()?;
    let new_commits =
      self
        .ctx
        .git()?
        .git()
        .get_commits_touching_paths(&self.owned_paths(), last_synced.as_deref(), "HEAD")?;

    Ok(
      new_commits
        .into_iter()
        .any(|commit| !self.mapping_store.has_mapping(&commit.sha)),
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
      .filter(|commit| !self.mapping_store.has_reverse_mapping(&commit.sha))
      .collect();

    Ok(!relevant_commits.is_empty())
  }
}

fn contains_conflict_markers(content: &[u8]) -> bool {
  content
    .split(|byte| *byte == b'\n')
    .any(|line| line.starts_with(b"<<<<<<<") || line.starts_with(b"=======") || line.starts_with(b">>>>>>>"))
}

fn remove_worktree_file(path: &Path) -> RailResult<()> {
  match std::fs::symlink_metadata(path) {
    Ok(metadata) if metadata.file_type().is_dir() => Err(crate::error::RailError::message(format!(
      "refusing to replace directory '{}' with a synced file",
      path.display()
    ))),
    Ok(_) => Ok(std::fs::remove_file(path)?),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error.into()),
  }
}

fn write_worktree_file(path: &Path, content: &[u8]) -> RailResult<()> {
  if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
    std::fs::remove_file(path)?;
  }
  std::fs::write(path, content)?;
  Ok(())
}

fn read_worktree_blob(path: &Path, fallback_mode: Option<&str>) -> RailResult<(Vec<u8>, String)> {
  let metadata = std::fs::symlink_metadata(path)?;
  if metadata.file_type().is_symlink() {
    let target = std::fs::read_link(path)?;
    #[cfg(unix)]
    let content = {
      use std::os::unix::ffi::OsStrExt as _;
      target.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let content = target.to_string_lossy().into_owned().into_bytes();
    return Ok((content, "120000".to_string()));
  }
  if !metadata.file_type().is_file() {
    return Err(crate::error::RailError::message(format!(
      "resolved sync path '{}' is not a file or symlink",
      path.display()
    )));
  }
  #[cfg(unix)]
  let executable = {
    use std::os::unix::fs::PermissionsExt as _;
    fallback_mode == Some("100755") || metadata.permissions().mode() & 0o111 != 0
  };
  #[cfg(not(unix))]
  let executable = fallback_mode == Some("100755");
  let mode = if executable { "100755" } else { "100644" };
  Ok((std::fs::read(path)?, mode.to_string()))
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> RailResult<()> {
  let bytes = serde_json::to_vec_pretty(value)
    .map_err(|error| crate::error::RailError::message(format!("failed to serialize sync receipt: {}", error)))?;
  utils::write_file_atomic(path, &bytes)
}

/// Read the crate identity from a workspace-owned conflict receipt.
pub fn conflict_receipt_crate(workspace_root: &Path, receipt_path: &Path) -> RailResult<String> {
  let path = utils::canonicalize_existing(receipt_path)?;
  let dir = utils::canonicalize_existing(&crate::workspace::cargo_rail_state_root(workspace_root).join("receipts"))?;
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
