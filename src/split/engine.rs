//! Split engine for deterministic crate extraction.
//!
//! Rebuilds crate history into a target repository while preserving stable commit
//! metadata and applying manifest transformations for split modes.

use crate::cargo::{CargoTransform, TransformContext};
use crate::config::{SplitMode, WorkspaceMode};
use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::git::git_cmd_for_path;
use crate::git::mappings::{HistorySide, MappingStore, OriginContext, append_origin_trailers, repository_identity};
use crate::git::{CommitInfo, CommitMetadata, SystemGit};
use crate::progress;
use crate::split::SplitPathCapabilities;
use crate::utils;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// One release boundary intersecting a split's owned Cargo members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseBoundary {
    /// Release version-group name.
    pub name: String,
    /// Complete, sorted membership of that boundary.
    pub members: Vec<String>,
}

/// Snapshot-derived ownership used by split, sync, plans, and Git trailers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOwnership {
    /// Authoritative workspace snapshot that resolved this ownership.
    pub snapshot_id: String,
    /// Cargo members whose roots are owned by the split.
    pub members: Vec<String>,
    /// Workspace-member dependencies reachable from the owned members.
    pub dependency_closure: Vec<String>,
    /// Complete release boundaries intersecting the owned members.
    pub release_boundaries: Vec<ReleaseBoundary>,
}

/// Runtime parameters for a split operation
///
/// Distinct from `config::SplitConfig` which is the deserialized config schema.
/// This struct holds computed/resolved values needed to execute the split.
#[derive(Debug)]
pub struct SplitParams {
    /// Name of the crate being split
    pub crate_name: String,
    /// Paths to crate directories in monorepo
    pub crate_paths: Vec<PathBuf>,
    /// Explicit non-Cargo assets resolved from the same workspace snapshot.
    pub asset_paths: Vec<PathBuf>,
    /// Snapshot-derived ownership and dependency/release evidence.
    pub ownership: SplitOwnership,
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
    /// Validated source/target path authority for all filesystem mutations.
    pub path_capabilities: SplitPathCapabilities,
}

/// Pre-fetched exact Git tree entries for a commit.
type PrefetchedFiles = Vec<crate::git::ops::GitTreeEntry>;

struct PrefetchedWindow {
    entries: FxHashMap<String, PrefetchedFiles>,
    blobs: FxHashMap<String, Vec<u8>>,
}

/// Maximum number of commits to prefetch at once
/// This bounds memory usage to O(window_size × avg_commit_size) instead of O(total_commits × avg_commit_size)
/// For a typical crate with ~1-2MB of files, 50 commits uses ~50-100MB max
const PREFETCH_WINDOW_SIZE: usize = 50;

/// Parameters for recreating a commit in the target repository
struct RecreateCommitParams<'a> {
    commit: &'a CommitInfo,
    source_paths: &'a [PathBuf],
    crate_paths: &'a [PathBuf],
    target_repo_path: &'a Path,
    crate_name: &'a str,
    mode: &'a SplitMode,
    workspace_mode: &'a WorkspaceMode,
    mapping_store: &'a MappingStore,
    origin: &'a OriginContext,
    last_recreated_sha: Option<&'a str>,
    /// Pre-fetched files (if available from parallel prefetch)
    prefetched_files: Option<&'a PrefetchedFiles>,
    /// Pre-fetched transform inputs keyed by blob object ID.
    prefetched_blobs: &'a FxHashMap<String, Vec<u8>>,
    path_capabilities: &'a SplitPathCapabilities,
}

/// Parameters for creating a git commit
struct CommitParams<'a> {
    repo_path: &'a Path,
    tree_sha: &'a str,
    message: &'a str,
    metadata: &'a CommitMetadata,
    parent_shas: &'a [String],
    expected_head: Option<&'a str>,
}

/// Split engine - extracts crates with full history
///
/// Deterministic git splitting: same input = same commit SHAs
/// Uses WorkspaceContext for git and cargo operations - no duplicate loads.
#[derive(Debug)]
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

    /// Return whether committed source history lacks target mapping evidence.
    pub fn has_pending_changes(ctx: &WorkspaceContext, config: &SplitParams) -> RailResult<bool> {
        if !config.target_repo_path.join(".git").exists() {
            return Ok(true);
        }
        config.path_capabilities.validate_target_repository()?;
        let target_git = SystemGit::open(&config.target_repo_path)?;
        let target_identity = repository_identity(&config.target_repo_path)?;
        let origin = OriginContext::discover(ctx.workspace_root(), &config.crate_name, &config.ownership.snapshot_id)?;
        let mut mappings = MappingStore::new(config.crate_name.clone());
        mappings.load_history(ctx.workspace_root(), HistorySide::Source, &target_identity)?;
        mappings.load_history(
            &config.target_repo_path,
            HistorySide::Target,
            origin.source_repository(),
        )?;
        mappings.load_legacy_notes(ctx.workspace_root())?;
        mappings.load_legacy_notes(&config.target_repo_path)?;
        if target_git.head_commit().is_ok() && mappings.count() == 0 {
            return Err(RailError::with_help(
                "existing split target has no cargo-rail origin evidence",
                "choose an empty target or migrate the target's legacy trailers/notes before splitting",
            ));
        }

        let mut owned_paths = config.crate_paths.clone();
        owned_paths.extend(config.asset_paths.iter().cloned());
        owned_paths.sort();
        owned_paths.dedup();
        let commits = ctx
            .git()?
            .git()
            .get_commits_touching_paths(&owned_paths, None, "HEAD")?;
        if commits.is_empty() {
            return Err(RailError::with_help(
                "split ownership has no committed Git history",
                "commit the Cargo members and explicit assets before splitting",
            ));
        }
        Ok(commits.iter().any(|commit| !mappings.has_mapping(&commit.sha)))
    }

    /// Walk commit history and filter commits that touch the given paths
    /// Returns commits in chronological order (oldest first)
    fn walk_filtered_history(&self, paths: &[PathBuf]) -> RailResult<Vec<CommitInfo>> {
        progress!("   Walking commit history to find commits touching crate...");

        // Use one batched Git command for all paths.
        let filtered_commits = self.ctx.git()?.git().get_commits_touching_paths(paths, None, "HEAD")?;

        progress!(
            "   Found {} total commits that touch the crate paths",
            filtered_commits.len()
        );

        Ok(filtered_commits)
    }

    /// Prefetch files for multiple commits in parallel.
    ///
    /// Returns a map from commit SHA to its prefetched files.
    /// Accepts references to avoid cloning CommitInfo structs.
    fn prefetch_commit_files(&self, commits: &[&CommitInfo], crate_paths: &[PathBuf]) -> RailResult<PrefetchedWindow> {
        let git_state = self.ctx.git()?;
        let git = git_state.git();
        let entries = commits
            .par_iter()
            .map(|commit| {
                let mut all_files = Vec::with_capacity(32);
                for crate_path in crate_paths {
                    all_files.extend(git.collect_tree_entries(&commit.sha, crate_path)?);
                }
                Ok((commit.sha.clone(), all_files))
            })
            .collect::<RailResult<FxHashMap<_, _>>>()?;
        let object_ids = entries
            .values()
            .flat_map(|files| files.iter())
            .filter(|entry| entry.path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")))
            .map(|entry| entry.object_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let requests = object_ids.iter().map(String::as_str).collect::<Vec<_>>();
        let contents = git.read_blobs_bulk(&requests)?;
        drop(requests);
        let blobs = object_ids.into_iter().zip(contents).collect();
        Ok(PrefetchedWindow { entries, blobs })
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
            let mut files = Vec::with_capacity(params.source_paths.len() * 32);
            for crate_path in params.source_paths {
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
                let content = if let Some(content) = params.prefetched_blobs.get(&entry.object_id) {
                    std::borrow::Cow::Borrowed(content.as_slice())
                } else {
                    let content = self
                        .ctx
                        .git()?
                        .git()
                        .read_blobs_bulk(&[entry.object_id.as_str()])?
                        .into_iter()
                        .next()
                        .ok_or_else(|| {
                            RailError::message(format!("manifest '{}' has no blob", entry.path.display()))
                        })?;
                    std::borrow::Cow::Owned(content)
                };
                let content = std::str::from_utf8(&content).map_err(|_| {
                    RailError::message(format!("manifest '{}' is not valid UTF-8", entry.path.display()))
                })?;
                let target_has_workspace =
                    *params.mode == SplitMode::Combined && *params.workspace_mode == WorkspaceMode::Workspace;
                let context = TransformContext {
                    crate_name: params.crate_name.to_string(),
                    workspace_root: self.ctx.workspace_root().to_path_buf(),
                    target_has_workspace,
                };
                let transformed = self.transform.transform_to_split(content, &context)?;
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
            .filter_map(|parent_sha| params.mapping_store.get_mapping(parent_sha))
            .collect();

        // Keep every ordinary target commit reachable, including provenance and
        // migration commits that do not define a one-to-one source mapping.
        if let Some(last) = params.last_recreated_sha
            && !mapped_parents.iter().any(|parent| parent == last)
        {
            mapped_parents.insert(0, last.to_string());
        }

        params.path_capabilities.validate_target_repository()?;
        let metadata = params.commit.metadata();
        let message = append_origin_trailers(&params.commit.message, &[params.origin.trailer(&params.commit.sha)?]);
        let sha = self.create_git_commit(&CommitParams {
            repo_path: params.target_repo_path,
            tree_sha: &tree_sha,
            message: &message,
            metadata: &metadata,
            parent_shas: &mapped_parents,
            expected_head: params.last_recreated_sha,
        })?;
        Ok(Some(sha))
    }

    /// Create a git commit using git commands for determinism
    /// Uses git commit-tree for full control over parents
    fn create_git_commit(&self, params: &CommitParams) -> RailResult<String> {
        if let Some(expected) = params.expected_head
            && !params.parent_shas.iter().any(|parent| parent == expected)
        {
            return Err(RailError::message(
                "split commit parents do not contain the current target head",
            ));
        }
        // Prepare environment for deterministic commit
        let author_date = format!(
            "{} {}",
            params.metadata.author_timestamp, params.metadata.author_timezone
        );
        let commit_date = format!(
            "{} {}",
            params.metadata.committer_timestamp, params.metadata.committer_timezone
        );

        // Build commit-tree command
        let mut cmd = git_cmd_for_path(params.repo_path);
        cmd.env("GIT_AUTHOR_NAME", &params.metadata.author)
            .env("GIT_AUTHOR_EMAIL", &params.metadata.author_email)
            .env("GIT_AUTHOR_DATE", &author_date)
            .env("GIT_COMMITTER_NAME", &params.metadata.committer)
            .env("GIT_COMMITTER_EMAIL", &params.metadata.committer_email)
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
        let update_output = if let Some(expected) = params.expected_head {
            Self::run_git_in_repo(params.repo_path, &["update-ref", "HEAD", &commit_sha, expected])?
        } else {
            Self::run_git_in_repo(params.repo_path, &["update-ref", "HEAD", &commit_sha])?
        };
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
        let index_path = tempfile::Builder::new()
            .prefix("cargo-rail-index-")
            .tempfile()
            .context("Failed to allocate split Git index")?
            .into_temp_path();
        std::fs::remove_file(&index_path).context("Failed to initialize split Git index path")?;
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
        cmd.output()
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
        for asset in &config.asset_paths {
            config.path_capabilities.authorize_source(asset)?;
            if config
                .crate_paths
                .iter()
                .any(|crate_root| asset.starts_with(crate_root))
            {
                return Err(RailError::message(format!(
                    "explicit asset '{}' overlaps a Cargo-owned member root",
                    asset.display()
                )));
            }
        }
        config.path_capabilities.validate_target_repository()?;
        progress!("🚂 Splitting crate: {}", config.crate_name);
        progress!("   Mode: {:?}", config.mode);
        progress!("   Target: {}", config.target_repo_path.display());

        if !config.asset_paths.is_empty() {
            progress!("   Explicit non-Cargo assets: {}", config.asset_paths.len());
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
        self.ensure_target_repo(&config.path_capabilities, &config.branch)?;
        self.import_source_objects(&config.target_repo_path)?;
        let origin = OriginContext::discover(
            self.ctx.workspace_root(),
            &config.crate_name,
            &config.ownership.snapshot_id,
        )?;

        // Ordinary history is authoritative. Legacy notes are read only so a later
        // migration commit can carry their exact mappings into normal history.
        let mut mapping_store = MappingStore::new(config.crate_name.clone());
        if target_exists {
            let target_identity = repository_identity(&config.target_repo_path)?;
            mapping_store.load_history(self.ctx.workspace_root(), HistorySide::Source, &target_identity)?;
            mapping_store.load_history(
                &config.target_repo_path,
                HistorySide::Target,
                origin.source_repository(),
            )?;
        }
        mapping_store.load_legacy_notes(self.ctx.workspace_root())?;
        mapping_store.load_legacy_notes(&config.target_repo_path)?;
        if target_exists
            && SystemGit::open(&config.target_repo_path)
                .and_then(|git| git.head_commit())
                .is_ok()
            && mapping_store.count() == 0
        {
            return Err(RailError::with_help(
                "existing split target has no cargo-rail origin evidence",
                "choose an empty target or migrate the target's legacy trailers/notes before splitting",
            ));
        }

        // Walk filtered history to find commits touching the crate
        let mut owned_paths = config.crate_paths.clone();
        owned_paths.extend(config.asset_paths.iter().cloned());
        owned_paths.sort();
        owned_paths.dedup();
        let filtered_commits = self.walk_filtered_history(&owned_paths)?;

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
            config.path_capabilities.validate_target_repository()?;
            mapping_store.migrate_legacy_mappings(&config.target_repo_path, &origin)?;
            progress!("\n✅ Split already up-to-date!");
            progress!("   All {} commits have been split previously.", filtered_commits.len());
            progress!("   Target repo: {}", config.target_repo_path.display());
            return Ok(());
        }

        if filtered_commits.is_empty() {
            return Err(RailError::with_help(
                "split ownership has no committed Git history",
                "commit the Cargo members and explicit assets before splitting",
            ));
        } else {
            // Recreate history in target repo
            progress!("   Processing {} commits...", filtered_commits.len());

            let mut last_recreated_sha: Option<String> = None;
            let mut skipped_commits = 0usize;
            let skipped_already_mapped = already_mapped_count;

            // Incremental history continues from the actual target head so migration
            // and evidence commits remain reachable.
            if target_exists {
                last_recreated_sha = SystemGit::open(&config.target_repo_path)
                    .and_then(|git| git.head_commit())
                    .ok();
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
            for (window_idx, window) in commits_to_process.chunks(PREFETCH_WINDOW_SIZE).enumerate() {
                if window_idx == 0 {
                    if total_new > PREFETCH_WINDOW_SIZE {
                        progress!(
                            "   Prefetching in windows of {} commits to bound memory...",
                            PREFETCH_WINDOW_SIZE
                        );
                    } else {
                        progress!("   Prefetching exact trees and transform inputs in parallel...");
                    }
                }
                let prefetched = self.prefetch_commit_files(window, &owned_paths)?;

                // Process this window's commits
                for (idx_in_window, commit) in window.iter().enumerate() {
                    let overall_idx = window_idx * PREFETCH_WINDOW_SIZE + idx_in_window + 1;

                    if overall_idx.is_multiple_of(10) || overall_idx == total_new {
                        progress!("   Progress: {}/{} new commits", overall_idx, total_new);
                    }

                    // Use prefetched files if available
                    let prefetched_files = prefetched.entries.get(&commit.sha);

                    let maybe_sha = self.recreate_commit_in_target(&RecreateCommitParams {
                        commit,
                        source_paths: &owned_paths,
                        crate_paths: &config.crate_paths,
                        target_repo_path: &config.target_repo_path,
                        crate_name: &config.crate_name,
                        mode: &config.mode,
                        workspace_mode: &config.workspace_mode,
                        mapping_store: &mapping_store,
                        origin: &origin,
                        last_recreated_sha: last_recreated_sha.as_deref(),
                        prefetched_files,
                        prefetched_blobs: &prefetched.blobs,
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

                // The bounded prefetch window is dropped here,
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
                self.create_workspace_cargo_toml(
                    &config.crate_paths,
                    &config.target_repo_path,
                    &config.path_capabilities,
                )?;
            }
        }

        config.path_capabilities.validate_target_repository()?;
        let target_git = SystemGit::open(&config.target_repo_path)?;
        let changed_paths = target_git.changed_paths()?;
        if !changed_paths.is_empty() {
            for path in &changed_paths {
                config.path_capabilities.authorize_target(path)?;
            }
            let source_head = self.ctx.git()?.git().get_commit("HEAD")?;
            let message = append_origin_trailers(
                "Add split-owned repository files",
                &[origin.evidence_trailer(&source_head.sha)?],
            );
            let parent_shas = target_git.head_commit().ok().into_iter().collect::<Vec<_>>();
            target_git.create_commit_with_metadata(&message, &source_head.metadata(), &parent_shas, &changed_paths)?;
        }

        config.path_capabilities.validate_target_repository()?;
        mapping_store.migrate_legacy_mappings(&config.target_repo_path, &origin)?;

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
    fn ensure_target_repo(&self, paths: &SplitPathCapabilities, branch: &str) -> RailResult<()> {
        let target_path = paths.authorize_target(paths.target_root())?;
        if !target_path.exists() {
            std::fs::create_dir_all(&target_path)
                .with_context(|| format!("Failed to create target directory: {}", target_path.display()))?;
        }

        // Check if it's already a git repo
        let git_dir = target_path.join(".git");
        if !git_dir.exists() {
            progress!("   Initializing git repository at {}", target_path.display());

            paths.validate_target_repository()?;
            crate::git::init_repo(&target_path, branch)?;

            paths.validate_target_repository()?;
            self.configure_git_identity(&target_path)?;
        } else {
            paths.validate_target_repository()?;
            let current_branch = SystemGit::open(&target_path)?.current_branch()?;
            if current_branch != branch {
                return Err(RailError::with_help(
                    format!(
                        "split target is on branch '{}', but configuration requires '{}'",
                        current_branch, branch
                    ),
                    format!("switch the target repository to '{}' and retry", branch),
                ));
            }
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
        let name = if user_name.is_empty() { "Cargo-Rail" } else { &user_name };
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
}
