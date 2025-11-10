#![allow(dead_code)]

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::core::config::{SecurityConfig, SplitMode};
use crate::core::conflict::{ConflictInfo, ConflictResolver, ConflictStrategy};
use crate::core::mapping::MappingStore;
use crate::core::security::SecurityValidator;
use crate::core::transform::{Transform, TransformContext};
use crate::core::vcs::Vcs;
use crate::core::vcs::git::GitBackend;

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

/// Bidirectional sync engine
pub struct SyncEngine {
  workspace_root: PathBuf,
  config: SyncConfig,
  mono_git: GitBackend,
  mapping_store: MappingStore,
  transformer: Box<dyn Transform>,
  security_config: SecurityConfig,
  security_validator: SecurityValidator,
  conflict_resolver: ConflictResolver,
}

impl SyncEngine {
  pub fn new(
    workspace_root: PathBuf,
    config: SyncConfig,
    transformer: Box<dyn Transform>,
    security_config: SecurityConfig,
    conflict_strategy: ConflictStrategy,
  ) -> Result<Self> {
    let mono_git = GitBackend::open(&workspace_root)?;
    let mapping_store = MappingStore::new(config.crate_name.clone());
    let security_validator = SecurityValidator::new(security_config.clone());

    // Create unique temporary directory for conflict resolution (avoid conflicts in parallel tests)
    let temp_dir = std::env::temp_dir().join(format!(
      "cargo-rail-conflicts-{}-{}-{}",
      config.crate_name,
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let conflict_resolver = ConflictResolver::new(conflict_strategy, temp_dir);

    Ok(Self {
      workspace_root,
      config,
      mono_git,
      mapping_store,
      transformer,
      security_config,
      security_validator,
      conflict_resolver,
    })
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

  pub fn sync_to_remote(&mut self) -> Result<SyncResult> {
    println!("   Syncing monorepo → remote...");

    // Validate SSH key before any remote operations
    if !self.is_local_remote() {
      self.security_validator.validate_ssh_key()?;
      self.security_validator.validate_signing_key()?;
    }

    // Load mappings
    self.mapping_store.load(&self.workspace_root)?;

    // Open remote repo
    let remote_git = GitBackend::open(&self.config.target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !self.is_local_remote() {
      remote_git.fetch_from_remote("origin")?;
      self
        .mapping_store
        .fetch_notes(&self.config.target_repo_path, "origin")?;
    } else {
      println!("   Skipping fetch (local testing mode)");
    }
    self.mapping_store.load(&self.config.target_repo_path)?;

    // Find last synced commit in mono
    let last_synced_mono = self.find_last_synced_mono_commit()?;

    // Get new commits in mono that touch the crate path
    let crate_path = &self.config.crate_paths[0]; // TODO: Handle combined mode
    let new_commits = self
      .mono_git
      .get_commits_touching_path(crate_path, last_synced_mono.as_deref(), "HEAD")?;

    println!("   Found {} new commits in monorepo", new_commits.len());

    let mut synced_count = 0;

    for commit in &new_commits {
      // Skip if already synced
      if self.mapping_store.has_mapping(&commit.sha) {
        println!("   Skipping {} (already synced)", &commit.sha[..7]);
        continue;
      }

      // Skip if this commit came from remote (check trailer)
      if commit.message.contains("Rail-Origin: remote@") {
        println!("   Skipping {} (from remote)", &commit.sha[..7]);
        continue;
      }

      println!("   Syncing {} - {}", &commit.sha[..7], commit.summary());

      // Apply commit to remote
      let remote_sha = self.apply_mono_commit_to_remote(commit, &remote_git)?;

      // Record mapping
      self.mapping_store.record_mapping(&commit.sha, &remote_sha)?;
      synced_count += 1;
    }

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

  pub fn sync_from_remote(&mut self) -> Result<SyncResult> {
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

    // Load mappings
    self.mapping_store.load(&self.workspace_root)?;

    // Open remote repo
    let remote_git = GitBackend::open(&self.config.target_repo_path)?;

    // Fetch latest from remote (skip for local paths)
    if !self.is_local_remote() {
      remote_git.fetch_from_remote("origin")?;
      self
        .mapping_store
        .fetch_notes(&self.config.target_repo_path, "origin")?;
    } else {
      println!("   Skipping fetch (local testing mode)");
    }
    self.mapping_store.load(&self.config.target_repo_path)?;

    // Find last synced commit in remote
    let last_synced_remote = self.find_last_synced_remote_commit(&remote_git)?;

    // Get new commits in remote
    let branch_ref = self.get_branch_ref();
    let new_commits = if let Some(ref last) = last_synced_remote {
      remote_git.get_commits_touching_path(Path::new("."), Some(last), &branch_ref)?
    } else {
      remote_git.get_commits_touching_path(Path::new("."), None, &branch_ref)?
    };

    println!("   Found {} new commits in remote", new_commits.len());

    let mut synced_count = 0;
    let mut conflicts = Vec::new();

    for commit in &new_commits {
      // Skip if this commit came from mono (check trailer)
      if commit.message.contains("Rail-Origin: mono@") {
        println!("   Skipping {} (from mono)", &commit.sha[..7]);
        continue;
      }

      // Skip if already synced (reverse mapping)
      if self.mapping_store.all_mappings().values().any(|v| v == &commit.sha) {
        println!("   Skipping {} (already synced)", &commit.sha[..7]);
        continue;
      }

      println!("   Syncing {} - {}", &commit.sha[..7], commit.summary());

      // Resolve conflicts using 3-way merge
      let conflict_infos = self.resolve_conflicts_for_commit(commit, &remote_git)?;

      // Collect paths of resolved files (don't overwrite these in apply_remote_commit_to_mono)
      let resolved_files: Vec<PathBuf> = conflict_infos.iter().map(|c| c.file_path.clone()).collect();

      if !conflict_infos.is_empty() {
        for info in &conflict_infos {
          if info.resolved {
            println!("      ✓ Auto-resolved: {}", info.message);
          } else {
            println!("      ⚠️  {}", info.message);
          }
        }
        conflicts.extend(conflict_infos);
        // Continue applying commit - files already merged by conflict resolver
      }

      // Apply commit to mono (skipping already-resolved files)
      let mono_sha = self.apply_remote_commit_to_mono(commit, &remote_git, &resolved_files)?;

      // Record mapping (remote -> mono)
      self.mapping_store.record_mapping(&mono_sha, &commit.sha)?;
      synced_count += 1;
    }

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

  pub fn sync_bidirectional(&mut self) -> Result<SyncResult> {
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

  fn find_last_synced_mono_commit(&self) -> Result<Option<String>> {
    // Find the most recent mono commit that has a mapping
    let commits = self.mono_git.commit_history(Path::new("."), Some(100))?;

    for commit in commits {
      if self.mapping_store.has_mapping(&commit.sha) {
        return Ok(Some(commit.sha));
      }
    }

    Ok(None)
  }

  fn find_last_synced_remote_commit(&self, remote_git: &GitBackend) -> Result<Option<String>> {
    // Find the most recent remote commit that has a reverse mapping
    let commits = remote_git.commit_history(Path::new("."), Some(100))?;
    let all_mappings = self.mapping_store.all_mappings();

    for commit in commits {
      // Check if any mapping points to this remote commit
      if all_mappings.values().any(|v| v == &commit.sha) {
        return Ok(Some(commit.sha));
      }
    }

    Ok(None)
  }

  fn apply_mono_commit_to_remote(
    &self,
    commit: &crate::core::vcs::CommitInfo,
    remote_git: &GitBackend,
  ) -> Result<String> {
    // Get changed files in mono
    let changed_files = self.mono_git.get_changed_files(&commit.sha)?;

    // Filter to only files in crate path
    let crate_path = &self.config.crate_paths[0];
    let relevant_files: Vec<_> = changed_files
      .into_iter()
      .filter(|(path, _)| path.starts_with(crate_path))
      .collect();

    // Apply each file to remote
    for (mono_path, change_type) in relevant_files {
      let remote_path = self.map_mono_path_to_remote(&mono_path)?;

      match change_type {
        'D' => {
          // Delete file in remote
          let full_remote_path = self.config.target_repo_path.join(&remote_path);
          if full_remote_path.exists() {
            std::fs::remove_file(&full_remote_path)?;
          }
        }
        _ => {
          // Add or modify file
          if let Some(content) = self.mono_git.get_file_at_commit(&commit.sha, &mono_path)? {
            let full_remote_path = self.config.target_repo_path.join(&remote_path);

            // Create parent directories
            if let Some(parent) = full_remote_path.parent() {
              std::fs::create_dir_all(parent)?;
            }

            // Transform if Cargo.toml
            if mono_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
              let content_str = String::from_utf8(content)?;
              let transformed = self.transformer.transform_to_split(
                &content_str,
                &TransformContext {
                  crate_name: self.config.crate_name.clone(),
                  workspace_root: self.workspace_root.clone(),
                },
              )?;
              std::fs::write(&full_remote_path, transformed)?;
            } else {
              std::fs::write(&full_remote_path, content)?;
            }
          }
        }
      }
    }

    // Create commit with trailer
    let message = format!("{}\n\nRail-Origin: mono@{}", commit.message.trim(), commit.sha);

    let parent_shas = vec![remote_git.head_commit()?];

    remote_git.create_commit_with_metadata(
      &message,
      &commit.author,
      &commit.author_email,
      commit.timestamp,
      &parent_shas,
    )
  }

  fn apply_remote_commit_to_mono(
    &self,
    commit: &crate::core::vcs::CommitInfo,
    remote_git: &GitBackend,
    resolved_files: &[PathBuf],
  ) -> Result<String> {
    // Get changed files in remote
    let changed_files = remote_git.get_changed_files(&commit.sha)?;

    // Apply each file to mono
    for (remote_path, change_type) in changed_files {
      let mono_path = self.map_remote_path_to_mono(&remote_path)?;

      // Skip files that were already resolved by conflict resolution
      if resolved_files.iter().any(|p| p == &mono_path) {
        println!("      Skipping {} (already resolved)", mono_path.display());
        continue;
      }

      match change_type {
        'D' => {
          // Delete file in mono
          let full_mono_path = self.workspace_root.join(&mono_path);
          if full_mono_path.exists() {
            std::fs::remove_file(&full_mono_path)?;
          }
        }
        _ => {
          // Add or modify file
          if let Some(content) = remote_git.get_file_at_commit(&commit.sha, &remote_path)? {
            let full_mono_path = self.workspace_root.join(&mono_path);

            // Create parent directories
            if let Some(parent) = full_mono_path.parent() {
              std::fs::create_dir_all(parent)?;
            }

            // Transform if Cargo.toml
            if remote_path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
              let content_str = String::from_utf8(content)?;
              let transformed = self.transformer.transform_to_mono(
                &content_str,
                &TransformContext {
                  crate_name: self.config.crate_name.clone(),
                  workspace_root: self.workspace_root.clone(),
                },
              )?;
              std::fs::write(&full_mono_path, transformed)?;
            } else {
              std::fs::write(&full_mono_path, content)?;
            }
          }
        }
      }
    }

    // Create commit with trailer
    let message = format!("{}\n\nRail-Origin: remote@{}", commit.message.trim(), commit.sha);

    let parent_shas = vec![self.mono_git.head_commit()?];

    self.mono_git.create_commit_with_metadata(
      &message,
      &commit.author,
      &commit.author_email,
      commit.timestamp,
      &parent_shas,
    )
  }

  fn map_mono_path_to_remote(&self, mono_path: &Path) -> Result<PathBuf> {
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

  fn map_remote_path_to_mono(&self, remote_path: &Path) -> Result<PathBuf> {
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
  /// Returns: Vec of conflict infos (empty if no conflicts or all resolved)
  fn resolve_conflicts_for_commit(
    &self,
    remote_commit: &crate::core::vcs::CommitInfo,
    remote_git: &GitBackend,
  ) -> Result<Vec<ConflictInfo>> {
    let mut conflicts = Vec::new();

    // Get files changed in this remote commit
    let changed_files = remote_git.get_changed_files(&remote_commit.sha)?;

    // Find the base commit (common ancestor)
    let last_synced = self.find_last_synced_mono_commit()?;

    for (remote_path, _) in changed_files {
      let mono_path = self.map_remote_path_to_mono(&remote_path)?;
      let full_mono_path = self.workspace_root.join(&mono_path);

      // Skip if file doesn't exist in monorepo (new file, no conflict)
      if !full_mono_path.exists() {
        continue;
      }

      // Check if file was modified in mono since last sync
      let mono_modified = if let Some(ref last) = last_synced {
        let mono_commits = self
          .mono_git
          .get_commits_touching_path(&mono_path, Some(last), "HEAD")?;
        !mono_commits.is_empty()
      } else {
        false
      };

      // If not modified in mono, no conflict - will be cleanly applied
      if !mono_modified {
        continue;
      }

      // Both sides modified - need 3-way merge
      // Get base content (last synced version)
      let base_content = if let Some(ref last_sha) = last_synced {
        match self.mono_git.get_file_at_commit(last_sha, &mono_path) {
          Ok(Some(content)) => content,
          Ok(None) | Err(_) => Vec::new(), // File didn't exist at base
        }
      } else {
        Vec::new()
      };

      // Get incoming content (from remote)
      let incoming_content = match remote_git.get_file_at_commit(&remote_commit.sha, &remote_path)? {
        Some(content) => content,
        None => continue, // File was deleted in remote, skip
      };

      // Perform 3-way merge
      match self
        .conflict_resolver
        .resolve_file(&full_mono_path, &base_content, &incoming_content)
      {
        Ok(crate::core::conflict::MergeResult::Success) => {
          // Merged successfully - add to resolved files to prevent overwriting
          println!("      ✅ Auto-merged {}", mono_path.display());
          conflicts.push(ConflictInfo {
            file_path: mono_path.clone(),
            message: format!("Auto-merged {} using {:?} strategy", mono_path.display(), self.conflict_resolver.strategy()),
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
    }

    Ok(conflicts)
  }

  /// Legacy method - kept for compatibility but now uses resolve_conflicts_for_commit
  fn check_for_conflicts(&self, remote_commit: &crate::core::vcs::CommitInfo, remote_git: &GitBackend) -> Result<bool> {
    let conflicts = self.resolve_conflicts_for_commit(remote_commit, remote_git)?;
    Ok(!conflicts.is_empty())
  }

  fn check_mono_has_changes(&self) -> Result<bool> {
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

  fn check_remote_has_changes(&self) -> Result<bool> {
    let remote_git = GitBackend::open(&self.config.target_repo_path)?;

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
