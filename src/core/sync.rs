use crate::core::error::RailResult;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cargo::helpers;
use crate::cargo::metadata::WorkspaceMetadata;
use crate::cargo::transform::{CargoTransform, TransformContext};
use crate::core::config::{SecurityConfig, SplitMode};
use crate::core::conflict::{ConflictInfo, ConflictResolver, ConflictStrategy};
use crate::core::mapping::MappingStore;
use crate::core::security::SecurityValidator;
use crate::core::vcs::SystemGit;
use crate::ui::progress::FileProgress;

/// Configuration for sync operation
pub struct SyncConfig {
  pub crate_name: String,
  pub crate_paths: Vec<PathBuf>,
  pub mode: SplitMode,
  pub target_repo_path: PathBuf,
  pub branch: String,
  pub remote_url: String,
}

/// Result of a sync operation
pub struct SyncResult {
  pub commits_synced: usize,
  /// Direction of sync operation - useful for logging/auditing
  #[allow(dead_code)]
  pub direction: SyncDirection,
  pub conflicts: Vec<ConflictInfo>,
}

#[derive(Debug, Clone)]
pub enum SyncDirection {
  MonoToRemote,
  RemoteToMono,
  Both,
  None,
}

/// Result of conflict resolution containing both conflict info and changed files
/// Changed files are cached for reuse in the apply step to avoid redundant git calls
type ConflictResolutionResult = (Vec<ConflictInfo>, Vec<(PathBuf, char)>);

/// Bidirectional sync engine
pub struct SyncEngine {
  workspace_root: PathBuf,
  config: SyncConfig,
  mono_git: SystemGit,
  mapping_store: MappingStore,
  transform: CargoTransform,
  /// Wrapped in Arc for cheap cloning in parallel execution
  security_config: Arc<SecurityConfig>,
  security_validator: SecurityValidator,
  conflict_resolver: ConflictResolver,
  /// Track which repos we've loaded mappings from (to avoid redundant loads)
  loaded_repos: std::collections::HashSet<PathBuf>,
}

impl SyncEngine {
  pub fn new(
    workspace_root: PathBuf,
    config: SyncConfig,
    security_config: Arc<SecurityConfig>,
    conflict_strategy: ConflictStrategy,
  ) -> RailResult<Self> {
    let mono_git = SystemGit::open(&workspace_root)?;
    let mapping_store = MappingStore::new(config.crate_name.clone());
    let metadata = WorkspaceMetadata::load(&workspace_root)?;
    let transform = CargoTransform::new(metadata); // No clone needed - metadata moved into transform
    let security_validator = SecurityValidator::new((*security_config).clone());

    // Create unique temporary directory for conflict resolution (avoid conflicts in parallel tests)
    let temp_dir = std::env::temp_dir().join(format!(
      "cargo-rail-conflicts-{}-{}-{}",
      config.crate_name,
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let conflict_resolver = ConflictResolver::new(conflict_strategy, temp_dir);

    Ok(Self {
      workspace_root,
      config,
      mono_git,
      mapping_store,
      transform,
      security_config,
      security_validator,
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

  /// Check if the remote URL is a local file path
  fn is_local_remote(&self) -> bool {
    self.config.remote_url.starts_with('/')
      || self.config.remote_url.starts_with("./")
      || self.config.remote_url.starts_with("../")
  }

  /// Get the appropriate branch reference (origin/branch for remotes, just branch for local)
  fn get_branch_ref(&self) -> String {
    if self.is_local_remote() {
      self.config.branch.clone()
    } else {
      format!("origin/{}", self.config.branch)
    }
  }

  pub fn sync_to_remote(&mut self) -> RailResult<SyncResult> {
    println!("   Syncing monorepo → remote...");

    // Validate SSH key before any remote operations
    if !self.is_local_remote() {
      self.security_validator.validate_ssh_key()?;
      self.security_validator.validate_signing_key()?;
    }

    // Load mappings (cached - only loads if not already loaded)
    let workspace_root = self.workspace_root.clone();
    self.ensure_mappings_loaded(&workspace_root)?;

    // Open remote repo
    let target_repo_path = self.config.target_repo_path.clone();
    let remote_git = SystemGit::open(&target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !self.is_local_remote() {
      remote_git.fetch_from_remote("origin")?;
      self.mapping_store.fetch_notes(&target_repo_path, "origin")?;
    } else {
      println!("   Skipping fetch (local testing mode)");
    }
    // Fetch updates mapping notes, so we need to reload from target repo
    // Clear the loaded flag and reload
    self.loaded_repos.remove(&target_repo_path);
    self.ensure_mappings_loaded(&target_repo_path)?;

    // Find last synced commit in mono
    let last_synced_mono = self.find_last_synced_mono_commit()?;

    // Get new commits in mono that touch any of the crate paths (handles both single and combined modes)
    let new_commits =
      self
        .mono_git
        .get_commits_touching_paths(&self.config.crate_paths, last_synced_mono.as_deref(), "HEAD")?;

    if new_commits.is_empty() {
      println!("   No new commits to sync");
    } else {
      use crate::ui::progress::CommitProgress;

      let mut progress = CommitProgress::new(
        new_commits.len(),
        format!("Syncing {} commits to remote", new_commits.len()),
      );

      let mut synced_count = 0;
      let mut current_remote_head = remote_git.head_commit()?; // Cache HEAD, update after each commit

      for commit in &new_commits {
        // Skip if already synced
        if self.mapping_store.has_mapping(&commit.sha) {
          progress.inc();
          continue;
        }

        // Skip if this commit came from remote (check trailer)
        if commit.message.contains("Rail-Origin: remote@") {
          progress.inc();
          continue;
        }

        // Apply commit to remote
        let remote_sha = self.apply_mono_commit_to_remote(commit, &remote_git, &current_remote_head)?;

        // Record mapping
        self.mapping_store.record_mapping(&commit.sha, &remote_sha)?;
        synced_count += 1;
        current_remote_head = remote_sha.clone(); // Update cached HEAD

        progress.inc();
      }

      // Save mappings after processing commits
      self.mapping_store.save(&self.workspace_root)?;
      self.mapping_store.save(&self.config.target_repo_path)?;

      // Push to remote (skip for local paths)
      if synced_count > 0 && !self.is_local_remote() {
        remote_git.push_to_remote("origin", &self.config.branch)?;
        self.mapping_store.push_notes(&self.config.target_repo_path, "origin")?;
      }

      return Ok(SyncResult {
        commits_synced: synced_count,
        direction: SyncDirection::MonoToRemote,
        conflicts: Vec::new(),
      });
    }

    let synced_count = 0;

    // Save mappings
    self.mapping_store.save(&self.workspace_root)?;
    self.mapping_store.save(&self.config.target_repo_path)?;

    // Push to remote (skip for local paths)
    if synced_count > 0 {
      if !self.is_local_remote() {
        remote_git.push_to_remote("origin", &self.config.branch)?;
        self.mapping_store.push_notes(&self.config.target_repo_path, "origin")?;
      } else {
        println!("   Skipping push (local testing mode)");
      }
    }

    Ok(SyncResult {
      commits_synced: synced_count,
      direction: SyncDirection::MonoToRemote,
      conflicts: Vec::new(),
    })
  }

  pub fn sync_from_remote(&mut self) -> RailResult<SyncResult> {
    println!("   Syncing remote → monorepo...");

    // Validate SSH key before any remote operations
    if !self.is_local_remote() {
      self.security_validator.validate_ssh_key()?;
      self.security_validator.validate_signing_key()?;
    }

    // Check current branch - NEVER commit directly to protected branches
    let current_branch = self.mono_git.current_branch()?;
    let needs_pr_branch = self.security_config.protected_branches.contains(&current_branch);

    let pr_branch_name = if needs_pr_branch {
      let pr_branch = self.security_validator.generate_pr_branch(&self.config.crate_name);
      println!("   ⚠️  Current branch '{}' is protected", current_branch);
      println!("   📝 Creating PR branch: {}", pr_branch);

      // Create and checkout the PR branch
      self.mono_git.create_and_checkout_branch(&pr_branch)?;

      Some(pr_branch)
    } else {
      None
    };

    // Load mappings (cached - only loads if not already loaded)
    let workspace_root = self.workspace_root.clone();
    self.ensure_mappings_loaded(&workspace_root)?;

    // Open remote repo
    let target_repo_path = self.config.target_repo_path.clone();
    let remote_git = SystemGit::open(&target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !self.is_local_remote() {
      remote_git.fetch_from_remote("origin")?;
      self.mapping_store.fetch_notes(&target_repo_path, "origin")?;
    } else {
      println!("   Skipping fetch (local testing mode)");
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

    let mut conflicts = Vec::new();

    let synced_count = if new_commits.is_empty() {
      println!("   No new commits to sync");
      0
    } else {
      use crate::ui::progress::CommitProgress;

      let mut progress = CommitProgress::new(
        new_commits.len(),
        format!("Syncing {} commits from remote", new_commits.len()),
      );

      let mut count = 0;
      let mut current_mono_head = self.mono_git.head_commit()?; // Cache HEAD, update after each commit

      for commit in &new_commits {
        // Skip if this commit came from mono (check trailer)
        if commit.message.contains("Rail-Origin: mono@") {
          progress.inc();
          continue;
        }

        // Skip if already synced (O(1) reverse mapping lookup)
        if self.mapping_store.has_reverse_mapping(&commit.sha) {
          progress.inc();
          continue;
        }

        // Resolve conflicts using 3-way merge (returns conflicts + changed_files for caching)
        let (conflict_infos, changed_files) = self.resolve_conflicts_for_commit(commit, &remote_git)?;

        // Collect paths of resolved files (don't overwrite these in apply_remote_commit_to_mono)
        // Using HashSet for O(1) membership testing instead of O(n)
        let resolved_files: HashSet<PathBuf> = conflict_infos.iter().map(|c| c.file_path.clone()).collect();

        if !conflict_infos.is_empty() {
          conflicts.extend(conflict_infos);
          // Continue applying commit - files already merged by conflict resolver
        }

        // Apply commit to mono (skipping already-resolved files, reusing cached changed_files)
        let mono_sha =
          self.apply_remote_commit_to_mono(commit, &remote_git, &resolved_files, &current_mono_head, &changed_files)?;

        // Record mapping (remote -> mono)
        self.mapping_store.record_mapping(&mono_sha, &commit.sha)?;
        count += 1;
        current_mono_head = mono_sha.clone(); // Update cached HEAD

        progress.inc();
      }

      count
    };

    // Save mappings
    self.mapping_store.save(&self.workspace_root)?;

    // If we created a PR branch, push it to remote and remind user to create PR
    if let Some(ref pr_branch) = pr_branch_name {
      println!("\n   🎯 Changes synced to PR branch: {}", pr_branch);

      // Push PR branch to remote (skip for local testing)
      if !self.is_local_remote() && synced_count > 0 {
        println!("   📤 Pushing PR branch to remote...");
        self.mono_git.push_to_remote("origin", pr_branch)?;
        println!("   ✅ PR branch pushed to origin/{}", pr_branch);

        println!("\n   📝 Next step:");
        println!("      • Create a pull request on GitHub/GitLab:");
        println!("        {} → {}", pr_branch, current_branch);
        println!("      • Or visit your repository's PR creation page");
      } else if synced_count == 0 {
        println!("   ℹ️  No new commits to sync - PR branch not pushed");
        println!("   📝 To review: git diff {}..{}", current_branch, pr_branch);
      } else {
        // Local testing mode
        println!("   📝 Next steps:");
        println!(
          "      1. Review the changes: git diff {}..{}",
          current_branch, pr_branch
        );
        println!("      2. Push the branch: git push origin {}", pr_branch);
        println!(
          "      3. Create a pull request from {} to {}",
          pr_branch, current_branch
        );
      }

      println!(
        "\n   ⚠️  Protected branch '{}' was NOT modified directly",
        current_branch
      );
    }

    Ok(SyncResult {
      commits_synced: synced_count,
      direction: SyncDirection::RemoteToMono,
      conflicts,
    })
  }

  pub fn sync_bidirectional(&mut self) -> RailResult<SyncResult> {
    println!("   Detecting changes...");

    // Check both directions
    let mono_has_changes = self.check_mono_has_changes()?;
    let remote_has_changes = self.check_remote_has_changes()?;

    match (mono_has_changes, remote_has_changes) {
      (true, false) => {
        println!("   Only monorepo has changes");
        self.sync_to_remote()
      }
      (false, true) => {
        println!("   Only remote has changes");
        self.sync_from_remote()
      }
      (true, true) => {
        println!("   Both sides have changes, syncing both directions");
        let to_remote = self.sync_to_remote()?;
        let from_remote = self.sync_from_remote()?;

        Ok(SyncResult {
          commits_synced: to_remote.commits_synced + from_remote.commits_synced,
          direction: SyncDirection::Both,
          conflicts: from_remote.conflicts,
        })
      }
      (false, false) => {
        println!("   No changes on either side");
        Ok(SyncResult {
          commits_synced: 0,
          direction: SyncDirection::None,
          conflicts: Vec::new(),
        })
      }
    }
  }

  // Helper methods

  fn find_last_synced_mono_commit(&self) -> RailResult<Option<String>> {
    // Find the most recent mono commit that has a mapping
    let commits = self.mono_git.commit_history(Path::new("."), Some(100))?;

    for commit in commits {
      if self.mapping_store.has_mapping(&commit.sha) {
        return Ok(Some(commit.sha));
      }
    }

    Ok(None)
  }

  fn find_last_synced_remote_commit(&self, remote_git: &SystemGit) -> RailResult<Option<String>> {
    // Find the most recent remote commit that has a reverse mapping (O(1) lookups)
    let commits = remote_git.commit_history(Path::new("."), Some(100))?;

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
    commit: &crate::core::vcs::CommitInfo,
    remote_git: &SystemGit,
    current_remote_head: &str,
  ) -> RailResult<String> {
    // Get changed files in mono
    let changed_files = self.mono_git.get_changed_files(&commit.sha)?;

    // Filter to only files in crate path
    let crate_path = &self.config.crate_paths[0];
    let relevant_files: Vec<_> = changed_files
      .into_iter()
      .filter(|(path, _)| path.starts_with(crate_path) && !helpers::should_exclude_cargo_path(path))
      .collect();

    // Separate deletions from additions/modifications
    let (deletions, modifications): (Vec<_>, Vec<_>) =
      relevant_files.iter().partition(|(_, change_type)| *change_type == 'D');

    // Handle deletions
    for (mono_path, _) in &deletions {
      let remote_path = self.map_mono_path_to_remote(mono_path)?;
      let full_remote_path = self.config.target_repo_path.join(&remote_path);
      if full_remote_path.exists() {
        std::fs::remove_file(&full_remote_path)?;
      }
    }

    // Bulk read all files that need to be added/modified (single git call instead of N calls)
    let bulk_items: Vec<(String, PathBuf)> = modifications
      .iter()
      .map(|(path, _)| (commit.sha.clone(), path.clone()))
      .collect();

    let file_contents = if !bulk_items.is_empty() {
      self.mono_git.read_files_bulk(&bulk_items)?
    } else {
      vec![]
    };

    // Apply each file to remote
    let mut progress = if !relevant_files.is_empty() {
      Some(FileProgress::new(relevant_files.len(), "Applying files to remote"))
    } else {
      None
    };

    for (idx, (mono_path, _)) in modifications.iter().enumerate() {
      let content = &file_contents[idx];
      let remote_path = self.map_mono_path_to_remote(mono_path)?;
      let full_remote_path = self.config.target_repo_path.join(&remote_path);

      // Create parent directories
      if let Some(parent) = full_remote_path.parent() {
        std::fs::create_dir_all(parent)?;
      }

      // Write file first, then transform manifest if applicable
      std::fs::write(&full_remote_path, content)?;

      // Transform Cargo.toml manifest
      if mono_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content = std::fs::read_to_string(&full_remote_path)?;
        let context = TransformContext {
          crate_name: self.config.crate_name.clone(),
          workspace_root: self.workspace_root.clone(),
        };
        let transformed = self.transform.transform_to_split(&content, &context)?;
        std::fs::write(&full_remote_path, transformed)?;
      }

      if let Some(ref mut p) = progress {
        p.inc();
      }
    }

    // Update progress for deletions
    if let Some(ref mut p) = progress {
      for _ in 0..deletions.len() {
        p.inc();
      }
    }

    // Security checks before creating commit
    // Note: Branch protection doesn't apply to remote repos - only to monorepo
    if self.security_config.require_signed_commits {
      self.security_validator.validate_signing_key()?;
    }

    // Create commit with trailer
    let message = format!("{}\n\nRail-Origin: mono@{}", commit.message.trim(), commit.sha);

    let parent_shas = vec![current_remote_head.to_string()];

    let new_commit_sha = remote_git.create_commit_with_metadata(
      &message,
      &commit.author,
      &commit.author_email,
      commit.timestamp,
      &parent_shas,
    )?;

    // Verify commit signature if required
    if self.security_config.require_signed_commits {
      self
        .security_validator
        .verify_commit_signature(&self.config.target_repo_path, &new_commit_sha)?;
    }

    Ok(new_commit_sha)
  }

  fn apply_remote_commit_to_mono(
    &self,
    commit: &crate::core::vcs::CommitInfo,
    remote_git: &SystemGit,
    resolved_files: &HashSet<PathBuf>,
    current_mono_head: &str,
    changed_files: &[(PathBuf, char)], // Pre-fetched from resolve_conflicts to avoid duplicate subprocess call
  ) -> RailResult<String> {
    // Use pre-fetched changed_files (already retrieved in resolve_conflicts_for_commit)

    // Apply each file to mono
    let mut progress = if !changed_files.is_empty() {
      Some(FileProgress::new(changed_files.len(), "Applying files to mono"))
    } else {
      None
    };

    // Filter and separate files by operation type
    let relevant_files: Vec<_> = changed_files
      .iter()
      .filter_map(|(remote_path, change_type)| {
        let mono_path = self.map_remote_path_to_mono(remote_path).ok()?;

        // Skip files excluded by Cargo helper (target, etc.)
        if helpers::should_exclude_cargo_path(&mono_path) {
          return None;
        }

        // Skip files that were already resolved by conflict resolution (O(1) HashSet lookup)
        if resolved_files.contains(&mono_path) {
          println!("      Skipping {} (already resolved)", mono_path.display());
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
      let full_mono_path = self.workspace_root.join(mono_path);
      if full_mono_path.exists() {
        std::fs::remove_file(&full_mono_path)?;
      }
    }

    // Bulk read all files that need to be added/modified (single git call instead of N calls)
    let bulk_items: Vec<(String, PathBuf)> = modifications
      .iter()
      .map(|(remote_path, _, _)| (commit.sha.clone(), (*remote_path).clone()))
      .collect();

    let file_contents = if !bulk_items.is_empty() {
      remote_git.read_files_bulk(&bulk_items)?
    } else {
      vec![]
    };

    // Apply files to mono
    for (idx, (remote_path, mono_path, _)) in modifications.iter().enumerate() {
      let content = &file_contents[idx];
      let full_mono_path = self.workspace_root.join(mono_path);

      // Create parent directories
      if let Some(parent) = full_mono_path.parent() {
        std::fs::create_dir_all(parent)?;
      }

      // Write file first, then transform manifest if applicable
      std::fs::write(&full_mono_path, content)?;

      // Transform Cargo.toml manifest
      if remote_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
        let content = std::fs::read_to_string(&full_mono_path)?;
        let context = TransformContext {
          crate_name: self.config.crate_name.clone(),
          workspace_root: self.workspace_root.clone(),
        };
        let transformed = self.transform.transform_to_mono(&content, &context)?;
        std::fs::write(&full_mono_path, transformed)?;
      }

      if let Some(ref mut p) = progress {
        p.inc();
      }
    }

    // Update progress for deletions
    if let Some(ref mut p) = progress {
      for _ in 0..deletions.len() {
        p.inc();
      }
    }

    // Security checks before creating commit
    // Note: Branch protection applies to the monorepo's current branch
    if self.security_config.require_signed_commits {
      self.security_validator.validate_signing_key()?;
    }

    // Create commit with trailer
    let message = format!("{}\n\nRail-Origin: remote@{}", commit.message.trim(), commit.sha);

    let parent_shas = vec![current_mono_head.to_string()];

    let new_commit_sha = self.mono_git.create_commit_with_metadata(
      &message,
      &commit.author,
      &commit.author_email,
      commit.timestamp,
      &parent_shas,
    )?;

    // Verify commit signature if required
    if self.security_config.require_signed_commits {
      self
        .security_validator
        .verify_commit_signature(&self.workspace_root, &new_commit_sha)?;
    }

    Ok(new_commit_sha)
  }

  fn map_mono_path_to_remote(&self, mono_path: &Path) -> RailResult<PathBuf> {
    let crate_path = &self.config.crate_paths[0];

    match self.config.mode {
      SplitMode::Single => {
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
    let crate_path = &self.config.crate_paths[0];

    match self.config.mode {
      SplitMode::Single => {
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
    remote_commit: &crate::core::vcs::CommitInfo,
    remote_git: &SystemGit,
  ) -> RailResult<ConflictResolutionResult> {
    let mut conflicts = Vec::new();

    // Get files changed in this remote commit
    let changed_files = remote_git.get_changed_files(&remote_commit.sha)?;

    // Show progress bar for conflict resolution if many files
    let mut progress = if changed_files.len() > 5 {
      Some(FileProgress::new(
        changed_files.len(),
        format!("Resolving conflicts for {} files", changed_files.len()),
      ))
    } else {
      None
    };

    // Find the base commit (common ancestor)
    let last_synced = self.find_last_synced_mono_commit()?;

    // Phase 1: Build cache of all files modified in mono since last sync
    // Single git call instead of N calls (one per remote file)
    let mono_changed_paths: std::collections::HashSet<PathBuf> = if let Some(ref last) = last_synced {
      self
        .mono_git
        .get_changed_files_between(last, "HEAD")?
        .into_iter()
        .map(|(path, _)| path)
        .collect()
    } else {
      std::collections::HashSet::new()
    };

    // Phase 2: Identify conflicting files (files modified on both sides)
    let mut conflicting_files = Vec::new();
    for (remote_path, _) in &changed_files {
      let mono_path = self.map_remote_path_to_mono(remote_path)?;
      let full_mono_path = self.workspace_root.join(&mono_path);

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

    // Phase 3: Bulk read base and incoming versions for all conflicting files
    let base_items: Vec<(String, PathBuf)> = conflicting_files
      .iter()
      .filter_map(|(_, mono_path, _)| last_synced.as_ref().map(|sha| (sha.clone(), mono_path.clone())))
      .collect();

    let incoming_items: Vec<(String, PathBuf)> = conflicting_files
      .iter()
      .map(|(remote_path, _, _)| (remote_commit.sha.clone(), remote_path.clone()))
      .collect();

    let base_contents = if !base_items.is_empty() {
      self.mono_git.read_files_bulk(&base_items)?
    } else {
      vec![Vec::new(); conflicting_files.len()]
    };

    let incoming_contents = if !incoming_items.is_empty() {
      remote_git.read_files_bulk(&incoming_items)?
    } else {
      vec![]
    };

    // Phase 4: Resolve conflicts with bulk-loaded content
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
        Ok(crate::core::conflict::MergeResult::Success) => {
          // Merged successfully - add to resolved files to prevent overwriting
          println!("      ✅ Auto-merged {}", mono_path.display());
          conflicts.push(ConflictInfo {
            file_path: mono_path.clone(),
            message: format!(
              "Auto-merged {} using {:?} strategy",
              mono_path.display(),
              self.conflict_resolver.strategy()
            ),
            resolved: true,
          });
        }
        Ok(crate::core::conflict::MergeResult::Conflicts(_paths)) => {
          // Check if using auto-resolve strategy
          let is_auto_resolved = matches!(
            self.conflict_resolver.strategy(),
            ConflictStrategy::Ours | ConflictStrategy::Theirs | ConflictStrategy::Union
          );
          conflicts.push(ConflictInfo {
            file_path: mono_path.clone(),
            message: format!("Conflict in {}", mono_path.display()),
            resolved: is_auto_resolved,
          });
        }
        Ok(crate::core::conflict::MergeResult::Failed(msg)) => {
          conflicts.push(ConflictInfo {
            file_path: mono_path.clone(),
            message: format!("Merge failed: {}", msg),
            resolved: false,
          });
        }
        Err(e) => {
          conflicts.push(ConflictInfo {
            file_path: mono_path.clone(),
            message: format!("Merge error: {}", e),
            resolved: false,
          });
        }
      }

      if let Some(ref mut p) = progress {
        p.inc();
      }
    }

    Ok((conflicts, changed_files))
  }

  /// Legacy method - kept for compatibility but now uses resolve_conflicts_for_commit
  /// TODO: Remove in favor of resolve_conflicts_for_commit once all call sites updated
  #[allow(dead_code)]
  fn check_for_conflicts(
    &self,
    remote_commit: &crate::core::vcs::CommitInfo,
    remote_git: &SystemGit,
  ) -> RailResult<bool> {
    let (conflicts, _) = self.resolve_conflicts_for_commit(remote_commit, remote_git)?;
    Ok(!conflicts.is_empty())
  }

  fn check_mono_has_changes(&self) -> RailResult<bool> {
    let last_synced = self.find_last_synced_mono_commit()?;
    let crate_path = &self.config.crate_paths[0];

    let new_commits = self
      .mono_git
      .get_commits_touching_path(crate_path, last_synced.as_deref(), "HEAD")?;

    // Filter out commits from remote
    let relevant_commits: Vec<_> = new_commits
      .into_iter()
      .filter(|c| !c.message.contains("Rail-Origin: remote@"))
      .collect();

    Ok(!relevant_commits.is_empty())
  }

  fn check_remote_has_changes(&self) -> RailResult<bool> {
    let remote_git = SystemGit::open(&self.config.target_repo_path)?;

    // Fetch from remote (skip for local paths)
    if !self.is_local_remote() {
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
