#![allow(dead_code)]

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::core::config::SplitMode;
use crate::core::mapping::MappingStore;
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

#[derive(Debug, Clone)]
pub struct ConflictInfo {
  pub file_path: PathBuf,
  pub message: String,
}

/// Bidirectional sync engine
pub struct SyncEngine {
  workspace_root: PathBuf,
  config: SyncConfig,
  mono_git: GitBackend,
  mapping_store: MappingStore,
  transformer: Box<dyn Transform>,
}

impl SyncEngine {
  pub fn new(workspace_root: PathBuf, config: SyncConfig, transformer: Box<dyn Transform>) -> Result<Self> {
    let mono_git = GitBackend::open(&workspace_root)?;
    let mapping_store = MappingStore::new(config.crate_name.clone());

    Ok(Self {
      workspace_root,
      config,
      mono_git,
      mapping_store,
      transformer,
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

      // Check for conflicts
      let has_conflict = self.check_for_conflicts(commit, &remote_git)?;
      if has_conflict {
        conflicts.push(ConflictInfo {
          file_path: PathBuf::from(&commit.sha),
          message: format!("Commit {} conflicts with local changes", &commit.sha[..7]),
        });
        println!("   ⚠️  Conflict detected, skipping");
        continue;
      }

      // Apply commit to mono
      let mono_sha = self.apply_remote_commit_to_mono(commit, &remote_git)?;

      // Record mapping (remote -> mono)
      self.mapping_store.record_mapping(&mono_sha, &commit.sha)?;
      synced_count += 1;
    }

    // Save mappings
    self.mapping_store.save(&self.workspace_root)?;

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
  ) -> Result<String> {
    // Get changed files in remote
    let changed_files = remote_git.get_changed_files(&commit.sha)?;

    // Apply each file to mono
    for (remote_path, change_type) in changed_files {
      let mono_path = self.map_remote_path_to_mono(&remote_path)?;

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

  fn check_for_conflicts(&self, remote_commit: &crate::core::vcs::CommitInfo, remote_git: &GitBackend) -> Result<bool> {
    // Get files changed in this remote commit
    let changed_files = remote_git.get_changed_files(&remote_commit.sha)?;

    // Map to mono paths
    for (remote_path, _) in changed_files {
      let mono_path = self.map_remote_path_to_mono(&remote_path)?;
      let full_mono_path = self.workspace_root.join(&mono_path);

      // Check if file was modified in mono since last sync
      if full_mono_path.exists() {
        // Simple check: if file exists and was modified, it might conflict
        // TODO: More sophisticated conflict detection
        let last_synced = self.find_last_synced_mono_commit()?;
        if let Some(last) = last_synced {
          let mono_commits = self
            .mono_git
            .get_commits_touching_path(&mono_path, Some(&last), "HEAD")?;

          if !mono_commits.is_empty() {
            return Ok(true); // Conflict detected
          }
        }
      }
    }

    Ok(false)
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
