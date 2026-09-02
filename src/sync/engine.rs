//! Bidirectional sync engine between monorepo and split repositories.
//!
//! Coordinates commit mapping, conflict detection/resolution, and Cargo.toml
//! transforms while preserving deterministic sync behavior.

use crate::cargo::ManifestTransformPolicy;
use crate::config::{SplitMode, WorkspaceMode};
use crate::error::RailResult;
use crate::git::mappings::{
    MappingAuthoritySnapshot, MappingStore, OriginContext, TargetPublicationSnapshot, append_origin_trailers,
    is_ancestor, migrate_v025_receipt_message, observe_target_branch, remote_endpoint_identity,
    remote_repository_identity, repository_identity,
};
use crate::git::ops::{GitIndexChange, GitObjectQuarantine, GitTreeEntry};
use crate::git::{CommitInfo, CommitMetadata, SystemGit};
use crate::mutation::git_effect::{
    GitCommitEffect, GitEffectCommitMetadata, GitEffectIntent, GitEffectJournal, GitEffectRecord, GitEffectStore,
    GitMappingBinding, GitPathImage, GitPathTransition, GitPublicationEffect, ordered_mapping_effect_indices,
};
use crate::split::{SplitOwnership, SplitPathCapabilities};
use crate::sync::conflict::{ConflictClass, ConflictInfo, ConflictResolver, ConflictStrategy};
use crate::utils;
use crate::verbose_progress as progress;
use crate::workspace::WorkspaceContext;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Configuration for sync operation
#[derive(Debug, Clone)]
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
#[derive(Debug)]
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

impl SyncDirection {
    pub(crate) fn authority_name(&self) -> &'static str {
        match self {
            Self::MonoToRemote => "mono_to_remote",
            Self::RemoteToMono => "remote_to_mono",
            Self::Both => "bidirectional",
            Self::None => "none",
        }
    }
}

pub(crate) fn selected_sync_source_head(
    source_repo: &Path,
    crate_name: &str,
    direction: &str,
) -> RailResult<Option<String>> {
    if !matches!(direction, "remote_to_mono" | "bidirectional") {
        return Ok(None);
    }
    let review_ref = format!("refs/heads/cargo-rail-sync-{crate_name}");
    SystemGit::open(source_repo)?.exact_branch_ref_oid(&review_ref)
}

/// Result of conflict resolution containing both conflict info and changed files
/// Changed files are cached for reuse in the apply step to avoid redundant git calls
#[derive(Debug)]
pub struct ConflictResolutionResult {
    /// Conflict information for files that had merge conflicts
    pub conflicts: Vec<ConflictInfo>,
    /// Paths already materialized by a merge strategy and not to overwrite.
    pub resolved_files: Vec<PathBuf>,
    /// Exact merged bytes prepared outside the real worktree.
    pub resolved_contents: HashMap<PathBuf, Vec<u8>>,
    /// Changed files from the commit (cached to avoid redundant git calls)
    pub changed_files: Vec<(PathBuf, char)>,
}

#[derive(Debug)]
struct ConflictMaterialization {
    digest: String,
    transitions: Vec<GitPathTransition>,
    entries: BTreeMap<PathBuf, MaterializedEntry>,
}

#[derive(Debug)]
struct MaterializedEntry {
    mode: String,
    content: Vec<u8>,
}

type CommitWithChanges = (CommitInfo, Vec<(PathBuf, char)>);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncConflictReceipt {
    schema_version: u32,
    status: String,
    crate_name: String,
    #[serde(default)]
    effect_payload_digest: Option<String>,
    #[serde(default)]
    materialization_digest: Option<String>,
    #[serde(default)]
    conflict_strategy: Option<String>,
    #[serde(default)]
    mapping_authority_direction: Option<String>,
    #[serde(default)]
    mapping_authority_digest: Option<String>,
    #[serde(default)]
    mapping_authority_ownership_snapshot: Option<String>,
    #[serde(default)]
    mapping_authority_target_head: Option<String>,
    #[serde(default)]
    mapping_authority_post_digest: Option<String>,
    #[serde(default)]
    mapping_authority_migration_digest: Option<String>,
    #[serde(default)]
    mapping_authority_migration_count: Option<usize>,
    #[serde(default)]
    publication_authority_present: Option<bool>,
    #[serde(default)]
    publication_authority_digest: Option<String>,
    #[serde(default)]
    publication_remote_repository: Option<String>,
    #[serde(default)]
    publication_remote_head: Option<String>,
    #[serde(default)]
    publication_local_head: Option<String>,
    #[serde(default)]
    publication_relation: Option<String>,
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

/// Exact durable conflict receipt written by v0.25.0. It is decoded only for
/// a one-time, read-only authority reconstruction and is persisted as schema 3
/// before any resumed mutation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct V025SyncConflictReceipt {
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

impl From<V025SyncConflictReceipt> for SyncConflictReceipt {
    fn from(receipt: V025SyncConflictReceipt) -> Self {
        Self {
            schema_version: 3,
            status: receipt.status,
            crate_name: receipt.crate_name,
            effect_payload_digest: None,
            materialization_digest: None,
            conflict_strategy: None,
            mapping_authority_direction: None,
            mapping_authority_digest: None,
            mapping_authority_ownership_snapshot: None,
            mapping_authority_target_head: None,
            mapping_authority_post_digest: None,
            mapping_authority_migration_digest: None,
            mapping_authority_migration_count: None,
            publication_authority_present: None,
            publication_authority_digest: None,
            publication_remote_repository: None,
            publication_remote_head: None,
            publication_local_head: None,
            publication_relation: None,
            branch: receipt.branch,
            expected_head: receipt.expected_head,
            remote_commit: receipt.remote_commit,
            message: receipt.message,
            author: receipt.author,
            author_email: receipt.author_email,
            author_timestamp: receipt.author_timestamp,
            author_timezone: receipt.author_timezone,
            committer: receipt.committer,
            committer_email: receipt.committer_email,
            committer_timestamp: receipt.committer_timestamp,
            committer_timezone: receipt.committer_timezone,
            commit_paths: receipt.commit_paths,
            conflicts: receipt.conflicts,
            resulting_commit: receipt.resulting_commit,
        }
    }
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

    fn compute_effect_payload_digest(&self) -> RailResult<String> {
        let payload = serde_json::json!({
            "schema": 3,
            "crate_name": self.crate_name,
            "mapping_authority_direction": self.mapping_authority_direction,
            "materialization_digest": self.materialization_digest,
            "conflict_strategy": self.conflict_strategy,
            "branch": self.branch,
            "expected_head": self.expected_head,
            "remote_commit": self.remote_commit,
            "message": self.message,
            "author": self.author,
            "author_email": self.author_email,
            "author_timestamp": self.author_timestamp,
            "author_timezone": self.author_timezone,
            "committer": self.committer,
            "committer_email": self.committer_email,
            "committer_timestamp": self.committer_timestamp,
            "committer_timezone": self.committer_timezone,
            "commit_paths": self.commit_paths,
            "conflicts": self.conflicts,
        });
        let bytes = serde_json::to_vec(&payload).map_err(|error| {
            crate::error::RailError::message(format!("failed to bind sync receipt payload: {error}"))
        })?;
        Ok(format!("sha256-{}", crate::source::ContentDigest::sha256(&bytes)))
    }

    fn bind_effect_payload(&mut self) -> RailResult<()> {
        self.effect_payload_digest = Some(self.compute_effect_payload_digest()?);
        Ok(())
    }

    fn bind_mapping_authority(&mut self, authority: &MappingAuthoritySnapshot) {
        self.mapping_authority_direction = Some(authority.direction().to_string());
        self.mapping_authority_digest = Some(authority.digest());
        self.mapping_authority_ownership_snapshot = Some(authority.ownership_snapshot().to_string());
        self.mapping_authority_target_head = authority.target_head().map(str::to_string);
        self.mapping_authority_migration_digest = Some(authority.migration_digest());
        self.mapping_authority_migration_count = Some(authority.count());
    }

    fn bind_publication_authority(&mut self, publication: Option<&TargetPublicationSnapshot>) {
        self.publication_authority_present = Some(publication.is_some());
        self.publication_authority_digest = publication.map(TargetPublicationSnapshot::digest);
        self.publication_remote_repository = publication.map(|snapshot| snapshot.remote_repository().to_string());
        self.publication_remote_head = publication.and_then(|snapshot| snapshot.remote_head().map(str::to_string));
        self.publication_local_head = publication.and_then(|snapshot| snapshot.local_head().map(str::to_string));
        self.publication_relation = publication.map(|snapshot| snapshot.relation().to_string());
    }

    fn matches_publication_authority(&self, publication: Option<&TargetPublicationSnapshot>) -> bool {
        self.publication_authority_present == Some(publication.is_some())
            && publication.map(TargetPublicationSnapshot::digest) == self.publication_authority_digest
            && publication.map(|snapshot| snapshot.remote_repository().to_string())
                == self.publication_remote_repository
            && publication.and_then(|snapshot| snapshot.remote_head().map(str::to_string))
                == self.publication_remote_head
            && publication.and_then(|snapshot| snapshot.local_head().map(str::to_string)) == self.publication_local_head
            && publication.map(|snapshot| snapshot.relation().to_string()) == self.publication_relation
    }
}

/// Bidirectional sync engine
#[derive(Debug)]
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
    /// One-way predecessor migration preparation state.
    mapping_preparation: MappingPreparation,
    /// Whether the configured remote was observed before mapping capture.
    remote_observed: bool,
    /// Exact owned local-ahead publication state bound by the command plan.
    expected_publication: Option<TargetPublicationSnapshot>,
    /// Whether this invocation is authorized to mutate/publish target history.
    target_publication_authorized: bool,
    /// Cargo.toml transformer
    transform: ManifestTransformPolicy,
    /// Conflict resolver
    conflict_resolver: ConflictResolver,
}

#[derive(Debug)]
enum MappingPreparation {
    Unprepared { expected: Option<MappingAuthoritySnapshot> },
    Prepared(MappingAuthoritySnapshot),
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
        let transformer = ManifestTransformPolicy::capture(ctx)?;

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
            mapping_preparation: MappingPreparation::Unprepared { expected: None },
            remote_observed: false,
            expected_publication: None,
            target_publication_authorized: false,
            transform: transformer,

            conflict_resolver,
        })
    }

    fn load_mapping_evidence(&mut self, direction: &str) -> RailResult<MappingAuthoritySnapshot> {
        let (mapping_store, authority) = self.capture_mapping_evidence(direction)?;
        self.mapping_store = mapping_store;
        Ok(authority)
    }

    fn capture_mapping_evidence(&self, direction: &str) -> RailResult<(MappingStore, MappingAuthoritySnapshot)> {
        self.config.path_capabilities.validate_target_repository()?;
        let selected_source_head =
            selected_sync_source_head(self.ctx.workspace_root(), &self.config.crate_name, direction)?;
        let selected_target_head = if utils::is_local_path(&self.config.remote_url) {
            None
        } else {
            observe_target_branch(
                self.ctx.workspace_root(),
                &self.config.target_repo_path,
                &self.config.remote_url,
                &self.config.branch,
            )?
            .effective_head()
            .map(str::to_string)
        };
        let captured = if let Some(selected_source_head) = selected_source_head.as_deref() {
            MappingStore::capture_v025_authority_at_source(
                self.ctx.workspace_root(),
                &self.config.target_repo_path,
                &self.source_origin,
                self.target_origin.source_repository(),
                self.config.path_capabilities.target_root(),
                &self.config.branch,
                direction,
                selected_source_head,
                selected_target_head.as_deref(),
            )?
        } else if let Some(selected_target_head) = selected_target_head.as_deref() {
            MappingStore::capture_v025_authority_at(
                self.ctx.workspace_root(),
                &self.config.target_repo_path,
                &self.source_origin,
                self.target_origin.source_repository(),
                self.config.path_capabilities.target_root(),
                &self.config.branch,
                direction,
                selected_target_head,
            )?
        } else {
            MappingStore::capture_v025_authority(
                self.ctx.workspace_root(),
                &self.config.target_repo_path,
                &self.source_origin,
                self.target_origin.source_repository(),
                self.config.path_capabilities.target_root(),
                &self.config.branch,
                direction,
            )?
        };
        self.config.path_capabilities.validate_target_repository()?;
        Ok(captured)
    }

    pub(crate) fn bind_origin_migration(&mut self, expected: MappingAuthoritySnapshot) -> RailResult<()> {
        let MappingPreparation::Unprepared {
            expected: current_expected,
        } = &mut self.mapping_preparation
        else {
            return Err(crate::error::RailError::message(
                "sync mapping authority cannot be rebound after preparation",
            ));
        };
        if current_expected.is_some() {
            return Err(crate::error::RailError::message(
                "sync mapping authority is already bound",
            ));
        }
        *current_expected = Some(expected);
        // Command-owned snapshots are captured only after their one remote
        // observation. Bound engines must not fetch a second, unplanned view.
        self.remote_observed = true;
        Ok(())
    }

    pub(crate) fn bind_publication(&mut self, expected: Option<TargetPublicationSnapshot>) -> RailResult<()> {
        if self.expected_publication.is_some() {
            return Err(crate::error::RailError::message(
                "sync target publication authority is already bound",
            ));
        }
        self.expected_publication = expected;
        Ok(())
    }

    fn observe_remote_before_mapping_capture(&mut self) -> RailResult<()> {
        if self.remote_observed {
            return Ok(());
        }
        if !utils::is_local_path(&self.config.remote_url) {
            let target = SystemGit::open(&self.config.target_repo_path)?;
            if !target.obstructing_worktree_paths()?.is_empty() {
                return Err(crate::error::RailError::with_help(
                    "sync target became dirty before remote observation",
                    "commit or restore target work and retry; cargo-rail will not fetch or mutate a dirty target",
                ));
            }
            if crate::git::mappings::repository_identity(&self.config.target_repo_path)?
                != remote_repository_identity(&self.config.remote_url)?
            {
                return Err(crate::error::RailError::with_help(
                    "configured sync remote does not match the target repository identity",
                    "restore remote.origin.url to the configured remote before retrying",
                ));
            }
            let observation = observe_target_branch(
                self.ctx.workspace_root(),
                &self.config.target_repo_path,
                &self.config.remote_url,
                &self.config.branch,
            )?;
            if let Some(remote_head) = observation.remote_head()
                && target.get_commit(remote_head).is_err()
            {
                return Err(crate::error::RailError::with_help(
                    format!(
                        "configured remote branch commit '{}' is absent from the local target object view",
                        remote_head
                    ),
                    format!(
                        "fetch it explicitly, for example: git -C '{}' fetch --no-tags <configured-url> refs/heads/{}",
                        self.config.target_repo_path.display(),
                        self.config.branch
                    ),
                ));
            }
        }
        self.remote_observed = true;
        Ok(())
    }

    fn prepare_mapping_evidence(&mut self, direction: &str) -> RailResult<()> {
        let expected = match &self.mapping_preparation {
            MappingPreparation::Unprepared { expected } => expected.clone(),
            MappingPreparation::Prepared(_) => return self.revalidate_prepared_mapping_evidence(direction),
        };
        let recovered_pre = self.resume_prepared_sync_effect(direction, expected.as_ref())?;
        let captured = self.load_mapping_evidence(direction)?;
        if expected.as_ref().is_some_and(|expected| {
            expected != &captured && recovered_pre.as_deref() != Some(expected.digest().as_str())
        }) {
            return Err(crate::error::RailError::with_help(
                "sync mapping authority changed after the operation was planned",
                "restart sync from a fresh check/apply plan after repository histories and mapping refs stop changing",
            ));
        }
        crate::split::engine::validate_predecessor_mapping_projections(
            self.ctx,
            &self.transform,
            &self.config.crate_paths,
            &self.config.path_capabilities,
            &self.config.target_repo_path,
            &self.config.mode,
            &self.config.workspace_mode,
            &captured,
        )?;
        self.validate_unproven_exact_pair_ancestry(direction, captured.source_head(), captured.target_selected_head())?;
        self.revalidate_publication_before_effect(direction, captured.count() > 0)?;
        if captured.count() == 0 {
            self.mapping_preparation = MappingPreparation::Prepared(captured);
            return Ok(());
        }
        self.config.path_capabilities.validate_target_repository()?;
        let obstructing = SystemGit::open(&self.config.target_repo_path)?.obstructing_worktree_paths()?;
        if !obstructing.is_empty() {
            return Err(crate::error::RailError::with_help(
                "sync target became dirty before predecessor migration",
                "commit, restore, or remove staged, unstaged, untracked, and ignored target paths before retrying",
            ));
        }
        if self
            .mapping_store
            .migrate_v025_evidence_bound(
                self.ctx.workspace_root(),
                &self.config.target_repo_path,
                &self.source_origin,
                self.target_origin.source_repository(),
                self.config.path_capabilities.target_root(),
                &self.config.branch,
                direction,
                expected.as_ref(),
            )?
            .is_some()
        {
            progress!("   Migrated predecessor mappings into ordinary Git history");
        }
        self.config.path_capabilities.validate_target_repository()?;
        let authority = self.mapping_store.mapping_authority_snapshot(
            direction,
            self.config.path_capabilities.target_root(),
            &self.config.branch,
        )?;
        if authority.count() > 0 {
            return Err(pending_origin_migration_after_preparation());
        }
        let actual = self.load_mapping_evidence(direction)?;
        if actual != authority {
            return Err(crate::error::RailError::with_help(
                "sync mapping authority changed during predecessor migration preparation",
                "restart sync from a fresh check/apply plan after repository histories and mapping refs stop changing",
            ));
        }
        self.mapping_preparation = MappingPreparation::Prepared(actual);
        Ok(())
    }

    fn resume_prepared_sync_effect(
        &mut self,
        direction: &str,
        expected: Option<&MappingAuthoritySnapshot>,
    ) -> RailResult<Option<String>> {
        let source = self.ctx.git()?.git().clone();
        let target = SystemGit::open(&self.config.target_repo_path)?;
        let to_prefix = format!("sync-to-remote-{}-", self.config.crate_name);
        let from_prefix = format!("sync-from-remote-{}-", self.config.crate_name);
        let publication_prefix = format!("sync-publication-{}-", self.config.crate_name);

        let target_journals = GitEffectStore::discover_unacknowledged_read_only(&target)?;
        let mut publications = target_journals
            .iter()
            .filter(|journal| journal.operation_id().starts_with(&publication_prefix))
            .cloned()
            .collect::<Vec<_>>();
        if publications.len() > 1 {
            return Err(crate::error::RailError::message(
                "sync target has multiple unacknowledged publication effects",
            ));
        }
        if let Some(journal) = publications.pop() {
            let store = GitEffectStore::open(&target)?;
            let record = store.resume(journal.effect_id())?;
            self.reconcile_target_publication_record(&store, record)?;
        }

        let effects = self.ordered_prepared_sync_effects(&source, &target, &from_prefix, &to_prefix)?;
        let Some((terminal_git, terminal_mutates_target, terminal_journal)) = effects.last() else {
            return Ok(None);
        };
        let first_mapping = effects[0]
            .2
            .mapping()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no mapping authority"))?;
        if expected.is_none_or(|expected| expected.digest() != first_mapping.pre_authority()) {
            return Err(sync_mapping_authority_changed_error());
        }

        for (index, (git, mutates_target, journal)) in effects.iter().enumerate().take(effects.len() - 1) {
            let has_later_effect_in_worktree = effects[index + 1..].iter().any(|(_, _, candidate)| {
                candidate.repository().common_dir_identity == journal.repository().common_dir_identity
                    && candidate.repository().worktree_identity == journal.repository().worktree_identity
            });
            self.validate_subsumed_sync_commit_journal(journal, git, *mutates_target, !has_later_effect_in_worktree)?;
        }

        let terminal_mapping = terminal_journal
            .mapping()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no mapping authority"))?;
        let pre = first_mapping.pre_authority().to_string();
        let terminal_pre = terminal_mapping.pre_authority().to_string();
        let post = terminal_mapping.post_authority().to_string();
        let store = GitEffectStore::open(terminal_git)?;
        let record = store.resume(terminal_journal.effect_id())?;
        self.reconcile_sync_commit_record(
            direction,
            &terminal_pre,
            &post,
            &store,
            record,
            terminal_git,
            *terminal_mutates_target,
            false,
        )?;
        Ok(Some(pre))
    }

    fn ordered_prepared_sync_effects(
        &self,
        source: &SystemGit,
        target: &SystemGit,
        from_prefix: &str,
        to_prefix: &str,
    ) -> RailResult<Vec<(SystemGit, bool, GitEffectJournal)>> {
        let effects = GitEffectStore::discover_unacknowledged_read_only(target)?
            .into_iter()
            .filter(|journal| journal.operation_id().starts_with(to_prefix))
            .map(|journal| (target.clone(), true, journal))
            .chain(
                GitEffectStore::discover_unacknowledged_read_only(source)?
                    .into_iter()
                    .filter(|journal| journal.operation_id().starts_with(from_prefix))
                    .map(|journal| (source.clone(), false, journal)),
            )
            .collect::<Vec<_>>();
        let journals = effects
            .iter()
            .map(|(_, _, journal)| journal.clone())
            .collect::<Vec<_>>();
        let order = ordered_mapping_effect_indices(&journals)?;
        let ordered = order
            .into_iter()
            .map(|index| effects[index].clone())
            .collect::<Vec<_>>();
        let mut latest_by_worktree = BTreeMap::<(&str, &str), &GitEffectJournal>::new();
        for (index, (_, _, journal)) in ordered.iter().enumerate() {
            let mapping = journal
                .mapping()
                .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no mapping authority"))?;
            if mapping.owner() != self.config.crate_name
                || mapping.ownership_snapshot() != self.config.ownership.snapshot_id
                || mapping.migration_count() != 0
                || mapping.migration_digest().is_some()
                || journal.publication().is_some()
                || (index + 1 < ordered.len() && !journal.is_terminal())
            {
                return Err(sync_mapping_authority_changed_error());
            }
            let repository = journal.repository();
            let key = (
                repository.common_dir_identity.as_str(),
                repository.worktree_identity.as_str(),
            );
            if let Some(previous) = latest_by_worktree.insert(key, journal) {
                let previous_repository = previous.repository();
                if previous_repository.logical_repository != repository.logical_repository
                    || previous_repository.object_format != repository.object_format
                    || previous_repository.ref_name != repository.ref_name
                    || previous_repository.symbolic_head != repository.symbolic_head
                    || repository.expected_oid.as_deref() != Some(previous_repository.result_oid.as_str())
                {
                    return Err(crate::error::RailError::message(
                        "prepared sync effects have a broken repository transition chain",
                    ));
                }
            }
        }
        Ok(ordered)
    }

    fn capture_publication(&self) -> RailResult<Option<TargetPublicationSnapshot>> {
        if utils::is_local_path(&self.config.remote_url) {
            return Ok(None);
        }
        let observation = observe_target_branch(
            self.ctx.workspace_root(),
            &self.config.target_repo_path,
            &self.config.remote_url,
            &self.config.branch,
        )?;
        TargetPublicationSnapshot::capture(observation, &self.config.target_repo_path, Some(&self.mapping_store))
            .map(Some)
    }

    fn revalidate_publication_before_effect(
        &mut self,
        direction: &str,
        migration_will_mutate_target: bool,
    ) -> RailResult<()> {
        let actual = self.capture_publication()?;
        if let Some(expected) = &self.expected_publication {
            if actual.as_ref() != Some(expected)
                && !self.matches_prepared_publication_transition(expected, actual.as_ref())?
            {
                return Err(crate::error::RailError::with_help(
                    "sync target publication authority changed after the operation was planned",
                    "fetch and retry; cargo-rail will not publish against a changed remote branch or local ahead range",
                ));
            } else if actual.as_ref() != Some(expected) {
                self.expected_publication = actual.clone();
            }
        } else {
            self.expected_publication = actual.clone();
        }
        let owned_publication_retry = actual.as_ref().is_some_and(|snapshot| snapshot.count() > 0);
        let target_will_mutate = migration_will_mutate_target
            || owned_publication_retry
            || matches!(direction, "mono_to_remote" | "bidirectional");
        self.target_publication_authorized = target_will_mutate;
        if target_will_mutate
            && actual
                .as_ref()
                .is_some_and(|snapshot| !snapshot.permits_target_mutation())
        {
            return Err(crate::error::RailError::with_help(
                "local split target is behind its configured remote branch",
                "fast-forward the local target branch before running an operation that mutates or publishes target history",
            ));
        }
        Ok(())
    }

    fn matches_prepared_publication_transition(
        &self,
        expected: &TargetPublicationSnapshot,
        actual: Option<&TargetPublicationSnapshot>,
    ) -> RailResult<bool> {
        let Some(actual) = actual else {
            return Ok(false);
        };
        let target = SystemGit::open(&self.config.target_repo_path)?;
        let journals = GitEffectStore::discover_unacknowledged_read_only(&target)?;
        let mapping_journals = journals
            .iter()
            .filter(|journal| {
                journal
                    .operation_id()
                    .starts_with(&format!("sync-to-remote-{}-", self.config.crate_name))
                    && journal.mapping().is_some()
            })
            .cloned()
            .collect::<Vec<_>>();
        let mapping_order = ordered_mapping_effect_indices(&mapping_journals)?;
        let prepared_local_transition = mapping_order.first().zip(mapping_order.last()).map(|(first, last)| {
            (
                mapping_journals[*first].repository().expected_oid.as_deref(),
                mapping_journals[*last].repository().result_oid.as_str(),
            )
        });
        for journal in journals {
            if !journal.permits_local_recovery_state(&target)? {
                continue;
            }
            let repository = journal.repository();
            if let Some(publication) = journal.publication() {
                let (expected_local_head, result_local_head) = prepared_local_transition
                    .unwrap_or((repository.expected_oid.as_deref(), repository.result_oid.as_str()));
                if publication.exact_endpoint_digest() == remote_endpoint_identity(&self.config.remote_url)?
                    && expected.remote_repository() == publication.logical_remote()
                    && expected.remote_head() == publication.expected_oid()
                    && expected.local_head() == expected_local_head
                    && actual.remote_repository() == publication.logical_remote()
                    && actual.remote_head() == Some(publication.desired_oid())
                    && actual.local_head() == Some(result_local_head)
                    && result_local_head == publication.desired_oid()
                {
                    return Ok(true);
                }
            } else if journal
                .operation_id()
                .starts_with(&format!("sync-to-remote-{}-", self.config.crate_name))
                && expected.remote_repository() == actual.remote_repository()
                && expected.remote_head() == actual.remote_head()
                && expected.local_head() == repository.expected_oid.as_deref()
                && actual.local_head() == Some(repository.result_oid.as_str())
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn reconcile_target_publication(&mut self) -> RailResult<usize> {
        if !self.target_publication_authorized {
            return Ok(0);
        }
        let Some(expected) = self.expected_publication.clone() else {
            return Ok(0);
        };
        let actual = self
            .capture_publication()?
            .ok_or_else(|| crate::error::RailError::message("sync lost its configured publication authority"))?;
        if !expected.same_remote_authority(&actual) {
            return Err(crate::error::RailError::with_help(
                "configured remote branch advanced during sync",
                "fetch and retry; cargo-rail will not publish against a remote head different from the checked authority",
            ));
        }
        // Pure remote-to-monorepo work does not publish or mutate the split
        // target. A normally remote-ahead target therefore needs no
        // reconciliation. Target-mutating directions were rejected before
        // their first effect by `revalidate_publication_before_effect`.
        if !actual.permits_target_mutation() {
            self.expected_publication = Some(actual);
            return Ok(0);
        }
        let pending = actual.count();
        if pending == 0 {
            self.expected_publication = Some(actual);
            return Ok(0);
        }
        let desired = actual
            .local_head()
            .ok_or_else(|| crate::error::RailError::message("sync publication has no exact local commit"))?;
        let target = SystemGit::open(&self.config.target_repo_path)?;
        let store = GitEffectStore::open(&target)?;
        let ref_name = format!("refs/heads/{}", self.config.branch);
        let repository = store.capture_repository_authority(
            &target,
            repository_identity(&self.config.target_repo_path)?,
            ref_name.clone(),
            Some(desired.to_string()),
            desired.to_string(),
        )?;
        let publication = GitPublicationEffect::new(
            actual.remote_repository().to_string(),
            remote_endpoint_identity(&self.config.remote_url)?,
            ref_name,
            actual.remote_head().map(str::to_string),
            desired.to_string(),
        );
        let intent = GitEffectIntent::new(
            format!("sync-publication-{}-{}", self.config.crate_name, actual.digest()),
            repository,
            None,
            Vec::new(),
            None,
            Some(publication),
            None,
        )?;
        let record = store.prepare(intent)?;
        self.reconcile_target_publication_record(&store, record)?;
        let published = self
            .capture_publication()?
            .ok_or_else(|| crate::error::RailError::message("sync lost publication authority after push"))?;
        if published.count() != 0
            || published.remote_head() != published.local_head()
            || published.remote_repository() != actual.remote_repository()
        {
            return Err(crate::error::RailError::with_help(
                "sync push did not reconcile the configured remote branch",
                "inspect the local and remote branch heads before retrying",
            ));
        }
        self.expected_publication = Some(published);
        Ok(pending)
    }

    fn reconcile_target_publication_record(&self, store: &GitEffectStore, record: GitEffectRecord) -> RailResult<()> {
        match record {
            GitEffectRecord::Active(mut active) => {
                active.mark_local_applied()?;
                self.reconcile_target_publication_journal(store, active.journal())?;
                active.mark_published()?;
                let _completed = active.finish()?;
                Ok(())
            }
            GitEffectRecord::Completed(completed) => {
                self.reconcile_target_publication_journal(store, completed.journal())
            }
        }
    }

    fn reconcile_target_publication_journal(
        &self,
        store: &GitEffectStore,
        journal: &GitEffectJournal,
    ) -> RailResult<()> {
        let publication = journal
            .publication()
            .ok_or_else(|| crate::error::RailError::message("prepared sync publication has no remote authority"))?;
        let ref_name = format!("refs/heads/{}", self.config.branch);
        if journal.repository().ref_name != ref_name
            || publication.ref_name() != ref_name
            || publication.desired_oid() != journal.repository().result_oid
            || publication.logical_remote() != remote_repository_identity(&self.config.remote_url)?
            || publication.exact_endpoint_digest() != remote_endpoint_identity(&self.config.remote_url)?
        {
            return Err(crate::error::RailError::with_help(
                "prepared sync publication authority changed before publication",
                "restore the exact target repository, branch, and configured endpoint before retrying",
            ));
        }
        let target = SystemGit::open(&self.config.target_repo_path)?;
        if target.exact_branch_ref_oid(&ref_name)?.as_deref() != Some(publication.desired_oid()) {
            return Err(crate::error::RailError::message(
                "prepared sync publication local branch is not at its exact desired commit",
            ));
        }
        let actual = self.capture_publication()?.ok_or_else(|| {
            crate::error::RailError::message("prepared sync publication lost its configured remote authority")
        })?;
        if actual.remote_repository() != publication.logical_remote()
            || actual.local_head() != Some(publication.desired_oid())
        {
            return Err(crate::error::RailError::message(
                "prepared sync publication no longer matches its exact local or remote repository",
            ));
        }
        if actual.remote_head() == Some(publication.desired_oid()) {
            return Ok(());
        }
        if actual.remote_head() != publication.expected_oid() {
            return Err(crate::error::RailError::with_help(
                "prepared sync publication found a third remote ref state",
                "preserve the remote branch and reconcile it manually; cargo-rail will not overwrite an unjournaled commit",
            ));
        }
        if !journal.matches_repository_authority(store, &target, Some(publication.desired_oid().to_string()))? {
            return Err(crate::error::RailError::message(
                "prepared sync publication repository authority changed before push",
            ));
        }
        target.push_commit_to_url_with_lease(
            &self.config.remote_url,
            publication.ref_name(),
            publication.desired_oid(),
            publication.expected_oid(),
        )?;
        let published = self
            .capture_publication()?
            .ok_or_else(|| crate::error::RailError::message("prepared sync publication lost authority after push"))?;
        if published.remote_repository() != publication.logical_remote()
            || published.remote_head() != Some(publication.desired_oid())
            || published.local_head() != Some(publication.desired_oid())
            || published.count() != 0
        {
            return Err(crate::error::RailError::with_help(
                "prepared sync publication did not converge to its exact desired commit",
                "inspect the local and remote branch heads before retrying",
            ));
        }
        Ok(())
    }

    fn revalidate_prepared_mapping_evidence(&mut self, direction: &str) -> RailResult<()> {
        let MappingPreparation::Prepared(expected) = &self.mapping_preparation else {
            return Err(crate::error::RailError::message(
                "sync mapping evidence has not been prepared",
            ));
        };
        let expected = expected.clone();
        let actual = self.load_mapping_evidence(direction)?;
        if actual != expected {
            return Err(crate::error::RailError::with_help(
                "sync mapping authority changed after predecessor migration preparation",
                "restart sync from a fresh check/apply plan after repository histories and mapping refs stop changing",
            ));
        }
        Ok(())
    }

    fn seal_prepared_mapping_evidence(&mut self, direction: &str) -> RailResult<()> {
        let MappingPreparation::Prepared(previous) = &self.mapping_preparation else {
            return Err(crate::error::RailError::message(
                "sync mapping evidence has not been prepared",
            ));
        };
        let previous = previous.clone();
        let expected = self.mapping_store.mapping_authority_snapshot(
            direction,
            self.config.path_capabilities.target_root(),
            &self.config.branch,
        )?;
        if !expected.same_binding(&previous) {
            return Err(crate::error::RailError::with_help(
                "sync repository authority changed during the prepared operation",
                "restart sync from a fresh check/apply plan without changing repository identity, target root, branch, or ownership",
            ));
        }
        if expected.count() > 0 {
            return Err(pending_origin_migration_after_preparation());
        }
        self.mapping_preparation = MappingPreparation::Prepared(expected);
        self.revalidate_prepared_mapping_evidence(direction)
    }

    /// Classify whether the selected direction has mapped commits pending.
    pub fn has_pending_changes(&mut self, direction: &SyncDirection) -> RailResult<bool> {
        Ok(self.pending_commit_count(direction)? > 0)
    }

    /// Count mapped-history commits pending in the selected public direction.
    pub fn pending_commit_count(&mut self, direction: &SyncDirection) -> RailResult<usize> {
        let actual = self.load_mapping_evidence(direction.authority_name())?;
        if let MappingPreparation::Unprepared {
            expected: Some(expected),
        } = &self.mapping_preparation
            && &actual != expected
            && !self.matches_prepared_mapping_transition(expected, &actual)?
        {
            return Err(crate::error::RailError::with_help(
                "sync origin evidence changed after the operation was planned",
                "retry after the remote observation and repository histories stop changing",
            ));
        }
        self.validate_unproven_exact_pair_ancestry(
            direction.authority_name(),
            actual.source_head(),
            actual.target_selected_head(),
        )?;
        let source_head = actual.source_head().to_string();
        let target_head = actual.target_selected_head().map(str::to_string);
        match direction {
            SyncDirection::MonoToRemote => self.pending_mono_commits_at(&source_head),
            SyncDirection::RemoteToMono => self.pending_remote_commits_at(target_head.as_deref()),
            SyncDirection::Both => Ok(self
                .pending_mono_commits_at(&source_head)?
                .saturating_add(self.pending_remote_commits_at(target_head.as_deref())?)),
            SyncDirection::None => Ok(0),
        }
    }

    fn matches_prepared_mapping_transition(
        &self,
        expected: &MappingAuthoritySnapshot,
        actual: &MappingAuthoritySnapshot,
    ) -> RailResult<bool> {
        let target = SystemGit::open(&self.config.target_repo_path)?;
        let source = self.ctx.git()?.git();
        let from_prefix = format!("sync-from-remote-{}-", self.config.crate_name);
        let to_prefix = format!("sync-to-remote-{}-", self.config.crate_name);
        let effects = self.ordered_prepared_sync_effects(source, &target, &from_prefix, &to_prefix)?;
        let (Some((_, _, first)), Some((_, _, last))) = (effects.first(), effects.last()) else {
            return Ok(false);
        };
        if first
            .mapping()
            .is_none_or(|mapping| mapping.pre_authority() != expected.digest())
            || last
                .mapping()
                .is_none_or(|mapping| mapping.post_authority() != actual.digest())
        {
            return Ok(false);
        }
        for (index, (git, mutates_target, journal)) in effects.iter().enumerate() {
            let is_last_in_worktree = !effects[index + 1..].iter().any(|(_, _, candidate)| {
                candidate.repository().common_dir_identity == journal.repository().common_dir_identity
                    && candidate.repository().worktree_identity == journal.repository().worktree_identity
            });
            if is_last_in_worktree
                && if *mutates_target {
                    !journal.permits_local_recovery_state(git)?
                } else {
                    !journal.permits_owned_path_recovery_state(git)?
                }
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Commit operator-resolved work from a durable conflict receipt, then
    /// continue with any remaining remote commits.
    pub fn resume_from_receipt(&mut self, receipt_path: &Path) -> RailResult<SyncResult> {
        let receipt_path = self.validate_conflict_receipt_path(receipt_path)?;
        let bytes = std::fs::read(&receipt_path)?;
        let envelope: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| crate::error::RailError::message(format!("invalid sync conflict receipt: {}", error)))?;
        let schema_version = envelope
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| crate::error::RailError::message("sync conflict receipt has no schema version"))?;
        let legacy_v025 = schema_version == 2;
        let mut receipt = match schema_version {
            2 => {
                let legacy: V025SyncConflictReceipt = serde_json::from_value(envelope).map_err(|error| {
                    crate::error::RailError::message(format!("invalid v0.25 sync conflict receipt: {}", error))
                })?;
                if legacy.schema_version != 2 || legacy.status != "conflicted" || legacy.resulting_commit.is_some() {
                    return Err(crate::error::RailError::message(
                        "sync conflict receipt is not an active v0.25 version-2 conflict",
                    ));
                }
                SyncConflictReceipt::from(legacy)
            }
            3 => serde_json::from_value(envelope).map_err(|error| {
                crate::error::RailError::message(format!("invalid sync conflict receipt: {}", error))
            })?,
            _ => {
                return Err(crate::error::RailError::message(format!(
                    "unsupported sync conflict receipt schema {}",
                    schema_version
                )));
            }
        };
        if receipt.schema_version != 3
            || !matches!(receipt.status.as_str(), "materializing" | "conflicted" | "prepared")
        {
            return Err(crate::error::RailError::message(
                "sync conflict receipt is not an active version-3 conflict",
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
        let prepared_commit = receipt.resulting_commit.as_deref();
        let head_is_prepared = receipt.status == "prepared" && prepared_commit == Some(head.as_str());
        if head != receipt.expected_head && !head_is_prepared {
            return Err(crate::error::RailError::with_help(
                "sync recovery branch moved after the conflict was recorded",
                "inspect the branch history and restart sync; cargo-rail will not commit against an unverified parent",
            ));
        }

        if legacy_v025 {
            let direction = SyncDirection::RemoteToMono.authority_name();
            self.observe_remote_before_mapping_capture()?;
            let authority = self.load_mapping_evidence(direction)?;
            crate::split::engine::validate_predecessor_mapping_projections(
                self.ctx,
                &self.transform,
                &self.config.crate_paths,
                &self.config.path_capabilities,
                &self.config.target_repo_path,
                &self.config.mode,
                &self.config.workspace_mode,
                &authority,
            )?;
            self.validate_unproven_exact_pair_ancestry(
                direction,
                authority.source_head(),
                authority.target_selected_head(),
            )?;
            let selected_target_head = authority
                .target_selected_head()
                .ok_or_else(|| crate::error::RailError::message("v0.25 sync receipt has no selected target history"))?;
            if !is_ancestor(
                &self.config.target_repo_path,
                &receipt.remote_commit,
                selected_target_head,
            )? {
                return Err(crate::error::RailError::with_help(
                    "v0.25 sync receipt remote commit is outside the configured target history",
                    "restart sync; cargo-rail will not reconstruct authority for an unrelated predecessor receipt",
                ));
            }
            if self.mapping_store.has_reverse_mapping(&receipt.remote_commit) {
                return Err(crate::error::RailError::with_help(
                    "v0.25 sync receipt remote commit is already mapped",
                    "inspect current history and start a fresh sync instead of replaying the predecessor receipt",
                ));
            }
            let publication = self.capture_publication()?;
            receipt.bind_mapping_authority(&authority);
            receipt.bind_publication_authority(publication.as_ref());
            receipt.message =
                migrate_v025_receipt_message(&receipt.message, &self.target_origin, &receipt.remote_commit)?;
            receipt.bind_effect_payload()?;
            self.validate_receipt_effect_payload(&receipt, true)?;
            // Persist the exact predecessor and publication authority before
            // a migration commit can move target HEAD. A crash after this write
            // is recoverable from the bound migration digest below.
            write_json_atomic(&receipt_path, &receipt)?;

            if authority.count() > 0 {
                self.expected_publication = publication;
                self.mapping_preparation = MappingPreparation::Unprepared {
                    expected: Some(authority),
                };
                self.prepare_mapping_evidence(direction)?;
                let MappingPreparation::Prepared(migrated) = &self.mapping_preparation else {
                    return Err(crate::error::RailError::message(
                        "predecessor receipt migration did not produce prepared authority",
                    ));
                };
                let migrated = migrated.clone();
                let migrated_publication = self.capture_publication()?;
                receipt.bind_mapping_authority(&migrated);
                receipt.bind_publication_authority(migrated_publication.as_ref());
                write_json_atomic(&receipt_path, &receipt)?;
            }
        }

        let resume_direction = receipt.mapping_authority_direction.clone().ok_or_else(|| {
            crate::error::RailError::with_help(
                "sync conflict receipt has no mapping authority binding",
                "restart sync to create a current conflict receipt before committing resolved work",
            )
        })?;
        if !matches!(resume_direction.as_str(), "remote_to_mono" | "bidirectional") {
            return Err(crate::error::RailError::message(
                "sync conflict receipt has an invalid mapping authority direction",
            ));
        }
        let mut expected_authority_digest = receipt.mapping_authority_digest.clone().ok_or_else(|| {
            crate::error::RailError::with_help(
                "sync conflict receipt has no mapping authority digest",
                "restart sync to create a current conflict receipt before committing resolved work",
            )
        })?;
        let ownership_snapshot = receipt.mapping_authority_ownership_snapshot.as_deref().ok_or_else(|| {
            crate::error::RailError::with_help(
                "sync conflict receipt has no ownership snapshot binding",
                "restart sync to create a current conflict receipt before committing resolved work",
            )
        })?;
        if self.config.ownership.snapshot_id != ownership_snapshot {
            return Err(crate::error::RailError::with_help(
                "sync ownership changed after the conflict receipt was written",
                "restart sync from the current configuration; cargo-rail never adopts stale receipt ownership into current path or transform authority",
            ));
        }
        self.observe_remote_before_mapping_capture()?;
        let mut resume_authority = self.load_mapping_evidence(&resume_direction)?;
        if resume_authority.count() > 0 {
            if resume_authority.digest() != expected_authority_digest
                || receipt.mapping_authority_migration_count != Some(resume_authority.count())
                || receipt.mapping_authority_migration_digest.as_deref()
                    != Some(resume_authority.migration_digest().as_str())
            {
                return Err(crate::error::RailError::with_help(
                    "sync resume found an unbound predecessor origin migration",
                    "restart sync from current repository histories; cargo-rail only resumes the exact migration persisted in the receipt",
                ));
            }
            let before_publication = self.capture_publication()?;
            if !receipt.matches_publication_authority(before_publication.as_ref()) {
                return Err(crate::error::RailError::with_help(
                    "sync target publication authority changed before receipt migration",
                    "restore the receipt's exact local/remote target heads before resuming",
                ));
            }
            crate::split::engine::validate_predecessor_mapping_projections(
                self.ctx,
                &self.transform,
                &self.config.crate_paths,
                &self.config.path_capabilities,
                &self.config.target_repo_path,
                &self.config.mode,
                &self.config.workspace_mode,
                &resume_authority,
            )?;
            self.validate_unproven_exact_pair_ancestry(
                &resume_direction,
                resume_authority.source_head(),
                resume_authority.target_selected_head(),
            )?;
            self.expected_publication = before_publication;
            self.mapping_preparation = MappingPreparation::Unprepared {
                expected: Some(resume_authority),
            };
            self.prepare_mapping_evidence(&resume_direction)?;
            let MappingPreparation::Prepared(migrated) = &self.mapping_preparation else {
                return Err(crate::error::RailError::message(
                    "receipt predecessor migration did not produce prepared authority",
                ));
            };
            resume_authority = migrated.clone();
            let publication = self.capture_publication()?;
            receipt.bind_mapping_authority(&resume_authority);
            receipt.bind_publication_authority(publication.as_ref());
            expected_authority_digest = resume_authority.digest();
            write_json_atomic(&receipt_path, &receipt)?;
        } else if resume_authority.digest() != expected_authority_digest
            && receipt.mapping_authority_migration_count.is_some_and(|count| count > 0)
        {
            let expected_parent = receipt.mapping_authority_target_head.as_deref().ok_or_else(|| {
                crate::error::RailError::message("sync receipt migration recovery has no bound predecessor target HEAD")
            })?;
            let actual_target_head = resume_authority
                .target_head()
                .ok_or_else(|| crate::error::RailError::message("sync receipt migration recovery lost target HEAD"))?;
            let expected_migration_digest = receipt.mapping_authority_migration_digest.as_deref().ok_or_else(|| {
                crate::error::RailError::message("sync receipt migration recovery has no migration digest")
            })?;
            MappingStore::validate_completed_v025_migration(
                &self.config.target_repo_path,
                actual_target_head,
                expected_parent,
                &self.source_origin,
                self.target_origin.source_repository(),
                expected_migration_digest,
            )?;
            let publication = self.capture_publication()?;
            let remote_unchanged = match (publication.as_ref(), receipt.publication_authority_present) {
                (None, Some(false)) => true,
                (Some(actual), Some(true)) => {
                    receipt.publication_remote_repository.as_deref() == Some(actual.remote_repository())
                        && receipt.publication_remote_head.as_deref() == actual.remote_head()
                }
                _ => false,
            };
            if !remote_unchanged {
                return Err(crate::error::RailError::with_help(
                    "remote publication authority changed during receipt migration recovery",
                    "restart sync; cargo-rail will not adopt a migration commit across remote drift",
                ));
            }
            receipt.bind_mapping_authority(&resume_authority);
            receipt.bind_publication_authority(publication.as_ref());
            expected_authority_digest = resume_authority.digest();
            write_json_atomic(&receipt_path, &receipt)?;
        }
        let required_authority_digest = if head_is_prepared {
            receipt.mapping_authority_post_digest.as_deref().ok_or_else(|| {
                crate::error::RailError::with_help(
                    "prepared sync receipt has no post-commit mapping authority digest",
                    "restart sync; cargo-rail cannot reconcile an unbound prepared HEAD",
                )
            })?
        } else {
            &expected_authority_digest
        };
        if resume_authority.digest() != required_authority_digest {
            return Err(crate::error::RailError::with_help(
                "sync mapping authority changed after the conflict receipt was written",
                "restart sync from the current repository histories; cargo-rail will not commit against stale recovery evidence",
            ));
        }
        receipt.publication_authority_present.ok_or_else(|| {
            crate::error::RailError::with_help(
                "sync conflict receipt has no target publication binding",
                "restart sync to create a current conflict receipt before committing resolved work",
            )
        })?;
        let resume_publication = self.capture_publication()?;
        if !receipt.matches_publication_authority(resume_publication.as_ref()) {
            return Err(crate::error::RailError::with_help(
                "sync target publication authority changed after the conflict receipt was written",
                "restart sync from current local and remote branch authority; cargo-rail will not resume or publish against a deleted, rewound, or advanced remote ref",
            ));
        }
        self.expected_publication = resume_publication.clone();
        self.revalidate_publication_before_effect(
            &resume_direction,
            resume_publication.as_ref().is_some_and(|snapshot| snapshot.count() > 0),
        )?;
        self.mapping_preparation = MappingPreparation::Prepared(resume_authority);
        self.validate_receipt_effect_payload(&receipt, receipt.status != "prepared")?;

        if receipt.status == "materializing" {
            let expected_strategy = receipt.conflict_strategy.as_deref().ok_or_else(|| {
                crate::error::RailError::message("materializing sync receipt has no bound conflict strategy")
            })?;
            if expected_strategy != self.conflict_resolver.strategy().authority_name() {
                return Err(crate::error::RailError::with_help(
                    "sync conflict strategy changed during materialization recovery",
                    format!("retry with --strategy {expected_strategy}"),
                ));
            }
            let remote_git = SystemGit::open(&self.config.target_repo_path)?;
            let remote = remote_git.get_commit(&receipt.remote_commit)?;
            let changed = remote_git
                .get_changed_files_bulk(std::slice::from_ref(&remote.sha))?
                .into_iter()
                .next()
                .unwrap_or_default();
            let resolution =
                self.resolve_conflicts_for_commit(&remote, &remote_git, &changed, &receipt.expected_head)?;
            if resolution.conflicts != receipt.conflicts {
                return Err(crate::error::RailError::with_help(
                    "sync conflict set changed during materialization recovery",
                    "preserve the receipt and repository; cargo-rail will not reconstruct a different conflict image",
                ));
            }
            let materialization =
                self.prepare_conflict_materialization(&remote, &remote_git, &resolution, &receipt.expected_head)?;
            if receipt.materialization_digest.as_deref() != Some(materialization.digest.as_str()) {
                return Err(crate::error::RailError::with_help(
                    "prepared sync conflict materialization changed during recovery",
                    "restore the original toolchain and repository inputs; cargo-rail preserved the partially materialized paths",
                ));
            }
            self.reconcile_conflict_materialization(&materialization)?;
            receipt.status = "conflicted".to_string();
            write_json_atomic(&receipt_path, &receipt)?;
            return Ok(SyncResult {
                commits_synced: 0,
                conflicts: receipt.conflicts,
                status: SyncStatus::Conflicted,
                conflict_receipt: Some(receipt_path),
            });
        }

        if receipt.status == "conflicted" {
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
        }

        let operation_prefix = format!("sync-from-remote-{}-", self.config.crate_name);
        let prepared = GitEffectStore::discover_unacknowledged_read_only(git)?
            .into_iter()
            .filter(|journal| {
                journal.operation_id().starts_with(&operation_prefix)
                    && receipt
                        .resulting_commit
                        .as_deref()
                        .is_some_and(|commit| journal.repository().result_oid == commit)
            })
            .collect::<Vec<_>>();
        if prepared.len() > 1 {
            return Err(crate::error::RailError::message(
                "sync conflict receipt matches multiple prepared Git effects",
            ));
        }

        if let Some(journal) = prepared.first() {
            let post_digest = receipt.mapping_authority_post_digest.as_deref().ok_or_else(|| {
                crate::error::RailError::message("prepared sync receipt has no post-effect mapping digest")
            })?;
            let store = GitEffectStore::open(git)?;
            let record = store.resume(journal.effect_id())?;
            self.reconcile_sync_commit_record(
                &resume_direction,
                &expected_authority_digest,
                post_digest,
                &store,
                record,
                git,
                false,
                true,
            )?;
        } else {
            if head_is_prepared {
                return Err(crate::error::RailError::with_help(
                    "prepared sync receipt lost its exact Git-effect journal",
                    "preserve the branch and receipt; cargo-rail will not infer recovery authority from a moved ref",
                ));
            }
            self.revalidate_prepared_mapping_evidence(&resume_direction)?;
            let quarantine = git.object_quarantine()?;
            quarantine.import_object_closure(git, &[&receipt.expected_head])?;
            let changes = prepare_worktree_changes(git, &quarantine, &receipt.commit_paths)?;
            let (tree, transitions) = prepare_sync_tree(git, &receipt.expected_head, &quarantine, &changes)?;
            let metadata = receipt.commit_metadata();
            let parents = vec![receipt.expected_head.clone()];
            let commit = quarantine.write_commit(&tree, &parents, &receipt.message, &metadata)?;
            if receipt
                .resulting_commit
                .as_ref()
                .is_some_and(|expected| expected != &commit)
            {
                return Err(crate::error::RailError::with_help(
                    "prepared sync receipt commit changed during reconstruction",
                    "restore the exact resolved path contents or restart sync from the remote commit",
                ));
            }
            let commit_effect = GitCommitEffect::new(
                commit.clone(),
                tree,
                parents,
                receipt.message.clone(),
                GitEffectCommitMetadata::from(&metadata),
            );
            self.mapping_store
                .record_target_frontier_mapping(&commit, &receipt.remote_commit)?;
            self.mapping_store.update_authority_heads(Some(&commit), None)?;
            let post_authority = self.mapping_store.mapping_authority_snapshot(
                &resume_direction,
                self.config.path_capabilities.target_root(),
                &self.config.branch,
            )?;
            if receipt
                .mapping_authority_post_digest
                .as_deref()
                .is_some_and(|expected| expected != post_authority.digest())
            {
                return Err(crate::error::RailError::with_help(
                    "prepared sync receipt post-commit authority changed before publication",
                    "restart sync; cargo-rail will not publish a prepared commit with mismatched mapping authority",
                ));
            }
            receipt.resulting_commit = Some(commit.clone());
            receipt.mapping_authority_post_digest = Some(post_authority.digest());
            receipt.status = "prepared".to_string();
            write_json_atomic(&receipt_path, &receipt)?;

            let store = GitEffectStore::open(git)?;
            let ref_name = format!("refs/heads/{}", receipt.branch);
            let repository = store.capture_repository_authority(
                git,
                repository_identity(self.ctx.workspace_root())?,
                ref_name,
                Some(receipt.expected_head.clone()),
                commit.clone(),
            )?;
            let mapping = GitMappingBinding::new(
                self.config.crate_name.clone(),
                self.config.ownership.snapshot_id.clone(),
                expected_authority_digest.clone(),
                post_authority.digest(),
                None,
                0,
            );
            let mut bundle = store.create_object_bundle_temp()?;
            let bundle_digest = quarantine.write_pack(&commit, Some(&receipt.expected_head), bundle.file_mut()?)?;
            let intent = GitEffectIntent::new(
                format!("{operation_prefix}{}", post_authority.digest()),
                repository,
                Some(commit_effect),
                transitions,
                Some(mapping),
                None,
                Some(bundle_digest.clone()),
            )?;
            let effect_id = intent.effect_id()?;
            drop(bundle.persist(&effect_id, &bundle_digest)?);
            let record = store.prepare(intent)?;
            self.reconcile_sync_commit_record(
                &resume_direction,
                &expected_authority_digest,
                &post_authority.digest(),
                &store,
                record,
                git,
                false,
                true,
            )?;
        }

        receipt.status = "resolved".to_string();
        write_json_atomic(&receipt_path, &receipt)?;
        Ok(SyncResult {
            commits_synced: 1,
            ..SyncResult::default()
        })
    }

    fn validate_receipt_effect_payload(
        &self,
        receipt: &SyncConflictReceipt,
        validate_conflicts: bool,
    ) -> RailResult<()> {
        let expected_digest = receipt.effect_payload_digest.as_deref().ok_or_else(|| {
            crate::error::RailError::with_help(
                "sync conflict receipt has no effect-payload binding",
                "restart sync to create a current strictly bound conflict receipt",
            )
        })?;
        if receipt.compute_effect_payload_digest()? != expected_digest {
            return Err(crate::error::RailError::with_help(
                "sync conflict receipt effect payload was modified",
                "restore the original receipt or restart sync; cargo-rail will not prepare a changed payload",
            ));
        }

        let remote_git = SystemGit::open(&self.config.target_repo_path)?;
        let remote = remote_git.get_commit(&receipt.remote_commit)?;
        let expected_message = append_origin_trailers(&remote.message, &[self.target_origin.trailer(&remote.sha)?]);
        if remote.sha != receipt.remote_commit
            || remote.metadata() != receipt.commit_metadata()
            || expected_message.trim_end() != receipt.message.trim_end()
        {
            return Err(crate::error::RailError::with_help(
                "sync conflict receipt does not match its exact remote commit metadata and current origin trailer",
                "restart sync; cargo-rail will not prepare substituted receipt content",
            ));
        }
        let changed = remote_git
            .get_changed_files_bulk(std::slice::from_ref(&remote.sha))?
            .into_iter()
            .next()
            .unwrap_or_default();
        let mut expected_paths = changed
            .iter()
            .filter_map(|(remote_path, _)| self.map_remote_path_to_mono(remote_path).ok())
            .collect::<Vec<_>>();
        expected_paths.sort();
        expected_paths.dedup();
        if expected_paths != receipt.commit_paths {
            return Err(crate::error::RailError::with_help(
                "sync conflict receipt commit path set differs from the bound remote commit",
                "restart sync; cargo-rail will not stage expanded, removed, or reordered receipt paths",
            ));
        }
        for path in &receipt.commit_paths {
            self.config.path_capabilities.authorize_source_mutation(path)?;
        }

        if validate_conflicts {
            let mut declared = receipt
                .conflicts
                .iter()
                .map(|conflict| {
                    if conflict.class != ConflictClass::Content || !receipt.commit_paths.contains(&conflict.file_path) {
                        return Err(crate::error::RailError::message(
                            "sync conflict receipt declares an invalid conflict path or class",
                        ));
                    }
                    Ok(conflict.file_path.clone())
                })
                .collect::<RailResult<Vec<_>>>()?;
            declared.sort();
            declared.dedup();
            if declared.len() != receipt.conflicts.len() {
                return Err(crate::error::RailError::message(
                    "sync conflict receipt contains duplicate conflict paths",
                ));
            }
            let last_synced = self.find_mono_base_for_remote_commit(&remote_git, &remote.sha)?;
            let mut actual = Vec::new();
            if let Some(base) = last_synced {
                let mono_git = self.ctx.git()?.git();
                let mono_changed = mono_git
                    .get_changed_files_between(&base, Some(&receipt.expected_head))?
                    .into_iter()
                    .map(|(path, _)| path)
                    .collect::<HashSet<_>>();
                for (remote_path, change_type) in &changed {
                    let mono_path = self.map_remote_path_to_mono(remote_path)?;
                    if *change_type == 'D' || !mono_changed.contains(&mono_path) {
                        continue;
                    }
                    let Some(current) = read_git_file_if_present(mono_git, &receipt.expected_head, &mono_path)? else {
                        continue;
                    };
                    let base_content = read_git_file_if_present(mono_git, &base, &mono_path)?.unwrap_or_default();
                    let incoming =
                        read_git_file_if_present(&remote_git, &remote.sha, remote_path)?.ok_or_else(|| {
                            crate::error::RailError::message("conflict receipt remote path has no incoming blob")
                        })?;
                    if merge_would_conflict(&base_content, &current, &incoming)? {
                        actual.push(mono_path);
                    }
                }
            }
            actual.sort();
            actual.dedup();
            if actual != declared {
                return Err(crate::error::RailError::with_help(
                    "sync conflict receipt conflict set differs from the deterministic three-way merge",
                    "restart sync; cargo-rail will not commit a receipt with added or removed conflict authority",
                ));
            }
        }
        Ok(())
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

    /// Check whether a monorepo path belongs to this sync scope.
    ///
    /// - `single`: only files under the single configured crate path
    /// - `combined`: files under any configured crate path
    fn mono_path_in_scope(&self, path: &Path) -> RailResult<bool> {
        self.config.path_capabilities.owns_source_path(path)
    }

    fn source_commits_in_scope_excluding(
        &self,
        excluded: &[&str],
        source_head: &str,
    ) -> RailResult<Vec<CommitWithChanges>> {
        let git = self.ctx.git()?.git();
        let commits = git.get_commits_excluding(excluded, source_head)?;
        let shas = commits.iter().map(|commit| commit.sha.clone()).collect::<Vec<_>>();
        let changed = git.get_changed_files_bulk(&shas)?;
        let mut selected = Vec::new();
        for (commit, paths) in commits.into_iter().zip(changed) {
            let owned = paths
                .iter()
                .map(|(path, _)| self.mono_path_in_scope(path))
                .collect::<RailResult<Vec<_>>>()?
                .into_iter()
                .any(|owned| owned);
            if owned {
                selected.push((commit, paths));
            }
        }
        Ok(selected)
    }

    /// Sync changes from monorepo to remote repository
    pub fn sync_to_remote(&mut self) -> RailResult<SyncResult> {
        let direction = SyncDirection::MonoToRemote.authority_name();
        self.observe_remote_before_mapping_capture()?;
        self.prepare_mapping_evidence(direction)?;
        let result = self.sync_to_remote_prepared()?;
        self.seal_prepared_mapping_evidence(direction)?;
        self.reconcile_target_publication()?;
        Ok(result)
    }

    fn sync_to_remote_prepared(&mut self) -> RailResult<SyncResult> {
        self.sync_to_remote_prepared_with_commits(None)
    }

    fn sync_to_remote_prepared_with_commits(
        &mut self,
        frozen_commits: Option<Vec<CommitWithChanges>>,
    ) -> RailResult<SyncResult> {
        progress!("Syncing local source to remote target...");

        // Open remote repo
        let target_repo_path = self.config.target_repo_path.clone();
        let remote_git = SystemGit::open(&target_repo_path)?;

        // Select descendants of the newest actual pair mapping from the unbounded path history,
        // then filter exact evidence per commit. Evidence-only commits never establish a frontier.
        let new_commits = frozen_commits.map_or_else(|| self.collect_pending_mono_commits_with_changes(), Ok)?;

        if new_commits.is_empty() {
            progress!("   No new commits to sync");
        } else {
            progress!("   Syncing {} commits to remote...", new_commits.len());

            let mut synced_count = 0;
            let mut current_remote_head = remote_git.head_commit()?; // Cache HEAD, update after each commit
            for (commit, changed_files) in &new_commits {
                // Skip if already synced
                if self.mapping_store.has_mapping(&commit.sha) {
                    continue;
                }

                // Apply commit to remote
                let remote_sha =
                    self.apply_mono_commit_to_remote(commit, changed_files, &remote_git, &current_remote_head)?;

                synced_count += 1;
                current_remote_head = remote_sha; // Update cached HEAD (move, not clone)
            }

            return Ok(SyncResult {
                commits_synced: synced_count,
                conflicts: Vec::new(),
                ..SyncResult::default()
            });
        }

        let synced_count = 0;

        Ok(SyncResult {
            commits_synced: synced_count,
            conflicts: Vec::new(),
            ..SyncResult::default()
        })
    }

    /// Sync changes from remote repository to monorepo
    pub fn sync_from_remote(&mut self) -> RailResult<SyncResult> {
        let direction = SyncDirection::RemoteToMono.authority_name();
        self.observe_remote_before_mapping_capture()?;
        self.prepare_mapping_evidence(direction)?;
        let result = self.sync_from_remote_prepared()?;
        self.seal_prepared_mapping_evidence(direction)?;
        if result.status != SyncStatus::Conflicted {
            self.reconcile_target_publication()?;
        }
        Ok(result)
    }

    fn sync_from_remote_prepared(&mut self) -> RailResult<SyncResult> {
        self.sync_from_remote_prepared_with_commits(None)
    }

    fn sync_from_remote_prepared_with_commits(
        &mut self,
        frozen_commits: Option<Vec<CommitInfo>>,
    ) -> RailResult<SyncResult> {
        progress!("Syncing remote source to local target...");

        // Check current branch - NEVER commit directly to protected branches
        let _current_branch = self.ctx.git()?.git().current_branch()?;

        // Open remote repo
        let target_repo_path = self.config.target_repo_path.clone();
        let remote_git = SystemGit::open(&target_repo_path)?;

        // Select descendants of the newest actual pair mapping from the unbounded target
        // history, then filter exact evidence per commit. In particular, migration evidence
        // above an independent target commit cannot become a frontier that hides that commit.
        let new_commits = frozen_commits.map_or_else(|| self.collect_pending_remote_commits(&remote_git), Ok)?;
        let commits_to_sync = new_commits.iter().collect::<Vec<_>>();

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
            let resolution =
                self.resolve_conflicts_for_commit(commit, &remote_git, changed_files, &current_mono_head)?;

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

            if !resolution.conflicts.is_empty() {
                let materialization =
                    self.prepare_conflict_materialization(commit, &remote_git, &resolution, &current_mono_head)?;
                self.mapping_store
                    .update_authority_heads(Some(&current_mono_head), None)?;
                let prepared_direction = match &self.mapping_preparation {
                    MappingPreparation::Prepared(authority) => authority.direction(),
                    MappingPreparation::Unprepared { .. } => {
                        return Err(crate::error::RailError::message(
                            "sync conflict recovery requires prepared mapping authority",
                        ));
                    }
                };
                let receipt_authority = self.mapping_store.mapping_authority_snapshot(
                    prepared_direction,
                    self.config.path_capabilities.target_root(),
                    &self.config.branch,
                )?;
                let branch = pr_branch
                    .as_deref()
                    .ok_or_else(|| crate::error::RailError::message("conflicted sync has no recovery branch"))?;
                let receipt_publication = self.capture_publication()?;
                let mut receipt_payload = SyncConflictReceipt {
                    schema_version: 3,
                    status: "materializing".to_string(),
                    crate_name: self.config.crate_name.clone(),
                    effect_payload_digest: None,
                    materialization_digest: Some(materialization.digest.clone()),
                    conflict_strategy: Some(self.conflict_resolver.strategy().authority_name().to_string()),
                    mapping_authority_direction: Some(prepared_direction.to_string()),
                    mapping_authority_digest: Some(receipt_authority.digest()),
                    mapping_authority_ownership_snapshot: Some(receipt_authority.ownership_snapshot().to_string()),
                    mapping_authority_target_head: receipt_authority.target_head().map(str::to_string),
                    mapping_authority_post_digest: None,
                    mapping_authority_migration_digest: Some(receipt_authority.migration_digest()),
                    mapping_authority_migration_count: Some(receipt_authority.count()),
                    publication_authority_present: Some(receipt_publication.is_some()),
                    publication_authority_digest: receipt_publication.as_ref().map(TargetPublicationSnapshot::digest),
                    publication_remote_repository: receipt_publication
                        .as_ref()
                        .map(|snapshot| snapshot.remote_repository().to_string()),
                    publication_remote_head: receipt_publication
                        .as_ref()
                        .and_then(|snapshot| snapshot.remote_head().map(str::to_string)),
                    publication_local_head: receipt_publication
                        .as_ref()
                        .and_then(|snapshot| snapshot.local_head().map(str::to_string)),
                    publication_relation: receipt_publication
                        .as_ref()
                        .map(|snapshot| snapshot.relation().to_string()),
                    branch: branch.to_string(),
                    expected_head: current_mono_head,
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
                };
                receipt_payload.bind_effect_payload()?;
                let receipt = self.write_conflict_receipt(receipt_payload.clone())?;
                self.reconcile_conflict_materialization(&materialization)?;
                receipt_payload.status = "conflicted".to_string();
                write_json_atomic(&receipt, &receipt_payload)?;
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
            let mono_sha = self.apply_remote_commit_to_mono(
                commit,
                &remote_git,
                &resolved_files,
                &resolution.resolved_contents,
                &current_mono_head,
                &resolution.changed_files,
            )?;
            let mono_sha =
                mono_sha.ok_or_else(|| crate::error::RailError::message("clean sync did not create a commit"))?;

            // Extend conflicts AFTER apply (resolved_files borrows from resolution.conflicts)
            if !resolution.conflicts.is_empty() {
                conflicts.extend(resolution.conflicts);
            }

            count += 1;
            current_mono_head = mono_sha; // Update cached HEAD (move, not clone)
        }

        let synced_count = count;
        // Print PR creation instructions if we created a branch with synced commits
        if let Some(branch_name) = pr_branch
            && synced_count > 0
        {
            progress!(
                "Synced {} commit{} to branch: {}",
                synced_count,
                if synced_count == 1 { "" } else { "s" },
                branch_name
            );
            progress!("To create a pull request:");
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
        let direction = SyncDirection::Both.authority_name();
        self.observe_remote_before_mapping_capture()?;
        self.prepare_mapping_evidence(direction)?;

        let remote_git = SystemGit::open(&self.config.target_repo_path)?;

        // Freeze both exact pending sets before either side mutates. Remote commits are
        // applied first against the original mapping frontier so a newly synthesized
        // mono-to-remote mapping cannot hide an older remote commit or suppress a conflict.
        let mono_commits = self.collect_pending_mono_commits_with_changes()?;
        let remote_commits = self.collect_pending_remote_commits(&remote_git)?;
        let mono_has_changes = !mono_commits.is_empty();
        let remote_has_changes = !remote_commits.is_empty();

        let result = match (mono_has_changes, remote_has_changes) {
            (true, false) => {
                progress!("   Only monorepo has changes");
                let result = self.sync_to_remote_prepared_with_commits(Some(mono_commits))?;
                self.seal_prepared_mapping_evidence(direction)?;
                result
            }
            (false, true) => {
                progress!("   Only remote has changes");
                let result = self.sync_from_remote_prepared_with_commits(Some(remote_commits))?;
                self.seal_prepared_mapping_evidence(direction)?;
                result
            }
            (true, true) => {
                progress!("   Both sides have changes, syncing both directions");
                let from_remote = self.sync_from_remote_prepared_with_commits(Some(remote_commits))?;
                self.seal_prepared_mapping_evidence(direction)?;
                if from_remote.status == SyncStatus::Conflicted {
                    from_remote
                } else {
                    let to_remote = self.sync_to_remote_prepared_with_commits(Some(mono_commits))?;
                    self.seal_prepared_mapping_evidence(direction)?;

                    SyncResult {
                        commits_synced: to_remote.commits_synced + from_remote.commits_synced,
                        conflicts: from_remote.conflicts,
                        status: from_remote.status,
                        conflict_receipt: from_remote.conflict_receipt,
                    }
                }
            }
            (false, false) => {
                progress!("   No changes on either side");
                SyncResult {
                    commits_synced: 0,
                    conflicts: Vec::new(),
                    ..SyncResult::default()
                }
            }
        };
        if result.status != SyncStatus::Conflicted {
            self.reconcile_target_publication()?;
        }
        Ok(result)
    }

    // Helper methods

    fn validate_unproven_exact_pair_ancestry(
        &self,
        direction: &str,
        source_head: &str,
        target_head: Option<&str>,
    ) -> RailResult<()> {
        let pairs = self.mapping_store.unproven_mapping_pairs();
        if pairs.is_empty() {
            return Ok(());
        }

        if matches!(direction, "mono_to_remote" | "bidirectional") {
            let pending = self
                .source_commits_in_scope_excluding(&self.mapping_store.source_frontier_commits(), source_head)?
                .into_iter()
                .map(|(commit, _)| commit)
                .filter(|commit| !self.mapping_store.has_mapping(&commit.sha))
                .collect::<Vec<_>>();
            for (source, _) in &pairs {
                for ancestor in &pending {
                    if is_ancestor(self.ctx.workspace_root(), &ancestor.sha, source)? {
                        return Err(unproven_mapping_ancestry_error(&ancestor.sha, source, "source"));
                    }
                }
            }
        }

        if matches!(direction, "remote_to_mono" | "bidirectional") {
            let Some(target_head) = target_head else {
                return Ok(());
            };
            let target_git = SystemGit::open(&self.config.target_repo_path)?;
            let pending = target_git
                .get_commits_touching_paths_excluding(
                    &[PathBuf::from(".")],
                    &self.mapping_store.target_frontier_commits(),
                    target_head,
                )?
                .into_iter()
                .filter(|commit| !self.mapping_store.has_reverse_mapping(&commit.sha))
                .collect::<Vec<_>>();
            for (_, target) in &pairs {
                for ancestor in &pending {
                    if is_ancestor(&self.config.target_repo_path, &ancestor.sha, target)? {
                        return Err(unproven_mapping_ancestry_error(&ancestor.sha, target, "target"));
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_pending_mono_commits_with_changes(&self) -> RailResult<Vec<CommitWithChanges>> {
        let MappingPreparation::Prepared(authority) = &self.mapping_preparation else {
            return Err(crate::error::RailError::message(
                "sync mapping authority must be prepared before collecting source history",
            ));
        };
        self.collect_pending_mono_commits_with_changes_at(authority.source_head())
    }

    fn collect_pending_mono_commits_at(&self, source_head: &str) -> RailResult<Vec<CommitInfo>> {
        self.collect_pending_mono_commits_with_changes_at(source_head)
            .map(|commits| commits.into_iter().map(|(commit, _)| commit).collect())
    }

    fn collect_pending_mono_commits_with_changes_at(&self, source_head: &str) -> RailResult<Vec<CommitWithChanges>> {
        Ok(self
            .source_commits_in_scope_excluding(&self.mapping_store.source_frontier_commits(), source_head)?
            .into_iter()
            .filter(|(commit, _)| !self.mapping_store.has_mapping(&commit.sha))
            .collect())
    }

    fn collect_pending_remote_commits(&self, remote_git: &SystemGit) -> RailResult<Vec<CommitInfo>> {
        let MappingPreparation::Prepared(authority) = &self.mapping_preparation else {
            return Err(crate::error::RailError::message(
                "sync mapping authority must be prepared before collecting target history",
            ));
        };
        self.collect_pending_remote_commits_at(remote_git, authority.target_selected_head())
    }

    fn collect_pending_remote_commits_at(
        &self,
        remote_git: &SystemGit,
        target_head: Option<&str>,
    ) -> RailResult<Vec<CommitInfo>> {
        let Some(target_head) = target_head else {
            return Ok(Vec::new());
        };
        Ok(remote_git
            .get_commits_touching_paths_excluding(
                &[PathBuf::from(".")],
                &self.mapping_store.target_frontier_commits(),
                target_head,
            )?
            .into_iter()
            .filter(|commit| !self.mapping_store.has_reverse_mapping(&commit.sha))
            .collect())
    }

    fn find_mono_base_for_remote_commit(
        &self,
        remote_git: &SystemGit,
        remote_commit: &str,
    ) -> RailResult<Option<String>> {
        let remote_history = remote_git.get_commits_touching_path(Path::new("."), None, remote_commit)?;
        Ok(remote_history
            .iter()
            .rev()
            .find_map(|commit| self.mapping_store.get_reverse_mapping(&commit.sha)))
    }

    fn apply_mono_commit_to_remote(
        &mut self,
        commit: &crate::git::CommitInfo,
        changed_files: &[(PathBuf, char)],
        remote_git: &SystemGit,
        current_remote_head: &str,
    ) -> RailResult<String> {
        if !remote_git.obstructing_worktree_paths()?.is_empty() {
            return Err(crate::error::RailError::with_help(
                "sync target became dirty before target commit materialization",
                "commit, restore, or remove target work and retry; cargo-rail will not overwrite it",
            ));
        }
        let MappingPreparation::Prepared(pre_authority) = &self.mapping_preparation else {
            return Err(crate::error::RailError::message(
                "sync mapping authority must be prepared before a target effect",
            ));
        };
        let pre_authority = pre_authority.clone();
        let source_git = self.ctx.git()?.git();

        // Filter to only files in configured crate path scope.
        let relevant_files = changed_files
            .iter()
            .map(|(path, kind)| self.mono_path_in_scope(path).map(|owned| (path, kind, owned)))
            .collect::<RailResult<Vec<_>>>()?
            .into_iter()
            .filter(|(_, _, owned)| *owned)
            .map(|(path, kind, _)| (path.clone(), *kind))
            .collect::<Vec<_>>();

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

        let quarantine = remote_git.object_quarantine()?;
        quarantine.import_object_closure(remote_git, &[current_remote_head])?;
        quarantine.import_object_closure(source_git, &[&commit.sha])?;
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
                quarantine.write_blob(&self.transform_manifest_to_split(&content[0])?)?
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
        let (tree, transitions) = prepare_sync_tree(remote_git, current_remote_head, &quarantine, &changes)?;
        let metadata = commit.metadata();
        let new_commit_sha = quarantine.write_commit(&tree, &parent_shas, &message, &metadata)?;
        let commit_effect = GitCommitEffect::new(
            new_commit_sha.clone(),
            tree,
            parent_shas,
            message,
            GitEffectCommitMetadata::from(&metadata),
        );

        let (recaptured_store, recaptured_pre) = self.capture_mapping_evidence(pre_authority.direction())?;
        if recaptured_pre != pre_authority {
            return Err(sync_mapping_authority_changed_error());
        }
        self.mapping_store = recaptured_store;
        self.mapping_store
            .record_source_frontier_mapping(&commit.sha, &new_commit_sha)?;
        self.mapping_store.update_authority_heads(None, Some(&new_commit_sha))?;
        let post_authority = self.mapping_store.mapping_authority_snapshot(
            pre_authority.direction(),
            self.config.path_capabilities.target_root(),
            &self.config.branch,
        )?;
        let store = GitEffectStore::open(remote_git)?;
        let ref_name = format!("refs/heads/{}", self.config.branch);
        let repository = store.capture_repository_authority(
            remote_git,
            repository_identity(&self.config.target_repo_path)?,
            ref_name,
            Some(current_remote_head.to_string()),
            new_commit_sha.clone(),
        )?;
        let mapping = GitMappingBinding::new(
            pre_authority.owner().to_string(),
            pre_authority.ownership_snapshot().to_string(),
            pre_authority.digest(),
            post_authority.digest(),
            None,
            0,
        );
        let mut bundle = store.create_object_bundle_temp()?;
        let bundle_digest = quarantine.write_pack(&new_commit_sha, Some(current_remote_head), bundle.file_mut()?)?;
        let intent = GitEffectIntent::new(
            format!("sync-to-remote-{}-{}", self.config.crate_name, post_authority.digest()),
            repository,
            Some(commit_effect),
            transitions,
            Some(mapping),
            None,
            Some(bundle_digest.clone()),
        )?;
        let effect_id = intent.effect_id()?;
        drop(bundle.persist(&effect_id, &bundle_digest)?);
        if !remote_git.obstructing_worktree_paths()?.is_empty() {
            return Err(crate::error::RailError::with_help(
                "sync target changed before prepared effect publication",
                "restore target work and retry the exact durable sync effect",
            ));
        }
        let record = store.prepare(intent)?;
        self.reconcile_sync_commit_record(
            pre_authority.direction(),
            &pre_authority.digest(),
            &post_authority.digest(),
            &store,
            record,
            remote_git,
            true,
            false,
        )?;
        Ok(new_commit_sha)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "sync commit preparation binds exact content, metadata, and mapping authority"
    )]
    fn reconcile_sync_commit_record(
        &mut self,
        direction: &str,
        pre_authority_digest: &str,
        post_authority_digest: &str,
        store: &GitEffectStore,
        record: GitEffectRecord,
        git: &SystemGit,
        mutates_target: bool,
        allows_prepared_paths_before_ref: bool,
    ) -> RailResult<()> {
        match record {
            GitEffectRecord::Active(mut active) => {
                self.reconcile_sync_commit_journal(
                    direction,
                    pre_authority_digest,
                    post_authority_digest,
                    store,
                    active.journal(),
                    git,
                    mutates_target,
                    allows_prepared_paths_before_ref,
                )?;
                active.mark_local_applied()?;
                let _completed = active.finish()?;
            }
            GitEffectRecord::Completed(completed) => self.reconcile_sync_commit_journal(
                direction,
                pre_authority_digest,
                post_authority_digest,
                store,
                completed.journal(),
                git,
                mutates_target,
                allows_prepared_paths_before_ref,
            )?,
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "sync commit reconciliation compares exact content, metadata, and mapping authority"
    )]
    fn reconcile_sync_commit_journal(
        &mut self,
        direction: &str,
        pre_authority_digest: &str,
        post_authority_digest: &str,
        store: &GitEffectStore,
        journal: &GitEffectJournal,
        git: &SystemGit,
        mutates_target: bool,
        allows_prepared_paths_before_ref: bool,
    ) -> RailResult<()> {
        self.validate_sync_commit_journal_contract(
            pre_authority_digest,
            post_authority_digest,
            journal,
            mutates_target,
        )?;
        let repository = journal.repository();
        let commit = journal
            .commit()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no commit"))?;
        let bundle_digest = journal
            .object_bundle_digest()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no object bundle"))?;
        let current_head = git.exact_branch_ref_oid(&repository.ref_name)?;
        if current_head.as_deref() != repository.expected_oid.as_deref()
            && current_head.as_deref() != Some(repository.result_oid.as_str())
        {
            return Err(crate::error::RailError::message(
                "prepared sync branch changed to a third ref state",
            ));
        }
        validate_sync_repository_authority(store, git, journal, current_head.clone())?;
        let (captured_store, current_authority) = self.capture_mapping_evidence(direction)?;
        let expected_authority_digest = if current_head.as_deref() == repository.expected_oid.as_deref() {
            validate_sync_path_images(git, journal, !allows_prepared_paths_before_ref)?;
            pre_authority_digest
        } else {
            post_authority_digest
        };
        if current_authority.digest() != expected_authority_digest {
            return Err(sync_mapping_authority_changed_error());
        }
        self.mapping_store = captured_store;

        validate_sync_path_images(git, journal, false)?;
        let bundle = store
            .open_object_bundle(journal.effect_id(), bundle_digest)?
            .ok_or_else(|| crate::error::RailError::message("prepared sync object bundle disappeared"))?;
        git.install_prepared_object_pack_and_update_ref(
            bundle.into_file(),
            &store.object_bundle_path(journal.effect_id())?,
            bundle_digest,
            commit,
            &repository.ref_name,
            repository.expected_oid.as_deref(),
            journal.effect_id(),
        )?;
        if !journal.matches_repository_authority(store, git, Some(repository.result_oid.clone()))? {
            return Err(crate::error::RailError::message(
                "prepared sync repository authority changed before final materialization",
            ));
        }
        validate_sync_journal_tree_images(git, journal)?;
        let paths = journal
            .paths()
            .iter()
            .map(|transition| transition.path().to_path_buf())
            .collect::<Vec<_>>();
        git.reconcile_prepared_commit_paths(repository.expected_oid.as_deref(), &repository.result_oid, &paths)?;
        validate_sync_path_images(git, journal, false)?;
        let (post_store, actual_post) = self.capture_mapping_evidence(direction)?;
        if actual_post.digest() != post_authority_digest {
            return Err(sync_mapping_authority_changed_error());
        }
        self.mapping_store = post_store;
        self.mapping_preparation = MappingPreparation::Prepared(actual_post);
        Ok(())
    }

    fn validate_sync_commit_journal_contract(
        &self,
        pre_authority_digest: &str,
        post_authority_digest: &str,
        journal: &GitEffectJournal,
        mutates_target: bool,
    ) -> RailResult<()> {
        let expected_prefix = if mutates_target {
            format!("sync-to-remote-{}-", self.config.crate_name)
        } else {
            format!("sync-from-remote-{}-", self.config.crate_name)
        };
        let repository = journal.repository();
        let commit = journal
            .commit()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no commit"))?;
        let mapping = journal
            .mapping()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no mapping authority"))?;
        if journal.publication().is_some()
            || !journal.operation_id().starts_with(&expected_prefix)
            || mapping.owner() != self.config.crate_name
            || mapping.ownership_snapshot() != self.config.ownership.snapshot_id
            || mapping.pre_authority() != pre_authority_digest
            || mapping.post_authority() != post_authority_digest
            || mapping.migration_count() != 0
            || mapping.migration_digest().is_some()
            || repository.result_oid != commit.oid()
            || journal.object_bundle_digest().is_none()
        {
            return Err(sync_mapping_authority_changed_error());
        }
        for transition in journal.paths() {
            if mutates_target {
                self.config.path_capabilities.authorize_target(transition.path())?;
            } else {
                self.config
                    .path_capabilities
                    .authorize_source_mutation(transition.path())?;
            }
        }
        Ok(())
    }

    fn validate_subsumed_sync_commit_journal(
        &self,
        journal: &GitEffectJournal,
        git: &SystemGit,
        mutates_target: bool,
        is_last_in_worktree: bool,
    ) -> RailResult<()> {
        let mapping = journal
            .mapping()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no mapping authority"))?;
        self.validate_sync_commit_journal_contract(
            mapping.pre_authority(),
            mapping.post_authority(),
            journal,
            mutates_target,
        )?;
        if !journal.is_terminal() {
            return Err(crate::error::RailError::message(
                "a non-terminal prepared sync effect is followed by another effect",
            ));
        }
        let bundle_digest = journal
            .object_bundle_digest()
            .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no object bundle"))?;
        let store = GitEffectStore::open(git)?;
        drop(
            store
                .open_object_bundle(journal.effect_id(), bundle_digest)?
                .ok_or_else(|| crate::error::RailError::message("prepared sync object bundle disappeared"))?,
        );
        git.verify_prepared_commit(
            journal
                .commit()
                .ok_or_else(|| crate::error::RailError::message("prepared sync effect has no commit"))?,
        )?;
        validate_sync_journal_tree_images(git, journal)?;
        if is_last_in_worktree {
            let repository = journal.repository();
            let current_head = git.exact_branch_ref_oid(&repository.ref_name)?;
            if current_head.as_deref() != Some(repository.result_oid.as_str()) {
                return Err(crate::error::RailError::message(
                    "subsumed prepared sync branch is not at its exact terminal ref",
                ));
            }
            validate_sync_repository_authority(&store, git, journal, current_head)?;
            validate_sync_terminal_path_images(git, journal)?;
        }
        Ok(())
    }

    fn apply_remote_commit_to_mono(
        &mut self,
        commit: &crate::git::CommitInfo,
        remote_git: &SystemGit,
        resolved_files: &HashSet<&Path>,
        resolved_contents: &HashMap<PathBuf, Vec<u8>>,
        current_mono_head: &str,
        changed_files: &[(PathBuf, char)], // Pre-fetched from resolve_conflicts to avoid duplicate subprocess call
    ) -> RailResult<Option<String>> {
        let relevant_files = changed_files
            .iter()
            .map(|(remote_path, change_type)| {
                self.map_remote_path_to_mono(remote_path)
                    .map(|mono_path| (remote_path, mono_path, change_type))
            })
            .collect::<RailResult<Vec<_>>>()?;

        let MappingPreparation::Prepared(pre_authority) = &self.mapping_preparation else {
            return Err(crate::error::RailError::message(
                "sync mapping authority must be prepared before a monorepo effect",
            ));
        };
        let pre_authority = pre_authority.clone();

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
        let quarantine = mono_git.object_quarantine()?;
        quarantine.import_object_closure(mono_git, &[current_mono_head])?;
        quarantine.import_object_closure(remote_git, &[&commit.sha])?;
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
            changes.push(GitIndexChange::Upsert(GitTreeEntry {
                mode: entry.mode.clone(),
                object_id: entry.object_id.clone(),
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
                self.config.path_capabilities.authorize_source_mutation(&mono_path)?;
                let content = resolved_contents.get(&mono_path).ok_or_else(|| {
                    crate::error::RailError::message(format!(
                        "prepared sync merge lost exact bytes for '{}'",
                        mono_path.display()
                    ))
                })?;
                let mode = modes.get(&mono_path).cloned().unwrap_or_else(|| "100644".to_string());
                changes.push(GitIndexChange::Upsert(GitTreeEntry {
                    mode,
                    object_id: quarantine.write_blob(content)?,
                    path: mono_path,
                }));
            }
        }

        // Create commit with trailer
        let message = append_origin_trailers(&commit.message, &[self.target_origin.trailer(&commit.sha)?]);

        let parent_shas = self.mapped_source_parents(commit, current_mono_head);
        let (tree, transitions) = prepare_sync_tree(mono_git, current_mono_head, &quarantine, &changes)?;
        let metadata = commit.metadata();
        let new_commit_sha = quarantine.write_commit(&tree, &parent_shas, &message, &metadata)?;
        let commit_effect = GitCommitEffect::new(
            new_commit_sha.clone(),
            tree,
            parent_shas,
            message,
            GitEffectCommitMetadata::from(&metadata),
        );
        let (recaptured_store, recaptured_pre) = self.capture_mapping_evidence(pre_authority.direction())?;
        if recaptured_pre != pre_authority {
            return Err(sync_mapping_authority_changed_error());
        }
        self.mapping_store = recaptured_store;
        self.mapping_store
            .record_target_frontier_mapping(&new_commit_sha, &commit.sha)?;
        self.mapping_store.update_authority_heads(Some(&new_commit_sha), None)?;
        let post_authority = self.mapping_store.mapping_authority_snapshot(
            pre_authority.direction(),
            self.config.path_capabilities.target_root(),
            &self.config.branch,
        )?;
        let store = GitEffectStore::open(mono_git)?;
        let ref_name = format!("refs/heads/{}", mono_git.current_branch()?);
        let repository = store.capture_repository_authority(
            mono_git,
            repository_identity(self.ctx.workspace_root())?,
            ref_name,
            Some(current_mono_head.to_string()),
            new_commit_sha.clone(),
        )?;
        let mapping = GitMappingBinding::new(
            pre_authority.owner().to_string(),
            pre_authority.ownership_snapshot().to_string(),
            pre_authority.digest(),
            post_authority.digest(),
            None,
            0,
        );
        let mut bundle = store.create_object_bundle_temp()?;
        let bundle_digest = quarantine.write_pack(&new_commit_sha, Some(current_mono_head), bundle.file_mut()?)?;
        let intent = GitEffectIntent::new(
            format!(
                "sync-from-remote-{}-{}",
                self.config.crate_name,
                post_authority.digest()
            ),
            repository,
            Some(commit_effect),
            transitions,
            Some(mapping),
            None,
            Some(bundle_digest.clone()),
        )?;
        let effect_id = intent.effect_id()?;
        drop(bundle.persist(&effect_id, &bundle_digest)?);
        let record = store.prepare(intent)?;
        self.reconcile_sync_commit_record(
            pre_authority.direction(),
            &pre_authority.digest(),
            &post_authority.digest(),
            &store,
            record,
            mono_git,
            false,
            false,
        )?;
        Ok(Some(new_commit_sha))
    }

    fn prepare_conflict_materialization(
        &self,
        commit: &crate::git::CommitInfo,
        remote_git: &SystemGit,
        resolution: &ConflictResolutionResult,
        current_mono_head: &str,
    ) -> RailResult<ConflictMaterialization> {
        let mono_git = self.ctx.git()?.git();
        let quarantine = mono_git.object_quarantine()?;
        quarantine.import_object_closure(mono_git, &[current_mono_head])?;
        quarantine.import_object_closure(remote_git, &[&commit.sha])?;

        let remote_paths = resolution
            .changed_files
            .iter()
            .filter(|(_, change_type)| *change_type != 'D')
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let remote_entries = remote_git
            .collect_tree_entries_for_paths(&commit.sha, &remote_paths)?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<HashMap<_, _>>();
        let remote_object_ids = remote_entries
            .values()
            .map(|entry| entry.object_id.as_str())
            .collect::<Vec<_>>();
        let remote_contents = remote_git.read_blobs_bulk(&remote_object_ids)?;
        let incoming = remote_entries
            .values()
            .zip(remote_contents)
            .map(|(entry, content)| (entry.path.clone(), content))
            .collect::<HashMap<_, _>>();
        let old_entries = mono_git
            .collect_tree_entries(current_mono_head, Path::new("."))?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect::<HashMap<_, _>>();

        let mut changes = Vec::with_capacity(resolution.changed_files.len());
        let mut entries = BTreeMap::new();
        for (remote_path, change_type) in &resolution.changed_files {
            let mono_path = self.map_remote_path_to_mono(remote_path)?;
            self.config.path_capabilities.authorize_source_mutation(&mono_path)?;
            if *change_type == 'D' {
                changes.push(GitIndexChange::Delete(mono_path));
                continue;
            }

            let (mode, object_id, content) = if let Some(content) = resolution.resolved_contents.get(&mono_path) {
                let mode = old_entries
                    .get(&mono_path)
                    .map(|entry| entry.mode.clone())
                    .ok_or_else(|| {
                        crate::error::RailError::message(format!(
                            "resolved sync path '{}' has no exact old tree entry",
                            mono_path.display()
                        ))
                    })?;
                if !matches!(mode.as_str(), "100644" | "100755") {
                    return Err(crate::error::RailError::with_help(
                        format!(
                            "resolved sync path '{}' has unsupported Git mode '{}'",
                            mono_path.display(),
                            mode
                        ),
                        "resolve type-changing conflicts in repository history before retrying sync",
                    ));
                }
                (mode, quarantine.write_blob(content)?, content.clone())
            } else {
                let entry = remote_entries.get(remote_path).ok_or_else(|| {
                    crate::error::RailError::message(format!(
                        "remote commit '{}' has no exact tree entry for '{}'",
                        commit.sha,
                        remote_path.display()
                    ))
                })?;
                if !matches!(entry.mode.as_str(), "100644" | "100755" | "120000") {
                    return Err(crate::error::RailError::message(format!(
                        "remote sync path '{}' has unsupported Git mode '{}'",
                        remote_path.display(),
                        entry.mode
                    )));
                }
                let content = incoming.get(remote_path).cloned().ok_or_else(|| {
                    crate::error::RailError::message("prepared conflict materialization lost an incoming blob")
                })?;
                (entry.mode.clone(), entry.object_id.clone(), content)
            };
            changes.push(GitIndexChange::Upsert(GitTreeEntry {
                mode: mode.clone(),
                object_id,
                path: mono_path.clone(),
            }));
            entries.insert(mono_path, MaterializedEntry { mode, content });
        }

        let (_, transitions) = prepare_sync_tree(mono_git, current_mono_head, &quarantine, &changes)?;
        let encoded = serde_json::to_vec(&(self.conflict_resolver.strategy().authority_name(), &transitions)).map_err(
            |error| {
                crate::error::RailError::message(format!("failed to bind prepared conflict materialization: {error}"))
            },
        )?;
        let digest = format!("sha256-{}", crate::source::ContentDigest::sha256(&encoded));
        Ok(ConflictMaterialization {
            digest,
            transitions,
            entries,
        })
    }

    fn reconcile_conflict_materialization(&self, prepared: &ConflictMaterialization) -> RailResult<()> {
        let git = self.ctx.git()?.git();
        let allowed = prepared
            .transitions
            .iter()
            .map(|transition| transition.path().to_path_buf())
            .collect::<BTreeSet<_>>();
        let unrelated = self
            .ctx
            .changed_source_paths()?
            .into_iter()
            .filter(|path| !allowed.contains(path))
            .collect::<Vec<_>>();
        if !unrelated.is_empty() {
            return Err(crate::error::RailError::with_help(
                format!(
                    "conflict materialization found unrelated worktree paths: {}",
                    unrelated
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "preserve or restore unrelated work before retrying the recorded sync receipt",
            ));
        }

        for transition in &prepared.transitions {
            self.config
                .path_capabilities
                .authorize_source_mutation(transition.path())?;
            let index = git.index_entry(transition.path())?;
            if !git_entry_matches_image(index.as_ref(), transition.old()) {
                return Err(crate::error::RailError::with_help(
                    format!(
                        "conflict materialization index path '{}' changed after preparation",
                        transition.path().display()
                    ),
                    "restore the exact pre-conflict index before retrying; cargo-rail preserved the unexpected state",
                ));
            }
            let worktree = git.worktree_entry(transition.path())?;
            if !git_entry_matches_image(worktree.as_ref(), transition.old())
                && !git_entry_matches_image(worktree.as_ref(), transition.new_image())
            {
                return Err(crate::error::RailError::with_help(
                    format!(
                        "conflict materialization path '{}' changed to a third state",
                        transition.path().display()
                    ),
                    "restore the exact old or recorded conflict image before retrying; cargo-rail preserved the unexpected bytes",
                ));
            }
        }

        let mut deletions = prepared
            .transitions
            .iter()
            .filter(|transition| transition.new_image().entry_parts().is_none())
            .map(|transition| transition.path().to_path_buf())
            .collect::<Vec<_>>();
        deletions.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        for path in deletions {
            let absolute = self.config.path_capabilities.authorize_source_mutation(&path)?;
            remove_materialized_leaf(&absolute)?;
        }

        for (path, entry) in &prepared.entries {
            let absolute = self.config.path_capabilities.authorize_source_mutation(path)?;
            materialize_worktree_entry(&absolute, &entry.mode, &entry.content)?;
        }
        for transition in &prepared.transitions {
            let worktree = git.worktree_entry(transition.path())?;
            if !git_entry_matches_image(worktree.as_ref(), transition.new_image()) {
                return Err(crate::error::RailError::message(format!(
                    "conflict materialization path '{}' did not converge to its recorded image",
                    transition.path().display()
                )));
            }
        }
        Ok(())
    }

    fn transform_manifest_to_split(&self, content: &[u8]) -> RailResult<Vec<u8>> {
        let content = std::str::from_utf8(content)
            .map_err(|error| crate::error::RailError::message(format!("Cargo.toml is not UTF-8: {error}")))?;
        let target_has_workspace =
            self.config.mode == SplitMode::Combined && self.config.workspace_mode == WorkspaceMode::Workspace;
        Ok(self
            .transform
            .transform_to_split(content, target_has_workspace)?
            .into_bytes())
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
        if self.config.path_capabilities.owns_asset_path(mono_path)? {
            return Ok(mono_path.to_path_buf());
        }
        match self.config.mode {
            SplitMode::Single => {
                let crate_path = self.config.crate_paths.first().ok_or_else(|| {
                    crate::error::RailError::message("single-mode sync requires exactly one crate path")
                })?;
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
        if self.config.path_capabilities.owns_asset_path(remote_path)? {
            return Ok(remote_path.to_path_buf());
        }
        match self.config.mode {
            SplitMode::Single => {
                let crate_path = self.config.crate_paths.first().ok_or_else(|| {
                    crate::error::RailError::message("single-mode sync requires exactly one crate path")
                })?;
                // Prepend crate path
                Ok(crate_path.join(remote_path))
            }
            SplitMode::Combined => {
                if self.mono_path_in_scope(remote_path)? {
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
        current_mono_head: &str,
    ) -> RailResult<ConflictResolutionResult> {
        // Resolve the closest mapped target ancestor of this exact incoming commit.
        // A later target mapping may be a descendant of an older pending commit and
        // therefore cannot serve as its merge base.
        let last_synced = self.find_mono_base_for_remote_commit(remote_git, &remote_commit.sha)?;

        // Build cache of all files modified in mono since last sync
        // Single git call instead of N calls (one per remote file)
        let mono_changed_paths: std::collections::HashSet<PathBuf> = if let Some(ref last) = last_synced {
            self.ctx
                .git()?
                .git()
                .get_changed_files_between(last, Some(current_mono_head))?
                .into_iter()
                .map(|(path, _)| path)
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // Identify conflicting files (files modified on both sides)
        // Pre-allocate for worst case (all files conflict) - typically much smaller
        let mut conflicting_files = Vec::with_capacity(changed_files.len());
        for (remote_path, change_type) in changed_files {
            let mono_path = self.map_remote_path_to_mono(remote_path)?;
            self.config.path_capabilities.authorize_source_mutation(&mono_path)?;

            // Check if file was modified in mono since last sync (O(1) HashSet lookup)
            let mono_modified = mono_changed_paths.contains(&mono_path);

            // If not modified in mono, no conflict - will be cleanly applied
            if !mono_modified {
                continue;
            }

            if *change_type == 'D' {
                return Err(crate::error::RailError::with_help(
                    format!(
                        "remote deletion of '{}' overlaps a monorepo modification",
                        mono_path.display()
                    ),
                    "resolve the delete/modify conflict in repository history before retrying sync",
                ));
            }

            let Some(current_content) = read_git_file_if_present(self.ctx.git()?.git(), current_mono_head, &mono_path)?
            else {
                continue;
            };

            // Both sides modified - this is a conflict
            conflicting_files.push((remote_path.clone(), mono_path, current_content));
        }

        // Pre-allocate conflicts vec now that we know the size
        let mut conflicts = Vec::with_capacity(conflicting_files.len());
        let mut resolved_files = Vec::with_capacity(conflicting_files.len());
        let mut resolved_contents = HashMap::with_capacity(conflicting_files.len());

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
        for (idx, (_, mono_path, current_content)) in conflicting_files.iter().enumerate() {
            let base_content = if idx < base_contents.len() {
                &base_contents[idx]
            } else {
                &Vec::new()
            };
            let incoming_content = &incoming_contents[idx];

            // Perform 3-way merge
            let (result, merged_content) =
                self.conflict_resolver
                    .resolve_content(mono_path, current_content, base_content, incoming_content)?;
            match result {
                crate::sync::conflict::MergeResult::Success => {
                    progress!("      Auto-merged {}", mono_path.display());
                    resolved_contents.insert(mono_path.clone(), merged_content);
                    resolved_files.push(mono_path.clone());
                }
                crate::sync::conflict::MergeResult::Conflicts(_paths) => {
                    conflicts.push(ConflictInfo {
                        file_path: mono_path.clone(),
                        class: ConflictClass::Content,
                    });
                    resolved_contents.insert(mono_path.clone(), merged_content);
                    resolved_files.push(mono_path.clone());
                }
                crate::sync::conflict::MergeResult::Failed(message) => {
                    return Err(crate::error::RailError::with_help(
                        format!("failed to merge '{}': {}", mono_path.display(), message),
                        "the sync branch and worktree were preserved; correct the underlying Git merge failure and retry",
                    ));
                }
            }
        }

        Ok(ConflictResolutionResult {
            conflicts,
            resolved_files,
            resolved_contents,
            changed_files: changed_files.to_vec(),
        })
    }

    fn pending_mono_commits_at(&self, source_head: &str) -> RailResult<usize> {
        self.collect_pending_mono_commits_at(source_head)
            .map(|commits| commits.len())
    }

    fn pending_remote_commits_at(&self, target_head: Option<&str>) -> RailResult<usize> {
        let remote_git = SystemGit::open(&self.config.target_repo_path)?;
        self.collect_pending_remote_commits_at(&remote_git, target_head)
            .map(|commits| commits.len())
    }
}

fn prepare_sync_tree(
    git: &SystemGit,
    expected_head: &str,
    quarantine: &GitObjectQuarantine,
    changes: &[GitIndexChange],
) -> RailResult<(String, Vec<GitPathTransition>)> {
    let old_entries = git.collect_tree_entries(expected_head, Path::new("."))?;
    let mut entries = old_entries
        .iter()
        .cloned()
        .map(|entry| (entry.path.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut changed = BTreeSet::new();
    for change in changes {
        match change {
            GitIndexChange::Upsert(entry) => {
                if entry.mode == "160000" {
                    return Err(crate::error::RailError::message(format!(
                        "sync path '{}' is an unsupported gitlink",
                        entry.path.display()
                    )));
                }
                if !changed.insert(entry.path.clone()) {
                    return Err(crate::error::RailError::message(format!(
                        "sync preparation repeats path '{}'",
                        entry.path.display()
                    )));
                }
                entries.insert(entry.path.clone(), entry.clone());
            }
            GitIndexChange::Delete(path) => {
                if !changed.insert(path.clone()) {
                    return Err(crate::error::RailError::message(format!(
                        "sync preparation repeats path '{}'",
                        path.display()
                    )));
                }
                entries.remove(path);
            }
        }
    }
    let final_entries = entries.into_values().collect::<Vec<_>>();
    let tree = quarantine.write_exact_tree(&final_entries)?;
    let old = old_entries
        .into_iter()
        .map(|entry| (entry.path, GitPathImage::entry(entry.mode, entry.object_id)))
        .collect::<BTreeMap<_, _>>();
    let new = final_entries
        .into_iter()
        .map(|entry| (entry.path, GitPathImage::entry(entry.mode, entry.object_id)))
        .collect::<BTreeMap<_, _>>();
    let paths = old.keys().chain(new.keys()).cloned().collect::<BTreeSet<_>>();
    let transitions = paths
        .into_iter()
        .filter_map(|path| {
            let old = old.get(&path).cloned().unwrap_or(GitPathImage::Absent);
            let new = new.get(&path).cloned().unwrap_or(GitPathImage::Absent);
            (old != new).then(|| GitPathTransition::new(&path, old, new))
        })
        .collect::<RailResult<Vec<_>>>()?;
    Ok((tree, transitions))
}

fn prepare_worktree_changes(
    git: &SystemGit,
    quarantine: &GitObjectQuarantine,
    paths: &[PathBuf],
) -> RailResult<Vec<GitIndexChange>> {
    let mut ordered = paths.to_vec();
    ordered.sort();
    ordered.dedup();
    let mut changes = Vec::with_capacity(ordered.len());
    for path in ordered {
        let path = crate::source::RepositoryPath::new(&path)?.as_path().to_path_buf();
        let absolute = git.worktree_root.join(&path);
        let metadata = match std::fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                changes.push(GitIndexChange::Delete(path));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let observed = git.worktree_entry(&path)?.ok_or_else(|| {
            crate::error::RailError::message(format!(
                "resolved sync path '{}' disappeared during preparation",
                path.display()
            ))
        })?;
        let content = if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(&absolute)?;
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt as _;
                target.as_os_str().as_bytes().to_vec()
            }
            #[cfg(not(unix))]
            {
                target.to_string_lossy().into_owned().into_bytes()
            }
        } else if metadata.is_file() {
            std::fs::read(&absolute)?
        } else {
            return Err(crate::error::RailError::message(format!(
                "resolved sync path '{}' is not a regular file or symlink",
                path.display()
            )));
        };
        let object_id = quarantine.write_blob(&content)?;
        if object_id != observed.object_id {
            return Err(crate::error::RailError::message(format!(
                "resolved sync path '{}' changed while its exact blob was prepared",
                path.display()
            )));
        }
        changes.push(GitIndexChange::Upsert(GitTreeEntry {
            mode: observed.mode,
            object_id,
            path,
        }));
    }
    Ok(changes)
}

fn validate_sync_path_images(git: &SystemGit, journal: &GitEffectJournal, require_old: bool) -> RailResult<()> {
    for transition in journal.paths() {
        let index = git.index_entry(transition.path())?;
        let worktree = git.worktree_entry(transition.path())?;
        let index_old = git_entry_matches_image(index.as_ref(), transition.old());
        let worktree_old = git_entry_matches_image(worktree.as_ref(), transition.old());
        let index_new = git_entry_matches_image(index.as_ref(), transition.new_image());
        let worktree_new = git_entry_matches_image(worktree.as_ref(), transition.new_image());
        let accepted = if require_old {
            index_old && worktree_old
        } else {
            (index_old || index_new) && (worktree_old || worktree_new)
        };
        if !accepted {
            return Err(crate::error::RailError::with_help(
                format!(
                    "prepared sync path '{}' changed to an unauthorized state",
                    transition.path().display()
                ),
                "cargo-rail preserved the path; restore its exact old or prepared image before retrying",
            ));
        }
    }
    Ok(())
}

fn validate_sync_terminal_path_images(git: &SystemGit, journal: &GitEffectJournal) -> RailResult<()> {
    for transition in journal.paths() {
        let index = git.index_entry(transition.path())?;
        let worktree = git.worktree_entry(transition.path())?;
        if !git_entry_matches_image(index.as_ref(), transition.new_image())
            || !git_entry_matches_image(worktree.as_ref(), transition.new_image())
        {
            return Err(crate::error::RailError::with_help(
                format!(
                    "terminal prepared sync path '{}' no longer has its exact result image",
                    transition.path().display()
                ),
                "restore the journaled result path before retrying the remaining effect chain",
            ));
        }
    }
    Ok(())
}

fn validate_sync_repository_authority(
    store: &GitEffectStore,
    git: &SystemGit,
    journal: &GitEffectJournal,
    current_head: Option<String>,
) -> RailResult<()> {
    let repository = journal.repository();
    let current_repository = store.capture_repository_authority(
        git,
        repository.logical_repository.clone(),
        repository.ref_name.clone(),
        current_head,
        repository.result_oid.clone(),
    )?;
    if current_repository.common_dir_identity != repository.common_dir_identity
        || current_repository.worktree_identity != repository.worktree_identity
        || current_repository.object_format != repository.object_format
        || current_repository.ref_name != repository.ref_name
        || current_repository.symbolic_head != repository.symbolic_head
    {
        return Err(crate::error::RailError::message(
            "prepared sync repository authority changed during recovery",
        ));
    }
    Ok(())
}

fn validate_sync_journal_tree_images(git: &SystemGit, journal: &GitEffectJournal) -> RailResult<()> {
    let repository = journal.repository();
    for transition in journal.paths() {
        let old = repository
            .expected_oid
            .as_deref()
            .map_or(Ok(None), |expected| git.tree_entry(expected, transition.path()))?;
        let new = git.tree_entry(&repository.result_oid, transition.path())?;
        if !git_entry_matches_image(old.as_ref(), transition.old())
            || !git_entry_matches_image(new.as_ref(), transition.new_image())
        {
            return Err(crate::error::RailError::message(format!(
                "prepared sync journal path '{}' disagrees with its exact old or result tree",
                transition.path().display()
            )));
        }
    }
    Ok(())
}

fn git_entry_matches_image(entry: Option<&GitTreeEntry>, image: &GitPathImage) -> bool {
    match (entry, image.entry_parts()) {
        (None, None) => true,
        (Some(entry), Some((mode, oid))) => entry.mode == mode && entry.object_id == oid,
        _ => false,
    }
}

fn sync_mapping_authority_changed_error() -> crate::error::RailError {
    crate::error::RailError::with_help(
        "sync mapping authority changed during a prepared Git effect",
        "restore the exact journaled histories, branch, ownership, and mapping evidence before retrying",
    )
}

fn pending_origin_migration_after_preparation() -> crate::error::RailError {
    crate::error::RailError::with_help(
        "sync found new predecessor origin evidence after migration preparation",
        "restart sync from a fresh check/apply plan; prepared sync phases never authorize another migration",
    )
}

fn unproven_mapping_ancestry_error(ancestor: &str, endpoint: &str, side: &str) -> crate::error::RailError {
    crate::error::RailError::with_help(
        format!(
            "exact predecessor mapping endpoint '{}' has unmatched {} ancestor '{}' without directional frontier proof",
            endpoint, side, ancestor
        ),
        "restore authoritative directional origin history or resolve the mapping topology manually; cargo-rail will not guess ancestry or replay an ancestor after its mapped descendant",
    )
}

fn contains_conflict_markers(content: &[u8]) -> bool {
    content
        .split(|byte| *byte == b'\n')
        .any(|line| line.starts_with(b"<<<<<<<") || line.starts_with(b"=======") || line.starts_with(b">>>>>>>"))
}

fn read_git_file_if_present(git: &SystemGit, commit: &str, path: &Path) -> RailResult<Option<Vec<u8>>> {
    let entry = git
        .collect_tree_entries_for_paths(commit, &[path.to_path_buf()])?
        .into_iter()
        .find(|entry| entry.path == path);
    let Some(entry) = entry else {
        return Ok(None);
    };
    Ok(git.read_blobs_bulk(&[entry.object_id.as_str()])?.into_iter().next())
}

fn merge_would_conflict(base: &[u8], current: &[u8], incoming: &[u8]) -> RailResult<bool> {
    let temp = tempfile::tempdir()?;
    let base_path = temp.path().join("base");
    let current_path = temp.path().join("current");
    let incoming_path = temp.path().join("incoming");
    std::fs::write(&base_path, base)?;
    std::fs::write(&current_path, current)?;
    std::fs::write(&incoming_path, incoming)?;
    let output = crate::git::git_command()
        .args(["merge-file", "-p"])
        .arg(&current_path)
        .arg(&base_path)
        .arg(&incoming_path)
        .output()?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(crate::error::RailError::with_help(
            "failed to reconstruct the conflict receipt three-way merge",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )),
    }
}

fn remove_materialized_leaf(path: &Path) -> RailResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Ok(std::fs::remove_file(path)?),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn materialize_worktree_entry(path: &Path, mode: &str, content: &[u8]) -> RailResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        std::fs::remove_dir(path).map_err(|error| {
            crate::error::RailError::message(format!(
                "refusing to replace non-empty conflict materialization directory '{}': {error}",
                path.display()
            ))
        })?;
    }
    match mode {
        "100644" | "100755" => {
            if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
                std::fs::remove_file(path)?;
            }
            utils::write_file_atomic(path, content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;

                let permissions = std::fs::Permissions::from_mode(if mode == "100755" { 0o755 } else { 0o644 });
                std::fs::set_permissions(path, permissions)?;
            }
            Ok(())
        }
        "120000" => {
            remove_materialized_leaf(path)?;
            #[cfg(unix)]
            {
                use std::ffi::OsString;
                use std::os::unix::ffi::OsStringExt as _;

                std::os::unix::fs::symlink(OsString::from_vec(content.to_vec()), path)?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = content;
                Err(crate::error::RailError::with_help(
                    format!(
                        "cannot safely materialize symlink conflict input '{}' on this host",
                        path.display()
                    ),
                    "resolve this sync on a host with native symlink support",
                ))
            }
        }
        _ => Err(crate::error::RailError::message(format!(
            "prepared conflict materialization has unsupported Git mode '{mode}'"
        ))),
    }
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
