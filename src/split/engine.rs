//! Split engine for deterministic crate extraction.
//!
//! Rebuilds crate history into a target repository while preserving stable commit
//! metadata and applying manifest transformations for split modes.

use crate::cargo::ManifestTransformPolicy;
use crate::config::{SplitMode, WorkspaceMode};
use crate::error::{RailError, RailResult};
use crate::git::mappings::{
    MappingAuthoritySnapshot, MappingStore, OriginContext, TargetPublicationSnapshot, append_origin_trailers,
    is_ancestor, observe_target_branch, remote_endpoint_identity, remote_repository_identity, repository_identity,
};
use crate::git::ops::{GitObjectQuarantine, GitTreeEntry};
use crate::git::{CommitInfo, CommitMetadata, SystemGit};
use crate::mutation::git_effect::{
    GitCommitEffect, GitEffectCommitMetadata, GitEffectIntent, GitEffectJournal, GitEffectRecord, GitEffectStore,
    GitMappingBinding, GitPathImage, GitPathTransition, GitPublicationEffect,
};
use crate::split::SplitPathCapabilities;
use crate::utils;
use crate::verbose_progress as progress;
use crate::workspace::WorkspaceContext;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};

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
    imported_objects: Vec<String>,
}

/// Maximum number of commits to prefetch at once
/// This bounds memory usage to O(window_size × avg_commit_size) instead of O(total_commits × avg_commit_size)
/// For a typical crate with ~1-2MB of files, 50 commits uses ~50-100MB max
const PREFETCH_WINDOW_SIZE: usize = 50;

const fn split_mode_name(mode: &SplitMode) -> &'static str {
    match mode {
        SplitMode::Single => "single",
        SplitMode::Combined => "combined",
    }
}

/// Parameters for recreating a commit in the target repository
struct RecreateCommitParams<'a> {
    commit: &'a CommitInfo,
    crate_paths: &'a [PathBuf],
    mode: &'a SplitMode,
    workspace_mode: &'a WorkspaceMode,
    mapping_store: &'a MappingStore,
    origin: &'a OriginContext,
    last_recreated_sha: Option<&'a str>,
    quarantine: &'a GitObjectQuarantine,
    /// Pre-fetched files (if available from parallel prefetch)
    prefetched_files: Option<&'a PrefetchedFiles>,
    /// Pre-written transformed manifest objects keyed by source blob ID.
    prefetched_manifest_objects: &'a FxHashMap<String, String>,
    path_capabilities: &'a SplitPathCapabilities,
}

struct PreparedSplitCommit {
    oid: String,
    tree: String,
    message: String,
    metadata: CommitMetadata,
    parents: Vec<String>,
    entries: Vec<GitTreeEntry>,
}

/// Split engine - extracts crates with full history
///
/// Deterministic git splitting: same input = same commit SHAs
/// Uses WorkspaceContext for git and cargo operations - no duplicate loads.
#[derive(Debug)]
pub struct SplitEngine<'a> {
    ctx: &'a WorkspaceContext,
    transform: ManifestTransformPolicy,
}

impl<'a> SplitEngine<'a> {
    /// Create a new split engine from workspace context
    pub fn new(ctx: &'a WorkspaceContext) -> RailResult<Self> {
        let transformer = ManifestTransformPolicy::capture(ctx)?;

        Ok(Self {
            ctx,
            transform: transformer,
        })
    }

    fn revalidate_clean_target(&self, target_repo_path: &Path) -> RailResult<()> {
        let obstructing = SystemGit::open(target_repo_path)?.obstructing_worktree_paths()?;
        if obstructing.is_empty() {
            return Ok(());
        }
        Err(RailError::with_help(
            format!(
                "split target became dirty or obstructed before materialization: {}",
                obstructing
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "commit, restore, or remove staged, unstaged, untracked, and ignored target paths before retrying",
        ))
    }

    /// Return whether committed source history lacks target mapping evidence.
    pub fn has_pending_changes(ctx: &WorkspaceContext, config: &SplitParams) -> RailResult<bool> {
        Ok(Self::pending_commit_count(ctx, config)? > 0)
    }

    /// Count committed source-history entries lacking target mapping evidence.
    pub fn pending_commit_count(ctx: &WorkspaceContext, config: &SplitParams) -> RailResult<usize> {
        let source_head = ctx.git()?.git().head_commit()?;
        Self::pending_commit_count_at_source_head(ctx, config, &source_head)
    }

    pub(crate) fn pending_commit_count_at_source_head(
        ctx: &WorkspaceContext,
        config: &SplitParams,
        source_head: &str,
    ) -> RailResult<usize> {
        if !config.target_repo_path.join(".git").exists() {
            return Ok(source_commits_matching_policy(ctx, &config.path_capabilities, &[], source_head)?.len());
        }
        config.path_capabilities.validate_target_repository()?;
        let target_git = SystemGit::open(&config.target_repo_path)?;
        let target_ref = format!("refs/heads/{}", config.branch);
        if target_git.exact_branch_ref_oid(&target_ref)?.is_none() {
            return Ok(source_commits_matching_policy(ctx, &config.path_capabilities, &[], source_head)?.len());
        }
        let target_identity = repository_identity(&config.target_repo_path)?;
        let origin = OriginContext::discover(ctx.workspace_root(), &config.crate_name, &config.ownership.snapshot_id)?;
        let mappings = MappingStore::capture_v025_evidence(
            ctx.workspace_root(),
            &config.target_repo_path,
            &origin,
            &target_identity,
        )?;
        if target_git.head_commit().is_ok() && mappings.count() == 0 {
            return Err(RailError::with_help(
                "existing split target has no cargo-rail origin evidence",
                "choose an empty target or initialize it with current Rail-Origin history before splitting",
            ));
        }

        let commits = source_commits_matching_policy(ctx, &config.path_capabilities, &[], source_head)?;
        if commits.is_empty() {
            return Err(RailError::with_help(
                "split ownership has no committed Git history",
                "commit the Cargo members and explicit assets before splitting",
            ));
        }
        Ok(commits
            .iter()
            .filter(|commit| !mappings.has_mapping(&commit.sha))
            .count())
    }

    /// Walk commit history and filter commits that touch the given paths
    /// Returns commits in chronological order (oldest first)
    fn walk_filtered_history(&self, paths: &SplitPathCapabilities, source_head: &str) -> RailResult<Vec<CommitInfo>> {
        progress!("   Walking commit history to find commits touching crate...");

        let filtered_commits = source_commits_matching_policy(self.ctx, paths, &[], source_head)?;

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
    fn prefetch_commit_files(
        &self,
        commits: &[&CommitInfo],
        paths: &SplitPathCapabilities,
    ) -> RailResult<PrefetchedWindow> {
        let git_state = self.ctx.git()?;
        let git = git_state.git();
        let entries = commits
            .par_iter()
            .map(|commit| {
                let all_files = collect_owned_source_entries(git, &commit.sha, paths)?;
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
        let imported_objects = entries
            .values()
            .flat_map(|files| files.iter())
            .filter(|entry| entry.path.file_name() != Some(std::ffi::OsStr::new("Cargo.toml")))
            .map(|entry| entry.object_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(PrefetchedWindow {
            entries,
            blobs,
            imported_objects,
        })
    }

    /// Recreate one commit in the target repository with split transforms applied.
    ///
    /// Returns `Some(new_sha)` when a commit is materialized, or `None` when the
    /// source commit should be skipped (for example, path was deleted at that point).
    fn prepare_commit_in_quarantine(&self, params: &RecreateCommitParams) -> RailResult<Option<PreparedSplitCommit>> {
        // Use pre-fetched files if available, otherwise collect them now
        // Use Cow to avoid cloning the prefetched Vec when it's already available
        let all_files: std::borrow::Cow<'_, PrefetchedFiles> = if let Some(prefetched) = params.prefetched_files {
            std::borrow::Cow::Borrowed(prefetched)
        } else {
            std::borrow::Cow::Owned(collect_owned_source_entries(
                self.ctx.git()?.git(),
                &params.commit.sha,
                params.path_capabilities,
            )?)
        };

        let mut target_entries = Vec::with_capacity(all_files.len());
        let mut target_sources = std::collections::BTreeMap::<PathBuf, PathBuf>::new();
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
            if let Some(previous) = target_sources.insert(target_path.clone(), entry.path.clone())
                && previous != entry.path
            {
                return Err(RailError::with_help(
                    format!(
                        "split ownership maps '{}' and '{}' to target path '{}'",
                        previous.display(),
                        entry.path.display(),
                        target_path.display()
                    ),
                    "narrow split.include/exclude so each historical source path has one injective target projection",
                ));
            }

            let object_id = if entry.path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
                if let Some(object_id) = params.prefetched_manifest_objects.get(&entry.object_id) {
                    object_id.clone()
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
                    let content = std::str::from_utf8(&content).map_err(|_| {
                        RailError::message(format!("manifest '{}' is not valid UTF-8", entry.path.display()))
                    })?;
                    let target_has_workspace =
                        *params.mode == SplitMode::Combined && *params.workspace_mode == WorkspaceMode::Workspace;
                    let transformed = self.transform.transform_to_split(content, target_has_workspace)?;
                    params.quarantine.write_blob(transformed.as_bytes())?
                }
            } else {
                entry.object_id.clone()
            };
            target_entries.push(GitTreeEntry {
                mode: entry.mode.clone(),
                object_id,
                path: target_path,
            });
        }

        // A fresh index per snapshot makes absence authoritative: deleted and
        // renamed files cannot leak forward from the previous worktree.
        let tree_sha = params.quarantine.write_exact_tree(&target_entries)?;

        // Create commit using git command for determinism
        // Map parent SHAs from monorepo to split repo
        let mut mapped_parents: Vec<String> = params
            .commit
            .parent_shas
            .iter()
            .filter_map(|parent_sha| params.mapping_store.get_mapping(parent_sha))
            .collect();

        // Keep every ordinary target commit reachable, including provenance commits
        // that do not define a one-to-one source mapping.
        if let Some(last) = params.last_recreated_sha
            && !mapped_parents.iter().any(|parent| parent == last)
        {
            mapped_parents.insert(0, last.to_string());
        }

        let metadata = params.commit.metadata();
        let message = append_origin_trailers(&params.commit.message, &[params.origin.trailer(&params.commit.sha)?]);
        let sha = params
            .quarantine
            .write_commit(&tree_sha, &mapped_parents, &message, &metadata)?;
        Ok(Some(PreparedSplitCommit {
            oid: sha,
            tree: tree_sha,
            message,
            metadata,
            parents: mapped_parents,
            entries: target_entries,
        }))
    }

    fn capture_publication(
        &self,
        config: &SplitParams,
        mappings: Option<&MappingStore>,
    ) -> RailResult<Option<TargetPublicationSnapshot>> {
        let Some(remote_url) = config
            .remote_url
            .as_deref()
            .filter(|remote| !utils::is_local_path(remote))
        else {
            return Ok(None);
        };
        let observation = observe_target_branch(
            self.ctx.workspace_root(),
            &config.target_repo_path,
            remote_url,
            &config.branch,
        )?;
        TargetPublicationSnapshot::capture(observation, &config.target_repo_path, mappings).map(Some)
    }

    fn revalidate_publication_before_target_effect(
        &self,
        config: &SplitParams,
        expected: Option<&TargetPublicationSnapshot>,
        mappings: Option<&MappingStore>,
    ) -> RailResult<Option<TargetPublicationSnapshot>> {
        let actual = self.capture_publication(config, mappings)?;
        if expected.is_some()
            && actual.as_ref() != expected
            && !self.matches_completed_prepared_publication(config, expected, actual.as_ref())?
        {
            return Err(RailError::with_help(
                "split target publication authority changed after the operation was planned",
                "fetch and retry; cargo-rail will not mutate a target bound to changed local or remote branch authority",
            ));
        }
        if actual
            .as_ref()
            .is_some_and(|snapshot| !snapshot.permits_target_mutation())
        {
            return Err(RailError::with_help(
                "local split target is behind its configured remote branch",
                "fast-forward the local target branch before splitting or publishing",
            ));
        }
        Ok(actual)
    }

    fn matches_completed_prepared_publication(
        &self,
        config: &SplitParams,
        expected: Option<&TargetPublicationSnapshot>,
        actual: Option<&TargetPublicationSnapshot>,
    ) -> RailResult<bool> {
        let (Some(expected), Some(actual), Some(remote_url)) = (
            expected,
            actual,
            config
                .remote_url
                .as_deref()
                .filter(|remote| !utils::is_local_path(remote)),
        ) else {
            return Ok(false);
        };
        let target = SystemGit::open(&config.target_repo_path)?;
        let ref_name = format!("refs/heads/{}", config.branch);
        let mut journals = GitEffectStore::discover_unacknowledged_read_only(&target)?
            .into_iter()
            .filter(|journal| journal.repository().ref_name == ref_name && journal.publication().is_some())
            .collect::<Vec<_>>();
        if journals.len() != 1 {
            return Ok(false);
        }
        let journal = journals.pop().expect("one prepared publication");
        if !journal.permits_local_recovery_state(&target)? {
            return Ok(false);
        }
        let publication = journal.publication().expect("filtered publication journal");
        let repository = journal.repository();
        Ok(
            remote_endpoint_identity(remote_url)? == publication.exact_endpoint_digest()
                && expected.remote_repository() == publication.logical_remote()
                && expected.remote_head() == publication.expected_oid()
                && expected.local_head() == repository.expected_oid.as_deref()
                && actual.remote_repository() == publication.logical_remote()
                && actual.remote_head() == Some(publication.desired_oid())
                && actual.local_head() == Some(repository.result_oid.as_str())
                && publication.desired_oid() == repository.result_oid
                && actual.count() == 0,
        )
    }

    fn validate_unproven_source_ancestry(
        &self,
        config: &SplitParams,
        mappings: &MappingStore,
        source_head: &str,
    ) -> RailResult<()> {
        let pairs = mappings.unproven_mapping_pairs();
        if pairs.is_empty() {
            return Ok(());
        }
        let pending = source_commits_matching_policy(
            self.ctx,
            &config.path_capabilities,
            &mappings.source_frontier_commits(),
            source_head,
        )?
        .into_iter()
        .filter(|commit| !mappings.has_mapping(&commit.sha))
        .collect::<Vec<_>>();
        for (source, _) in pairs {
            for ancestor in &pending {
                if is_ancestor(self.ctx.workspace_root(), &ancestor.sha, &source)? {
                    return Err(RailError::with_help(
                        format!(
                            "exact predecessor mapping endpoint '{}' has unmatched source ancestor '{}' without directional frontier proof",
                            source, ancestor.sha
                        ),
                        "restore authoritative directional origin history or resolve the mapping topology manually; cargo-rail will not guess ancestry or replay an ancestor after its mapped descendant",
                    ));
                }
            }
        }
        Ok(())
    }

    fn reconcile_publication(
        &self,
        config: &SplitParams,
        expected: Option<&TargetPublicationSnapshot>,
        origin: &OriginContext,
    ) -> RailResult<usize> {
        let Some(expected) = expected else {
            return Ok(0);
        };
        let remote_url = config
            .remote_url
            .as_deref()
            .ok_or_else(|| RailError::message("split publication authority has no configured remote URL"))?;
        let target_git = SystemGit::open(&config.target_repo_path)?;
        let target_identity = repository_identity(&config.target_repo_path)?;
        let selected_head = target_git.head_commit()?;
        let selected_source_head = self.ctx.git()?.git().head_commit()?;
        let selected_source_heads = vec![selected_source_head];
        let mappings = MappingStore::capture_v025_evidence_at(
            self.ctx.workspace_root(),
            &config.target_repo_path,
            origin,
            &target_identity,
            &selected_head,
            &selected_source_heads,
        )?;
        let actual = self
            .capture_publication(config, Some(&mappings))?
            .ok_or_else(|| RailError::message("split lost its configured publication authority"))?;
        if !expected.same_remote_authority(&actual) {
            return Err(RailError::with_help(
                "configured remote branch advanced during split",
                "fetch and retry; cargo-rail will not publish against a remote head different from the checked authority",
            ));
        }
        if !actual.permits_target_mutation() {
            return Err(RailError::with_help(
                "local split target is behind its configured remote branch",
                "fast-forward the local target branch before splitting or publishing",
            ));
        }
        let pending = actual.count();
        if pending == 0 {
            return Ok(0);
        }
        let desired = actual
            .local_head()
            .ok_or_else(|| RailError::message("split publication has no exact local commit"))?;
        let store = GitEffectStore::open(&target_git)?;
        let ref_name = format!("refs/heads/{}", config.branch);
        let repository = store.capture_repository_authority(
            &target_git,
            repository_identity(&config.target_repo_path)?,
            ref_name.clone(),
            Some(desired.to_string()),
            desired.to_string(),
        )?;
        let publication = GitPublicationEffect::new(
            actual.remote_repository().to_string(),
            remote_endpoint_identity(remote_url)?,
            ref_name,
            actual.remote_head().map(str::to_string),
            desired.to_string(),
        );
        let intent = GitEffectIntent::new(
            format!("split-publication-{}-{}", config.crate_name, actual.digest()),
            repository,
            None,
            Vec::new(),
            None,
            Some(publication),
            None,
        )?;
        let record = store.prepare(intent)?;
        self.reconcile_split_publication_record(config, origin, &store, record)?;
        Ok(pending)
    }

    fn reconcile_split_publication_record(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        store: &GitEffectStore,
        record: GitEffectRecord,
    ) -> RailResult<()> {
        match record {
            GitEffectRecord::Active(mut active) => {
                active.mark_local_applied()?;
                self.reconcile_split_publication_journal(config, origin, store, active.journal())?;
                active.mark_published()?;
                let _completed = active.finish()?;
                Ok(())
            }
            GitEffectRecord::Completed(completed) => {
                self.reconcile_split_publication_journal(config, origin, store, completed.journal())
            }
        }
    }

    fn reconcile_split_publication_journal(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        store: &GitEffectStore,
        journal: &GitEffectJournal,
    ) -> RailResult<()> {
        let publication = journal
            .publication()
            .ok_or_else(|| RailError::message("prepared split publication has no remote authority"))?;
        let remote_url = config
            .remote_url
            .as_deref()
            .filter(|remote| !utils::is_local_path(remote))
            .ok_or_else(|| RailError::message("prepared split publication lost its configured remote endpoint"))?;
        let ref_name = format!("refs/heads/{}", config.branch);
        if journal.repository().ref_name != ref_name
            || publication.ref_name() != ref_name
            || publication.desired_oid() != journal.repository().result_oid
            || publication.logical_remote() != remote_repository_identity(remote_url)?
            || publication.exact_endpoint_digest() != remote_endpoint_identity(remote_url)?
        {
            return Err(RailError::with_help(
                "prepared split publication authority changed before publication",
                "restore the exact target repository, branch, and configured endpoint before retrying",
            ));
        }
        let target = SystemGit::open(&config.target_repo_path)?;
        if target.exact_branch_ref_oid(&ref_name)?.as_deref() != Some(publication.desired_oid()) {
            return Err(RailError::message(
                "prepared split publication local branch is not at its exact desired commit",
            ));
        }
        let target_identity = repository_identity(&config.target_repo_path)?;
        let source_head = self.ctx.git()?.git().head_commit()?;
        let mappings = MappingStore::capture_v025_evidence_at(
            self.ctx.workspace_root(),
            &config.target_repo_path,
            origin,
            &target_identity,
            publication.desired_oid(),
            &[source_head],
        )?;
        let actual = self
            .capture_publication(config, Some(&mappings))?
            .ok_or_else(|| RailError::message("prepared split publication lost remote authority"))?;
        if actual.remote_repository() != publication.logical_remote()
            || actual.local_head() != Some(publication.desired_oid())
        {
            return Err(RailError::message(
                "prepared split publication no longer matches its exact local or remote repository",
            ));
        }
        if actual.remote_head() == Some(publication.desired_oid()) {
            return Ok(());
        }
        if actual.remote_head() != publication.expected_oid() {
            return Err(RailError::with_help(
                "prepared split publication found a third remote ref state",
                "preserve the remote branch and reconcile it manually; cargo-rail will not overwrite an unjournaled commit",
            ));
        }
        if !journal.matches_repository_authority(store, &target, Some(publication.desired_oid().to_string()))? {
            return Err(RailError::message(
                "prepared split publication repository authority changed before push",
            ));
        }
        target.push_commit_to_url_with_lease(
            remote_url,
            publication.ref_name(),
            publication.desired_oid(),
            publication.expected_oid(),
        )?;
        let published = self
            .capture_publication(config, Some(&mappings))?
            .ok_or_else(|| RailError::message("prepared split publication lost authority after push"))?;
        if published.remote_repository() != publication.logical_remote()
            || published.remote_head() != Some(publication.desired_oid())
            || published.local_head() != Some(publication.desired_oid())
            || published.count() != 0
        {
            return Err(RailError::with_help(
                "prepared split publication did not converge to its exact desired commit",
                "inspect the local and remote branch heads before retrying",
            ));
        }
        Ok(())
    }

    /// Execute a split operation (idempotent - re-runs sync new commits only)
    pub fn split(&self, config: &SplitParams) -> RailResult<()> {
        self.split_with_pending_count(config, None, None).map(|_| ())
    }

    /// Execute a split and return the exact pre-apply pending commit count.
    pub(crate) fn split_with_pending_count(
        &self,
        config: &SplitParams,
        expected_origin_migration: Option<&MappingAuthoritySnapshot>,
        expected_publication: Option<&TargetPublicationSnapshot>,
    ) -> RailResult<usize> {
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
        self.validate_target_repo(&config.path_capabilities, &config.branch)?;
        progress!("Splitting crate: {}", config.crate_name);
        progress!("   Mode: {}", split_mode_name(&config.mode));
        progress!("   Target: {}", config.target_repo_path.display());

        if !config.asset_paths.is_empty() {
            progress!("   Explicit non-Cargo assets: {}", config.asset_paths.len());
        }

        let origin = if let Some(expected) = expected_origin_migration {
            OriginContext::new(
                expected.source_repository().to_string(),
                &config.crate_name,
                &config.ownership.snapshot_id,
            )?
        } else {
            OriginContext::discover(
                self.ctx.workspace_root(),
                &config.crate_name,
                &config.ownership.snapshot_id,
            )?
        };
        let recovered_pre_authority = self
            .resume_active_split_effect(config, &origin, expected_origin_migration)
            .map_err(|error| {
                if !config.target_repo_path.join(".git").exists() {
                    split_mapping_authority_changed_error("target repository disappearance")
                } else {
                    error
                }
            })?;
        let target_ref = format!("refs/heads/{}", config.branch);

        // Reuse an exact zero-migration authority captured by the command.
        // The source and target scalar refs are checked again immediately
        // before the prepared effect is journaled; recapturing them here would
        // only repeat the same probes before any mutation is possible.
        let (captured_store, captured_origin) = if let Some(expected) = expected_origin_migration
            && expected.count() == 0
            && recovered_pre_authority.is_none()
        {
            (MappingStore::from_current_snapshot(expected)?, expected.clone())
        } else {
            // Recapture both mapping and publication authority before the
            // first target effect when no checked authority can be reused.
            // The read-only store also proves every commit in an ahead
            // publication retry is Cargo-Rail-owned.
            let target_git = SystemGit::open(&config.target_repo_path)?;
            let selected_target_head = target_git.exact_branch_ref_oid(&target_ref)?;
            if selected_target_head.is_none() {
                (
                    MappingStore::new(config.crate_name.clone()),
                    MappingAuthoritySnapshot::empty_initialized(
                        self.ctx.workspace_root(),
                        &origin,
                        &config.target_repo_path,
                        config.path_capabilities.target_root(),
                        &config.branch,
                        "mono_to_remote",
                    )?,
                )
            } else {
                let target_identity =
                    crate::git::mappings::repository_identity_from_git(&target_git, selected_target_head.as_deref())
                        .map_err(|error| {
                            if expected_origin_migration.is_some() && !config.target_repo_path.join(".git").exists() {
                                split_mapping_authority_changed_error("target initialization")
                            } else {
                                error
                            }
                        })?;
                if let Some(selected_target_head) =
                    expected_origin_migration.and_then(MappingAuthoritySnapshot::target_selected_head)
                {
                    MappingStore::capture_v025_authority_at(
                        self.ctx.workspace_root(),
                        &config.target_repo_path,
                        &origin,
                        &target_identity,
                        config.path_capabilities.target_root(),
                        &config.branch,
                        "mono_to_remote",
                        selected_target_head,
                    )?
                } else {
                    MappingStore::capture_v025_authority(
                        self.ctx.workspace_root(),
                        &config.target_repo_path,
                        &origin,
                        &target_identity,
                        config.path_capabilities.target_root(),
                        &config.branch,
                        "mono_to_remote",
                    )?
                }
            }
        };
        if expected_origin_migration.is_some_and(|expected| {
            expected != &captured_origin && recovered_pre_authority.as_deref() != Some(expected.digest().as_str())
        }) {
            return Err(split_mapping_authority_changed_error("checked predecessor capture"));
        }
        validate_predecessor_mapping_projections(
            self.ctx,
            &self.transform,
            &config.crate_paths,
            &config.path_capabilities,
            &config.target_repo_path,
            &config.mode,
            &config.workspace_mode,
            &captured_origin,
        )?;
        self.validate_unproven_source_ancestry(config, &captured_store, captured_origin.source_head())?;
        let bound_publication =
            self.revalidate_publication_before_target_effect(config, expected_publication, Some(&captured_store))?;
        let dirty_target_paths = SystemGit::open(&config.target_repo_path)?.obstructing_worktree_paths()?;
        if !dirty_target_paths.is_empty() {
            return Err(RailError::with_help(
                format!(
                    "split target became dirty after planning: {}",
                    dirty_target_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "commit or restore target work and restart; cargo-rail will not reset staged, unstaged, or untracked target bytes",
            ));
        }
        if SystemGit::open(&config.target_repo_path)?
            .exact_branch_ref_oid(&target_ref)?
            .is_some()
            && captured_store.count() == 0
        {
            return Err(RailError::with_help(
                "existing split target has no cargo-rail origin evidence",
                "choose an empty target or initialize it with a validated current Rail-Origin pair before splitting",
            ));
        }

        // Migrate only the exact checked predecessor set before importing
        // objects or changing any reconstructed target history.
        let migration_required =
            captured_origin.count() > 0 || expected_origin_migration.is_some_and(|expected| expected.count() > 0);
        let mut mapping_store = if captured_origin.target_head().is_some() && migration_required {
            let target_identity = repository_identity(&config.target_repo_path)?;
            let mut store = captured_store;
            let migrated = store
                .migrate_v025_evidence_bound(
                    self.ctx.workspace_root(),
                    &config.target_repo_path,
                    &origin,
                    &target_identity,
                    config.path_capabilities.target_root(),
                    &config.branch,
                    "mono_to_remote",
                    expected_origin_migration,
                )
                .map_err(|error| {
                    if expected_origin_migration.is_some() && !config.target_repo_path.join(".git").exists() {
                        split_mapping_authority_changed_error("predecessor migration")
                    } else {
                        error
                    }
                })?;
            if migrated.is_some() {
                progress!("   Migrated predecessor mappings into ordinary Git history");
            }
            store
        } else {
            captured_store
        };

        // Walk filtered history to find commits touching the crate
        let filtered_commits = self.walk_filtered_history(&config.path_capabilities, captured_origin.source_head())?;

        // Count how many commits are already mapped (for idempotency)
        let already_mapped_count = filtered_commits
            .iter()
            .filter(|c| mapping_store.has_mapping(&c.sha))
            .count();
        let pending_commits = filtered_commits.len().saturating_sub(already_mapped_count);

        if already_mapped_count > 0 {
            progress!("   Found {} commits already split (will skip)", already_mapped_count);
        }

        // Check if all commits are already mapped - nothing to do
        if already_mapped_count == filtered_commits.len() && !filtered_commits.is_empty() {
            progress!("Split already up to date.");
            progress!("   All {} commits have been split previously.", filtered_commits.len());
            progress!("   Target repo: {}", config.target_repo_path.display());
            self.reconcile_publication(config, bound_publication.as_ref(), &origin)?;
            return Ok(0);
        }

        if filtered_commits.is_empty() {
            return Err(RailError::with_help(
                "split ownership has no committed Git history",
                "commit the Cargo members and explicit assets before splitting",
            ));
        } else {
            // Prepare the complete pending history in one private object view.
            // No target object, ref, index, or worktree state changes until the
            // exact chain, path transitions, and post-mapping authority are
            // durably journaled together.
            progress!("   Processing {} commits...", filtered_commits.len());

            let target_git = SystemGit::open(&config.target_repo_path)?;
            let target_ref = format!("refs/heads/{}", config.branch);
            let expected_target_head = target_git.exact_branch_ref_oid(&target_ref)?;
            let pre_authority = if expected_target_head.is_some() {
                mapping_store.mapping_authority_snapshot(
                    "mono_to_remote",
                    config.path_capabilities.target_root(),
                    &config.branch,
                )?
            } else {
                captured_origin.clone()
            };
            if pre_authority.target_head() != expected_target_head.as_deref() {
                return Err(split_mapping_authority_changed_error("prepared target HEAD"));
            }
            let quarantine = target_git.object_quarantine()?;
            if let Some(expected) = expected_target_head.as_deref() {
                quarantine.import_object_closure(&target_git, &[expected])?;
            }

            let mut last_recreated_sha = expected_target_head.clone();
            let mut skipped_commits = 0usize;
            let skipped_already_mapped = already_mapped_count;
            let mut prepared_last = None::<PreparedSplitCommit>;
            let mut final_entries = Vec::<GitTreeEntry>::new();
            let mut new_mappings = Vec::<(String, String)>::new();
            let mut new_target_evidence = Vec::<(String, String)>::new();

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
                let prefetched = self.prefetch_commit_files(window, &config.path_capabilities)?;
                let imported_objects = prefetched
                    .imported_objects
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                quarantine.import_object_closure(self.ctx.git()?.git(), &imported_objects)?;
                let mut manifest_inputs = prefetched.blobs.iter().collect::<Vec<_>>();
                manifest_inputs.sort_by(|left, right| left.0.cmp(right.0));
                let target_has_workspace =
                    config.mode == SplitMode::Combined && config.workspace_mode == WorkspaceMode::Workspace;
                let transformed = manifest_inputs
                    .iter()
                    .map(|(_, content)| {
                        let content = std::str::from_utf8(content)
                            .map_err(|_| RailError::message("prefetched split manifest is not valid UTF-8"))?;
                        Ok(self
                            .transform
                            .transform_to_split(content, target_has_workspace)?
                            .into_bytes())
                    })
                    .collect::<RailResult<Vec<_>>>()?;
                let manifest_objects = quarantine.write_blobs(&transformed)?;
                let prefetched_manifest_objects = manifest_inputs
                    .into_iter()
                    .map(|(object_id, _)| object_id.clone())
                    .zip(manifest_objects)
                    .collect::<FxHashMap<_, _>>();

                // Process this window's commits
                for (idx_in_window, commit) in window.iter().enumerate() {
                    let overall_idx = window_idx * PREFETCH_WINDOW_SIZE + idx_in_window + 1;

                    if overall_idx.is_multiple_of(10) || overall_idx == total_new {
                        progress!("   Progress: {}/{} new commits", overall_idx, total_new);
                    }

                    // Use prefetched files if available
                    let prefetched_files = prefetched.entries.get(&commit.sha);

                    let maybe_prepared = self.prepare_commit_in_quarantine(&RecreateCommitParams {
                        commit,
                        crate_paths: &config.crate_paths,
                        mode: &config.mode,
                        workspace_mode: &config.workspace_mode,
                        mapping_store: &mapping_store,
                        origin: &origin,
                        last_recreated_sha: last_recreated_sha.as_deref(),
                        quarantine: &quarantine,
                        prefetched_files,
                        prefetched_manifest_objects: &prefetched_manifest_objects,
                        path_capabilities: &config.path_capabilities,
                    })?;

                    // Handle skipped commits (dirty history - path didn't exist at this commit)
                    let Some(prepared) = maybe_prepared else {
                        skipped_commits += 1;
                        continue;
                    };

                    // Advance only the private preparation view. The ordinary
                    // history remains the durable authority after reconciliation.
                    mapping_store.record_source_frontier_mapping(&commit.sha, &prepared.oid)?;
                    new_mappings.push((commit.sha.clone(), prepared.oid.clone()));

                    last_recreated_sha = Some(prepared.oid.clone());
                    final_entries.clone_from(&prepared.entries);
                    prepared_last = Some(prepared);
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
                let bytes = self.render_workspace_cargo_toml(&config.crate_paths, captured_origin.source_head())?;
                let object_id = quarantine.write_blob(&bytes)?;
                let path = PathBuf::from("Cargo.toml");
                config.path_capabilities.authorize_target(&path)?;
                final_entries.retain(|entry| entry.path != path);
                final_entries.push(GitTreeEntry {
                    mode: "100644".to_string(),
                    object_id,
                    path,
                });
                final_entries.sort_by(|left, right| left.path.cmp(&right.path));
                let tree = quarantine.write_exact_tree(&final_entries)?;
                let source_head = self.ctx.git()?.git().get_commit(captured_origin.source_head())?;
                let message = append_origin_trailers(
                    "Add split-owned repository files",
                    &[origin.evidence_trailer(&source_head.sha)?],
                );
                let parents = last_recreated_sha.into_iter().collect::<Vec<_>>();
                let metadata = source_head.metadata();
                let oid = quarantine.write_commit(&tree, &parents, &message, &metadata)?;
                new_target_evidence.push((source_head.sha, oid.clone()));
                prepared_last = Some(PreparedSplitCommit {
                    oid,
                    tree,
                    message,
                    metadata,
                    parents,
                    entries: final_entries.clone(),
                });
            }

            let prepared = prepared_last
                .ok_or_else(|| RailError::message("split preparation produced no ordinary-history commit"))?;
            let result_repository = if expected_target_head.is_none()
                && config.remote_url.as_deref().is_none_or(crate::utils::is_local_path)
            {
                let root = new_mappings
                    .first()
                    .map(|(_, target)| target.clone())
                    .ok_or_else(|| RailError::message("prepared split chain has no root mapping"))?;
                crate::git::mappings::repository_identity_from_roots([root])?
            } else {
                pre_authority
                    .target_repository()
                    .ok_or_else(|| RailError::message("prepared split authority lost its target repository"))?
                    .to_string()
            };
            let post_authority = pre_authority.after_split_chain(
                &new_mappings,
                &new_target_evidence,
                &prepared.oid,
                result_repository,
            )?;
            let transitions =
                self.split_path_transitions(&target_git, expected_target_head.as_deref(), &prepared.entries)?;
            self.apply_prepared_split_chain(
                config,
                &origin,
                &pre_authority,
                &post_authority,
                &quarantine,
                &prepared,
                transitions,
                &mut mapping_store,
                bound_publication.as_ref(),
            )?;
        }

        config.path_capabilities.validate_target_repository()?;
        if let Some(ref remote_url) = config.remote_url {
            if remote_url.is_empty() || utils::is_local_path(remote_url) {
                progress!("Split repository created locally.");
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
            progress!("No remote URL configured; repository created locally only.");
            progress!("   To push manually:");
            progress!("   cd {}", config.target_repo_path.display());
            progress!("   git remote add origin <url>");
            progress!("   git push -u origin {}", config.branch);
        }

        progress!("Split complete.");
        progress!("   Target repo: {}", config.target_repo_path.display());

        Ok(pending_commits)
    }

    /// Reconcile the one durable target-branch effect before deriving fresh
    /// pending work. Returning the pre-effect digest lets an exact saved plan
    /// complete recovery even when the branch ref already reached the bound
    /// post-effect commit before the previous process stopped.
    fn resume_active_split_effect(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        expected_plan: Option<&MappingAuthoritySnapshot>,
    ) -> RailResult<Option<String>> {
        let target = SystemGit::open(&config.target_repo_path)?;
        let ref_name = format!("refs/heads/{}", config.branch);
        let mut matching = GitEffectStore::discover_unacknowledged_read_only(&target)?
            .into_iter()
            .filter(|journal| journal.repository().ref_name == ref_name)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Ok(None);
        }
        if matching.len() != 1 {
            return Err(RailError::message(format!(
                "split target branch '{ref_name}' has multiple active prepared effects"
            )));
        }
        let journal = matching.pop().expect("one active split effect");
        if journal
            .operation_id()
            .starts_with(&format!("split-publication-{}-sha256-", config.crate_name))
            && journal.mapping().is_none()
            && journal.publication().is_some()
        {
            let store = GitEffectStore::open(&target)?;
            let record = store.resume(journal.effect_id())?;
            self.reconcile_split_publication_record(config, origin, &store, record)?;
            return Ok(None);
        }
        let mapping = journal
            .mapping()
            .ok_or_else(|| RailError::message("active split target effect has no mapping authority"))?;
        let pre_digest = mapping.pre_authority().to_string();
        if mapping.migration_count() > 0 {
            let target_identity = repository_identity(&config.target_repo_path)?;
            let mut mappings = MappingStore::new(config.crate_name.clone());
            mappings.migrate_v025_evidence_bound(
                self.ctx.workspace_root(),
                &config.target_repo_path,
                origin,
                &target_identity,
                config.path_capabilities.target_root(),
                &config.branch,
                "mono_to_remote",
                expected_plan,
            )?;
            return Ok(Some(pre_digest));
        }
        if !journal.operation_id().starts_with("split-chain-sha256-") {
            return Err(RailError::with_help(
                format!(
                    "split target branch '{ref_name}' has unrelated active effect '{}'",
                    journal.effect_id()
                ),
                "finish or reconcile that exact prepared effect before starting split",
            ));
        }
        let store = GitEffectStore::open(&target)?;
        let record = store.resume(journal.effect_id())?;
        self.reconcile_resumed_split_record(config, origin, expected_plan, &store, record)?;
        Ok(Some(pre_digest))
    }

    fn reconcile_resumed_split_record(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        expected_plan: Option<&MappingAuthoritySnapshot>,
        store: &GitEffectStore,
        record: GitEffectRecord,
    ) -> RailResult<()> {
        match record {
            GitEffectRecord::Active(mut active) => {
                self.reconcile_resumed_split_journal(config, origin, expected_plan, store, active.journal())?;
                active.mark_local_applied()?;
                if active.journal().publication().is_some() {
                    self.reconcile_split_publication_journal(config, origin, store, active.journal())?;
                    active.mark_published()?;
                }
                let _completed = active.finish()?;
                Ok(())
            }
            GitEffectRecord::Completed(completed) => {
                self.reconcile_resumed_split_journal(config, origin, expected_plan, store, completed.journal())?;
                if completed.journal().publication().is_some() {
                    self.reconcile_split_publication_journal(config, origin, store, completed.journal())?;
                }
                Ok(())
            }
        }
    }

    fn reconcile_resumed_split_journal(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        _expected_plan: Option<&MappingAuthoritySnapshot>,
        store: &GitEffectStore,
        journal: &GitEffectJournal,
    ) -> RailResult<()> {
        let target = SystemGit::open(&config.target_repo_path)?;
        let repository = journal.repository();
        let mapping = journal
            .mapping()
            .ok_or_else(|| RailError::message("prepared split effect has no mapping authority"))?;
        let commit = journal
            .commit()
            .ok_or_else(|| RailError::message("prepared split effect has no commit"))?;
        let bundle_digest = journal
            .object_bundle_digest()
            .ok_or_else(|| RailError::message("prepared split effect has no object bundle"))?;
        let target_identity = repository_identity(&config.target_repo_path)?;
        let expected_ref = format!("refs/heads/{}", config.branch);
        let mut mismatches = Vec::new();
        if !journal.operation_id().starts_with("split-chain-sha256-") {
            mismatches.push("operation");
        }
        if mapping.owner() != config.crate_name {
            mismatches.push("owner");
        }
        if mapping.ownership_snapshot() != config.ownership.snapshot_id {
            mismatches.push("ownership");
        }
        if mapping.migration_count() != 0 || mapping.migration_digest().is_some() {
            mismatches.push("migration");
        }
        if repository.ref_name != expected_ref {
            mismatches.push("branch");
        }
        if repository.result_oid != commit.oid() {
            mismatches.push("result");
        }
        if !mismatches.is_empty() {
            return Err(RailError::message(format!(
                "active prepared split journal does not match the current split authority: {}",
                mismatches.join(", ")
            )));
        }
        for transition in journal.paths() {
            config.path_capabilities.authorize_target(transition.path())?;
        }
        let current_head = target.exact_branch_ref_oid(&repository.ref_name)?;
        if current_head.as_deref() != repository.expected_oid.as_deref()
            && current_head.as_deref() != Some(repository.result_oid.as_str())
        {
            return Err(RailError::message("prepared split branch changed to a third ref state"));
        }
        if current_head.as_deref() == repository.expected_oid.as_deref()
            && repository.logical_repository != target_identity
        {
            return Err(RailError::message(
                "active prepared split journal old repository identity changed",
            ));
        }
        let current_repository = store.capture_repository_authority(
            &target,
            repository.logical_repository.clone(),
            repository.ref_name.clone(),
            current_head.clone(),
            repository.result_oid.clone(),
        )?;
        if current_repository.common_dir_identity != repository.common_dir_identity
            || current_repository.worktree_identity != repository.worktree_identity
            || current_repository.object_format != repository.object_format
            || current_repository.ref_name != repository.ref_name
            || current_repository.symbolic_head != repository.symbolic_head
        {
            return Err(RailError::message(
                "prepared split repository authority changed during recovery",
            ));
        }
        let current_authority = self.capture_current_split_authority(config, origin)?.1;
        let expected_digest = if current_head.as_deref() == repository.expected_oid.as_deref() {
            self.validate_split_path_images(&target, journal, true)?;
            mapping.pre_authority()
        } else {
            mapping.post_authority()
        };
        if current_authority.digest() != expected_digest {
            return Err(RailError::with_help(
                format!(
                    "active prepared split mapping authority is '{}', expected '{}' for the observed ref state",
                    current_authority.digest(),
                    expected_digest
                ),
                "restore the exact prepared ref and ordinary-history authority before retrying",
            ));
        }

        let bundle = store
            .open_object_bundle(journal.effect_id(), bundle_digest)?
            .ok_or_else(|| RailError::message("prepared split object bundle disappeared"))?;
        target.install_prepared_object_pack_and_update_ref(
            bundle.into_file(),
            &store.object_bundle_path(journal.effect_id())?,
            bundle_digest,
            commit,
            &repository.ref_name,
            repository.expected_oid.as_deref(),
            journal.effect_id(),
        )?;
        #[cfg(test)]
        {
            fail_split_after_ref_cas()?;
        }
        if !journal.matches_repository_authority(store, &target, Some(repository.result_oid.clone()))? {
            return Err(RailError::message(
                "prepared split repository authority changed before final materialization",
            ));
        }
        self.validate_split_journal_tree_images(&target, journal)?;
        let paths = journal
            .paths()
            .iter()
            .map(|transition| transition.path().to_path_buf())
            .collect::<Vec<_>>();
        target.reconcile_prepared_commit_paths(repository.expected_oid.as_deref(), &repository.result_oid, &paths)?;
        let actual_post = self.capture_current_split_authority(config, origin)?.1;
        if actual_post.digest() != mapping.post_authority() {
            return Err(RailError::with_help(
                format!(
                    "recovered split mapping authority is '{}', expected '{}'",
                    actual_post.digest(),
                    mapping.post_authority()
                ),
                "do not start new split work until the prepared effect is exactly reconciled",
            ));
        }
        Ok(())
    }

    fn split_path_transitions(
        &self,
        target: &SystemGit,
        expected: Option<&str>,
        result_entries: &[GitTreeEntry],
    ) -> RailResult<Vec<GitPathTransition>> {
        let old_entries = expected.map_or_else(
            || Ok(Vec::new()),
            |expected| target.collect_tree_entries(expected, Path::new(".")),
        )?;
        let old = old_entries
            .into_iter()
            .map(|entry| (entry.path, GitPathImage::entry(entry.mode, entry.object_id)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut new = std::collections::BTreeMap::new();
        for entry in result_entries {
            if entry.mode == "160000" {
                return Err(RailError::message(format!(
                    "split result path '{}' is an unsupported gitlink",
                    entry.path.display()
                )));
            }
            if new
                .insert(
                    entry.path.clone(),
                    GitPathImage::entry(entry.mode.clone(), entry.object_id.clone()),
                )
                .is_some()
            {
                return Err(RailError::message(format!(
                    "split result repeats target path '{}'",
                    entry.path.display()
                )));
            }
        }
        let paths = old
            .keys()
            .chain(new.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        paths
            .into_iter()
            .filter_map(|path| {
                let old = old.get(&path).cloned().unwrap_or(GitPathImage::Absent);
                let new = new.get(&path).cloned().unwrap_or(GitPathImage::Absent);
                (old != new).then(|| GitPathTransition::new(&path, old, new))
            })
            .collect()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "split recovery validates each persisted and live authority independently"
    )]
    fn apply_prepared_split_chain(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        pre_authority: &MappingAuthoritySnapshot,
        post_authority: &MappingAuthoritySnapshot,
        quarantine: &GitObjectQuarantine,
        prepared: &PreparedSplitCommit,
        transitions: Vec<GitPathTransition>,
        mapping_store: &mut MappingStore,
        publication: Option<&TargetPublicationSnapshot>,
    ) -> RailResult<()> {
        let target = SystemGit::open(&config.target_repo_path)?;
        let target_identity = crate::git::mappings::repository_identity_from_git(&target, pre_authority.target_head())?;
        if pre_authority.target_repository() != Some(target_identity.as_str()) {
            return Err(split_mapping_authority_changed_error("target repository identity"));
        }
        let store = GitEffectStore::open(&target)?;
        let ref_name = format!("refs/heads/{}", config.branch);
        let repository = store.capture_repository_authority(
            &target,
            target_identity,
            ref_name,
            pre_authority.target_head().map(str::to_string),
            prepared.oid.clone(),
        )?;
        let mapping = GitMappingBinding::new(
            pre_authority.owner().to_string(),
            pre_authority.ownership_snapshot().to_string(),
            pre_authority.digest(),
            post_authority.digest(),
            None,
            0,
        );
        let commit = GitCommitEffect::new(
            prepared.oid.clone(),
            prepared.tree.clone(),
            prepared.parents.clone(),
            prepared.message.clone(),
            GitEffectCommitMetadata::from(&prepared.metadata),
        );
        let publication = publication
            .map(|publication| -> RailResult<GitPublicationEffect> {
                let remote_url = config
                    .remote_url
                    .as_deref()
                    .ok_or_else(|| RailError::message("checked split publication has no configured remote endpoint"))?;
                Ok(GitPublicationEffect::new(
                    publication.remote_repository().to_string(),
                    remote_endpoint_identity(remote_url)?,
                    format!("refs/heads/{}", config.branch),
                    publication.remote_head().map(str::to_string),
                    prepared.oid.clone(),
                ))
            })
            .transpose()?;
        let mut bundle = store.create_object_bundle_temp()?;
        let bundle_digest = quarantine.write_pack(&prepared.oid, pre_authority.target_head(), bundle.file_mut()?)?;
        let intent = GitEffectIntent::new(
            format!("split-chain-{}", post_authority.digest()),
            repository,
            Some(commit),
            transitions,
            Some(mapping),
            publication,
            Some(bundle_digest.clone()),
        )?;
        let effect_id = intent.effect_id()?;
        drop(bundle.persist(&effect_id, &bundle_digest)?);

        pre_authority
            .revalidate_split_repository_state(self.ctx.workspace_root(), &config.target_repo_path)
            .map_err(|error| {
                RailError::with_help(
                    format!("split mapping authority changed after it was checked: {error}"),
                    "retry after the source and target repositories stop changing",
                )
            })?;
        self.revalidate_clean_target(&config.target_repo_path)?;
        let record = store.prepare(intent)?;
        self.reconcile_prepared_split_record(
            config,
            origin,
            pre_authority,
            post_authority,
            &store,
            record,
            mapping_store,
        )?;
        Ok(())
    }

    fn capture_current_split_authority(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
    ) -> RailResult<(MappingStore, MappingAuthoritySnapshot)> {
        let target = SystemGit::open(&config.target_repo_path)?;
        let target_ref = format!("refs/heads/{}", config.branch);
        if target.exact_branch_ref_oid(&target_ref)?.is_none() {
            return Ok((
                MappingStore::new(config.crate_name.clone()),
                MappingAuthoritySnapshot::empty_initialized(
                    self.ctx.workspace_root(),
                    origin,
                    &config.target_repo_path,
                    config.path_capabilities.target_root(),
                    &config.branch,
                    "mono_to_remote",
                )?,
            ));
        }
        let target_identity = repository_identity(&config.target_repo_path)?;
        MappingStore::capture_v025_authority(
            self.ctx.workspace_root(),
            &config.target_repo_path,
            origin,
            &target_identity,
            config.path_capabilities.target_root(),
            &config.branch,
            "mono_to_remote",
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "split commit preparation binds every exact repository transition"
    )]
    fn reconcile_prepared_split_record(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        pre_authority: &MappingAuthoritySnapshot,
        post_authority: &MappingAuthoritySnapshot,
        store: &GitEffectStore,
        record: GitEffectRecord,
        mapping_store: &mut MappingStore,
    ) -> RailResult<()> {
        match record {
            GitEffectRecord::Active(mut active) => {
                self.reconcile_prepared_split_journal(
                    config,
                    origin,
                    pre_authority,
                    post_authority,
                    store,
                    active.journal(),
                    mapping_store,
                )?;
                active.mark_local_applied()?;
                if active.journal().publication().is_some() {
                    self.reconcile_split_publication_journal(config, origin, store, active.journal())?;
                    active.mark_published()?;
                }
                let _completed = active.finish()?;
                Ok(())
            }
            GitEffectRecord::Completed(completed) => {
                self.reconcile_prepared_split_journal(
                    config,
                    origin,
                    pre_authority,
                    post_authority,
                    store,
                    completed.journal(),
                    mapping_store,
                )?;
                if completed.journal().publication().is_some() {
                    self.reconcile_split_publication_journal(config, origin, store, completed.journal())?;
                }
                Ok(())
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "split journal reconciliation compares every captured authority"
    )]
    fn reconcile_prepared_split_journal(
        &self,
        config: &SplitParams,
        origin: &OriginContext,
        pre_authority: &MappingAuthoritySnapshot,
        post_authority: &MappingAuthoritySnapshot,
        store: &GitEffectStore,
        journal: &GitEffectJournal,
        mapping_store: &mut MappingStore,
    ) -> RailResult<()> {
        let target = SystemGit::open(&config.target_repo_path)?;
        let repository = journal.repository();
        let mapping = journal
            .mapping()
            .ok_or_else(|| RailError::message("prepared split effect has no mapping authority"))?;
        let commit = journal
            .commit()
            .ok_or_else(|| RailError::message("prepared split effect has no commit"))?;
        let bundle_digest = journal
            .object_bundle_digest()
            .ok_or_else(|| RailError::message("prepared split effect has no object bundle"))?;
        let contract_changed = [
            !journal.operation_id().starts_with("split-chain-sha256-"),
            mapping.owner() != pre_authority.owner(),
            mapping.ownership_snapshot() != pre_authority.ownership_snapshot(),
            mapping.pre_authority() != pre_authority.digest(),
            mapping.post_authority() != post_authority.digest(),
            mapping.migration_count() != 0,
            mapping.migration_digest().is_some(),
            repository.ref_name != format!("refs/heads/{}", config.branch),
            repository.logical_repository != pre_authority.target_repository().unwrap_or_default(),
            repository.expected_oid.as_deref() != pre_authority.target_head(),
            repository.result_oid != commit.oid(),
            post_authority.target_head() != Some(commit.oid()),
        ]
        .into_iter()
        .any(|changed| changed);
        if contract_changed {
            return Err(RailError::message(
                "prepared split journal does not match the checked split authority",
            ));
        }
        for transition in journal.paths() {
            config.path_capabilities.authorize_target(transition.path())?;
        }

        let current_head = target.exact_branch_ref_oid(&repository.ref_name)?;
        if current_head.as_deref() != repository.expected_oid.as_deref()
            && current_head.as_deref() != Some(repository.result_oid.as_str())
        {
            return Err(RailError::message("prepared split branch changed to a third ref state"));
        }
        let current_repository = store.capture_repository_authority(
            &target,
            repository.logical_repository.clone(),
            repository.ref_name.clone(),
            current_head.clone(),
            repository.result_oid.clone(),
        )?;
        if current_repository.common_dir_identity != repository.common_dir_identity
            || current_repository.worktree_identity != repository.worktree_identity
            || current_repository.object_format != repository.object_format
            || current_repository.ref_name != repository.ref_name
            || current_repository.symbolic_head != repository.symbolic_head
        {
            return Err(RailError::message(
                "prepared split repository authority changed during recovery",
            ));
        }
        let actual_authority = self.capture_current_split_authority(config, origin)?;
        if current_head.as_deref() == repository.expected_oid.as_deref() {
            if actual_authority.1 != *pre_authority {
                return Err(split_mapping_authority_changed_error("prepared pre-effect recovery"));
            }
            self.validate_split_path_images(&target, journal, true)?;
        } else if actual_authority.1 != *post_authority {
            return Err(split_mapping_authority_changed_error("prepared post-ref recovery"));
        }

        let bundle = store
            .open_object_bundle(journal.effect_id(), bundle_digest)?
            .ok_or_else(|| RailError::message("prepared split object bundle disappeared"))?;
        target.install_prepared_object_pack_and_update_ref(
            bundle.into_file(),
            &store.object_bundle_path(journal.effect_id())?,
            bundle_digest,
            commit,
            &repository.ref_name,
            repository.expected_oid.as_deref(),
            journal.effect_id(),
        )?;
        #[cfg(test)]
        {
            fail_split_after_ref_cas()?;
        }
        if !journal.matches_repository_authority(store, &target, Some(repository.result_oid.clone()))? {
            return Err(RailError::message(
                "prepared split repository authority changed before final materialization",
            ));
        }
        self.validate_split_journal_tree_images(&target, journal)?;
        let paths = journal
            .paths()
            .iter()
            .map(|transition| transition.path().to_path_buf())
            .collect::<Vec<_>>();
        target.reconcile_prepared_commit_paths(repository.expected_oid.as_deref(), &repository.result_oid, &paths)?;
        post_authority
            .revalidate_split_repository_state(self.ctx.workspace_root(), &config.target_repo_path)
            .map_err(|error| {
                RailError::with_help(
                    format!("split mapping authority changed after materialization: {error}"),
                    "retry after the source and target repositories stop changing",
                )
            })?;
        *mapping_store = MappingStore::from_current_snapshot(post_authority)?;
        Ok(())
    }

    fn validate_split_path_images(
        &self,
        target: &SystemGit,
        journal: &GitEffectJournal,
        require_old: bool,
    ) -> RailResult<()> {
        let paths = journal
            .paths()
            .iter()
            .map(|transition| transition.path().to_path_buf())
            .collect::<Vec<_>>();
        let images = target.exact_path_images(&paths)?;
        for (transition, images) in journal.paths().iter().zip(images) {
            let index_old = git_entry_matches_image(images.index.as_ref(), transition.old());
            let worktree_old = git_entry_matches_image(images.worktree.as_ref(), transition.old());
            let index_new = git_entry_matches_image(images.index.as_ref(), transition.new_image());
            let worktree_new = git_entry_matches_image(images.worktree.as_ref(), transition.new_image());
            let accepted = if require_old {
                index_old && worktree_old
            } else {
                (index_old || index_new) && (worktree_old || worktree_new)
            };
            if !accepted {
                return Err(RailError::with_help(
                    format!(
                        "prepared split path '{}' changed to an unauthorized state",
                        transition.path().display()
                    ),
                    "cargo-rail preserved the target bytes; restore the exact old or prepared path image before retrying",
                ));
            }
        }
        Ok(())
    }

    fn validate_split_journal_tree_images(&self, target: &SystemGit, journal: &GitEffectJournal) -> RailResult<()> {
        let repository = journal.repository();
        let paths = journal
            .paths()
            .iter()
            .map(|transition| transition.path().to_path_buf())
            .collect::<Vec<_>>();
        let old = repository
            .expected_oid
            .as_deref()
            .map(|expected| target.collect_tree_entries_for_paths(expected, &paths))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        let new = target
            .collect_tree_entries_for_paths(&repository.result_oid, &paths)?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<std::collections::BTreeMap<_, _>>();
        for transition in journal.paths() {
            if !git_entry_matches_image(old.get(transition.path()), transition.old())
                || !git_entry_matches_image(new.get(transition.path()), transition.new_image())
            {
                return Err(RailError::message(format!(
                    "prepared split journal path '{}' disagrees with its exact old or result tree",
                    transition.path().display()
                )));
            }
        }
        Ok(())
    }

    fn validate_target_repo(&self, paths: &SplitPathCapabilities, branch: &str) -> RailResult<()> {
        let target_path = paths.authorize_target(paths.target_root())?;
        let git_dir = target_path.join(".git");
        if !git_dir.exists() {
            return Err(RailError::with_help(
                format!(
                    "split target '{}' is not an initialized Git repository",
                    target_path.display()
                ),
                format!(
                    "initialize it explicitly with: git init -b {branch} '{}'",
                    target_path.display()
                ),
            ));
        }
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

        Ok(())
    }

    /// Render the auxiliary combined-workspace manifest from the exact bound
    /// source commit without touching the target worktree.
    fn render_workspace_cargo_toml(&self, crate_paths: &[PathBuf], source_head: &str) -> RailResult<Vec<u8>> {
        // Extract workspace members from crate paths
        let members: Vec<String> = crate_paths.iter().map(|p| p.to_string_lossy().to_string()).collect();

        let source_git = self.ctx.git()?.git();
        let source_manifest = source_git
            .tree_entry(source_head, Path::new("Cargo.toml"))?
            .ok_or_else(|| RailError::message("bound source commit has no workspace Cargo.toml"))?;
        let source_bytes = source_git
            .read_blobs_bulk(&[source_manifest.object_id.as_str()])?
            .into_iter()
            .next()
            .ok_or_else(|| RailError::message("bound workspace Cargo.toml blob is unavailable"))?;
        let source_content = std::str::from_utf8(&source_bytes)
            .map_err(|_| RailError::message("bound workspace Cargo.toml is not valid UTF-8"))?;

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

        progress!("   Created workspace Cargo.toml with {} members", members.len());
        Ok(doc.to_string().into_bytes())
    }
}

fn git_entry_matches_image(entry: Option<&GitTreeEntry>, image: &GitPathImage) -> bool {
    match (entry, image.entry_parts()) {
        (None, None) => true,
        (Some(entry), Some((mode, object_id))) => entry.mode == mode && entry.object_id == object_id,
        _ => false,
    }
}

#[cfg(test)]
std::thread_local! {
    static FAIL_SPLIT_AFTER_REF_CAS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_split_after_ref_cas() -> RailResult<()> {
    if FAIL_SPLIT_AFTER_REF_CAS.replace(false) {
        Err(RailError::message("injected interruption after prepared split ref CAS"))
    } else {
        Ok(())
    }
}

fn source_commits_matching_policy(
    ctx: &WorkspaceContext,
    paths: &SplitPathCapabilities,
    excluded: &[&str],
    source_head: &str,
) -> RailResult<Vec<CommitInfo>> {
    let git = ctx.git()?.git();
    let commits = git.get_commits_excluding(excluded, source_head)?;
    let shas = commits.iter().map(|commit| commit.sha.clone()).collect::<Vec<_>>();
    let changes = git.get_changed_files_bulk(&shas)?;
    let mut selected = Vec::new();
    for (commit, changed) in commits.into_iter().zip(changes) {
        let mut owned = changes_include_owned_path(paths, changed)?;
        if !owned && commit.parent_shas.len() > 1 {
            for parent in &commit.parent_shas {
                if merge_parent_changes_include_owned_path(git, parent, &commit.sha, paths)? {
                    owned = true;
                    break;
                }
            }
        }
        if owned {
            selected.push(commit);
        }
    }
    Ok(selected)
}

fn changes_include_owned_path(paths: &SplitPathCapabilities, changed: Vec<(PathBuf, char)>) -> RailResult<bool> {
    for (path, _) in changed {
        if paths.owns_source_path(&path)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn merge_parent_changes_include_owned_path(
    git: &SystemGit,
    parent: &str,
    commit: &str,
    paths: &SplitPathCapabilities,
) -> RailResult<bool> {
    changes_include_owned_path(paths, git.get_changed_files_between(parent, Some(commit))?)
}

fn collect_owned_source_entries(
    git: &SystemGit,
    commit: &str,
    paths: &SplitPathCapabilities,
) -> RailResult<Vec<GitTreeEntry>> {
    let mut entries = Vec::new();
    for entry in git.collect_tree_entries(commit, Path::new("."))? {
        if paths.owns_source_path(&entry.path)? {
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

/// Prove that every ownership-less/predecessor exact mapping still represents
/// the current split projection before it can be stamped with current v2
/// ownership authority. This is read-only and covers notes, weak trailers, and
/// exact v1 pairs alike.
#[expect(
    clippy::too_many_arguments,
    reason = "projection validation binds each captured transform and path authority explicitly"
)]
pub(crate) fn validate_predecessor_mapping_projections(
    ctx: &WorkspaceContext,
    transform: &ManifestTransformPolicy,
    crate_paths: &[PathBuf],
    path_capabilities: &SplitPathCapabilities,
    target_repo_path: &Path,
    mode: &SplitMode,
    workspace_mode: &WorkspaceMode,
    authority: &MappingAuthoritySnapshot,
) -> RailResult<()> {
    use std::collections::BTreeMap;

    if authority.count() == 0 {
        return Ok(());
    }
    let source_git = ctx.git()?.git();
    let target_git = SystemGit::open(target_repo_path)?;
    for (source, target) in authority.migration_candidate_pairs() {
        let mut expected = BTreeMap::new();
        for entry in collect_owned_source_entries(source_git, &source, path_capabilities)? {
            let target_path = match mode {
                SplitMode::Single => crate_paths
                    .iter()
                    .find_map(|crate_path| entry.path.strip_prefix(crate_path).ok().map(Path::to_path_buf))
                    .unwrap_or_else(|| entry.path.clone()),
                SplitMode::Combined => entry.path.clone(),
            };
            let expected_content = if entry.path.file_name() == Some(std::ffi::OsStr::new("Cargo.toml")) {
                let content = source_git
                    .read_blobs_bulk(&[entry.object_id.as_str()])?
                    .into_iter()
                    .next()
                    .ok_or_else(|| RailError::message("predecessor source manifest has no blob"))?;
                let content = std::str::from_utf8(&content)
                    .map_err(|_| RailError::message("predecessor source manifest is not valid UTF-8"))?;
                let target_has_workspace = *mode == SplitMode::Combined && *workspace_mode == WorkspaceMode::Workspace;
                Some(
                    transform
                        .transform_to_split(content, target_has_workspace)?
                        .into_bytes(),
                )
            } else {
                None
            };
            if expected
                .insert(target_path, (entry.mode, entry.object_id, expected_content))
                .is_some()
            {
                return Err(RailError::message(
                    "current split ownership maps multiple source paths to one predecessor target path",
                ));
            }
        }

        let actual_entries = target_git.collect_tree_entries(&target, Path::new("."))?;
        let actual = actual_entries
            .iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        if actual.len() != expected.len() || actual.keys().ne(expected.keys()) {
            return Err(RailError::with_help(
                "predecessor mapping does not match the current owned split projection",
                "restore the ownership policy used by the predecessor mapping or rebuild the split history explicitly; cargo-rail will not stamp guessed v2 authority",
            ));
        }
        for (path, (expected_mode, expected_object, expected_content)) in expected {
            let entry = actual[&path];
            let content_matches = if let Some(expected_content) = expected_content {
                target_git
                    .read_blobs_bulk(&[entry.object_id.as_str()])?
                    .first()
                    .is_some_and(|actual_content| actual_content == &expected_content)
            } else {
                entry.object_id == expected_object
            };
            if entry.mode != expected_mode || !content_matches {
                return Err(RailError::with_help(
                    format!(
                        "predecessor mapping has a different current-owned projection at '{}'",
                        path.display()
                    ),
                    "restore the ownership and transform policy used by the predecessor mapping or rebuild the split history explicitly",
                ));
            }
        }
    }
    Ok(())
}

fn split_mapping_authority_changed_error(boundary: &str) -> RailError {
    RailError::with_help(
        format!("split mapping authority changed after it was checked at the {boundary} boundary"),
        "retry after the source and target repositories stop changing",
    )
}
