//! Durable, repository-owned authority for prepared Git effects.
//!
//! Split and sync effects cross several independently durable boundaries: Git
//! objects, a local branch ref, index/worktree materialization, and optionally a
//! remote ref. This module owns the small journal needed to reconcile those
//! boundaries after interruption. It deliberately does not execute Git effects;
//! callers must first persist a [`GitEffectIntent`], then use the journal's
//! monotonic phase as recovery evidence while observing the real repository.

use crate::error::{RailError, RailResult};
use crate::git::ops::GitTreeEntry;
use crate::git::{CommitMetadata, SystemGit};
use crate::source::{ContentDigest, RepositoryPath};
use crate::utils;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::ffi::{OsStr, OsString};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const EFFECT_SCHEMA_VERSION: u32 = 1;
const EFFECT_ROOT: &str = "effects-v1";
const OWNER_MARKER: &str = "OWNER";
const OWNER_MARKER_BYTES: &[u8] = b"cargo-rail prepared git effects v1\n";
const MAX_JOURNAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_JOURNALS: usize = 4_096;
const MAX_PATHS: usize = 100_000;
const MAX_PARENTS: usize = 1_024;
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const TEMP_PREFIX: &str = ".cargo-rail-";
const TEMP_SUFFIX: &str = ".tmp";
const MAX_OBJECT_BUNDLE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const ORPHAN_GRACE_SECONDS: u64 = 60 * 60;

/// Durable phase of one prepared Git effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GitEffectPhase {
    /// Exact immutable intent exists before any repository-owned effect.
    Prepared,
    /// The exact local ref and owned paths have converged to their result state.
    LocalApplied,
    /// The exact remote ref has converged to the desired result state.
    Published,
}

/// Complete physical and logical authority for one local Git branch effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitEffectRepositoryAuthority {
    /// Stable logical repository identity used by provenance contracts.
    pub(crate) logical_repository: String,
    /// Stable identity of the physical Git common directory.
    pub(crate) common_dir_identity: String,
    /// Stable identity of the exact worktree whose paths may be materialized.
    pub(crate) worktree_identity: String,
    /// Git object format (`sha1` or `sha256`).
    pub(crate) object_format: String,
    /// Exact local branch ref updated by compare-and-swap.
    pub(crate) ref_name: String,
    /// Exact symbolic `HEAD`, which must equal `ref_name`.
    pub(crate) symbolic_head: String,
    /// Exact old branch object ID, or absence for an unborn branch.
    pub(crate) expected_oid: Option<String>,
    /// Exact desired branch object ID.
    pub(crate) result_oid: String,
}

/// Exact author and committer metadata for a prepared commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitEffectCommitMetadata {
    /// Author name.
    pub(crate) author: String,
    /// Author email address.
    pub(crate) author_email: String,
    /// Author timestamp in seconds since the Unix epoch.
    pub(crate) author_timestamp: i64,
    /// Author time-zone offset in Git's `+HHMM` or `-HHMM` form.
    pub(crate) author_timezone: String,
    /// Committer name.
    pub(crate) committer: String,
    /// Committer email address.
    pub(crate) committer_email: String,
    /// Committer timestamp in seconds since the Unix epoch.
    pub(crate) committer_timestamp: i64,
    /// Committer time-zone offset in Git's `+HHMM` or `-HHMM` form.
    pub(crate) committer_timezone: String,
}

impl From<&CommitMetadata> for GitEffectCommitMetadata {
    fn from(metadata: &CommitMetadata) -> Self {
        Self {
            author: metadata.author.clone(),
            author_email: metadata.author_email.clone(),
            author_timestamp: metadata.author_timestamp,
            author_timezone: metadata.author_timezone.clone(),
            committer: metadata.committer.clone(),
            committer_email: metadata.committer_email.clone(),
            committer_timestamp: metadata.committer_timestamp,
            committer_timezone: metadata.committer_timezone.clone(),
        }
    }
}

impl GitCommitEffect {
    /// Construct one exact prepared commit description.
    pub(crate) fn new(
        oid: String,
        tree: String,
        parents: Vec<String>,
        message: String,
        metadata: GitEffectCommitMetadata,
    ) -> Self {
        Self {
            oid,
            tree,
            parents,
            message,
            metadata,
        }
    }

    /// Return the prepared commit object ID.
    pub(crate) fn oid(&self) -> &str {
        &self.oid
    }

    /// Return the prepared root tree object ID.
    pub(crate) fn tree(&self) -> &str {
        &self.tree
    }

    /// Return the ordered prepared parent object IDs.
    pub(crate) fn parents(&self) -> &[String] {
        &self.parents
    }

    /// Return the exact prepared commit message.
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// Return the exact prepared commit metadata.
    pub(crate) fn metadata(&self) -> &GitEffectCommitMetadata {
        &self.metadata
    }
}

/// Exact commit object prepared for the local branch effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitCommitEffect {
    /// Exact prepared commit object ID.
    pub(crate) oid: String,
    /// Exact root tree object ID.
    pub(crate) tree: String,
    /// Ordered parent object IDs.
    pub(crate) parents: Vec<String>,
    /// Exact raw commit message supplied to `commit-tree`.
    pub(crate) message: String,
    /// Exact author and committer identity.
    pub(crate) metadata: GitEffectCommitMetadata,
}

/// One exact path image in a Git tree or worktree transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum GitPathImage {
    /// The path does not exist.
    Absent,
    /// The path names one exact Git entry.
    Entry {
        /// Git tree mode (`100644`, `100755`, or `120000`).
        mode: String,
        /// Exact blob or gitlink object ID.
        oid: String,
    },
}

/// Exact old-to-new transition for one repository-relative owned path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitPathTransition {
    /// Canonical UTF-8 Git path using `/` separators.
    path: String,
    /// Exact image accepted before local application.
    pub(crate) old: GitPathImage,
    /// Exact image required after local application.
    pub(crate) new: GitPathImage,
}

impl GitPathTransition {
    /// Construct a canonical repository-relative path transition.
    pub(crate) fn new(path: &Path, old: GitPathImage, new: GitPathImage) -> RailResult<Self> {
        let path = RepositoryPath::new(path)?.to_string();
        let transition = Self { path, old, new };
        if transition.old == transition.new {
            return Err(RailError::message(format!(
                "prepared Git path '{}' has no state transition",
                transition.path
            )));
        }
        Ok(transition)
    }

    /// Return the canonical repository-relative path.
    pub(crate) fn path(&self) -> &Path {
        Path::new(&self.path)
    }

    /// Return the exact accepted old image.
    pub(crate) fn old(&self) -> &GitPathImage {
        &self.old
    }

    /// Return the exact required new image.
    pub(crate) fn new_image(&self) -> &GitPathImage {
        &self.new
    }
}

impl GitPathImage {
    /// Construct one exact present path image.
    pub(crate) fn entry(mode: String, oid: String) -> Self {
        Self::Entry { mode, oid }
    }

    /// Return the present mode and object ID, or `None` for absence.
    pub(crate) fn entry_parts(&self) -> Option<(&str, &str)> {
        match self {
            Self::Absent => None,
            Self::Entry { mode, oid } => Some((mode, oid)),
        }
    }
}

/// Mapping authority bound to a prepared split or sync effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitMappingBinding {
    /// Split owner whose mappings are advanced.
    pub(crate) owner: String,
    /// Stable ownership-policy identity.
    pub(crate) ownership_snapshot: String,
    /// Exact mapping authority digest before the effect.
    pub(crate) pre_authority: String,
    /// Exact mapping authority digest after the effect.
    pub(crate) post_authority: String,
    /// Exact predecessor migration candidate digest, when migration is present.
    pub(crate) migration_digest: Option<String>,
    /// Exact predecessor migration candidate count.
    pub(crate) migration_count: usize,
}

/// Exact remote branch publication intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitPublicationEffect {
    /// Logical identity of the remote repository.
    pub(crate) logical_remote: String,
    /// Digest of the exact configured endpoint used for publication.
    pub(crate) exact_endpoint_digest: String,
    /// Exact destination branch ref.
    pub(crate) ref_name: String,
    /// Exact old remote object ID, or absence for first publication.
    pub(crate) expected_oid: Option<String>,
    /// Exact desired remote object ID.
    pub(crate) desired_oid: String,
}

impl GitMappingBinding {
    /// Construct one exact mapping-authority transition.
    pub(crate) fn new(
        owner: String,
        ownership_snapshot: String,
        pre_authority: String,
        post_authority: String,
        migration_digest: Option<String>,
        migration_count: usize,
    ) -> Self {
        Self {
            owner,
            ownership_snapshot,
            pre_authority,
            post_authority,
            migration_digest,
            migration_count,
        }
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) fn ownership_snapshot(&self) -> &str {
        &self.ownership_snapshot
    }

    pub(crate) fn pre_authority(&self) -> &str {
        &self.pre_authority
    }

    pub(crate) fn post_authority(&self) -> &str {
        &self.post_authority
    }

    pub(crate) fn migration_digest(&self) -> Option<&str> {
        self.migration_digest.as_deref()
    }

    pub(crate) fn migration_count(&self) -> usize {
        self.migration_count
    }
}

impl GitPublicationEffect {
    /// Construct one exact remote publication transition.
    pub(crate) fn new(
        logical_remote: String,
        exact_endpoint_digest: String,
        ref_name: String,
        expected_oid: Option<String>,
        desired_oid: String,
    ) -> Self {
        Self {
            logical_remote,
            exact_endpoint_digest,
            ref_name,
            expected_oid,
            desired_oid,
        }
    }

    pub(crate) fn logical_remote(&self) -> &str {
        &self.logical_remote
    }

    pub(crate) fn exact_endpoint_digest(&self) -> &str {
        &self.exact_endpoint_digest
    }

    pub(crate) fn ref_name(&self) -> &str {
        &self.ref_name
    }

    pub(crate) fn expected_oid(&self) -> Option<&str> {
        self.expected_oid.as_deref()
    }

    pub(crate) fn desired_oid(&self) -> &str {
        &self.desired_oid
    }
}

/// Immutable payload that must be durable before a Git effect begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitEffectIntent {
    operation_id: String,
    repository: GitEffectRepositoryAuthority,
    commit: Option<GitCommitEffect>,
    paths: Vec<GitPathTransition>,
    mapping: Option<GitMappingBinding>,
    publication: Option<GitPublicationEffect>,
    object_bundle_digest: Option<String>,
}

impl GitEffectIntent {
    /// Construct and validate one canonical prepared effect intent.
    pub(crate) fn new(
        operation_id: String,
        repository: GitEffectRepositoryAuthority,
        commit: Option<GitCommitEffect>,
        mut paths: Vec<GitPathTransition>,
        mapping: Option<GitMappingBinding>,
        publication: Option<GitPublicationEffect>,
        object_bundle_digest: Option<String>,
    ) -> RailResult<Self> {
        paths.sort_by(|left, right| left.path.cmp(&right.path));
        let intent = Self {
            operation_id,
            repository,
            commit,
            paths,
            mapping,
            publication,
            object_bundle_digest,
        };
        intent.validate()?;
        Ok(intent)
    }

    /// Return the deterministic identity of this exact effect.
    pub(crate) fn effect_id(&self) -> RailResult<String> {
        let payload_digest = self.payload_digest()?;
        let mut canonical = CanonicalBytes::new(b"cargo-rail-git-effect-id-v1");
        canonical.field(b"operation-id", self.operation_id.as_bytes())?;
        canonical.field(b"payload-digest", payload_digest.as_bytes())?;
        Ok(format!(
            "git-effect-v1-sha256-{}",
            ContentDigest::sha256(canonical.as_slice())
        ))
    }

    /// Return the canonical SHA-256 digest of every immutable payload field.
    pub(crate) fn payload_digest(&self) -> RailResult<String> {
        Ok(format!(
            "sha256-{}",
            ContentDigest::sha256(&self.canonical_payload_bytes()?)
        ))
    }

    /// Return the exact repository mutation authority.
    #[cfg(test)]
    pub(crate) fn repository(&self) -> &GitEffectRepositoryAuthority {
        &self.repository
    }

    fn validate(&self) -> RailResult<()> {
        validate_token("operation ID", &self.operation_id)?;
        self.repository.validate()?;
        if self.paths.len() > MAX_PATHS {
            return Err(RailError::message(format!(
                "prepared Git effect exceeds its {MAX_PATHS}-path bound"
            )));
        }
        let mut previous = None;
        for transition in &self.paths {
            transition.validate(&self.repository.object_format)?;
            if previous == Some(transition.path.as_str()) {
                return Err(RailError::message(format!(
                    "prepared Git effect repeats path '{}'",
                    transition.path
                )));
            }
            if previous.is_some_and(|previous| previous > transition.path.as_str()) {
                return Err(RailError::message("prepared Git effect paths are not canonical"));
            }
            previous = Some(transition.path.as_str());
        }
        if let Some(commit) = &self.commit {
            commit.validate(&self.repository.object_format)?;
            if commit.oid != self.repository.result_oid {
                return Err(RailError::message(
                    "prepared commit object ID does not match the repository result object ID",
                ));
            }
        }
        if let Some(mapping) = &self.mapping {
            mapping.validate()?;
        }
        if let Some(publication) = &self.publication {
            publication.validate(&self.repository.object_format)?;
            if publication.desired_oid != self.repository.result_oid {
                return Err(RailError::message(
                    "prepared publication object ID does not match the repository result object ID",
                ));
            }
        }
        if let Some(digest) = &self.object_bundle_digest {
            validate_sha256("object bundle digest", digest)?;
        }
        if self.repository.expected_oid.as_deref() == Some(self.repository.result_oid.as_str()) {
            let push_only = self.commit.is_none()
                && self.paths.is_empty()
                && self.mapping.is_none()
                && self.publication.is_some()
                && self.object_bundle_digest.is_none();
            if !push_only {
                return Err(RailError::message(
                    "prepared Git effect may keep its local ref unchanged only for an exact push-only intent",
                ));
            }
        }
        Ok(())
    }

    fn canonical_payload_bytes(&self) -> RailResult<Vec<u8>> {
        let mut canonical = CanonicalBytes::new(b"cargo-rail-git-effect-payload-v1");
        canonical.field(b"operation-id", self.operation_id.as_bytes())?;
        canonical.nested(b"repository", |repository| self.repository.encode(repository))?;
        canonical.optional_nested(b"commit", self.commit.as_ref(), GitCommitEffect::encode)?;
        canonical.sequence(b"paths", &self.paths, GitPathTransition::encode)?;
        canonical.optional_nested(b"mapping", self.mapping.as_ref(), GitMappingBinding::encode)?;
        canonical.optional_nested(b"publication", self.publication.as_ref(), GitPublicationEffect::encode)?;
        canonical.optional_field(b"object-bundle-digest", self.object_bundle_digest.as_deref())?;
        Ok(canonical.into_bytes())
    }
}

/// Strict durable representation of one prepared effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitEffectJournal {
    /// Journal schema version.
    schema_version: u32,
    /// Deterministic exact effect identity.
    effect_id: String,
    /// Mutation-plan operation identity.
    operation_id: String,
    /// Last durably completed state-machine phase.
    phase: GitEffectPhase,
    /// Exact local repository authority.
    repository: GitEffectRepositoryAuthority,
    /// Exact prepared commit, when this effect creates one.
    commit: Option<GitCommitEffect>,
    /// Complete sorted old-to-new path transition set.
    paths: Vec<GitPathTransition>,
    /// Mapping authority transition, when applicable.
    mapping: Option<GitMappingBinding>,
    /// Remote publication authority, when applicable.
    publication: Option<GitPublicationEffect>,
    /// Digest of the exact prepared object bundle, when applicable.
    object_bundle_digest: Option<String>,
    /// Canonical digest of every immutable field above.
    payload_digest: String,
}

impl GitEffectJournal {
    fn prepared(intent: GitEffectIntent) -> RailResult<Self> {
        let payload_digest = intent.payload_digest()?;
        let effect_id = intent.effect_id()?;
        Ok(Self {
            schema_version: EFFECT_SCHEMA_VERSION,
            effect_id,
            operation_id: intent.operation_id,
            phase: GitEffectPhase::Prepared,
            repository: intent.repository,
            commit: intent.commit,
            paths: intent.paths,
            mapping: intent.mapping,
            publication: intent.publication,
            object_bundle_digest: intent.object_bundle_digest,
            payload_digest,
        })
    }

    /// Return this journal's deterministic effect identity.
    pub(crate) fn effect_id(&self) -> &str {
        &self.effect_id
    }

    /// Return the digest binding every immutable effect field.
    pub(crate) fn payload_digest(&self) -> &str {
        &self.payload_digest
    }

    /// Return the last durably completed phase.
    #[cfg(test)]
    pub(crate) fn phase(&self) -> GitEffectPhase {
        self.phase
    }

    /// Return the exact repository mutation authority.
    pub(crate) fn repository(&self) -> &GitEffectRepositoryAuthority {
        &self.repository
    }

    /// Return the mutation-plan operation identity.
    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Return the exact prepared commit, when present.
    pub(crate) fn commit(&self) -> Option<&GitCommitEffect> {
        self.commit.as_ref()
    }

    /// Return the complete canonical path transition set.
    pub(crate) fn paths(&self) -> &[GitPathTransition] {
        &self.paths
    }

    /// Return the optional mapping-authority transition.
    pub(crate) fn mapping(&self) -> Option<&GitMappingBinding> {
        self.mapping.as_ref()
    }

    /// Return the optional publication authority.
    pub(crate) fn publication(&self) -> Option<&GitPublicationEffect> {
        self.publication.as_ref()
    }

    /// Return the optional exact prepared object-bundle digest.
    pub(crate) fn object_bundle_digest(&self) -> Option<&str> {
        self.object_bundle_digest.as_deref()
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.phase, GitEffectPhase::LocalApplied | GitEffectPhase::Published)
    }

    fn audit(&self) -> GitEffectAudit {
        GitEffectAudit {
            effect_id: self.effect_id.clone(),
            operation_id: self.operation_id.clone(),
            payload_digest: self.payload_digest.clone(),
            result_oid: self.repository.result_oid.clone(),
            published_oid: self
                .publication
                .as_ref()
                .map(|publication| publication.desired_oid.clone()),
        }
    }

    /// Return whether the repository is in an exact old/new local state owned
    /// by this active journal.
    ///
    /// This is the narrow exception to ordinary target-clean preflight: after
    /// a crash, the bound ref may already name the prepared commit while the
    /// index or worktree still contains some old images. Every obstructing path
    /// must be journal-owned and every index/worktree image must be exactly old
    /// or prepared. A third state remains user-owned and fails closed.
    pub(crate) fn permits_local_recovery_state(&self, git: &SystemGit) -> RailResult<bool> {
        self.permits_recovery_state(git, true)
    }

    /// Return whether this journal still owns its exact ref and path images,
    /// while preserving unrelated source-worktree state authorized by an
    /// `--allow-dirty` sync plan.
    pub(crate) fn permits_owned_path_recovery_state(&self, git: &SystemGit) -> RailResult<bool> {
        self.permits_recovery_state(git, false)
    }

    /// Revalidate the exact physical worktree and symbolic branch bound by
    /// this journal at a final local or remote mutation boundary.
    pub(crate) fn matches_repository_authority(
        &self,
        store: &GitEffectStore,
        git: &SystemGit,
        current_oid: Option<String>,
    ) -> RailResult<bool> {
        let repository = &self.repository;
        let current = store.capture_repository_authority(
            git,
            repository.logical_repository.clone(),
            repository.ref_name.clone(),
            current_oid,
            repository.result_oid.clone(),
        )?;
        Ok(current.logical_repository == repository.logical_repository
            && current.common_dir_identity == repository.common_dir_identity
            && current.worktree_identity == repository.worktree_identity
            && current.object_format == repository.object_format
            && current.ref_name == repository.ref_name
            && current.symbolic_head == repository.symbolic_head
            && current.result_oid == repository.result_oid)
    }

    fn permits_recovery_state(&self, git: &SystemGit, require_exclusive_worktree: bool) -> RailResult<bool> {
        let Some(store) = GitEffectStore::observe(git)? else {
            return Ok(false);
        };
        if store.common_dir_identity != self.repository.common_dir_identity {
            return Ok(false);
        }
        if git.run_git_stdout(&["symbolic-ref", "-q", "HEAD"])? != self.repository.symbolic_head {
            return Ok(false);
        }
        let current = git.exact_branch_ref_oid(&self.repository.ref_name)?;
        if current.as_deref() != self.repository.expected_oid.as_deref()
            && current.as_deref() != Some(self.repository.result_oid.as_str())
        {
            return Ok(false);
        }
        let owned = self
            .paths
            .iter()
            .map(|transition| transition.path())
            .collect::<std::collections::BTreeSet<_>>();
        if require_exclusive_worktree
            && git
                .obstructing_worktree_paths()?
                .iter()
                .any(|path| !owned.contains(path.as_path()))
        {
            return Ok(false);
        }
        let paths = self
            .paths
            .iter()
            .map(|transition| transition.path().to_path_buf())
            .collect::<Vec<_>>();
        let images = git.exact_path_images(&paths)?;
        for (transition, images) in self.paths.iter().zip(images) {
            if !entry_matches_image(images.index.as_ref(), transition.old())
                && !entry_matches_image(images.index.as_ref(), transition.new_image())
            {
                return Ok(false);
            }
            if !entry_matches_image(images.worktree.as_ref(), transition.old())
                && !entry_matches_image(images.worktree.as_ref(), transition.new_image())
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn intent(&self) -> RailResult<GitEffectIntent> {
        GitEffectIntent::new(
            self.operation_id.clone(),
            self.repository.clone(),
            self.commit.clone(),
            self.paths.clone(),
            self.mapping.clone(),
            self.publication.clone(),
            self.object_bundle_digest.clone(),
        )
    }

    fn validate(&self) -> RailResult<()> {
        if self.schema_version != EFFECT_SCHEMA_VERSION {
            return Err(RailError::message(format!(
                "unsupported prepared Git effect schema {}",
                self.schema_version
            )));
        }
        let intent = self.intent()?;
        if intent.payload_digest()? != self.payload_digest {
            return Err(RailError::message(format!(
                "prepared Git effect '{}' payload digest does not match its immutable fields",
                self.effect_id
            )));
        }
        if intent.effect_id()? != self.effect_id {
            return Err(RailError::message(format!(
                "prepared Git effect '{}' identity does not match its payload",
                self.effect_id
            )));
        }
        if self.phase == GitEffectPhase::Published && self.publication.is_none() {
            return Err(RailError::message(
                "local-only prepared Git effect cannot have a published phase",
            ));
        }
        Ok(())
    }

    fn same_immutable_payload(&self, other: &Self) -> RailResult<bool> {
        Ok(self.intent()? == other.intent()?
            && self.payload_digest == other.payload_digest
            && self.effect_id == other.effect_id)
    }
}

/// Order a complete mapping-effect chain by exact pre/post authority edges.
pub(crate) fn ordered_mapping_effect_indices(journals: &[GitEffectJournal]) -> RailResult<Vec<usize>> {
    if journals.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_pre = std::collections::BTreeMap::new();
    let mut posts = std::collections::BTreeSet::new();
    for (index, journal) in journals.iter().enumerate() {
        let mapping = journal
            .mapping()
            .ok_or_else(|| RailError::message("prepared mapping chain contains an effect without mapping authority"))?;
        if by_pre.insert(mapping.pre_authority(), index).is_some() || !posts.insert(mapping.post_authority()) {
            return Err(RailError::message(
                "prepared mapping effects contain a forked or joined authority chain",
            ));
        }
    }
    let starts = by_pre
        .iter()
        .filter_map(|(pre, index)| (!posts.contains(pre)).then_some(*index))
        .collect::<Vec<_>>();
    if starts.len() != 1 {
        return Err(RailError::message(
            "prepared mapping effects do not have one exact chain start",
        ));
    }
    let mut ordered = Vec::with_capacity(journals.len());
    let mut visited = vec![false; journals.len()];
    let mut index = starts[0];
    loop {
        if visited[index] {
            return Err(RailError::message(
                "prepared mapping effects contain an authority cycle",
            ));
        }
        visited[index] = true;
        ordered.push(index);
        let post = journals[index]
            .mapping()
            .expect("mapping journals validated above")
            .post_authority();
        let Some(next) = by_pre.get(post).copied() else {
            break;
        };
        index = next;
    }
    if ordered.len() != journals.len() {
        return Err(RailError::message(
            "prepared mapping effects contain disconnected authority chains",
        ));
    }
    Ok(ordered)
}

fn entry_matches_image(entry: Option<&GitTreeEntry>, image: &GitPathImage) -> bool {
    match (entry, image) {
        (None, GitPathImage::Absent) => true,
        (Some(entry), GitPathImage::Entry { mode, oid }) => entry.mode == *mode && entry.object_id == *oid,
        _ => false,
    }
}

/// Result of preparing or resuming an effect journal.
#[derive(Debug)]
pub(crate) enum GitEffectRecord {
    /// Effect still requires local or remote reconciliation.
    Active(ActiveGitEffect),
    /// Effect already reached its durable terminal state.
    Completed(CompletedGitEffect),
}

/// Immutable fields copied into an ordinary mutation receipt before terminal
/// recovery state is acknowledged and removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GitEffectAudit {
    pub(crate) effect_id: String,
    pub(crate) operation_id: String,
    pub(crate) payload_digest: String,
    pub(crate) result_oid: String,
    pub(crate) published_oid: Option<String>,
}

/// A ref-locked active effect journal.
#[derive(Debug)]
pub(crate) struct ActiveGitEffect {
    store: GitEffectStore,
    journal: GitEffectJournal,
    _lock: File,
}

impl ActiveGitEffect {
    /// Return the currently durable journal state.
    pub(crate) fn journal(&self) -> &GitEffectJournal {
        &self.journal
    }

    /// Durably advance from `prepared` to `local_applied`.
    pub(crate) fn mark_local_applied(&mut self) -> RailResult<()> {
        match self.journal.phase {
            GitEffectPhase::Prepared => self.transition(GitEffectPhase::LocalApplied),
            GitEffectPhase::LocalApplied | GitEffectPhase::Published => Ok(()),
        }
    }

    /// Durably advance from `local_applied` to `published`.
    pub(crate) fn mark_published(&mut self) -> RailResult<()> {
        if self.journal.publication.is_none() {
            return Err(RailError::message(
                "local-only prepared Git effect has no publication phase",
            ));
        }
        match self.journal.phase {
            GitEffectPhase::Prepared => Err(RailError::message(
                "prepared Git effect cannot be published before local application",
            )),
            GitEffectPhase::LocalApplied => self.transition(GitEffectPhase::Published),
            GitEffectPhase::Published => Ok(()),
        }
    }

    /// Publish the terminal journal in `completed` and remove its active copy.
    ///
    /// The completed copy is made durable first. A crash between the two writes
    /// can therefore leave both identical copies; [`GitEffectStore::prepare`] and
    /// [`GitEffectStore::resume`] collapse that overlap safely under the worktree lock.
    pub(crate) fn finish(self) -> RailResult<CompletedGitEffect> {
        let expected_terminal = if self.journal.publication.is_some() {
            GitEffectPhase::Published
        } else {
            GitEffectPhase::LocalApplied
        };
        if self.journal.phase != expected_terminal {
            return Err(RailError::message(format!(
                "prepared Git effect '{}' is not in its terminal phase",
                self.journal.effect_id
            )));
        }
        let active = self.store.read_active(&self.journal.effect_id)?.ok_or_else(|| {
            RailError::message(format!(
                "active prepared Git effect '{}' disappeared while locked",
                self.journal.effect_id
            ))
        })?;
        if active != self.journal {
            return Err(RailError::message(format!(
                "active prepared Git effect '{}' changed while locked",
                self.journal.effect_id
            )));
        }
        if let Some(completed) = self.store.read_completed(&self.journal.effect_id)? {
            if completed != self.journal {
                return Err(RailError::message(format!(
                    "completed prepared Git effect '{}' disagrees with its active state",
                    self.journal.effect_id
                )));
            }
        } else {
            self.store
                .write_new_journal(&self.store.completed, &self.journal.effect_id, &self.journal)?;
        }
        let completed = self.store.read_completed(&self.journal.effect_id)?.ok_or_else(|| {
            RailError::message(format!(
                "completed prepared Git effect '{}' was not durable",
                self.journal.effect_id
            ))
        })?;
        if completed != self.journal {
            return Err(RailError::message(format!(
                "completed prepared Git effect '{}' changed during publication",
                self.journal.effect_id
            )));
        }
        self.store.remove_active(&self.journal.effect_id, &self.journal)?;
        Ok(CompletedGitEffect {
            store: self.store,
            journal: completed,
        })
    }

    fn transition(&mut self, phase: GitEffectPhase) -> RailResult<()> {
        let current = self.store.read_active(&self.journal.effect_id)?.ok_or_else(|| {
            RailError::message(format!(
                "active prepared Git effect '{}' disappeared while locked",
                self.journal.effect_id
            ))
        })?;
        if current != self.journal {
            return Err(RailError::message(format!(
                "active prepared Git effect '{}' changed while locked",
                self.journal.effect_id
            )));
        }
        let mut next = self.journal.clone();
        next.phase = phase;
        next.validate()?;
        self.store
            .write_journal_atomic(&self.store.active, &next.effect_id, &next)?;
        let durable = self.store.read_active(&next.effect_id)?.ok_or_else(|| {
            RailError::message(format!(
                "active prepared Git effect '{}' transition was not durable",
                next.effect_id
            ))
        })?;
        if durable != next {
            return Err(RailError::message(format!(
                "active prepared Git effect '{}' changed during transition",
                next.effect_id
            )));
        }
        self.journal = durable;
        Ok(())
    }
}

/// One immutable terminal effect journal.
#[derive(Debug, Clone)]
pub(crate) struct CompletedGitEffect {
    store: GitEffectStore,
    journal: GitEffectJournal,
}

impl CompletedGitEffect {
    /// Return the terminal journal.
    pub(crate) fn journal(&self) -> &GitEffectJournal {
        &self.journal
    }

    /// Remove the terminal recovery copy after its immutable audit receipt is durable.
    ///
    /// Callers must not acknowledge completion until a separate durable receipt
    /// binds this exact effect and payload digest. Completed journals are recovery
    /// state, not an unbounded audit log.
    pub(crate) fn acknowledge(self) -> RailResult<()> {
        let _lock = self.store.lock_worktree(&self.journal.repository.worktree_identity)?;
        // The completed journal requires its bundle, but an orphaned bundle is
        // independently bounded and reaped by startup reconciliation. Remove
        // authority first so interruption can never leave an unsatisfiable
        // journal that points at an already-deleted bundle.
        self.store
            .remove_completed_if_present(&self.journal.effect_id, &self.journal)?;
        if let Some(digest) = self.journal.object_bundle_digest.as_deref() {
            self.store
                .remove_object_bundle_if_present(&self.journal.effect_id, digest)?;
        }
        Ok(())
    }
}

/// One private, store-owned temporary object bundle.
#[derive(Debug)]
pub(crate) struct GitObjectBundleTemp {
    store: GitEffectStore,
    name: OsString,
    path: PathBuf,
    file: Option<File>,
}

/// One exact, no-follow opened object bundle retained after durable publication.
#[derive(Debug)]
pub(crate) struct PreparedGitObjectBundle {
    path: PathBuf,
    file: File,
}

impl PreparedGitObjectBundle {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn into_file(self) -> File {
        self.file
    }
}

impl GitObjectBundleTemp {
    /// Return the exact opened file used by deterministic Git pack preparation.
    pub(crate) fn file_mut(&mut self) -> RailResult<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| RailError::message("prepared Git object bundle file is unavailable"))
    }

    /// Publish this exact pack under the deterministic effect identity.
    pub(crate) fn persist(mut self, effect_id: &str, expected_digest: &str) -> RailResult<PreparedGitObjectBundle> {
        validate_effect_id(effect_id)?;
        validate_sha256("object bundle digest", expected_digest)?;
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| RailError::message("prepared Git object bundle file is unavailable"))?;
        file.flush()?;
        file.sync_all()?;
        let length = file.metadata()?.len();
        if length == 0 || length > MAX_OBJECT_BUNDLE_BYTES {
            return Err(RailError::message(format!(
                "prepared Git object bundle must contain 1..={MAX_OBJECT_BUNDLE_BYTES} bytes"
            )));
        }
        if !private_file_matches_directory_entry(file, &self.store.objects, &self.name, length)? {
            return Err(RailError::message(
                "prepared Git object bundle changed before durable publication",
            ));
        }
        let actual_digest = digest_opened_file(file)?;
        if actual_digest != expected_digest {
            return Err(RailError::message(format!(
                "prepared Git object bundle digest changed: expected {expected_digest}, found {actual_digest}"
            )));
        }
        let destination_name = OsString::from(format!("{effect_id}.pack"));
        let destination = self.store.objects.path.join(&destination_name);
        let publish = rename_entry_noclobber(
            &self.store.objects,
            &self.name,
            &destination_name,
            &self.path,
            &destination,
        );
        let retained_matches = if let Some(file) = self.file.as_ref() {
            private_file_matches_directory_entry(file, &self.store.objects, &destination_name, length)?
        } else {
            false
        };
        drop(self.file.take());
        if let Err(error) = publish {
            if retained_matches && let Err(sync_error) = sync_retained_directory(&self.store.objects) {
                if self.store.object_bundle_matches(effect_id, expected_digest, length)? {
                    return self
                        .store
                        .open_object_bundle(effect_id, expected_digest)?
                        .ok_or_else(|| RailError::message("prepared Git object bundle disappeared after publication"));
                }
                return Err(sync_error);
            }
            if self.store.object_bundle_matches(effect_id, expected_digest, length)? {
                return self
                    .store
                    .open_object_bundle(effect_id, expected_digest)?
                    .ok_or_else(|| RailError::message("prepared Git object bundle disappeared after publication"));
            }
            return Err(error);
        }
        if !retained_matches {
            return Err(RailError::message(
                "prepared Git object bundle destination changed during publication",
            ));
        }
        if let Err(error) = sync_retained_directory(&self.store.objects) {
            if self.store.object_bundle_matches(effect_id, expected_digest, length)? {
                return self
                    .store
                    .open_object_bundle(effect_id, expected_digest)?
                    .ok_or_else(|| RailError::message("prepared Git object bundle disappeared after publication"));
            }
            return Err(error);
        }
        if !self.store.object_bundle_matches(effect_id, expected_digest, length)? {
            return Err(RailError::message(format!(
                "prepared Git object bundle '{}' was not durably published",
                destination.display()
            )));
        }
        self.store
            .open_object_bundle(effect_id, expected_digest)?
            .ok_or_else(|| RailError::message("prepared Git object bundle disappeared after publication"))
    }
}

impl Drop for GitObjectBundleTemp {
    fn drop(&mut self) {
        drop(remove_directory_entry(&self.store.objects, &self.name, &self.path));
    }
}

#[derive(Debug, Clone)]
struct RetainedDirectory {
    path: PathBuf,
    handle: Arc<File>,
}

/// Repository-owned store for active and completed prepared Git effects.
#[derive(Debug, Clone)]
pub(crate) struct GitEffectStore {
    common_dir_identity: String,
    root: RetainedDirectory,
    active: RetainedDirectory,
    objects: RetainedDirectory,
    completed: RetainedDirectory,
    locks: RetainedDirectory,
    _namespace_guards: Arc<Vec<File>>,
}

impl GitEffectStore {
    /// Open the journal store in the exact repository's canonical Git common directory.
    pub(crate) fn open(git: &SystemGit) -> RailResult<Self> {
        let store = Self::open_common_dir(&git.common_dir()?)?;
        store.reconcile_startup_state(git)?;
        Ok(store)
    }

    /// Observe an existing journal store without creating files, directories, or locks.
    ///
    /// Absence is a valid empty state. A present but malformed, incomplete, or
    /// unowned namespace fails closed instead of being adopted.
    pub(crate) fn observe(git: &SystemGit) -> RailResult<Option<Self>> {
        Self::observe_common_dir(&git.common_dir()?)
    }

    /// Discover active recovery authority without creating a store when none exists.
    pub(crate) fn discover_active_read_only(git: &SystemGit) -> RailResult<Vec<GitEffectJournal>> {
        Self::observe(git)?.map_or_else(|| Ok(Vec::new()), |store| store.discover_active())
    }

    /// Discover active and completed-but-unacknowledged effects without
    /// creating store state. Duplicate crash-overlap copies must agree.
    pub(crate) fn discover_unacknowledged_read_only(git: &SystemGit) -> RailResult<Vec<GitEffectJournal>> {
        let Some(store) = Self::observe(git)? else {
            return Ok(Vec::new());
        };
        store.discover_unacknowledged()
    }

    /// Return terminal audit bindings for a command-owned operation family.
    pub(crate) fn completed_audits_read_only(
        git: &SystemGit,
        owner: &str,
        operation_prefixes: &[&str],
    ) -> RailResult<Vec<GitEffectAudit>> {
        let Some(store) = Self::observe(git)? else {
            return Ok(Vec::new());
        };
        let mut audits = store
            .read_journal_directory(&store.completed)?
            .into_iter()
            .filter(|journal| {
                operation_prefixes
                    .iter()
                    .any(|prefix| journal.operation_id.starts_with(prefix))
                    && journal.mapping.as_ref().is_none_or(|mapping| mapping.owner == owner)
            })
            .map(|journal| journal.audit())
            .collect::<Vec<_>>();
        audits.sort_by(|left, right| left.effect_id.cmp(&right.effect_id));
        Ok(audits)
    }

    /// Remove one terminal journal and bundle only after the caller has made
    /// the returned audit binding durable in an ordinary receipt.
    pub(crate) fn acknowledge_completed(git: &SystemGit, effect_id: &str, payload_digest: &str) -> RailResult<()> {
        let store = Self::observe(git)?.ok_or_else(|| {
            RailError::message(format!(
                "prepared Git effect store disappeared before acknowledging '{effect_id}'"
            ))
        })?;
        match store.resume(effect_id)? {
            GitEffectRecord::Completed(completed) => {
                if completed.journal.payload_digest != payload_digest {
                    return Err(RailError::message(format!(
                        "completed prepared Git effect '{effect_id}' payload changed before acknowledgement"
                    )));
                }
                git.remove_owned_pack_keeps(effect_id)?;
                completed.acknowledge()
            }
            GitEffectRecord::Active(_) => Err(RailError::message(format!(
                "prepared Git effect '{effect_id}' is not terminal and cannot be acknowledged"
            ))),
        }
    }

    /// Capture exact local repository authority suitable for a new intent.
    pub(crate) fn capture_repository_authority(
        &self,
        git: &SystemGit,
        logical_repository: String,
        ref_name: String,
        expected_oid: Option<String>,
        result_oid: String,
    ) -> RailResult<GitEffectRepositoryAuthority> {
        validate_sha256("logical repository identity", &logical_repository)?;
        validate_branch_ref(&ref_name)?;
        let actual_store = Self::open(git)?;
        if actual_store.common_dir_identity != self.common_dir_identity {
            return Err(RailError::message(
                "prepared Git effect store does not match the supplied repository common directory",
            ));
        }
        let symbolic_head = git.run_git_stdout(&["symbolic-ref", "-q", "HEAD"])?;
        if symbolic_head != ref_name {
            return Err(RailError::message(format!(
                "prepared Git effect requires symbolic HEAD '{}', found '{}'",
                ref_name, symbolic_head
            )));
        }
        let object_format = git.object_format()?;
        let actual_oid = git.exact_branch_ref_oid_with_format(&ref_name, &object_format)?;
        let expected_oid = expected_oid.map(|oid| oid.to_ascii_lowercase());
        if actual_oid != expected_oid {
            return Err(RailError::message(format!(
                "prepared Git effect expected ref '{}' at {}, found {}",
                ref_name,
                expected_oid.as_deref().unwrap_or("absent"),
                actual_oid.as_deref().unwrap_or("absent")
            )));
        }
        let authority = GitEffectRepositoryAuthority {
            logical_repository,
            common_dir_identity: self.common_dir_identity.clone(),
            worktree_identity: physical_directory_identity(&git.worktree_root)?,
            object_format,
            ref_name,
            symbolic_head,
            expected_oid,
            result_oid: result_oid.to_ascii_lowercase(),
        };
        authority.validate()?;
        Ok(authority)
    }

    /// Prepare a new journal or adopt the exact existing active/completed record.
    pub(crate) fn prepare(&self, intent: GitEffectIntent) -> RailResult<GitEffectRecord> {
        intent.validate()?;
        if intent.repository.common_dir_identity != self.common_dir_identity {
            return Err(RailError::message(
                "prepared Git effect intent belongs to another Git common directory",
            ));
        }
        let expected = GitEffectJournal::prepared(intent)?;
        let lock = self.lock_worktree(&expected.repository.worktree_identity)?;
        self.prepare_locked(expected, lock)
    }

    /// Resume an exact active journal, or adopt its completed terminal record.
    pub(crate) fn resume(&self, effect_id: &str) -> RailResult<GitEffectRecord> {
        validate_effect_id(effect_id)?;
        let observed = self
            .read_active(effect_id)?
            .or(self.read_completed(effect_id)?)
            .ok_or_else(|| RailError::message(format!("prepared Git effect '{effect_id}' was not found")))?;
        let lock = self.lock_worktree(&observed.repository.worktree_identity)?;
        if let Some(completed) = self.read_completed(effect_id)? {
            if let Some(active) = self.read_active(effect_id)? {
                self.collapse_completed_overlap(&active, &completed)?;
            }
            return Ok(GitEffectRecord::Completed(CompletedGitEffect {
                store: self.clone(),
                journal: completed,
            }));
        }
        let active = self.read_active(effect_id)?.ok_or_else(|| {
            RailError::message(format!(
                "prepared Git effect '{effect_id}' disappeared while its worktree lock was acquired"
            ))
        })?;
        Ok(GitEffectRecord::Active(ActiveGitEffect {
            store: self.clone(),
            journal: active,
            _lock: lock,
        }))
    }

    /// Discover every strict active journal in deterministic effect-ID order.
    ///
    /// Discovery is observational. A caller must use [`Self::resume`] before
    /// acting because the worktree lock and second read establish mutation authority.
    pub(crate) fn discover_active(&self) -> RailResult<Vec<GitEffectJournal>> {
        self.read_journal_directory(&self.active)
    }

    fn discover_unacknowledged(&self) -> RailResult<Vec<GitEffectJournal>> {
        let mut journals = std::collections::BTreeMap::<String, GitEffectJournal>::new();
        for journal in self
            .read_journal_directory(&self.active)?
            .into_iter()
            .chain(self.read_journal_directory(&self.completed)?)
        {
            if let Some(existing) = journals.get(&journal.effect_id) {
                if existing != &journal {
                    return Err(RailError::message(format!(
                        "active and completed prepared Git effect '{}' disagree",
                        journal.effect_id
                    )));
                }
            } else {
                journals.insert(journal.effect_id.clone(), journal);
            }
        }
        Ok(journals.into_values().collect())
    }

    /// Return the private path reserved for one prepared object bundle.
    pub(crate) fn object_bundle_path(&self, effect_id: &str) -> RailResult<PathBuf> {
        validate_effect_id(effect_id)?;
        Ok(self.objects.path.join(format!("{effect_id}.pack")))
    }

    /// Create one private file for deterministic pack preparation.
    pub(crate) fn create_object_bundle_temp(&self) -> RailResult<GitObjectBundleTemp> {
        let (name, path, file) = create_private_temp_file(&self.objects, "object")?;
        Ok(GitObjectBundleTemp {
            store: self.clone(),
            name,
            path,
            file: Some(file),
        })
    }

    /// Open and verify the exact private pack bound to an effect journal.
    pub(crate) fn open_object_bundle(
        &self,
        effect_id: &str,
        expected_digest: &str,
    ) -> RailResult<Option<PreparedGitObjectBundle>> {
        validate_effect_id(effect_id)?;
        validate_sha256("object bundle digest", expected_digest)?;
        let name = OsString::from(format!("{effect_id}.pack"));
        let path = self.objects.path.join(&name);
        let Some(mut file) = open_existing_entry(&self.objects, &name, &path, false)? else {
            return Ok(None);
        };
        let length = file.metadata()?.len();
        if length == 0
            || length > MAX_OBJECT_BUNDLE_BYTES
            || !private_file_matches_directory_entry(&file, &self.objects, &name, length)?
        {
            return Err(RailError::message(format!(
                "prepared Git object bundle '{}' is not a bounded private regular file",
                path.display()
            )));
        }
        let actual = digest_opened_file(&mut file)?;
        if actual != expected_digest || !private_file_matches_directory_entry(&file, &self.objects, &name, length)? {
            return Err(RailError::message(format!(
                "prepared Git object bundle digest changed: expected {expected_digest}, found {actual}"
            )));
        }
        Ok(Some(PreparedGitObjectBundle { path, file }))
    }

    fn open_common_dir(common_dir: &Path) -> RailResult<Self> {
        let common_dir = utils::canonicalize_existing(common_dir)?;
        validate_real_directory(&common_dir, "Git common directory")?;
        let common_dir_identity = physical_directory_identity(&common_dir)?;
        let common = retain_directory(&common_dir, "Git common directory")?;
        let (cargo_rail, _) = ensure_owned_directory(&common, "cargo-rail")?;
        let (root, root_created) = ensure_owned_directory(&cargo_rail, EFFECT_ROOT)?;
        let marker = root.path.join(OWNER_MARKER);
        if root_created {
            write_new_bytes(&root, OsStr::new(OWNER_MARKER), OWNER_MARKER_BYTES, &marker)?;
        } else {
            validate_owner_marker(&root)?;
        }
        let (active, _) = ensure_owned_directory(&root, "active")?;
        let (objects, _) = ensure_owned_directory(&root, "objects")?;
        let (completed, _) = ensure_owned_directory(&root, "completed")?;
        let (locks, _) = ensure_owned_directory(&root, "locks")?;
        let namespace_guards = Arc::new(vec![
            common.handle.as_ref().try_clone()?,
            cargo_rail.handle.as_ref().try_clone()?,
        ]);
        let store = Self {
            common_dir_identity,
            root,
            active,
            objects,
            completed,
            locks,
            _namespace_guards: namespace_guards,
        };
        store.validate_layout()?;
        Ok(store)
    }

    fn observe_common_dir(common_dir: &Path) -> RailResult<Option<Self>> {
        let common_dir = utils::canonicalize_existing(common_dir)?;
        validate_real_directory(&common_dir, "Git common directory")?;
        let common_dir_identity = physical_directory_identity(&common_dir)?;
        let common = retain_directory(&common_dir, "Git common directory")?;
        let Some(cargo_rail) = observe_owned_directory(&common, "cargo-rail")? else {
            return Ok(None);
        };
        let Some(root) = observe_owned_directory(&cargo_rail, EFFECT_ROOT)? else {
            return Ok(None);
        };
        validate_owner_marker(&root)?;
        let active = require_observed_directory(&root, "active")?;
        let objects = require_observed_directory(&root, "objects")?;
        let completed = require_observed_directory(&root, "completed")?;
        let locks = require_observed_directory(&root, "locks")?;
        let namespace_guards = Arc::new(vec![
            common.handle.as_ref().try_clone()?,
            cargo_rail.handle.as_ref().try_clone()?,
        ]);
        let store = Self {
            common_dir_identity,
            root,
            active,
            objects,
            completed,
            locks,
            _namespace_guards: namespace_guards,
        };
        store.validate_layout()?;
        Ok(Some(store))
    }

    fn validate_layout(&self) -> RailResult<()> {
        validate_owner_marker(&self.root)?;
        let expected = [OWNER_MARKER, "active", "completed", "locks", "objects"];
        let mut actual = directory_entry_names(&self.root)?;
        actual.sort();
        let actual = actual
            .into_iter()
            .map(|entry| {
                entry.into_string().map_err(|entry| {
                    RailError::message(format!(
                        "prepared Git effect root contains a non-UTF-8 entry '{entry:?}'"
                    ))
                })
            })
            .collect::<RailResult<Vec<_>>>()?;
        if actual != expected {
            return Err(RailError::message(format!(
                "prepared Git effect root '{}' contains unexpected or incomplete state",
                self.root.path.display()
            )));
        }
        for (directory, description) in [
            (&self.active, "active effect directory"),
            (&self.objects, "prepared object directory"),
            (&self.completed, "completed effect directory"),
            (&self.locks, "effect lock directory"),
        ] {
            validate_retained_directory(directory, description)?;
        }
        validate_area(&self.active, "active prepared Git effects", StoreAreaKind::Journal)?;
        validate_area(
            &self.completed,
            "completed prepared Git effects",
            StoreAreaKind::Journal,
        )?;
        validate_area(
            &self.objects,
            "prepared Git object bundles",
            StoreAreaKind::ObjectBundle,
        )?;
        validate_area(&self.locks, "prepared Git effect locks", StoreAreaKind::Lock)?;
        Ok(())
    }

    fn reconcile_startup_state(&self, git: &SystemGit) -> RailResult<()> {
        let active = self
            .read_journal_directory(&self.active)?
            .into_iter()
            .map(|journal| (journal.effect_id.clone(), journal))
            .collect::<std::collections::BTreeMap<_, _>>();
        let completed = self
            .read_journal_directory(&self.completed)?
            .into_iter()
            .map(|journal| (journal.effect_id.clone(), journal))
            .collect::<std::collections::BTreeMap<_, _>>();
        for (effect_id, observed_active) in &active {
            let Some(observed_completed) = completed.get(effect_id) else {
                continue;
            };
            if observed_active != observed_completed {
                return Err(RailError::message(format!(
                    "active and completed prepared Git effect '{effect_id}' disagree"
                )));
            }
            let _lock = self.lock_worktree(&observed_active.repository.worktree_identity)?;
            if let (Some(actual_active), Some(actual_completed)) =
                (self.read_active(effect_id)?, self.read_completed(effect_id)?)
            {
                self.collapse_completed_overlap(&actual_active, &actual_completed)?;
            }
        }

        let journals = self.discover_unacknowledged()?;
        let mut referenced_bundles = std::collections::BTreeSet::new();
        for journal in &journals {
            if let Some(digest) = journal.object_bundle_digest() {
                if self.open_object_bundle(journal.effect_id(), digest)?.is_none() {
                    return Err(RailError::message(format!(
                        "prepared Git effect '{}' lost its exact object bundle",
                        journal.effect_id()
                    )));
                }
                referenced_bundles.insert(journal.effect_id().to_string());
            }
            if git.exact_branch_ref_oid(&journal.repository.ref_name)?.as_deref()
                == Some(journal.repository.result_oid.as_str())
            {
                git.remove_owned_pack_keeps(journal.effect_id())?;
            }
        }
        self.remove_stale_temporary_entries(&self.active)?;
        self.remove_stale_temporary_entries(&self.completed)?;
        self.remove_stale_object_entries(&referenced_bundles)?;
        self.validate_layout()
    }

    fn remove_stale_temporary_entries(&self, directory: &RetainedDirectory) -> RailResult<()> {
        let mut removed = false;
        for name in directory_entry_names(directory)? {
            let Some(text) = name.to_str() else {
                continue;
            };
            if text.starts_with(TEMP_PREFIX)
                && text.ends_with(TEMP_SUFFIX)
                && entry_is_older_than_grace(directory, &name)?
            {
                remove_directory_entry(directory, &name, &directory.path.join(&name))?;
                removed = true;
            }
        }
        if removed {
            sync_retained_directory(directory)?;
        }
        Ok(())
    }

    fn remove_stale_object_entries(&self, referenced: &std::collections::BTreeSet<String>) -> RailResult<()> {
        let mut removed = false;
        for name in directory_entry_names(&self.objects)? {
            let Some(text) = name.to_str() else {
                continue;
            };
            let temporary = text.starts_with(TEMP_PREFIX) && text.ends_with(TEMP_SUFFIX);
            let orphan = text
                .strip_suffix(".pack")
                .is_some_and(|effect_id| !referenced.contains(effect_id));
            if (temporary || orphan) && entry_is_older_than_grace(&self.objects, &name)? {
                remove_directory_entry(&self.objects, &name, &self.objects.path.join(&name))?;
                removed = true;
            }
        }
        if removed {
            sync_retained_directory(&self.objects)?;
        }
        Ok(())
    }

    fn prepare_locked(&self, expected: GitEffectJournal, lock: File) -> RailResult<GitEffectRecord> {
        let active_journals = self.discover_active()?;
        if let Some(other) = active_journals.iter().find(|journal| {
            journal.repository.worktree_identity == expected.repository.worktree_identity
                && journal.effect_id != expected.effect_id
        }) {
            return Err(RailError::with_help(
                format!(
                    "Git worktree '{}' already has active prepared effect '{}' on ref '{}'",
                    expected.repository.worktree_identity, other.effect_id, other.repository.ref_name
                ),
                "resume or reconcile that exact effect before preparing another mutation for the physical worktree",
            ));
        }

        let active = active_journals
            .into_iter()
            .find(|journal| journal.effect_id == expected.effect_id);
        if let Some(completed) = self.read_completed(&expected.effect_id)? {
            if !completed.same_immutable_payload(&expected)? {
                return Err(RailError::message(format!(
                    "completed prepared Git effect '{}' disagrees with the requested intent",
                    expected.effect_id
                )));
            }
            if let Some(active) = active {
                self.collapse_completed_overlap(&active, &completed)?;
            }
            return Ok(GitEffectRecord::Completed(CompletedGitEffect {
                store: self.clone(),
                journal: completed,
            }));
        }
        if let Some(active) = active {
            if !active.same_immutable_payload(&expected)? {
                return Err(RailError::message(format!(
                    "active prepared Git effect '{}' disagrees with the requested intent",
                    expected.effect_id
                )));
            }
            return Ok(GitEffectRecord::Active(ActiveGitEffect {
                store: self.clone(),
                journal: active,
                _lock: lock,
            }));
        }

        self.write_new_journal(&self.active, &expected.effect_id, &expected)?;
        let durable = self.read_active(&expected.effect_id)?.ok_or_else(|| {
            RailError::message(format!(
                "prepared Git effect '{}' was not durably published",
                expected.effect_id
            ))
        })?;
        if durable != expected {
            return Err(RailError::message(format!(
                "prepared Git effect '{}' changed during publication",
                expected.effect_id
            )));
        }
        Ok(GitEffectRecord::Active(ActiveGitEffect {
            store: self.clone(),
            journal: durable,
            _lock: lock,
        }))
    }

    fn lock_worktree(&self, worktree_identity: &str) -> RailResult<File> {
        validate_sha256("Git worktree identity", worktree_identity)?;
        let mut identity = CanonicalBytes::new(b"cargo-rail-git-effect-lock-v1");
        identity.field(b"worktree", worktree_identity.as_bytes())?;
        let name = format!("{}.lock", ContentDigest::sha256(identity.as_slice()));
        let path = self.locks.path.join(&name);
        let file = open_or_create_lock(&self.locks, OsStr::new(&name), &path)?;
        if !private_file_matches_directory_entry(&file, &self.locks, OsStr::new(&name), 0)? {
            return Err(RailError::with_help(
                format!(
                    "prepared Git effect lock '{}' is not a private empty file",
                    path.display()
                ),
                "remove the hostile lock path; cargo-rail will not follow or share effect locks",
            ));
        }
        file.lock()?;
        if !private_file_matches_directory_entry(&file, &self.locks, OsStr::new(&name), 0)? {
            return Err(RailError::message(format!(
                "prepared Git effect lock '{}' changed while it was acquired",
                path.display()
            )));
        }
        Ok(file)
    }

    fn read_journal_directory(&self, directory: &RetainedDirectory) -> RailResult<Vec<GitEffectJournal>> {
        let mut entries = directory_entry_names(directory)?;
        if entries.len() > MAX_JOURNALS {
            return Err(RailError::message(format!(
                "prepared Git effect directory '{}' exceeds its {MAX_JOURNALS}-entry bound",
                directory.path.display()
            )));
        }
        entries.sort();
        let mut journals = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = entry.to_str().ok_or_else(|| {
                RailError::message(format!(
                    "prepared Git effect entry '{}' is not valid UTF-8",
                    directory.path.join(&entry).display()
                ))
            })?;
            if name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX) {
                continue;
            }
            let effect_id = name.strip_suffix(".json").ok_or_else(|| {
                RailError::message(format!(
                    "prepared Git effect directory contains unexpected entry '{}'",
                    directory.path.join(&entry).display()
                ))
            })?;
            validate_effect_id(effect_id)?;
            let journal = self
                .read_journal_entry(directory, &entry)?
                .ok_or_else(|| RailError::message("prepared Git effect disappeared during discovery"))?;
            if journal.effect_id != effect_id {
                return Err(RailError::message(format!(
                    "prepared Git effect file '{}' contains identity '{}'",
                    directory.path.join(&entry).display(),
                    journal.effect_id
                )));
            }
            journals.push(journal);
        }
        Ok(journals)
    }

    fn read_active(&self, effect_id: &str) -> RailResult<Option<GitEffectJournal>> {
        validate_effect_id(effect_id)?;
        self.read_journal_entry(&self.active, OsStr::new(&format!("{effect_id}.json")))
    }

    fn read_completed(&self, effect_id: &str) -> RailResult<Option<GitEffectJournal>> {
        validate_effect_id(effect_id)?;
        self.read_journal_entry(&self.completed, OsStr::new(&format!("{effect_id}.json")))
    }

    fn read_journal_entry(&self, directory: &RetainedDirectory, name: &OsStr) -> RailResult<Option<GitEffectJournal>> {
        let path = directory.path.join(name);
        let Some(file) = open_existing_entry(directory, name, &path, false)? else {
            return Ok(None);
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_JOURNAL_BYTES {
            return Err(RailError::message(format!(
                "prepared Git effect journal '{}' is not a bounded private regular file",
                path.display()
            )));
        }
        if !private_file_matches_directory_entry(&file, directory, name, metadata.len())? {
            return Err(RailError::message(format!(
                "prepared Git effect journal '{}' changed before it was read",
                path.display()
            )));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        (&file).take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_JOURNAL_BYTES
            || !private_file_matches_directory_entry(&file, directory, name, metadata.len())?
        {
            return Err(RailError::message(format!(
                "prepared Git effect journal '{}' changed while it was read",
                path.display()
            )));
        }
        let journal: GitEffectJournal = serde_json::from_slice(&bytes).map_err(|error| {
            RailError::message(format!(
                "invalid prepared Git effect journal '{}': {error}",
                path.display()
            ))
        })?;
        journal.validate()?;
        if journal.repository.common_dir_identity != self.common_dir_identity {
            return Err(RailError::message(format!(
                "prepared Git effect journal '{}' belongs to another Git common directory",
                path.display()
            )));
        }
        Ok(Some(journal))
    }

    fn write_new_journal(
        &self,
        directory: &RetainedDirectory,
        effect_id: &str,
        journal: &GitEffectJournal,
    ) -> RailResult<()> {
        validate_effect_id(effect_id)?;
        let bytes = journal_bytes(journal)?;
        let name = OsString::from(format!("{effect_id}.json"));
        let path = directory.path.join(&name);
        let result = write_new_bytes(directory, &name, &bytes, &path);
        if let Err(error) = result {
            if self
                .read_journal_entry(directory, &name)?
                .is_some_and(|durable| durable == *journal)
            {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }

    fn write_journal_atomic(
        &self,
        directory: &RetainedDirectory,
        effect_id: &str,
        journal: &GitEffectJournal,
    ) -> RailResult<()> {
        validate_effect_id(effect_id)?;
        let bytes = journal_bytes(journal)?;
        let name = OsString::from(format!("{effect_id}.json"));
        let path = directory.path.join(&name);
        let result = write_replace_bytes(directory, &name, &bytes, &path);
        if let Err(error) = result {
            if self
                .read_journal_entry(directory, &name)?
                .is_some_and(|durable| durable == *journal)
            {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }

    fn collapse_completed_overlap(&self, active: &GitEffectJournal, completed: &GitEffectJournal) -> RailResult<()> {
        if active != completed {
            return Err(RailError::message(format!(
                "active and completed prepared Git effect '{}' disagree",
                completed.effect_id
            )));
        }
        self.remove_active(&active.effect_id, active)
    }

    fn remove_active(&self, effect_id: &str, expected: &GitEffectJournal) -> RailResult<()> {
        let actual = self
            .read_active(effect_id)?
            .ok_or_else(|| RailError::message(format!("active prepared Git effect '{effect_id}' disappeared")))?;
        if &actual != expected {
            return Err(RailError::message(format!(
                "active prepared Git effect '{effect_id}' changed before completion"
            )));
        }
        let name = OsString::from(format!("{effect_id}.json"));
        remove_directory_entry(&self.active, &name, &self.active.path.join(&name))?;
        sync_retained_directory(&self.active)
    }

    fn remove_completed_if_present(&self, effect_id: &str, expected: &GitEffectJournal) -> RailResult<()> {
        let Some(actual) = self.read_completed(effect_id)? else {
            return Ok(());
        };
        if &actual != expected {
            return Err(RailError::message(format!(
                "completed prepared Git effect '{effect_id}' changed before acknowledgement"
            )));
        }
        let name = OsString::from(format!("{effect_id}.json"));
        remove_directory_entry(&self.completed, &name, &self.completed.path.join(&name))?;
        sync_retained_directory(&self.completed)
    }

    fn remove_object_bundle_if_present(&self, effect_id: &str, expected_digest: &str) -> RailResult<()> {
        let name = OsString::from(format!("{effect_id}.pack"));
        if self.open_object_bundle(effect_id, expected_digest)?.is_none() {
            return Ok(());
        }
        remove_directory_entry(&self.objects, &name, &self.objects.path.join(&name))?;
        sync_retained_directory(&self.objects)
    }

    #[cfg(test)]
    fn active_path(&self, effect_id: &str) -> RailResult<PathBuf> {
        validate_effect_id(effect_id)?;
        Ok(self.active.path.join(format!("{effect_id}.json")))
    }

    #[cfg(test)]
    fn completed_path(&self, effect_id: &str) -> RailResult<PathBuf> {
        validate_effect_id(effect_id)?;
        Ok(self.completed.path.join(format!("{effect_id}.json")))
    }

    fn object_bundle_matches(&self, effect_id: &str, digest: &str, expected_len: u64) -> RailResult<bool> {
        Ok(self.open_object_bundle(effect_id, digest)?.is_some_and(|bundle| {
            bundle
                .file
                .metadata()
                .is_ok_and(|metadata| metadata.len() == expected_len)
        }))
    }
}

impl GitEffectRepositoryAuthority {
    fn validate(&self) -> RailResult<()> {
        validate_sha256("logical repository identity", &self.logical_repository)?;
        validate_sha256("Git common directory identity", &self.common_dir_identity)?;
        validate_sha256("Git worktree identity", &self.worktree_identity)?;
        validate_object_format(&self.object_format)?;
        validate_branch_ref(&self.ref_name)?;
        if self.symbolic_head != self.ref_name {
            return Err(RailError::message(
                "prepared Git effect symbolic HEAD does not match its exact branch ref",
            ));
        }
        if let Some(expected) = &self.expected_oid {
            validate_oid("expected local ref", expected, &self.object_format)?;
        }
        validate_oid("result local ref", &self.result_oid, &self.object_format)?;
        Ok(())
    }

    fn encode(&self, canonical: &mut CanonicalBytes) -> RailResult<()> {
        canonical.field(b"logical-repository", self.logical_repository.as_bytes())?;
        canonical.field(b"common-dir-identity", self.common_dir_identity.as_bytes())?;
        canonical.field(b"worktree-identity", self.worktree_identity.as_bytes())?;
        canonical.field(b"object-format", self.object_format.as_bytes())?;
        canonical.field(b"ref-name", self.ref_name.as_bytes())?;
        canonical.field(b"symbolic-head", self.symbolic_head.as_bytes())?;
        canonical.optional_field(b"expected-oid", self.expected_oid.as_deref())?;
        canonical.field(b"result-oid", self.result_oid.as_bytes())
    }
}

impl GitEffectCommitMetadata {
    fn validate(&self) -> RailResult<()> {
        for (field, value) in [
            ("author name", self.author.as_str()),
            ("author email", self.author_email.as_str()),
            ("committer name", self.committer.as_str()),
            ("committer email", self.committer_email.as_str()),
        ] {
            validate_bounded_text(field, value, MAX_TOKEN_BYTES)?;
            if value.contains(['\0', '\n', '\r']) {
                return Err(RailError::message(format!(
                    "prepared Git effect {field} contains a forbidden control character"
                )));
            }
        }
        validate_timezone("author time zone", &self.author_timezone)?;
        validate_timezone("committer time zone", &self.committer_timezone)
    }

    fn encode(&self, canonical: &mut CanonicalBytes) -> RailResult<()> {
        canonical.field(b"author", self.author.as_bytes())?;
        canonical.field(b"author-email", self.author_email.as_bytes())?;
        canonical.field(b"author-timestamp", &self.author_timestamp.to_be_bytes())?;
        canonical.field(b"author-timezone", self.author_timezone.as_bytes())?;
        canonical.field(b"committer", self.committer.as_bytes())?;
        canonical.field(b"committer-email", self.committer_email.as_bytes())?;
        canonical.field(b"committer-timestamp", &self.committer_timestamp.to_be_bytes())?;
        canonical.field(b"committer-timezone", self.committer_timezone.as_bytes())
    }
}

impl GitCommitEffect {
    fn validate(&self, object_format: &str) -> RailResult<()> {
        validate_oid("prepared commit", &self.oid, object_format)?;
        validate_oid("prepared tree", &self.tree, object_format)?;
        if self.parents.len() > MAX_PARENTS {
            return Err(RailError::message(format!(
                "prepared Git commit exceeds its {MAX_PARENTS}-parent bound"
            )));
        }
        for parent in &self.parents {
            validate_oid("prepared parent", parent, object_format)?;
        }
        validate_bounded_text("commit message", &self.message, MAX_MESSAGE_BYTES)?;
        self.metadata.validate()
    }

    fn encode(&self, canonical: &mut CanonicalBytes) -> RailResult<()> {
        canonical.field(b"oid", self.oid.as_bytes())?;
        canonical.field(b"tree", self.tree.as_bytes())?;
        canonical.sequence_bytes(b"parents", self.parents.iter().map(String::as_bytes))?;
        canonical.field(b"message", self.message.as_bytes())?;
        canonical.nested(b"metadata", |metadata| self.metadata.encode(metadata))
    }
}

impl GitPathImage {
    fn validate(&self, object_format: &str) -> RailResult<()> {
        match self {
            Self::Absent => Ok(()),
            Self::Entry { mode, oid } => {
                if !matches!(mode.as_str(), "100644" | "100755" | "120000") {
                    return Err(RailError::message(format!(
                        "prepared Git path image has unsupported mode '{mode}'"
                    )));
                }
                validate_oid("prepared path image", oid, object_format)
            }
        }
    }

    fn encode(&self, canonical: &mut CanonicalBytes) -> RailResult<()> {
        match self {
            Self::Absent => canonical.field(b"kind", b"absent"),
            Self::Entry { mode, oid } => {
                canonical.field(b"kind", b"entry")?;
                canonical.field(b"mode", mode.as_bytes())?;
                canonical.field(b"oid", oid.as_bytes())
            }
        }
    }
}

impl GitPathTransition {
    fn validate(&self, object_format: &str) -> RailResult<()> {
        let canonical = RepositoryPath::new(Path::new(&self.path))?;
        if canonical.as_str() != self.path {
            return Err(RailError::message(format!(
                "prepared Git path '{}' is not canonical",
                self.path
            )));
        }
        if self.old == self.new {
            return Err(RailError::message(format!(
                "prepared Git path '{}' has no state transition",
                self.path
            )));
        }
        self.old.validate(object_format)?;
        self.new.validate(object_format)
    }

    fn encode(&self, canonical: &mut CanonicalBytes) -> RailResult<()> {
        canonical.field(b"path", self.path.as_bytes())?;
        canonical.nested(b"old", |old| self.old.encode(old))?;
        canonical.nested(b"new", |new| self.new.encode(new))
    }
}

impl GitMappingBinding {
    fn validate(&self) -> RailResult<()> {
        validate_token("mapping owner", &self.owner)?;
        validate_token("ownership snapshot", &self.ownership_snapshot)?;
        validate_sha256("pre-effect mapping authority", &self.pre_authority)?;
        validate_sha256("post-effect mapping authority", &self.post_authority)?;
        if let Some(digest) = &self.migration_digest {
            validate_sha256("mapping migration digest", digest)?;
        }
        if self.migration_count == 0 && self.migration_digest.is_some() {
            return Err(RailError::message(
                "prepared Git effect has a migration digest but no migration candidates",
            ));
        }
        if self.migration_count > 0 && self.migration_digest.is_none() {
            return Err(RailError::message(
                "prepared Git effect has migration candidates but no migration digest",
            ));
        }
        Ok(())
    }

    fn encode(&self, canonical: &mut CanonicalBytes) -> RailResult<()> {
        canonical.field(b"owner", self.owner.as_bytes())?;
        canonical.field(b"ownership-snapshot", self.ownership_snapshot.as_bytes())?;
        canonical.field(b"pre-authority", self.pre_authority.as_bytes())?;
        canonical.field(b"post-authority", self.post_authority.as_bytes())?;
        canonical.optional_field(b"migration-digest", self.migration_digest.as_deref())?;
        canonical.field(
            b"migration-count",
            &u64::try_from(self.migration_count)
                .map_err(|_| RailError::message("mapping migration count exceeds u64"))?
                .to_be_bytes(),
        )
    }
}

impl GitPublicationEffect {
    fn validate(&self, object_format: &str) -> RailResult<()> {
        validate_sha256("logical remote identity", &self.logical_remote)?;
        validate_sha256("remote endpoint digest", &self.exact_endpoint_digest)?;
        validate_branch_ref(&self.ref_name)?;
        if let Some(expected) = &self.expected_oid {
            validate_oid("expected remote ref", expected, object_format)?;
        }
        validate_oid("desired remote ref", &self.desired_oid, object_format)?;
        if self.expected_oid.as_deref() == Some(self.desired_oid.as_str()) {
            return Err(RailError::message(
                "prepared Git publication desired ref is identical to its expected ref",
            ));
        }
        Ok(())
    }

    fn encode(&self, canonical: &mut CanonicalBytes) -> RailResult<()> {
        canonical.field(b"logical-remote", self.logical_remote.as_bytes())?;
        canonical.field(b"endpoint-digest", self.exact_endpoint_digest.as_bytes())?;
        canonical.field(b"ref-name", self.ref_name.as_bytes())?;
        canonical.optional_field(b"expected-oid", self.expected_oid.as_deref())?;
        canonical.field(b"desired-oid", self.desired_oid.as_bytes())
    }
}

struct CanonicalBytes(Vec<u8>);

impl CanonicalBytes {
    fn new(domain: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(domain.len() + 32);
        bytes.extend_from_slice(domain);
        Self(bytes)
    }

    fn field(&mut self, label: &[u8], value: &[u8]) -> RailResult<()> {
        self.0.extend_from_slice(
            &u64::try_from(label.len())
                .map_err(|_| RailError::message("canonical Git effect label exceeds u64"))?
                .to_be_bytes(),
        );
        self.0.extend_from_slice(label);
        self.0.extend_from_slice(
            &u64::try_from(value.len())
                .map_err(|_| RailError::message("canonical Git effect value exceeds u64"))?
                .to_be_bytes(),
        );
        self.0.extend_from_slice(value);
        Ok(())
    }

    fn optional_field(&mut self, label: &[u8], value: Option<&str>) -> RailResult<()> {
        self.field(label, value.unwrap_or_default().as_bytes())?;
        self.0.push(u8::from(value.is_some()));
        Ok(())
    }

    fn nested(&mut self, label: &[u8], encode: impl FnOnce(&mut Self) -> RailResult<()>) -> RailResult<()> {
        let mut nested = Self::new(b"");
        encode(&mut nested)?;
        self.field(label, nested.as_slice())
    }

    fn optional_nested<T>(
        &mut self,
        label: &[u8],
        value: Option<&T>,
        encode: impl Fn(&T, &mut Self) -> RailResult<()>,
    ) -> RailResult<()> {
        let mut nested = Self::new(b"");
        if let Some(value) = value {
            encode(value, &mut nested)?;
        }
        self.field(label, nested.as_slice())?;
        self.0.push(u8::from(value.is_some()));
        Ok(())
    }

    fn sequence<T>(
        &mut self,
        label: &[u8],
        values: &[T],
        encode: impl Fn(&T, &mut Self) -> RailResult<()>,
    ) -> RailResult<()> {
        let mut sequence = Self::new(b"");
        sequence.0.extend_from_slice(
            &u64::try_from(values.len())
                .map_err(|_| RailError::message("canonical Git effect sequence exceeds u64"))?
                .to_be_bytes(),
        );
        for value in values {
            let mut item = Self::new(b"");
            encode(value, &mut item)?;
            sequence.field(b"item", item.as_slice())?;
        }
        self.field(label, sequence.as_slice())
    }

    fn sequence_bytes<'a>(&mut self, label: &[u8], values: impl ExactSizeIterator<Item = &'a [u8]>) -> RailResult<()> {
        let mut sequence = Self::new(b"");
        sequence.0.extend_from_slice(
            &u64::try_from(values.len())
                .map_err(|_| RailError::message("canonical Git effect sequence exceeds u64"))?
                .to_be_bytes(),
        );
        for value in values {
            sequence.field(b"item", value)?;
        }
        self.field(label, sequence.as_slice())
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }

    fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

fn journal_bytes(journal: &GitEffectJournal) -> RailResult<Vec<u8>> {
    journal.validate()?;
    let mut bytes = serde_json::to_vec_pretty(journal)
        .map_err(|error| RailError::message(format!("failed to serialize prepared Git effect: {error}")))?;
    bytes.push(b'\n');
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_JOURNAL_BYTES {
        return Err(RailError::message(format!(
            "prepared Git effect journal exceeds its {MAX_JOURNAL_BYTES}-byte bound"
        )));
    }
    Ok(bytes)
}

fn validate_real_directory(path: &Path, description: &str) -> RailResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "{description} '{}' is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

fn retain_directory(path: &Path, description: &str) -> RailResult<RetainedDirectory> {
    #[cfg(unix)]
    let handle = {
        use rustix::fs::{Mode, OFlags};

        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| RailError::message(format!("failed to retain {description} '{}': {error}", path.display())))?
    };
    #[cfg(windows)]
    let handle = crate::windows_fs::open_for_execution_guard(path)
        .map_err(|error| RailError::message(format!("failed to retain {description} '{}': {error}", path.display())))?;
    #[cfg(not(any(unix, windows)))]
    let handle = File::open(path)?;

    let retained = RetainedDirectory {
        path: path.to_path_buf(),
        handle: Arc::new(handle),
    };
    validate_retained_directory(&retained, description)?;
    Ok(retained)
}

fn validate_retained_directory(directory: &RetainedDirectory, description: &str) -> RailResult<()> {
    let opened = directory.handle.metadata()?;
    if !opened.is_dir() {
        return Err(RailError::message(format!(
            "{description} '{}' is not a retained directory",
            directory.path.display()
        )));
    }
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::MetadataExt as _;

        let named = match rustix::fs::open(
            &directory.path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(named) => File::from(named),
            Err(_) => {
                return Err(RailError::message(format!(
                    "{description} '{}' no longer names its retained directory",
                    directory.path.display()
                )));
            }
        };
        let named = named.metadata()?;
        if opened.dev() != named.dev() || opened.ino() != named.ino() {
            return Err(RailError::message(format!(
                "{description} '{}' changed after it was retained",
                directory.path.display()
            )));
        }
    }
    #[cfg(windows)]
    {
        let named = crate::windows_fs::open_for_observation(&directory.path)?;
        let opened = crate::windows_fs::observe_file(&directory.handle)?;
        let named = crate::windows_fs::observe_file(&named)?;
        if opened.volume_serial_number != named.volume_serial_number || opened.file_id != named.file_id {
            return Err(RailError::message(format!(
                "{description} '{}' changed after it was retained",
                directory.path.display()
            )));
        }
    }
    Ok(())
}

fn retain_child_directory(parent: &RetainedDirectory, name: &str) -> RailResult<RetainedDirectory> {
    let path = parent.path.join(name);
    #[cfg(unix)]
    let handle = {
        use rustix::fs::{Mode, OFlags};

        rustix::fs::openat(
            parent.handle.as_ref(),
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| {
            RailError::message(format!(
                "failed to retain prepared Git effect directory '{}': {error}",
                path.display()
            ))
        })?
    };
    #[cfg(windows)]
    let handle = crate::windows_fs::open_for_execution_guard(&path)?;
    #[cfg(not(any(unix, windows)))]
    let handle = File::open(&path)?;
    let directory = RetainedDirectory {
        path,
        handle: Arc::new(handle),
    };
    validate_retained_directory(&directory, "prepared Git effect directory")?;
    Ok(directory)
}

fn ensure_owned_directory(parent: &RetainedDirectory, name: &str) -> RailResult<(RetainedDirectory, bool)> {
    validate_retained_directory(parent, "prepared Git effect parent")?;
    #[cfg(unix)]
    let created = {
        use rustix::fs::Mode;

        match rustix::fs::mkdirat(parent.handle.as_ref(), name, Mode::from_raw_mode(0o700)) {
            Ok(()) => true,
            Err(error) if error == rustix::io::Errno::EXIST => false,
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    };
    #[cfg(not(unix))]
    let created = match fs::create_dir(parent.path.join(name)) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    let directory = retain_child_directory(parent, name)?;
    #[cfg(unix)]
    if created {
        use rustix::fs::Mode;

        rustix::fs::fchmod(directory.handle.as_ref(), Mode::from_raw_mode(0o700)).map_err(std::io::Error::from)?;
    }
    if created {
        sync_retained_directory(parent)?;
    }
    Ok((directory, created))
}

fn observe_owned_directory(parent: &RetainedDirectory, name: &str) -> RailResult<Option<RetainedDirectory>> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};

        match rustix::fs::openat(
            parent.handle.as_ref(),
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(handle) => {
                let directory = RetainedDirectory {
                    path: parent.path.join(name),
                    handle: Arc::new(File::from(handle)),
                };
                validate_retained_directory(&directory, "prepared Git effect directory")?;
                Ok(Some(directory))
            }
            Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
            Err(error) => Err(std::io::Error::from(error).into()),
        }
    }
    #[cfg(not(unix))]
    {
        let path = parent.path.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => retain_child_directory(parent, name).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

fn require_observed_directory(parent: &RetainedDirectory, name: &str) -> RailResult<RetainedDirectory> {
    observe_owned_directory(parent, name)?.ok_or_else(|| {
        RailError::message(format!(
            "prepared Git effect store '{}' is missing required directory '{name}'",
            parent.path.display()
        ))
    })
}

fn validate_owner_marker(root: &RetainedDirectory) -> RailResult<()> {
    let name = OsStr::new(OWNER_MARKER);
    let path = root.path.join(name);
    let Some(mut file) = open_existing_entry(root, name, &path, false)? else {
        return Err(RailError::message(format!(
            "prepared Git effect store '{}' has no ownership marker",
            root.path.display()
        )));
    };
    if !private_file_matches_directory_entry(&file, root, name, OWNER_MARKER_BYTES.len() as u64)? {
        return Err(RailError::message(format!(
            "prepared Git effect ownership marker '{}' is not private and exact",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes != OWNER_MARKER_BYTES
        || !private_file_matches_directory_entry(&file, root, name, OWNER_MARKER_BYTES.len() as u64)?
    {
        return Err(RailError::message(format!(
            "prepared Git effect ownership marker '{}' is invalid",
            path.display()
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum StoreAreaKind {
    Journal,
    ObjectBundle,
    Lock,
}

fn validate_area(directory: &RetainedDirectory, description: &str, kind: StoreAreaKind) -> RailResult<()> {
    let entries = directory_entry_names(directory)?;
    if entries.len() > MAX_JOURNALS {
        return Err(RailError::message(format!(
            "{description} directory '{}' exceeds its {MAX_JOURNALS}-entry bound",
            directory.path.display()
        )));
    }
    for entry in entries {
        let Some(name) = entry.to_str() else {
            return Err(RailError::message(format!(
                "{description} directory '{}' contains a non-UTF-8 entry",
                directory.path.display()
            )));
        };
        let temporary = name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX);
        let valid_name = temporary
            || match kind {
                StoreAreaKind::Journal => name
                    .strip_suffix(".json")
                    .is_some_and(|effect_id| validate_effect_id(effect_id).is_ok()),
                StoreAreaKind::ObjectBundle => name
                    .strip_suffix(".pack")
                    .is_some_and(|effect_id| validate_effect_id(effect_id).is_ok()),
                StoreAreaKind::Lock => name.strip_suffix(".lock").is_some_and(|digest| {
                    digest.len() == 64
                        && digest
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                }),
            };
        if !valid_name {
            return Err(RailError::message(format!(
                "{description} directory contains unexpected entry '{}'",
                directory.path.join(&entry).display()
            )));
        }
        let Some(length) = private_entry_length(directory, &entry)? else {
            return Err(RailError::message(format!(
                "{description} entry '{}' is not a private regular file",
                directory.path.join(&entry).display()
            )));
        };
        let valid_length = match kind {
            StoreAreaKind::Journal => length <= MAX_JOURNAL_BYTES,
            StoreAreaKind::ObjectBundle => length <= MAX_OBJECT_BUNDLE_BYTES && (temporary || length > 0),
            StoreAreaKind::Lock => length == 0,
        };
        if !valid_length {
            return Err(RailError::message(format!(
                "{description} entry '{}' has an invalid size",
                directory.path.join(&entry).display()
            )));
        }
    }
    Ok(())
}

fn private_entry_length(directory: &RetainedDirectory, name: &OsStr) -> RailResult<Option<u64>> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::MetadataExt as _;

        let file = match rustix::fs::openat(
            directory.handle.as_ref(),
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => File::from(file),
            Err(_) => return Ok(None),
        };
        let metadata = file.metadata()?;
        Ok((metadata.is_file() && metadata.nlink() == 1 && metadata.mode() & 0o077 == 0).then_some(metadata.len()))
    }
    #[cfg(windows)]
    {
        let file = match crate::windows_fs::open_for_observation(&directory.path.join(name)) {
            Ok(file) => file,
            Err(_) => return Ok(None),
        };
        let metadata = file.metadata()?;
        let observation = crate::windows_fs::observe_file(&file)?;
        Ok((metadata.is_file() && observation.number_of_links == 1).then_some(observation.size))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = fs::symlink_metadata(directory.path.join(name))?;
        Ok((metadata.is_file() && !utils::is_symlink_or_reparse(&metadata)).then_some(metadata.len()))
    }
}

fn entry_is_older_than_grace(directory: &RetainedDirectory, name: &OsStr) -> RailResult<bool> {
    let path = directory.path.join(name);
    let Some(file) = open_existing_entry(directory, name, &path, false)? else {
        return Ok(false);
    };
    let modified = file.metadata()?.modified()?;
    Ok(std::time::SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age.as_secs() >= ORPHAN_GRACE_SECONDS))
}

#[cfg(unix)]
fn directory_entry_names(directory: &RetainedDirectory) -> RailResult<Vec<OsString>> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut entries = Vec::new();
    for entry in rustix::fs::Dir::read_from(directory.handle.as_ref()).map_err(std::io::Error::from)? {
        let entry = entry.map_err(std::io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if bytes != b"." && bytes != b".." {
            entries.push(OsStr::from_bytes(bytes).to_os_string());
        }
    }
    Ok(entries)
}

#[cfg(not(unix))]
fn directory_entry_names(directory: &RetainedDirectory) -> RailResult<Vec<OsString>> {
    validate_retained_directory(directory, "prepared Git effect directory")?;
    let entries = fs::read_dir(&directory.path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    validate_retained_directory(directory, "prepared Git effect directory")?;
    Ok(entries)
}

fn sync_retained_directory(directory: &RetainedDirectory) -> RailResult<()> {
    #[cfg(unix)]
    directory.handle.sync_all()?;
    #[cfg(not(unix))]
    validate_retained_directory(directory, "prepared Git effect directory")?;
    Ok(())
}

fn random_temp_name(kind: &str) -> RailResult<OsString> {
    let mut entropy = [0_u8; 16];
    getrandom::fill(&mut entropy).map_err(|error| {
        RailError::message(format!(
            "failed to generate prepared Git {kind} temporary name: {error}"
        ))
    })?;
    let nonce = lower_hex(&entropy);
    Ok(OsString::from(format!("{TEMP_PREFIX}{kind}-{nonce}{TEMP_SUFFIX}")))
}

fn create_private_temp_file(directory: &RetainedDirectory, kind: &str) -> RailResult<(OsString, PathBuf, File)> {
    for _ in 0..64 {
        let name = random_temp_name(kind)?;
        let path = directory.path.join(&name);
        match create_new_entry(directory, &name, &path) {
            Ok(file) => return Ok((name, path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(RailError::message(format!(
        "failed to allocate a private prepared Git {kind} temporary after 64 attempts"
    )))
}

#[cfg(unix)]
fn create_new_entry(directory: &RetainedDirectory, name: &OsStr, _path: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let file = rustix::fs::openat(
        directory.handle.as_ref(),
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map(File::from)
    .map_err(std::io::Error::from)?;
    rustix::fs::fchmod(&file, Mode::from_raw_mode(0o600)).map_err(std::io::Error::from)?;
    Ok(file)
}

#[cfg(windows)]
fn create_new_entry(_directory: &RetainedDirectory, _name: &OsStr, path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options.open(path)
}

#[cfg(not(any(unix, windows)))]
fn create_new_entry(_directory: &RetainedDirectory, _name: &OsStr, path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(true).create_new(true).open(path)
}

fn open_existing_entry(
    _directory: &RetainedDirectory,
    _name: &OsStr,
    path: &Path,
    writable: bool,
) -> RailResult<Option<File>> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        let _ = path;

        let access = if writable { OFlags::RDWR } else { OFlags::RDONLY };
        match rustix::fs::openat(
            _directory.handle.as_ref(),
            _name,
            access | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => Ok(Some(File::from(file))),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
            Err(error) => Err(std::io::Error::from(error).into()),
        }
    }
    #[cfg(windows)]
    {
        let result = if writable {
            let mut options = OpenOptions::new();
            use std::os::windows::fs::OpenOptionsExt as _;
            const FILE_SHARE_READ: u32 = 0x0000_0001;
            const FILE_SHARE_WRITE: u32 = 0x0000_0002;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options
                .read(true)
                .write(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
            options.open(path)
        } else {
            crate::windows_fs::open_for_stable_byte_observation(path)
        };
        match result {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        match options.open(path) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

fn open_or_create_lock(directory: &RetainedDirectory, name: &OsStr, path: &Path) -> RailResult<File> {
    match create_new_entry(directory, name, path) {
        Ok(file) => {
            file.sync_all()?;
            sync_retained_directory(directory)?;
            Ok(file)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open_existing_entry(directory, name, path, true)?
                .ok_or_else(|| RailError::message("prepared Git effect lock disappeared while it was opened"))
        }
        Err(error) => Err(error.into()),
    }
}

fn private_file_matches_directory_entry(
    opened: &File,
    directory: &RetainedDirectory,
    name: &OsStr,
    expected_len: u64,
) -> RailResult<bool> {
    let opened_metadata = opened.metadata()?;
    if !opened_metadata.is_file() || opened_metadata.len() != expected_len {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::MetadataExt as _;

        let named = match rustix::fs::openat(
            directory.handle.as_ref(),
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(named) => File::from(named),
            Err(_) => return Ok(false),
        };
        let named_metadata = named.metadata()?;
        Ok(named_metadata.is_file()
            && opened_metadata.dev() == named_metadata.dev()
            && opened_metadata.ino() == named_metadata.ino()
            && opened_metadata.nlink() == 1
            && named_metadata.nlink() == 1
            && opened_metadata.mode() & 0o077 == 0
            && named_metadata.mode() & 0o077 == 0
            && named_metadata.len() == expected_len)
    }
    #[cfg(windows)]
    {
        let path = directory.path.join(name);
        let named = match crate::windows_fs::open_for_observation(&path) {
            Ok(named) => named,
            Err(_) => return Ok(false),
        };
        let opened = crate::windows_fs::observe_file(opened)?;
        let named = crate::windows_fs::observe_file(&named)?;
        Ok(opened.volume_serial_number == named.volume_serial_number
            && opened.file_id == named.file_id
            && opened.number_of_links == 1
            && named.number_of_links == 1
            && opened.size == expected_len
            && named.size == expected_len)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let path = directory.path.join(name);
        let named = fs::symlink_metadata(path)?;
        Ok(named.is_file() && !utils::is_symlink_or_reparse(&named) && named.len() == expected_len)
    }
}

fn write_new_bytes(directory: &RetainedDirectory, destination: &OsStr, bytes: &[u8], path: &Path) -> RailResult<()> {
    let (temporary_name, temporary_path, mut temporary) = create_private_temp_file(directory, "journal")?;
    let result = (|| {
        temporary.write_all(bytes)?;
        temporary.flush()?;
        temporary.sync_all()?;
        if !private_file_matches_directory_entry(
            &temporary,
            directory,
            &temporary_name,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        )? {
            return Err(RailError::message(
                "prepared Git journal temporary changed before publication",
            ));
        }
        rename_entry_noclobber(directory, &temporary_name, destination, &temporary_path, path)?;
        if !private_file_matches_directory_entry(
            &temporary,
            directory,
            destination,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        )? {
            return Err(RailError::message(
                "prepared Git journal destination changed during publication",
            ));
        }
        sync_retained_directory(directory)
    })();
    if result.is_err() {
        drop(remove_directory_entry(directory, &temporary_name, &temporary_path));
    }
    result
}

fn write_replace_bytes(
    directory: &RetainedDirectory,
    destination: &OsStr,
    bytes: &[u8],
    path: &Path,
) -> RailResult<()> {
    let (temporary_name, temporary_path, mut temporary) = create_private_temp_file(directory, "journal")?;
    let result = (|| {
        temporary.write_all(bytes)?;
        temporary.flush()?;
        temporary.sync_all()?;
        if !private_file_matches_directory_entry(
            &temporary,
            directory,
            &temporary_name,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        )? {
            return Err(RailError::message(
                "prepared Git journal temporary changed before replacement",
            ));
        }
        rename_entry_replace(directory, &temporary_name, destination, &temporary_path, path)?;
        if !private_file_matches_directory_entry(
            &temporary,
            directory,
            destination,
            u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        )? {
            return Err(RailError::message(
                "prepared Git journal destination changed during replacement",
            ));
        }
        sync_retained_directory(directory)
    })();
    if result.is_err() {
        drop(remove_directory_entry(directory, &temporary_name, &temporary_path));
    }
    result
}

#[cfg(unix)]
fn rename_entry_noclobber(
    directory: &RetainedDirectory,
    from: &OsStr,
    to: &OsStr,
    _from_path: &Path,
    _to_path: &Path,
) -> RailResult<()> {
    rustix::fs::renameat_with(
        directory.handle.as_ref(),
        from,
        directory.handle.as_ref(),
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(windows)]
fn rename_entry_noclobber(
    _directory: &RetainedDirectory,
    _from: &OsStr,
    _to: &OsStr,
    from_path: &Path,
    to_path: &Path,
) -> RailResult<()> {
    crate::windows_fs::rename_write_through(from_path, to_path, false)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn rename_entry_noclobber(
    _directory: &RetainedDirectory,
    _from: &OsStr,
    _to: &OsStr,
    from_path: &Path,
    to_path: &Path,
) -> RailResult<()> {
    if to_path.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "destination exists").into());
    }
    fs::rename(from_path, to_path)?;
    Ok(())
}

#[cfg(unix)]
fn rename_entry_replace(
    directory: &RetainedDirectory,
    from: &OsStr,
    to: &OsStr,
    _from_path: &Path,
    _to_path: &Path,
) -> RailResult<()> {
    rustix::fs::renameat(directory.handle.as_ref(), from, directory.handle.as_ref(), to)
        .map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(windows)]
fn rename_entry_replace(
    _directory: &RetainedDirectory,
    _from: &OsStr,
    _to: &OsStr,
    from_path: &Path,
    to_path: &Path,
) -> RailResult<()> {
    crate::windows_fs::rename_write_through(from_path, to_path, true)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn rename_entry_replace(
    _directory: &RetainedDirectory,
    _from: &OsStr,
    _to: &OsStr,
    from_path: &Path,
    to_path: &Path,
) -> RailResult<()> {
    fs::rename(from_path, to_path)?;
    Ok(())
}

#[cfg(unix)]
fn remove_directory_entry(directory: &RetainedDirectory, name: &OsStr, _path: &Path) -> RailResult<()> {
    match rustix::fs::unlinkat(directory.handle.as_ref(), name, rustix::fs::AtFlags::empty()) {
        Ok(()) => Ok(()),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

#[cfg(windows)]
fn remove_directory_entry(_directory: &RetainedDirectory, _name: &OsStr, path: &Path) -> RailResult<()> {
    match crate::windows_fs::open_for_stable_byte_observation_and_delete(path) {
        Ok(file) => crate::windows_fs::delete_file_by_handle(file).map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(any(unix, windows)))]
fn remove_directory_entry(_directory: &RetainedDirectory, _name: &OsStr, path: &Path) -> RailResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn digest_opened_file(file: &mut File) -> RailResult<String> {
    file.rewind()?;
    let length = file.metadata()?.len();
    if length == 0 || length > MAX_OBJECT_BUNDLE_BYTES {
        return Err(RailError::message("prepared Git object bundle has an invalid size"));
    }
    let mut digest = Sha256::new();
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(RailError::message(
                "prepared Git object bundle was truncated while hashing",
            ));
        }
        digest.update(&buffer[..read]);
        remaining -= read as u64;
    }
    file.rewind()?;
    let digest = digest.finalize();
    let hex = lower_hex(&digest);
    Ok(format!("sha256-{hex}"))
}

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn physical_directory_identity(path: &Path) -> RailResult<String> {
    let canonical = utils::canonicalize_existing(path)?;
    validate_real_directory(&canonical, "prepared Git effect authority directory")?;
    let mut identity = CanonicalBytes::new(b"cargo-rail-physical-directory-v1");
    identity.field(b"path", canonical.as_os_str().as_encoded_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = fs::symlink_metadata(&canonical)?;
        identity.field(b"device", &metadata.dev().to_be_bytes())?;
        identity.field(b"inode", &metadata.ino().to_be_bytes())?;
    }
    #[cfg(windows)]
    {
        let directory = crate::windows_fs::open_for_observation(&canonical)?;
        let observation = crate::windows_fs::observe_file(&directory)?;
        identity.field(b"volume", &observation.volume_serial_number.to_be_bytes())?;
        identity.field(b"file-id", &observation.file_id.to_be_bytes())?;
        identity.field(b"created", &observation.creation_time.to_be_bytes())?;
    }
    Ok(format!("sha256-{}", ContentDigest::sha256(identity.as_slice())))
}

fn validate_token(field: &str, value: &str) -> RailResult<()> {
    validate_bounded_text(field, value, MAX_TOKEN_BYTES)?;
    if value.chars().any(char::is_whitespace) || value.contains(['\0', '/', '\\']) {
        return Err(RailError::message(format!(
            "prepared Git effect {field} must be one path-safe token"
        )));
    }
    Ok(())
}

fn validate_bounded_text(field: &str, value: &str, max: usize) -> RailResult<()> {
    if value.is_empty() || value.len() > max || value.contains('\0') {
        return Err(RailError::message(format!(
            "prepared Git effect {field} must contain 1..={max} non-NUL bytes"
        )));
    }
    Ok(())
}

fn validate_sha256(field: &str, value: &str) -> RailResult<()> {
    let Some(hex) = value.strip_prefix("sha256-") else {
        return Err(RailError::message(format!(
            "prepared Git effect {field} must use sha256"
        )));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(RailError::message(format!(
            "prepared Git effect {field} has an invalid SHA-256 digest"
        )));
    }
    Ok(())
}

fn validate_effect_id(value: &str) -> RailResult<()> {
    let Some(hex) = value.strip_prefix("git-effect-v1-sha256-") else {
        return Err(RailError::message("invalid prepared Git effect identity"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(RailError::message("invalid prepared Git effect identity"));
    }
    Ok(())
}

fn validate_object_format(value: &str) -> RailResult<()> {
    if matches!(value, "sha1" | "sha256") {
        Ok(())
    } else {
        Err(RailError::message(format!(
            "prepared Git effect has unsupported object format '{value}'"
        )))
    }
}

fn validate_oid(field: &str, value: &str, object_format: &str) -> RailResult<()> {
    let length = match object_format {
        "sha1" => 40,
        "sha256" => 64,
        other => {
            return Err(RailError::message(format!(
                "prepared Git effect has unsupported object format '{other}'"
            )));
        }
    };
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || value.bytes().all(|byte| byte == b'0')
    {
        return Err(RailError::message(format!(
            "prepared Git effect {field} has an invalid {object_format} object ID"
        )));
    }
    Ok(())
}

fn validate_branch_ref(value: &str) -> RailResult<()> {
    let Some(branch) = value.strip_prefix("refs/heads/") else {
        return Err(RailError::message(
            "prepared Git effect ref must be an exact refs/heads/* branch",
        ));
    };
    let invalid = branch.is_empty()
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("@{")
        || branch.contains("//")
        || branch
            .bytes()
            .any(|byte| byte <= b' ' || byte == 0x7f || b"~^:?*[\\".contains(&byte));
    if invalid {
        return Err(RailError::message(format!(
            "prepared Git effect branch ref '{value}' is invalid"
        )));
    }
    Ok(())
}

fn validate_timezone(field: &str, value: &str) -> RailResult<()> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 5 && matches!(bytes[0], b'+' | b'-') && bytes[1..].iter().all(u8::is_ascii_digit);
    let valid = valid && {
        let hours = (bytes[1] - b'0') * 10 + (bytes[2] - b'0');
        let minutes = (bytes[3] - b'0') * 10 + (bytes[4] - b'0');
        minutes < 60 && (hours < 14 || (hours == 14 && minutes == 0))
    };
    if valid {
        Ok(())
    } else {
        Err(RailError::message(format!(
            "prepared Git effect {field} '{value}' is invalid"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn oid(digit: char) -> String {
        std::iter::repeat_n(digit, 40).collect()
    }

    fn digest(digit: char) -> String {
        format!("sha256-{}", std::iter::repeat_n(digit, 64).collect::<String>())
    }

    fn repository() -> (tempfile::TempDir, SystemGit) {
        let repo = tempfile::tempdir().unwrap();
        crate::git::init_repo(repo.path(), "main").unwrap();
        let git = SystemGit::open(repo.path()).unwrap();
        (repo, git)
    }

    fn intent(store: &GitEffectStore, git: &SystemGit, publication: bool) -> GitEffectIntent {
        let result_oid = oid('b');
        let repository = store
            .capture_repository_authority(
                git,
                digest('a'),
                "refs/heads/main".to_string(),
                None,
                result_oid.clone(),
            )
            .unwrap();
        GitEffectIntent::new(
            "sync-apply-0123456789ab".to_string(),
            repository,
            Some(GitCommitEffect {
                oid: result_oid.clone(),
                tree: oid('c'),
                parents: Vec::new(),
                message: "sync exact change\n".to_string(),
                metadata: GitEffectCommitMetadata {
                    author: "Test Author".to_string(),
                    author_email: "author@example.com".to_string(),
                    author_timestamp: 1_700_000_000,
                    author_timezone: "+0000".to_string(),
                    committer: "Test Committer".to_string(),
                    committer_email: "committer@example.com".to_string(),
                    committer_timestamp: 1_700_000_001,
                    committer_timezone: "+0000".to_string(),
                },
            }),
            vec![
                GitPathTransition::new(
                    Path::new("src/lib.rs"),
                    GitPathImage::Absent,
                    GitPathImage::Entry {
                        mode: "100644".to_string(),
                        oid: oid('d'),
                    },
                )
                .unwrap(),
            ],
            Some(GitMappingBinding {
                owner: "demo".to_string(),
                ownership_snapshot: "v2-stable-policy".to_string(),
                pre_authority: digest('e'),
                post_authority: digest('f'),
                migration_digest: None,
                migration_count: 0,
            }),
            publication.then(|| GitPublicationEffect {
                logical_remote: digest('1'),
                exact_endpoint_digest: digest('2'),
                ref_name: "refs/heads/main".to_string(),
                expected_oid: None,
                desired_oid: result_oid,
            }),
            Some(digest('3')),
        )
        .unwrap()
    }

    fn active(record: GitEffectRecord) -> ActiveGitEffect {
        match record {
            GitEffectRecord::Active(active) => active,
            GitEffectRecord::Completed(_) => panic!("expected active effect"),
        }
    }

    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
            let mut entries = fs::read_dir(path).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            entries.sort_by_key(fs::DirEntry::file_name);
            for entry in entries {
                let absolute = entry.path();
                let relative = absolute.strip_prefix(root).unwrap().to_path_buf();
                let metadata = fs::symlink_metadata(&absolute).unwrap();
                if metadata.is_dir() {
                    snapshot.insert(relative.clone(), b"directory".to_vec());
                    visit(root, &absolute, snapshot);
                } else if metadata.is_file() {
                    snapshot.insert(relative, fs::read(&absolute).unwrap());
                } else {
                    snapshot.insert(relative, b"other".to_vec());
                }
            }
        }

        let mut snapshot = BTreeMap::new();
        visit(root, root, &mut snapshot);
        snapshot
    }

    #[test]
    fn read_only_observation_leaves_absent_store_byte_for_byte_unchanged() {
        let (_repo, git) = repository();
        let common_dir = git.common_dir().unwrap();
        let before = snapshot_tree(&common_dir);
        assert!(GitEffectStore::observe(&git).unwrap().is_none());
        assert!(GitEffectStore::discover_active_read_only(&git).unwrap().is_empty());
        assert_eq!(snapshot_tree(&common_dir), before);
        assert!(!common_dir.join("cargo-rail").exists());
    }

    #[cfg(unix)]
    #[test]
    fn opening_store_does_not_repermission_or_adopt_parent_namespace() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let (_repo, git) = repository();
        let common_dir = git.common_dir().unwrap();
        let namespace = common_dir.join("cargo-rail");
        fs::create_dir(&namespace).unwrap();
        fs::write(namespace.join("unrelated"), b"user-owned\n").unwrap();
        fs::set_permissions(&namespace, fs::Permissions::from_mode(0o755)).unwrap();
        let before_inode = fs::metadata(&namespace).unwrap().ino();
        let store = GitEffectStore::open(&git).unwrap();
        assert_eq!(fs::metadata(&namespace).unwrap().ino(), before_inode);
        assert_eq!(fs::metadata(&namespace).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read(namespace.join("unrelated")).unwrap(), b"user-owned\n");
        assert_eq!(
            fs::read(store.root.path.join(OWNER_MARKER)).unwrap(),
            OWNER_MARKER_BYTES
        );
    }

    #[cfg(unix)]
    #[test]
    fn hostile_lock_symlink_never_opens_or_changes_outside_sentinel() {
        use std::os::unix::fs::symlink;

        let (repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let sentinel = repo.path().join("outside-sentinel");
        fs::write(&sentinel, b"unchanged\n").unwrap();
        let worktree_identity = physical_directory_identity(&git.worktree_root).unwrap();
        let mut identity = CanonicalBytes::new(b"cargo-rail-git-effect-lock-v1");
        identity.field(b"worktree", worktree_identity.as_bytes()).unwrap();
        let name = format!("{}.lock", ContentDigest::sha256(identity.as_slice()));
        symlink(&sentinel, store.locks.path.join(name)).unwrap();
        store.lock_worktree(&worktree_identity).unwrap_err();
        assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged\n");
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_lock_holder() {
        let Some(repository) = std::env::var_os("CARGO_RAIL_GIT_EFFECT_LOCK_REPOSITORY") else {
            return;
        };
        let ready = PathBuf::from(std::env::var_os("CARGO_RAIL_GIT_EFFECT_LOCK_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("CARGO_RAIL_GIT_EFFECT_LOCK_RELEASE").unwrap());
        let git = SystemGit::open(Path::new(&repository)).unwrap();
        let store = GitEffectStore::observe(&git).unwrap().unwrap();
        let worktree_identity = physical_directory_identity(&git.worktree_root).unwrap();
        let _lock = store.lock_worktree(&worktree_identity).unwrap();
        utils::write_file_atomic(&ready, b"locked\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !release.exists() {
            assert!(std::time::Instant::now() < deadline, "lock release timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn worktree_lock_excludes_another_process_until_exact_handle_is_released() {
        let (repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let worktree_identity = physical_directory_identity(&git.worktree_root).unwrap();
        let coordination = tempfile::tempdir().unwrap();
        let ready = coordination.path().join("ready");
        let release = coordination.path().join("release");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mutation::git_effect::tests::subprocess_lock_holder",
                "--nocapture",
            ])
            .env("CARGO_RAIL_GIT_EFFECT_LOCK_REPOSITORY", repo.path())
            .env("CARGO_RAIL_GIT_EFFECT_LOCK_READY", &ready)
            .env("CARGO_RAIL_GIT_EFFECT_LOCK_RELEASE", &release)
            .spawn()
            .unwrap();
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !ready.exists() {
            assert!(std::time::Instant::now() < ready_deadline, "child did not acquire lock");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let (sender, receiver) = std::sync::mpsc::channel();
        let contender = std::thread::spawn(move || {
            let result = store.lock_worktree(&worktree_identity);
            sender.send(result.is_ok()).unwrap();
        });
        assert!(
            receiver.recv_timeout(std::time::Duration::from_millis(150)).is_err(),
            "second process bypassed the retained worktree lock"
        );
        fs::write(&release, b"release\n").unwrap();
        assert!(receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
        contender.join().unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_same_effect_finisher() {
        let Some(repository) = std::env::var_os("CARGO_RAIL_GIT_EFFECT_RETRY_REPOSITORY") else {
            return;
        };
        let ready = PathBuf::from(std::env::var_os("CARGO_RAIL_GIT_EFFECT_RETRY_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("CARGO_RAIL_GIT_EFFECT_RETRY_RELEASE").unwrap());
        let git = SystemGit::open(Path::new(&repository)).unwrap();
        let store = GitEffectStore::observe(&git).unwrap().unwrap();
        let mut intent = intent(&store, &git, false);
        intent.object_bundle_digest = None;
        let mut effect = active(store.prepare(intent).unwrap());
        utils::write_file_atomic(&ready, effect.journal().effect_id().as_bytes()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !release.exists() {
            assert!(std::time::Instant::now() < deadline, "retry release timed out");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        effect.mark_local_applied().unwrap();
        effect.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_same_effect_retries_converge_to_one_terminal_journal() {
        let (repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let mut expected = intent(&store, &git, false);
        expected.object_bundle_digest = None;
        let effect_id = expected.effect_id().unwrap();
        let coordination = tempfile::tempdir().unwrap();
        let ready = coordination.path().join("ready");
        let release = coordination.path().join("release");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mutation::git_effect::tests::subprocess_same_effect_finisher",
                "--nocapture",
            ])
            .env("CARGO_RAIL_GIT_EFFECT_RETRY_REPOSITORY", repo.path())
            .env("CARGO_RAIL_GIT_EFFECT_RETRY_READY", &ready)
            .env("CARGO_RAIL_GIT_EFFECT_RETRY_RELEASE", &release)
            .spawn()
            .unwrap();
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < ready_deadline,
                "child did not prepare the effect"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(fs::read_to_string(&ready).unwrap(), effect_id);
        assert_eq!(store.discover_active().unwrap().len(), 1);

        let (sender, receiver) = std::sync::mpsc::channel();
        let contender_store = store.clone();
        let contender = std::thread::spawn(move || {
            let observed = match contender_store.prepare(expected).unwrap() {
                GitEffectRecord::Completed(completed) => (true, completed.journal().effect_id().to_string()),
                GitEffectRecord::Active(active) => (false, active.journal().effect_id().to_string()),
            };
            sender.send(observed).unwrap();
        });
        assert!(
            receiver.recv_timeout(std::time::Duration::from_millis(150)).is_err(),
            "concurrent retry bypassed the retained worktree lock"
        );
        fs::write(&release, b"release\n").unwrap();
        let (completed, observed_id) = receiver.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        contender.join().unwrap();
        assert!(child.wait().unwrap().success());
        assert!(completed, "the serialized retry recreated an active effect");
        assert_eq!(observed_id, effect_id);
        assert!(store.discover_active().unwrap().is_empty());

        let mut replay = intent(&store, &git, false);
        replay.object_bundle_digest = None;
        match store.prepare(replay).unwrap() {
            GitEffectRecord::Completed(completed) => {
                assert_eq!(completed.journal().effect_id(), effect_id);
                completed.acknowledge().unwrap();
            }
            GitEffectRecord::Active(_) => panic!("terminal concurrent retry was recreated"),
        }
    }

    #[test]
    fn timezone_validation_rejects_near_miss_offsets() {
        validate_timezone("test", "+1400").unwrap();
        validate_timezone("test", "-1400").unwrap();
        for invalid in ["+1401", "-1459", "+1360", "+1460", "+9999", "0000"] {
            validate_timezone("test", invalid).unwrap_err();
        }
    }

    #[test]
    fn strict_journal_rejects_unknown_top_level_and_nested_fields() {
        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let intent = intent(&store, &git, false);
        let effect_id = intent.effect_id().unwrap();
        drop(active(store.prepare(intent).unwrap()));
        let path = store.active_path(&effect_id).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        utils::write_file_atomic(&path, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let error = store.discover_active().unwrap_err();
        assert!(error.to_string().contains("unknown field `unexpected`"), "{error}");

        value.as_object_mut().unwrap().remove("unexpected");
        value["repository"]["unexpected_nested"] = serde_json::json!(true);
        utils::write_file_atomic(&path, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let error = store.discover_active().unwrap_err();
        assert!(
            error.to_string().contains("unknown field `unexpected_nested`"),
            "{error}"
        );
    }

    #[test]
    fn journal_tampering_is_rejected_by_payload_and_filename_identity() {
        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let intent = intent(&store, &git, false);
        let effect_id = intent.effect_id().unwrap();
        drop(active(store.prepare(intent).unwrap()));
        let path = store.active_path(&effect_id).unwrap();
        let original = fs::read(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["operation_id"] = serde_json::json!("sync-apply-tampered");
        utils::write_file_atomic(&path, &serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let error = store.discover_active().unwrap_err();
        assert!(error.to_string().contains("payload digest"), "{error}");

        utils::write_file_atomic(&path, &original).unwrap();
        let wrong = format!("git-effect-v1-sha256-{}", "9".repeat(64));
        let renamed = store.active.path.join(format!("{wrong}.json"));
        fs::rename(&path, &renamed).unwrap();
        let error = store.discover_active().unwrap_err();
        assert!(error.to_string().contains("contains identity"), "{error}");
    }

    #[test]
    fn interrupted_atomic_state_is_resumed_monotonically_and_completed_once() {
        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let intent = intent(&store, &git, true);
        let effect_id = intent.effect_id().unwrap();
        let mut prepared = active(store.prepare(intent.clone()).unwrap());
        assert_eq!(prepared.journal().phase(), GitEffectPhase::Prepared);

        fs::write(store.active.path.join(".cargo-rail-interrupted.tmp"), b"{truncated").unwrap();
        assert_eq!(store.discover_active().unwrap().len(), 1);
        drop(prepared);

        prepared = active(store.resume(&effect_id).unwrap());
        prepared.mark_local_applied().unwrap();
        assert_eq!(prepared.journal().phase(), GitEffectPhase::LocalApplied);
        drop(prepared);

        let mut local = active(store.resume(&effect_id).unwrap());
        local.mark_published().unwrap();
        assert_eq!(local.journal().phase(), GitEffectPhase::Published);
        let completed = local.finish().unwrap();
        assert_eq!(completed.journal().phase(), GitEffectPhase::Published);
        assert!(store.completed_path(&effect_id).unwrap().is_file());
        assert!(store.discover_active().unwrap().is_empty());

        match store.prepare(intent).unwrap() {
            GitEffectRecord::Completed(completed) => {
                assert_eq!(completed.journal().effect_id(), effect_id);
                completed.acknowledge().unwrap();
            }
            GitEffectRecord::Active(_) => panic!("completed effect was recreated as active"),
        }
        assert!(!store.completed_path(&effect_id).unwrap().exists());
    }

    #[test]
    fn completion_overlap_is_collapsed_without_losing_terminal_authority() {
        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let mut intent = intent(&store, &git, false);
        intent.object_bundle_digest = None;
        let effect_id = intent.effect_id().unwrap();
        let mut active = active(store.prepare(intent.clone()).unwrap());
        active.mark_local_applied().unwrap();
        let terminal = active.journal().clone();
        drop(active);

        store
            .write_new_journal(&store.completed, &effect_id, &terminal)
            .unwrap();
        assert_eq!(store.discover_active().unwrap().len(), 1);
        let reopened = GitEffectStore::open(&git).unwrap();
        assert!(reopened.discover_active().unwrap().is_empty());
        match reopened.prepare(intent).unwrap() {
            GitEffectRecord::Completed(completed) => {
                assert_eq!(completed.journal(), &terminal);
            }
            GitEffectRecord::Active(_) => panic!("completion overlap remained active"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_bundle_cleanup_cannot_leave_a_completed_journal_without_its_bundle() {
        use std::os::unix::fs::PermissionsExt as _;

        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let bytes = b"PACK\0acknowledgement-order";
        let bundle_digest = format!("sha256-{}", ContentDigest::sha256(bytes));
        let mut intent = intent(&store, &git, false);
        intent.object_bundle_digest = Some(bundle_digest.clone());
        let effect_id = intent.effect_id().unwrap();
        let mut temporary = store.create_object_bundle_temp().unwrap();
        temporary.file_mut().unwrap().write_all(bytes).unwrap();
        drop(temporary.persist(&effect_id, &bundle_digest).unwrap());

        let mut active = active(store.prepare(intent).unwrap());
        active.mark_local_applied().unwrap();
        let completed = active.finish().unwrap();
        let completed_path = store.completed_path(&effect_id).unwrap();
        let bundle_path = store.object_bundle_path(&effect_id).unwrap();
        assert!(completed_path.is_file());
        assert!(bundle_path.is_file());

        fs::set_permissions(&store.objects.path, fs::Permissions::from_mode(0o500)).unwrap();
        let cleanup = completed.acknowledge();
        fs::set_permissions(&store.objects.path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(cleanup.is_err(), "read-only object storage must stop bundle cleanup");
        assert!(
            !completed_path.exists(),
            "completed authority must be removed before fallible bundle cleanup"
        );
        assert!(
            bundle_path.is_file(),
            "failed cleanup must leave the exact bundle intact"
        );

        let reopened = GitEffectStore::open(&git).unwrap();
        assert!(reopened.discover_unacknowledged().unwrap().is_empty());
        reopened
            .remove_object_bundle_if_present(&effect_id, &bundle_digest)
            .unwrap();
        assert!(!bundle_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn startup_removes_only_stale_unreferenced_private_state() {
        use rustix::fs::{Timespec, Timestamps};
        use std::os::unix::fs::PermissionsExt as _;

        fn make_private_file(path: &Path, bytes: &[u8], stale: bool) {
            fs::write(path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            if stale {
                let old = Timespec {
                    tv_sec: 1_600_000_000,
                    tv_nsec: 0,
                };
                let file = File::open(path).unwrap();
                rustix::fs::futimens(
                    &file,
                    &Timestamps {
                        last_access: old,
                        last_modification: old,
                    },
                )
                .unwrap();
            }
        }

        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let stale_active = store.active.path.join(".cargo-rail-stale.tmp");
        let stale_completed = store.completed.path.join(".cargo-rail-stale.tmp");
        let stale_object = store.objects.path.join(".cargo-rail-stale.tmp");
        let fresh_object = store.objects.path.join(".cargo-rail-fresh.tmp");
        let orphan_effect = format!("git-effect-v1-sha256-{}", "7".repeat(64));
        let orphan_pack = store.objects.path.join(format!("{orphan_effect}.pack"));
        for path in [&stale_active, &stale_completed, &stale_object, &orphan_pack] {
            make_private_file(path, b"stale\n", true);
        }
        make_private_file(&fresh_object, b"fresh\n", false);

        let reopened = GitEffectStore::open(&git).unwrap();
        for path in [&stale_active, &stale_completed, &stale_object, &orphan_pack] {
            assert!(!path.exists(), "stale private state survived: {}", path.display());
        }
        assert_eq!(fs::read(&fresh_object).unwrap(), b"fresh\n");
        reopened.validate_layout().unwrap();
    }

    #[test]
    fn startup_removes_only_the_reached_effects_owned_pack_keep() {
        let (repo, git) = repository();
        git.set_config("user.name", "Test User").unwrap();
        git.set_config("user.email", "test@example.com").unwrap();
        fs::write(repo.path().join("file.txt"), b"old\n").unwrap();
        git.stage_all().unwrap();
        git.commit("old").unwrap();
        let old = git.head_commit().unwrap();
        fs::write(repo.path().join("file.txt"), b"result\n").unwrap();
        git.stage_all().unwrap();
        git.commit("result").unwrap();
        let result = git.head_commit().unwrap();
        let observed = git.get_commit(&result).unwrap();
        let metadata = GitEffectCommitMetadata::from(&observed.metadata());
        let commit = GitCommitEffect {
            oid: result.clone(),
            tree: git
                .run_git_stdout(&["rev-parse", &format!("{result}^{{tree}}")])
                .unwrap(),
            parents: observed.parent_shas,
            message: observed.message,
            metadata,
        };
        git.run_git(&["reset", "--hard", &old]).unwrap();

        let store = GitEffectStore::open(&git).unwrap();
        let repository = store
            .capture_repository_authority(
                &git,
                digest('a'),
                "refs/heads/main".to_string(),
                Some(old.clone()),
                result.clone(),
            )
            .unwrap();
        let intent = GitEffectIntent::new(
            "split-apply-startup-keep".to_string(),
            repository,
            Some(commit),
            Vec::new(),
            None,
            None,
            None,
        )
        .unwrap();
        let effect_id = intent.effect_id().unwrap();
        drop(active(store.prepare(intent).unwrap()));

        let objects = PathBuf::from(git.run_git_stdout(&["rev-parse", "--git-path", "objects"]).unwrap());
        let objects = if objects.is_absolute() {
            objects
        } else {
            repo.path().join(objects)
        };
        let packs = objects.join("pack");
        fs::create_dir_all(&packs).unwrap();
        let owned = packs.join("pack-owned.keep");
        let foreign = packs.join("pack-foreign.keep");
        fs::write(&owned, format!("cargo-rail prepared effect {effect_id}\n")).unwrap();
        fs::write(&foreign, b"another owner\n").unwrap();
        git.run_git(&["update-ref", "refs/heads/main", &result, &old]).unwrap();

        GitEffectStore::open(&git).unwrap();
        assert!(!owned.exists());
        assert_eq!(fs::read(&foreign).unwrap(), b"another owner\n");
    }

    #[test]
    fn canonical_intent_identity_does_not_depend_on_path_input_order() {
        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let first = intent(&store, &git, false);
        let mut repository = first.repository;
        repository.result_oid = oid('4');
        let transitions = vec![
            GitPathTransition::new(
                Path::new("z.txt"),
                GitPathImage::Absent,
                GitPathImage::Entry {
                    mode: "100644".to_string(),
                    oid: oid('5'),
                },
            )
            .unwrap(),
            GitPathTransition::new(
                Path::new("a.txt"),
                GitPathImage::Absent,
                GitPathImage::Entry {
                    mode: "100644".to_string(),
                    oid: oid('6'),
                },
            )
            .unwrap(),
        ];
        let forward = GitEffectIntent::new(
            "split-apply-stable".to_string(),
            repository.clone(),
            None,
            transitions.clone(),
            None,
            None,
            None,
        )
        .unwrap();
        let reverse = GitEffectIntent::new(
            "split-apply-stable".to_string(),
            repository,
            None,
            transitions.into_iter().rev().collect(),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(forward.payload_digest().unwrap(), reverse.payload_digest().unwrap());
        assert_eq!(forward.effect_id().unwrap(), reverse.effect_id().unwrap());
    }

    #[test]
    fn push_only_intent_is_the_only_valid_unchanged_local_ref_shape() {
        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let mut repository = intent(&store, &git, false).repository().clone();
        repository.expected_oid = Some(repository.result_oid.clone());
        let publication = GitPublicationEffect::new(
            digest('1'),
            digest('2'),
            "refs/heads/main".to_string(),
            None,
            repository.result_oid.clone(),
        );
        GitEffectIntent::new(
            "sync-push-only".to_string(),
            repository.clone(),
            None,
            Vec::new(),
            None,
            Some(publication),
            None,
        )
        .unwrap();
        GitEffectIntent::new(
            "sync-local-noop".to_string(),
            repository,
            None,
            Vec::new(),
            None,
            None,
            None,
        )
        .unwrap_err();
    }

    #[test]
    fn v1_path_images_reject_unmaterializable_gitlinks() {
        GitPathImage::entry("160000".to_string(), oid('a'))
            .validate("sha1")
            .unwrap_err();
    }

    #[test]
    fn object_bundle_is_published_and_reopened_through_the_exact_private_inode() {
        let (_repo, git) = repository();
        let store = GitEffectStore::open(&git).unwrap();
        let intent = intent(&store, &git, false);
        let effect_id = intent.effect_id().unwrap();
        let bytes = b"PACK\0prepared-object-bundle";
        let expected_digest = format!("sha256-{}", ContentDigest::sha256(bytes));
        let mut temporary = store.create_object_bundle_temp().unwrap();
        temporary.file_mut().unwrap().write_all(bytes).unwrap();
        let mut bundle = temporary.persist(&effect_id, &expected_digest).unwrap();
        assert_eq!(bundle.path(), store.object_bundle_path(&effect_id).unwrap());
        let mut observed = Vec::new();
        bundle.file.read_to_end(&mut observed).unwrap();
        assert_eq!(observed, bytes);
        let reopened = store.open_object_bundle(&effect_id, &expected_digest).unwrap().unwrap();
        assert_eq!(reopened.file().metadata().unwrap().len(), bytes.len() as u64);
        drop(reopened.into_file());
    }
}
