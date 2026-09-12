//! Git-native split/sync origin mapping.
//!
//! Synthesized commits carry a versioned `Rail-Origin` trailer, so ordinary
//! clone history is sufficient to recover source/target mappings.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::git::{SystemGit, git_cmd_for_path};

use crate::source::ContentDigest;
use crate::utils;

const TRAILER_PREFIX: &str = "Rail-Origin: ";
const TRAILER_SCHEMA: &str = "v2";

/// Split/sync transform schema recorded in every synthesized commit.
pub const TRANSFORM_VERSION: u32 = 1;

/// Which side of a source-to-target mapping owns the scanned history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySide {
    /// The monorepo history. Current commits map to source commits in the target.
    Source,
    /// The split-repository history. Origin commits map to current target commits.
    Target,
}

/// Stable context required to synthesize a versioned origin trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginContext {
    source_repository: String,
    owner: String,
    ownership_snapshot: String,
}

impl OriginContext {
    /// Bind a source repository, split owner, and stable ownership-policy digest.
    pub fn new(
        source_repository: impl Into<String>,
        owner: impl Into<String>,
        ownership_snapshot: impl Into<String>,
    ) -> RailResult<Self> {
        let context = Self {
            source_repository: source_repository.into(),
            owner: owner.into(),
            ownership_snapshot: ownership_snapshot.into(),
        };
        validate_repository_identity(&context.source_repository)?;
        if context.owner.is_empty() {
            return Err(RailError::message("Rail-Origin owner must not be empty"));
        }
        validate_token("ownership snapshot", &context.ownership_snapshot)?;
        Ok(context)
    }

    /// Discover the source repository identity without serializing credentials or paths.
    pub fn discover(
        repo_path: &Path,
        owner: impl Into<String>,
        ownership_snapshot: impl Into<String>,
    ) -> RailResult<Self> {
        Self::new(repository_identity(repo_path)?, owner, ownership_snapshot)
    }

    /// Opaque stable source-repository identity.
    pub fn source_repository(&self) -> &str {
        &self.source_repository
    }

    /// Format a normal mapping trailer whose target is the containing commit.
    pub fn trailer(&self, source_commit: &str) -> RailResult<String> {
        self.format_trailer(source_commit, true)
    }

    /// Format provenance for a synthesized commit that does not define a new mapping.
    pub fn evidence_trailer(&self, source_commit: &str) -> RailResult<String> {
        self.format_trailer(source_commit, false)
    }

    fn format_trailer(&self, source_commit: &str, mapping: bool) -> RailResult<String> {
        let source_commit = normalize_object_id("source", source_commit)?;
        let mut trailer = format!(
            "{TRAILER_PREFIX}{TRAILER_SCHEMA} source={} commit={} owner={} snapshot={} transform={TRANSFORM_VERSION}",
            self.source_repository,
            source_commit,
            encode_hex(self.owner.as_bytes()),
            self.ownership_snapshot,
        );
        if !mapping {
            trailer.push_str(" mapping=evidence");
        }
        Ok(trailer)
    }
}

/// Append one or more trailers without rewriting the original message body.
pub fn append_origin_trailers(message: &str, trailers: &[String]) -> String {
    if trailers.is_empty() {
        return message.to_string();
    }
    let mut output = message.to_string();
    if !output.is_empty() {
        if !output.ends_with('\n') {
            output.push('\n');
        }
        if !output.ends_with("\n\n") {
            output.push('\n');
        }
    }
    output.push_str(&trailers.join("\n"));
    output
}

/// Derive a path-independent, credential-free repository identity.
///
/// A non-local `remote.origin.url` is normalized and hashed. Repositories
/// without such a remote use their sorted root commit IDs, which remain stable
/// across ordinary clones.
pub fn repository_identity(repo_path: &Path) -> RailResult<String> {
    let git = SystemGit::open(repo_path)?;
    let head = git.head_commit().ok();
    repository_identity_from_git(&git, head.as_deref())
}

/// Derive repository identity from an already-open worktree and an exact HEAD
/// observation. Callers that also bind HEAD avoid reopening the repository and
/// resolving the same ref twice.
pub(crate) fn repository_identity_from_git(git: &SystemGit, head: Option<&str>) -> RailResult<String> {
    if let Some(url) = git.get_config("remote.origin.url")?
        && !utils::is_local_path(&url)
    {
        return Ok(format!(
            "sha256-{}",
            ContentDigest::sha256(format!("remote\0{}", normalize_remote_url(&url)?).as_bytes())
        ));
    }
    let Some(head) = head else {
        let identity_input = format!(
            "unborn\0{}",
            utils::canonicalize_existing(&git.worktree_root)?.display()
        );
        return Ok(format!("sha256-{}", ContentDigest::sha256(identity_input.as_bytes())));
    };
    let output = git
        .git_cmd()
        .args(["rev-list", "--max-parents=0", head])
        .output()
        .context("Failed to discover Git root commits")?;
    if !output.status.success() {
        return Err(RailError::message(format!(
            "failed to discover repository identity: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    repository_identity_from_roots(String::from_utf8(output.stdout)?.lines().map(str::to_string))
}

pub(crate) fn repository_identity_from_roots(roots: impl IntoIterator<Item = String>) -> RailResult<String> {
    let mut roots = roots
        .into_iter()
        .map(|root| root.trim().to_string())
        .filter(|root| !root.is_empty())
        .collect::<Vec<_>>();
    roots.sort_unstable();
    roots.dedup();
    if roots.is_empty() {
        return Err(RailError::message(
            "cannot identify a repository without a remote or root commit",
        ));
    }
    for root in &roots {
        validate_object_id("root", root)?;
    }
    let identity_input = format!("roots\0{}", roots.join("\n"));
    Ok(format!("sha256-{}", ContentDigest::sha256(identity_input.as_bytes())))
}

fn normalize_remote_url(url: &str) -> RailResult<String> {
    validate_token("Git remote URL", url)?;
    let without_query = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
    let normalized = if let Some((scheme, remainder)) = without_query.split_once("://") {
        let slash = remainder.find('/').unwrap_or(remainder.len());
        let (authority, path) = remainder.split_at(slash);
        let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        format!(
            "{}://{}{}",
            scheme.to_ascii_lowercase(),
            authority.to_ascii_lowercase(),
            path
        )
    } else {
        without_query.to_string()
    };
    Ok(normalized.trim_end_matches(".git").to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CommitMapping {
    source: String,
    target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MappingFrontier {
    Neither,
    Source,
    Target,
    Both,
}

impl MappingFrontier {
    fn parse(value: &str) -> RailResult<Self> {
        match value {
            "none" => Ok(Self::Neither),
            "source" => Ok(Self::Source),
            "target" => Ok(Self::Target),
            "both" => Ok(Self::Both),
            _ => Err(RailError::message(format!(
                "Rail-Origin frontier '{}' is unsupported",
                value
            ))),
        }
    }

    fn proves_source(self) -> bool {
        matches!(self, Self::Source | Self::Both)
    }

    fn proves_target(self) -> bool {
        matches!(self, Self::Target | Self::Both)
    }
}

impl CommitMapping {
    fn new(source: &str, target: &str) -> RailResult<Self> {
        Ok(Self {
            source: normalize_object_id("source", source)?,
            target: normalize_object_id("target", target)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTrailer {
    source_repository: String,
    source_commit: String,
    owner: String,
    ownership_snapshot: String,
    transform_version: u32,
    mapping: bool,
    target_commit: Option<String>,
    frontier: Option<MappingFrontier>,
    evidence_commit: Option<String>,
    evidence_side: Option<HistorySide>,
}

impl ParsedTrailer {
    fn parse(value: &str) -> RailResult<Self> {
        let mut fields = value.split_whitespace();
        if fields.next() != Some(TRAILER_SCHEMA) {
            return Err(RailError::message(format!(
                "unsupported Rail-Origin trailer '{}'",
                value
            )));
        }
        let source_repository = parse_field(fields.next(), "source")?.to_string();
        validate_repository_identity(&source_repository)?;
        let source_commit = normalize_object_id("source", parse_field(fields.next(), "commit")?)?;
        let owner = decode_hex(parse_field(fields.next(), "owner")?)?;
        let ownership_snapshot = parse_field(fields.next(), "snapshot")?.to_string();
        validate_token("ownership snapshot", &ownership_snapshot)?;
        let transform_version = parse_field(fields.next(), "transform")?
            .parse::<u32>()
            .map_err(|_| RailError::message("Rail-Origin transform must be an unsigned integer"))?;
        let (mapping, target_commit, frontier, evidence_commit, evidence_side) = match fields.next() {
            None => (true, None, None, None, None),
            Some("mapping=evidence") => {
                let evidence_commit = {
                    fields
                        .next()
                        .map(|field| normalize_object_id("evidence", parse_field(Some(field), "evidence")?))
                        .transpose()?
                };
                let evidence_side = if evidence_commit.is_some() {
                    match parse_field(fields.next(), "side")? {
                        "source" => Some(HistorySide::Source),
                        "target" => Some(HistorySide::Target),
                        value => {
                            return Err(RailError::message(format!(
                                "Rail-Origin evidence side '{}' is unsupported",
                                value
                            )));
                        }
                    }
                } else {
                    None
                };
                (false, None, None, evidence_commit, evidence_side)
            }
            Some(target) if target.starts_with("target=") => {
                let target = normalize_object_id("target", parse_field(Some(target), "target")?)?;
                let frontier = {
                    fields
                        .next()
                        .map(|field| MappingFrontier::parse(parse_field(Some(field), "frontier")?))
                        .transpose()?
                };
                (true, Some(target), frontier, None, None)
            }
            Some(_) => return Err(RailError::message("Rail-Origin trailer has unknown fields")),
        };
        if fields.next().is_some() {
            return Err(RailError::message("Rail-Origin trailer has unknown fields"));
        }
        Ok(Self {
            source_repository,
            source_commit,
            owner,
            ownership_snapshot,
            transform_version,
            mapping,
            target_commit,
            frontier,
            evidence_commit,
            evidence_side,
        })
    }
}

fn parse_field<'a>(field: Option<&'a str>, name: &str) -> RailResult<&'a str> {
    field
        .and_then(|field| field.strip_prefix(name).and_then(|value| value.strip_prefix('=')))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RailError::message(format!("Rail-Origin trailer is missing {name}")))
}

/// One-to-one source/target mapping recovered from ordinary Git history.
#[derive(Debug)]
pub struct MappingStore {
    owner: String,
    expected_ownership_snapshot: Option<String>,
    repository_authority: Option<RepositoryAuthority>,
    mappings: FxHashMap<String, String>,
    reverse_mappings: FxHashMap<String, String>,
    source_frontiers: FxHashSet<String>,
    target_frontiers: FxHashSet<String>,
    explicit_pair_commits: FxHashSet<String>,
    source_evidence: FxHashSet<String>,
    target_evidence: FxHashSet<String>,
    source_evidence_pairs: FxHashSet<(String, String)>,
    target_evidence_pairs: FxHashSet<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositoryAuthority {
    source_repository: String,
    source_head: String,
    source_selected_heads: Vec<String>,
    target_repository: String,
    target_head: String,
    target_selected_head: String,
    ownership_snapshot: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetBranchRelation {
    Missing,
    RemoteOnly,
    Current,
    Ahead,
    Behind,
}

impl TargetBranchRelation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::RemoteOnly => "remote_only",
            Self::Current => "current",
            Self::Ahead => "ahead",
            Self::Behind => "behind",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetBranchObservation {
    remote_repository: String,
    remote_head: Option<String>,
    local_head: Option<String>,
    relation: TargetBranchRelation,
    effective_head: Option<String>,
}

impl TargetBranchObservation {
    pub(crate) fn effective_head(&self) -> Option<&str> {
        self.effective_head.as_deref()
    }

    pub(crate) fn remote_repository(&self) -> &str {
        &self.remote_repository
    }

    pub(crate) fn remote_head(&self) -> Option<&str> {
        self.remote_head.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetPublicationSnapshot {
    observation: TargetBranchObservation,
    owned_ahead: Vec<String>,
    digest: ContentDigest,
}

impl TargetPublicationSnapshot {
    pub(crate) fn capture(
        observation: TargetBranchObservation,
        target_repo: &Path,
        mappings: Option<&MappingStore>,
    ) -> RailResult<Self> {
        let owned_ahead = if observation.relation == TargetBranchRelation::Ahead {
            let local_head = observation
                .local_head
                .as_deref()
                .ok_or_else(|| RailError::message("an ahead split target has no local branch head"))?;
            let commits = revision_range(target_repo, observation.remote_head.as_deref(), local_head)?;
            let mappings = mappings
                .ok_or_else(|| RailError::message("owned split publication validation requires mapping evidence"))?;
            for commit in &commits {
                if !mappings.owns_target_commit(commit) {
                    return Err(RailError::with_help(
                        format!(
                            "local split target has unrelated commit '{}' ahead of its configured remote branch",
                            commit
                        ),
                        "publish or remove unrelated local commits manually; cargo-rail only publishes its own exact origin history",
                    ));
                }
            }
            commits
        } else {
            Vec::new()
        };
        let digest = ContentDigest::sha256(&canonical_publication_bytes(&observation, &owned_ahead));
        Ok(Self {
            observation,
            owned_ahead,
            digest,
        })
    }

    /// Reconstruct the exact remote/local observation before a prepared effect.
    #[expect(
        clippy::too_many_arguments,
        reason = "prepared publication authority has several independent identity fields"
    )]
    pub(crate) fn capture_prepared_authority(
        actual: &TargetBranchObservation,
        target_repo: &Path,
        mappings: Option<&MappingStore>,
        logical_remote: &str,
        expected_remote_head: Option<&str>,
        desired_remote_head: &str,
        expected_local_head: Option<&str>,
        result_local_head: &str,
    ) -> RailResult<Self> {
        if actual.remote_repository != logical_remote {
            return Err(RailError::message(
                "prepared publication logical remote authority changed",
            ));
        }
        if actual.remote_head.as_deref() != expected_remote_head
            && actual.remote_head.as_deref() != Some(desired_remote_head)
        {
            return Err(RailError::with_help(
                "prepared publication remote branch is in a third ref state",
                "restore the exact journaled old or desired remote ref before retrying",
            ));
        }
        if actual.local_head.as_deref() != expected_local_head
            && actual.local_head.as_deref() != Some(result_local_head)
        {
            return Err(RailError::with_help(
                "prepared publication local branch is in a third ref state",
                "restore the exact journaled old or result local ref before retrying",
            ));
        }
        let observation = target_branch_observation_from_heads(
            target_repo,
            logical_remote.to_string(),
            expected_remote_head.map(str::to_string),
            expected_local_head.map(str::to_string),
        )?;
        Self::capture(observation, target_repo, mappings)
    }

    pub(crate) fn count(&self) -> usize {
        self.owned_ahead.len()
    }

    pub(crate) fn digest(&self) -> String {
        format!("sha256-{}", self.digest)
    }

    pub(crate) fn relation(&self) -> &'static str {
        self.observation.relation.as_str()
    }

    pub(crate) fn remote_head(&self) -> Option<&str> {
        self.observation.remote_head.as_deref()
    }

    pub(crate) fn local_head(&self) -> Option<&str> {
        self.observation.local_head.as_deref()
    }

    pub(crate) fn remote_repository(&self) -> &str {
        &self.observation.remote_repository
    }

    pub(crate) fn permits_target_mutation(&self) -> bool {
        !matches!(
            self.observation.relation,
            TargetBranchRelation::Behind | TargetBranchRelation::RemoteOnly
        )
    }

    pub(crate) fn same_remote_authority(&self, other: &Self) -> bool {
        self.observation.remote_repository == other.observation.remote_repository
            && self.observation.remote_head == other.observation.remote_head
    }
}

pub(crate) fn observe_target_branch(
    observation_repo: &Path,
    target_repo: &Path,
    remote_url: &str,
    branch: &str,
) -> RailResult<TargetBranchObservation> {
    let normalized_remote = normalize_remote_url(remote_url)?;
    let remote_repository = format!(
        "sha256-{}",
        ContentDigest::sha256(format!("remote\0{normalized_remote}").as_bytes())
    );
    let remote_head = SystemGit::open(observation_repo)?.remote_branch_head(remote_url, branch)?;
    let local_git = target_repo
        .join(".git")
        .exists()
        .then(|| SystemGit::open(target_repo))
        .transpose()?;
    if let (Some(target_git), Some(remote)) = (local_git.as_ref(), remote_head.as_deref())
        && target_git.get_commit(remote).is_err()
    {
        return Err(RailError::with_help(
            format!("configured remote branch commit '{remote}' is absent from the local target object view"),
            format!(
                "fetch it explicitly, for example: git -C '{}' fetch --no-tags <configured-url> refs/heads/{branch}",
                target_repo.display()
            ),
        ));
    }
    let local_head = local_git.as_ref().and_then(|git| git.head_commit().ok());
    target_branch_observation_from_heads(target_repo, remote_repository, remote_head, local_head)
}

fn target_branch_observation_from_heads(
    target_repo: &Path,
    remote_repository: String,
    remote_head: Option<String>,
    local_head: Option<String>,
) -> RailResult<TargetBranchObservation> {
    let (relation, effective_head) = match (local_head.as_deref(), remote_head.as_deref()) {
        (None, None) => (TargetBranchRelation::Missing, None),
        (None, Some(remote)) => (TargetBranchRelation::RemoteOnly, Some(remote.to_string())),
        (Some(local), None) => (TargetBranchRelation::Ahead, Some(local.to_string())),
        (Some(local), Some(remote)) if local == remote => (TargetBranchRelation::Current, Some(local.to_string())),
        (Some(local), Some(remote)) => {
            if is_ancestor(target_repo, remote, local)? {
                (TargetBranchRelation::Ahead, Some(local.to_string()))
            } else if is_ancestor(target_repo, local, remote)? {
                (TargetBranchRelation::Behind, Some(remote.to_string()))
            } else {
                return Err(RailError::with_help(
                    "local split target and its configured remote branch have diverged",
                    "reconcile the branches manually; cargo-rail will not select or overwrite either history",
                ));
            }
        }
    };
    Ok(TargetBranchObservation {
        remote_repository,
        remote_head,
        local_head,
        relation,
        effective_head,
    })
}

/// Identity derived from the exact configured non-local remote URL.
pub(crate) fn remote_repository_identity(remote_url: &str) -> RailResult<String> {
    let normalized_remote = normalize_remote_url(remote_url)?;
    Ok(format!(
        "sha256-{}",
        ContentDigest::sha256(format!("remote\0{normalized_remote}").as_bytes())
    ))
}

/// Identity of the exact configured endpoint string used by a publication.
///
/// The logical repository identity deliberately normalizes harmless URL
/// aliases. Publication authority additionally binds the exact endpoint so a
/// retry cannot be redirected by changing spelling, credentials, or transport.
pub(crate) fn remote_endpoint_identity(remote_url: &str) -> RailResult<String> {
    let _ = normalize_remote_url(remote_url)?;
    Ok(format!(
        "sha256-{}",
        ContentDigest::sha256(&[b"cargo-rail-remote-endpoint-v1\0".as_slice(), remote_url.as_bytes()].concat())
    ))
}

fn canonical_publication_bytes(observation: &TargetBranchObservation, owned_ahead: &[String]) -> Vec<u8> {
    let mut bytes = b"cargo-rail-target-publication-v1".to_vec();
    append_authority_frame(
        &mut bytes,
        b"remote-repository",
        observation.remote_repository.as_bytes(),
    );
    append_optional_authority_frame(
        &mut bytes,
        b"remote-head",
        observation.remote_head.as_deref().map(str::as_bytes),
    );
    append_optional_authority_frame(
        &mut bytes,
        b"local-head",
        observation.local_head.as_deref().map(str::as_bytes),
    );
    append_authority_frame(&mut bytes, b"relation", observation.relation.as_str().as_bytes());
    for commit in owned_ahead {
        append_authority_frame(&mut bytes, b"owned-ahead", commit.as_bytes());
    }
    bytes
}

/// Exact split/sync mapping authority bound into check/apply plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MappingAuthoritySnapshot {
    direction: String,
    target_root: std::path::PathBuf,
    branch: String,
    source_repository: String,
    source_head: String,
    source_selected_heads: Vec<String>,
    target_repository: Option<String>,
    target_head: Option<String>,
    target_selected_head: Option<String>,
    owner: String,
    ownership_snapshot: String,
    transform_version: u32,
    mappings: Vec<(String, String)>,
    source_frontiers: Vec<String>,
    target_frontiers: Vec<String>,
    source_evidence: Vec<String>,
    target_evidence: Vec<String>,
    digest: ContentDigest,
}

impl MappingAuthoritySnapshot {
    #[expect(
        clippy::too_many_arguments,
        reason = "the constructor validates every field of one complete authority record"
    )]
    fn from_authority(
        direction: &str,
        target_root: &Path,
        branch: &str,
        source_repository: String,
        source_head: String,
        source_selected_heads: Vec<String>,
        target_repository: Option<String>,
        target_head: Option<String>,
        target_selected_head: Option<String>,
        owner: String,
        ownership_snapshot: String,
        mappings: Vec<(String, String)>,
        source_frontiers: Vec<String>,
        target_frontiers: Vec<String>,
        source_evidence: Vec<String>,
        target_evidence: Vec<String>,
    ) -> RailResult<Self> {
        validate_token("mapping direction", direction)?;
        validate_token("mapping branch", branch)?;
        validate_repository_identity(&source_repository)?;
        if let Some(target_repository) = &target_repository {
            validate_repository_identity(target_repository)?;
        }
        validate_object_id("source HEAD", &source_head)?;
        for selected_source_head in &source_selected_heads {
            validate_object_id("selected source HEAD", selected_source_head)?;
        }
        if let Some(target_head) = &target_head {
            validate_object_id("target HEAD", target_head)?;
        }
        if let Some(target_selected_head) = &target_selected_head {
            validate_object_id("selected target HEAD", target_selected_head)?;
        }
        validate_token("ownership snapshot", &ownership_snapshot)?;

        let mut snapshot = Self {
            direction: direction.to_string(),
            target_root: target_root.to_path_buf(),
            branch: branch.to_string(),
            source_repository,
            source_head,
            source_selected_heads,
            target_repository,
            target_head,
            target_selected_head,
            owner,
            ownership_snapshot,
            transform_version: TRANSFORM_VERSION,
            mappings,
            source_frontiers,
            target_frontiers,
            source_evidence,
            target_evidence,
            digest: ContentDigest::sha256(&[]),
        };
        snapshot.digest = ContentDigest::sha256(&snapshot.canonical_bytes());
        Ok(snapshot)
    }

    /// Capture an already-initialized, clean, unborn target repository. The
    /// configured directory and repository identity exist, but no target
    /// history or mapping authority exists yet.
    pub(crate) fn empty_initialized(
        source_repo: &Path,
        source_context: &OriginContext,
        target_repo: &Path,
        target_root: &Path,
        branch: &str,
        direction: &str,
    ) -> RailResult<Self> {
        let source_repository = repository_identity(source_repo)?;
        if source_repository != source_context.source_repository {
            return Err(RailError::message(
                "mapping source repository identity changed during authority capture",
            ));
        }
        let target = SystemGit::open(target_repo)?;
        if target.head_commit().is_ok() {
            return Err(RailError::message(
                "unborn target authority capture found existing target history",
            ));
        }
        let actual_branch = target.current_branch()?;
        if actual_branch != branch {
            return Err(RailError::with_help(
                format!("unborn split target is on branch '{actual_branch}', not '{branch}'"),
                format!("reinitialize the empty target with: git init -b {branch}"),
            ));
        }
        Self::from_authority(
            direction,
            target_root,
            branch,
            source_repository,
            SystemGit::open(source_repo)?.head_commit()?,
            selected_source_heads(source_repo)?,
            Some(repository_identity(target_repo)?),
            None,
            None,
            source_context.owner.clone(),
            source_context.ownership_snapshot.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Build unborn-target authority from repository and HEAD observations the
    /// caller already captured at the same planning boundary.
    pub(crate) fn empty_initialized_from_observed(
        source_context: &OriginContext,
        source_head: String,
        target_repository: String,
        target_root: &Path,
        branch: &str,
        direction: &str,
    ) -> RailResult<Self> {
        validate_repository_identity(&target_repository)?;
        validate_object_id("source HEAD", &source_head)?;
        Self::from_authority(
            direction,
            target_root,
            branch,
            source_context.source_repository.clone(),
            source_head.clone(),
            vec![source_head],
            Some(target_repository),
            None,
            None,
            source_context.owner.clone(),
            source_context.ownership_snapshot.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Reconstruct the exact pre-effect authority of a journaled unborn target.
    ///
    /// The caller must first bind the physical repository, configured branch,
    /// and observed old/result ref state to the prepared journal. This method
    /// rebuilds only the immutable mapping image that existed before the ref.
    pub(crate) fn empty_initialized_bound(
        source_repo: &Path,
        source_context: &OriginContext,
        target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
    ) -> RailResult<Self> {
        validate_repository_identity(target_repository)?;
        let source_repository = repository_identity(source_repo)?;
        if source_repository != source_context.source_repository {
            return Err(RailError::message(
                "mapping source repository identity changed during prepared recovery",
            ));
        }
        Self::from_authority(
            direction,
            target_root,
            branch,
            source_repository,
            SystemGit::open(source_repo)?.head_commit()?,
            selected_source_heads(source_repo)?,
            Some(target_repository.to_string()),
            None,
            None,
            source_context.owner.clone(),
            source_context.ownership_snapshot.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        let mut canonical = b"cargo-rail-mapping-authority-v3".to_vec();
        append_authority_frame(&mut canonical, b"direction", self.direction.as_bytes());
        append_authority_frame(
            &mut canonical,
            b"target-root",
            self.target_root.as_os_str().as_encoded_bytes(),
        );
        append_authority_frame(&mut canonical, b"branch", self.branch.as_bytes());
        append_authority_frame(&mut canonical, b"source-repository", self.source_repository.as_bytes());
        append_authority_frame(&mut canonical, b"source-head", self.source_head.as_bytes());
        for selected_source_head in &self.source_selected_heads {
            append_authority_frame(&mut canonical, b"source-selected-head", selected_source_head.as_bytes());
        }
        append_optional_authority_frame(
            &mut canonical,
            b"target-repository",
            self.target_repository.as_deref().map(str::as_bytes),
        );
        append_optional_authority_frame(
            &mut canonical,
            b"target-head",
            self.target_head.as_deref().map(str::as_bytes),
        );
        append_optional_authority_frame(
            &mut canonical,
            b"target-selected-head",
            self.target_selected_head.as_deref().map(str::as_bytes),
        );
        append_authority_frame(&mut canonical, b"owner", self.owner.as_bytes());
        append_authority_frame(
            &mut canonical,
            b"ownership-snapshot",
            self.ownership_snapshot.as_bytes(),
        );
        append_authority_frame(
            &mut canonical,
            b"transform-version",
            &self.transform_version.to_be_bytes(),
        );
        append_mapping_pairs(&mut canonical, b"mapping", &self.mappings);
        for commit in &self.source_frontiers {
            append_authority_frame(&mut canonical, b"source-frontier", commit.as_bytes());
        }
        for commit in &self.target_frontiers {
            append_authority_frame(&mut canonical, b"target-frontier", commit.as_bytes());
        }
        for commit in &self.source_evidence {
            append_authority_frame(&mut canonical, b"source-evidence", commit.as_bytes());
        }
        for commit in &self.target_evidence {
            append_authority_frame(&mut canonical, b"target-evidence", commit.as_bytes());
        }
        canonical
    }

    pub(crate) fn digest(&self) -> String {
        format!("sha256-{}", self.digest)
    }

    /// Derive the exact authority produced by one prepared mono-to-remote
    /// split chain. Every source mapping advances only the source frontier;
    /// optional target evidence is an exact skip pair and never grants an
    /// ancestry frontier. The complete result is known before the prepared
    /// object pack is installed or the target ref is moved.
    pub(crate) fn after_split_chain(
        &self,
        new_mappings: &[(String, String)],
        new_target_evidence: &[(String, String)],
        target_head: &str,
        target_repository: String,
    ) -> RailResult<Self> {
        if self.target_repository.is_none() {
            return Err(RailError::message(
                "ordinary split authority requires an initialized target repository",
            ));
        }
        let target_head = normalize_object_id("prepared split target HEAD", target_head)?;
        validate_repository_identity(&target_repository)?;
        let mut mappings = self.mappings.clone();
        let mut source_frontiers = self.source_frontiers.clone();
        let target_frontiers = self.target_frontiers.clone();
        let source_evidence = self.source_evidence.clone();
        let mut target_evidence = self.target_evidence.clone();

        for (source, target) in new_mappings {
            let source = normalize_object_id("prepared split source mapping", source)?;
            let target = normalize_object_id("prepared split target mapping", target)?;
            mappings.push((source.clone(), target));
            source_frontiers.push(source);
        }
        for (source, target) in new_target_evidence {
            let source = normalize_object_id("prepared split evidence origin", source)?;
            let target = normalize_object_id("prepared split evidence commit", target)?;
            target_evidence.push(format!("endpoint:{target}"));
            target_evidence.push(format!("pair:{source}:{target}"));
        }

        mappings.sort();
        mappings.dedup();
        source_frontiers.sort();
        source_frontiers.dedup();
        target_evidence.sort();
        target_evidence.dedup();

        let mut source_targets = std::collections::BTreeMap::new();
        let mut target_sources = std::collections::BTreeMap::new();
        for (source, target) in &mappings {
            if source_targets
                .insert(source.clone(), target.clone())
                .is_some_and(|existing| existing != *target)
            {
                return Err(mapping_resolution_error(
                    source,
                    "prepared split source maps to multiple targets",
                ));
            }
            if target_sources
                .insert(target.clone(), source.clone())
                .is_some_and(|existing| existing != *source)
            {
                return Err(mapping_resolution_error(
                    source,
                    "prepared split target maps from multiple sources",
                ));
            }
        }

        Self::from_authority(
            &self.direction,
            &self.target_root,
            &self.branch,
            self.source_repository.clone(),
            self.source_head.clone(),
            self.source_selected_heads.clone(),
            Some(target_repository),
            Some(target_head.clone()),
            Some(target_head),
            self.owner.clone(),
            self.ownership_snapshot.clone(),
            mappings,
            source_frontiers,
            target_frontiers,
            source_evidence,
            target_evidence,
        )
    }

    pub(crate) fn direction(&self) -> &str {
        &self.direction
    }

    pub(crate) fn target_root(&self) -> &Path {
        &self.target_root
    }

    pub(crate) fn branch(&self) -> &str {
        &self.branch
    }

    pub(crate) fn source_repository(&self) -> &str {
        &self.source_repository
    }

    pub(crate) fn source_head(&self) -> &str {
        &self.source_head
    }

    pub(crate) fn source_selected_head_count(&self) -> usize {
        self.source_selected_heads.len()
    }

    pub(crate) fn target_repository(&self) -> Option<&str> {
        self.target_repository.as_deref()
    }

    pub(crate) fn target_head(&self) -> Option<&str> {
        self.target_head.as_deref()
    }

    pub(crate) fn target_selected_head(&self) -> Option<&str> {
        self.target_selected_head.as_deref()
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) fn ownership_snapshot(&self) -> &str {
        &self.ownership_snapshot
    }

    pub(crate) fn transform_version(&self) -> u32 {
        self.transform_version
    }

    pub(crate) fn mappings(&self) -> &[(String, String)] {
        &self.mappings
    }

    pub(crate) fn source_evidence(&self) -> &[String] {
        &self.source_evidence
    }

    pub(crate) fn target_evidence(&self) -> &[String] {
        &self.target_evidence
    }

    pub(crate) fn source_frontier_count(&self) -> usize {
        self.source_frontiers.len()
    }

    pub(crate) fn target_frontier_count(&self) -> usize {
        self.target_frontiers.len()
    }

    pub(crate) fn same_binding(&self, other: &Self) -> bool {
        self.direction == other.direction
            && self.target_root == other.target_root
            && self.branch == other.branch
            && self.source_repository == other.source_repository
            && self.target_repository == other.target_repository
            && self.owner == other.owner
            && self.ownership_snapshot == other.ownership_snapshot
            && self.transform_version == other.transform_version
    }

    /// Revalidate the scalar refs bound by a prepared split result.
    ///
    /// Commit contents are immutable under their object IDs. Once a split has
    /// installed its authenticated object pack, final drift detection therefore
    /// needs to recapture refs, not reparse both complete histories and re-prove
    /// every mapping ancestry edge. Physical repository identity is checked by
    /// the workspace snapshot and prepared-effect store at their owning mutation
    /// boundaries.
    pub(crate) fn revalidate_split_repository_state(&self, source_repo: &Path, target_repo: &Path) -> RailResult<()> {
        self.revalidate_split_repository_state_with_projection(source_repo, target_repo, None)
    }

    /// Revalidate a checked split authority while one exact prepared effect
    /// may own the branch transition from its captured predecessor to its
    /// authenticated result.
    pub(crate) fn revalidate_split_repository_state_with_projection(
        &self,
        source_repo: &Path,
        target_repo: &Path,
        projection: Option<(Option<&str>, &str)>,
    ) -> RailResult<()> {
        if self.direction != "mono_to_remote" {
            return Err(RailError::message(
                "split repository revalidation received a non-split mapping authority",
            ));
        }
        if utils::canonicalize_existing(target_repo)? != self.target_root {
            return Err(RailError::message(
                "split target root changed during final repository revalidation",
            ));
        }
        MappingStore::reject_mapping_notes(source_repo, &self.owner)?;
        MappingStore::reject_mapping_notes(target_repo, &self.owner)?;
        let source = SystemGit::open(source_repo)?;
        let source_head = source.head_commit()?;
        if source_head != self.source_head || self.source_selected_heads.as_slice() != [source_head.as_str()] {
            return Err(RailError::message(
                "split source HEAD changed during final repository revalidation",
            ));
        }

        let target = SystemGit::open(target_repo)?;
        if target.current_branch()? != self.branch {
            return Err(RailError::message(
                "split target branch changed during final repository revalidation",
            ));
        }
        let ref_name = format!("refs/heads/{}", self.branch);
        let target_head = target.exact_branch_ref_oid(&ref_name)?;
        let captured_head_matches = target_head.as_deref() == self.target_head.as_deref()
            && target_head.as_deref() == self.target_selected_head.as_deref();
        let projected_head_matches = projection.is_some_and(|(expected, result)| {
            expected == self.target_head.as_deref()
                && expected == self.target_selected_head.as_deref()
                && target_head.as_deref() == Some(result)
        });
        if !captured_head_matches && !projected_head_matches {
            return Err(RailError::message(
                "split target HEAD changed during final repository revalidation",
            ));
        }
        Ok(())
    }
}

fn append_authority_frame(output: &mut Vec<u8>, label: &[u8], value: &[u8]) {
    output.extend_from_slice(&(label.len() as u64).to_be_bytes());
    output.extend_from_slice(label);
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn append_optional_authority_frame(output: &mut Vec<u8>, label: &[u8], value: Option<&[u8]>) {
    append_authority_frame(output, label, value.unwrap_or_default());
    output.push(u8::from(value.is_some()));
}

fn append_mapping_pairs(output: &mut Vec<u8>, label: &[u8], mappings: &[(String, String)]) {
    for (source, target) in mappings {
        let mut pair = Vec::with_capacity(source.len() + target.len() + 16);
        append_authority_frame(&mut pair, b"source", source.as_bytes());
        append_authority_frame(&mut pair, b"target", target.as_bytes());
        append_authority_frame(output, label, &pair);
    }
}

impl MappingStore {
    /// Create an empty store scoped to one split owner.
    pub fn new(owner: String) -> Self {
        Self {
            owner,
            expected_ownership_snapshot: None,
            repository_authority: None,
            mappings: FxHashMap::default(),
            reverse_mappings: FxHashMap::default(),
            source_frontiers: FxHashSet::default(),
            target_frontiers: FxHashSet::default(),
            explicit_pair_commits: FxHashSet::default(),
            source_evidence: FxHashSet::default(),
            target_evidence: FxHashSet::default(),
            source_evidence_pairs: FxHashSet::default(),
            target_evidence_pairs: FxHashSet::default(),
        }
    }

    /// Rebuild the command-local lookup view from an already validated current
    /// mapping snapshot.
    ///
    /// This is used only after Cargo-Rail installs the exact prepared split
    /// commit and revalidates its repository/ref binding. It avoids reparsing
    /// immutable histories merely to recover the in-memory maps that the same
    /// command derived before writing the commit.
    pub(crate) fn from_current_snapshot(snapshot: &MappingAuthoritySnapshot) -> RailResult<Self> {
        let target_repository = snapshot
            .target_repository
            .clone()
            .ok_or_else(|| RailError::message("current mapping snapshot has no target repository"))?;
        let mut store = Self::new(snapshot.owner.clone());
        store.expected_ownership_snapshot = Some(snapshot.ownership_snapshot.clone());
        let Some(target_head) = snapshot.target_head.clone() else {
            if snapshot.target_selected_head.is_some()
                || !snapshot.mappings.is_empty()
                || !snapshot.source_frontiers.is_empty()
                || !snapshot.target_frontiers.is_empty()
                || !snapshot.source_evidence.is_empty()
                || !snapshot.target_evidence.is_empty()
            {
                return Err(RailError::message(
                    "unborn current mapping snapshot contains history-derived authority",
                ));
            }
            return Ok(store);
        };
        let target_selected_head = snapshot
            .target_selected_head
            .clone()
            .ok_or_else(|| RailError::message("current mapping snapshot has no selected target HEAD"))?;
        store.repository_authority = Some(RepositoryAuthority {
            source_repository: snapshot.source_repository.clone(),
            source_head: snapshot.source_head.clone(),
            source_selected_heads: snapshot.source_selected_heads.clone(),
            target_repository,
            target_head,
            target_selected_head,
            ownership_snapshot: snapshot.ownership_snapshot.clone(),
        });
        for (source, target) in &snapshot.mappings {
            store.record_mapping(source, target)?;
        }
        store.source_frontiers.extend(snapshot.source_frontiers.iter().cloned());
        store.target_frontiers.extend(snapshot.target_frontiers.iter().cloned());
        restore_snapshot_evidence(
            &snapshot.source_evidence,
            &mut store.source_evidence,
            &mut store.source_evidence_pairs,
        )?;
        restore_snapshot_evidence(
            &snapshot.target_evidence,
            &mut store.target_evidence,
            &mut store.target_evidence_pairs,
        )?;

        let rebound = store.mapping_authority_snapshot(&snapshot.direction, &snapshot.target_root, &snapshot.branch)?;
        if &rebound != snapshot {
            return Err(RailError::message(
                "current mapping snapshot could not be reconstructed without semantic drift",
            ));
        }
        Ok(store)
    }

    /// Load mappings from ordinary history in one Git log stream.
    pub fn load_history(
        &mut self,
        repo_path: &Path,
        side: HistorySide,
        expected_source_repository: &str,
    ) -> RailResult<()> {
        validate_repository_identity(expected_source_repository)?;
        let git = SystemGit::open(repo_path)?;
        Self::reject_mapping_notes(repo_path, &self.owner)?;
        let commits = git.ordinary_commit_history()?;

        self.load_current_commits(repo_path, side, expected_source_repository, &commits)
    }

    fn load_current_commits(
        &mut self,
        repo_path: &Path,
        side: HistorySide,
        expected_source_repository: &str,
        commits: &[crate::git::CommitInfo],
    ) -> RailResult<()> {
        for commit in commits {
            for value in origin_trailer_values(&commit.message) {
                let parsed = ParsedTrailer::parse(value).map_err(|error| RailError::with_help(
                    format!("unsupported Cargo-Rail origin in '{}' at {}: {error}", repo_path.display(), commit.sha),
                    "preserve the history; continue this relationship with its originating executable (release version unknown), or use a separately reviewed fresh target",
                ))?;
                self.record_current_trailer(repo_path, parsed, &commit.sha, side, expected_source_repository)?;
            }
        }
        Ok(())
    }

    fn load_history_at(
        &mut self,
        repo_path: &Path,
        side: HistorySide,
        expected_source_repository: &str,
        revision: &str,
    ) -> RailResult<()> {
        validate_repository_identity(expected_source_repository)?;
        let commits = SystemGit::open(repo_path)?.ordinary_commit_history_at(revision)?;
        self.load_current_commits(repo_path, side, expected_source_repository, &commits)
    }

    /// Reject reserved mapping notes without interpreting their contents.
    pub(crate) fn reject_mapping_notes(repo_path: &Path, owner: &str) -> RailResult<()> {
        let output = git_cmd_for_path(repo_path)
            .args(["show-ref", "--verify", "--quiet", &format!("refs/notes/rail/{owner}")])
            .output()
            .context("Failed to inspect mapping authority notes")?;
        match output.status.code() {
            Some(1) => Ok(()),
            Some(0) => Err(RailError::with_help(
                format!("unsupported Cargo-Rail mapping notes in '{}'", repo_path.display()),
                "preserve the notes and history; continue this relationship with its originating executable (release version unknown), or use a separately reviewed fresh target",
            )),
            _ => Err(RailError::message("failed to inspect Cargo-Rail mapping notes")),
        }
    }

    /// Validate current evidence against the selected repository histories.
    fn validate_evidence(
        &mut self,
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        selected_target_head: &str,
        selected_source_heads: &[String],
    ) -> RailResult<RepositoryAuthority> {
        if source_context.owner != self.owner {
            return Err(RailError::message(
                "mapping owner does not match the current split owner",
            ));
        }
        validate_token("ownership snapshot", &source_context.ownership_snapshot)?;
        validate_repository_identity(expected_target_repository)?;
        if repository_identity(source_repo)? != source_context.source_repository {
            return Err(RailError::message(
                "mapping source repository identity changed during validation",
            ));
        }
        if repository_identity(target_repo)? != expected_target_repository {
            return Err(RailError::message(
                "mapping target repository identity changed during validation",
            ));
        }

        let source_head = SystemGit::open(source_repo)?.head_commit()?;
        let target_head = SystemGit::open(target_repo)?.head_commit()?;
        let selected_target_head = normalize_object_id("selected target HEAD", selected_target_head)?;
        let mut source_evidence_by_commit = FxHashMap::default();
        for (source, target) in &self.source_evidence_pairs {
            if !is_ancestor_of_any(source_repo, source, selected_source_heads)?
                || !is_ancestor(target_repo, target, &selected_target_head)?
            {
                return Err(mapping_resolution_error(
                    source,
                    "source-history evidence endpoints are outside the selected repository histories",
                ));
            }
            if source_evidence_by_commit
                .insert(source.clone(), target.clone())
                .is_some_and(|existing| existing != *target)
            {
                return Err(mapping_resolution_error(
                    source,
                    "source-history evidence declares multiple target origins",
                ));
            }
        }
        let mut target_evidence_by_commit = FxHashMap::default();
        for (source, target) in &self.target_evidence_pairs {
            if !is_ancestor_of_any(source_repo, source, selected_source_heads)?
                || !is_ancestor(target_repo, target, &selected_target_head)?
            {
                return Err(mapping_resolution_error(
                    target,
                    "target-history evidence endpoints are outside the selected repository histories",
                ));
            }
            if target_evidence_by_commit
                .insert(target.clone(), source.clone())
                .is_some_and(|existing| existing != *source)
            {
                return Err(mapping_resolution_error(
                    target,
                    "target-history evidence declares multiple source origins",
                ));
            }
        }
        for commit in &self.explicit_pair_commits {
            if !is_ancestor(target_repo, commit, &selected_target_head)? {
                return Err(mapping_resolution_error(
                    commit,
                    "the explicit-pair trailer commit is not an ancestor of the selected target HEAD",
                ));
            }
        }
        for (source, target) in &self.mappings {
            if !is_ancestor_of_any(source_repo, source, selected_source_heads)? {
                return Err(mapping_resolution_error(
                    source,
                    "the source endpoint is not an ancestor of any selected source history head",
                ));
            }
            if !is_ancestor(target_repo, target, &selected_target_head)? {
                return Err(mapping_resolution_error(
                    source,
                    &format!(
                        "target '{}' is not an ancestor of the configured target branch head",
                        target
                    ),
                ));
            }
        }
        self.target_evidence.extend(self.explicit_pair_commits.iter().cloned());
        Ok(RepositoryAuthority {
            source_repository: source_context.source_repository.clone(),
            source_head,
            source_selected_heads: selected_source_heads.to_vec(),
            target_repository: expected_target_repository.to_string(),
            target_head,
            target_selected_head: selected_target_head,
            ownership_snapshot: source_context.ownership_snapshot.clone(),
        })
    }

    pub(crate) fn capture_authority(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
    ) -> RailResult<(Self, MappingAuthoritySnapshot)> {
        let selected_target_head = SystemGit::open(target_repo)?.head_commit()?;
        Self::capture_authority_at(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            target_root,
            branch,
            direction,
            &selected_target_head,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the capture boundary keeps selected history and repository authority explicit"
    )]
    pub(crate) fn capture_authority_at(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        selected_target_head: &str,
    ) -> RailResult<(Self, MappingAuthoritySnapshot)> {
        let canonical_target = utils::canonicalize_existing(target_repo)?;
        if canonical_target != target_root {
            return Err(RailError::message(
                "mapping target root changed during authority capture",
            ));
        }
        let source_git = SystemGit::open(source_repo)?;
        let target_git = SystemGit::open(target_repo)?;
        let source_repository_before = repository_identity(source_repo)?;
        let target_repository_before = repository_identity(target_repo)?;
        let source_head_before = source_git.head_commit()?;
        let target_head_before = target_git.head_commit()?;
        let actual_branch = target_git.current_branch()?;
        if actual_branch != branch {
            return Err(RailError::with_help(
                format!(
                    "mapping target is on branch '{}', but configuration requires '{}'",
                    actual_branch, branch
                ),
                format!("switch the target repository to '{}' and retry", branch),
            ));
        }
        let selected_target_head = normalize_object_id("selected target HEAD", selected_target_head)?;
        target_git.get_commit(&selected_target_head)?;
        let selected_source_heads = selected_source_heads(source_repo)?;
        let store = Self::capture_evidence_at(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            &selected_target_head,
            &selected_source_heads,
        )?;
        let authority = store
            .repository_authority
            .as_ref()
            .ok_or_else(|| RailError::message("mapping authority capture returned no repository binding"))?;
        let branch_after = target_git.current_branch()?;
        if source_repository_before != authority.source_repository
            || target_repository_before != authority.target_repository
            || source_head_before != authority.source_head
            || selected_source_heads != authority.source_selected_heads
            || target_head_before != authority.target_head
            || selected_target_head != authority.target_selected_head
            || branch_after != actual_branch
        {
            return Err(mapping_authority_drift_error());
        }
        let snapshot = store.mapping_authority_snapshot(direction, target_root, branch)?;
        Ok((store, snapshot))
    }

    /// Capture mapping authority from an explicitly selected source commit.
    ///
    /// Remote-to-monorepo sync writes to its deterministic review branch. A
    /// check performed while another branch is checked out must therefore
    /// read the review branch's ordinary-history evidence without switching
    /// the worktree or weakening the source binding.
    #[expect(
        clippy::too_many_arguments,
        reason = "selected source history is explicit at the capture boundary"
    )]
    pub(crate) fn capture_authority_at_source(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        selected_source_head: &str,
        selected_target_head: Option<&str>,
    ) -> RailResult<(Self, MappingAuthoritySnapshot)> {
        let canonical_target = utils::canonicalize_existing(target_repo)?;
        if canonical_target != target_root {
            return Err(RailError::message(
                "mapping target root changed during selected-source authority capture",
            ));
        }
        let source_git = SystemGit::open(source_repo)?;
        let target_git = SystemGit::open(target_repo)?;
        let source_repository_before = repository_identity(source_repo)?;
        let target_repository_before = repository_identity(target_repo)?;
        let source_head_before = source_git.head_commit()?;
        let target_head_before = target_git.head_commit()?;
        let actual_branch = target_git.current_branch()?;
        if actual_branch != branch {
            return Err(RailError::with_help(
                format!(
                    "mapping target is on branch '{}', but configuration requires '{}'",
                    actual_branch, branch
                ),
                format!("switch the target repository to '{}' and retry", branch),
            ));
        }

        let selected_source_head = normalize_object_id("selected source HEAD", selected_source_head)?;
        source_git.get_commit(&selected_source_head)?;
        let selected_target_head = selected_target_head
            .map(|head| normalize_object_id("selected target HEAD", head))
            .transpose()?
            .unwrap_or_else(|| target_head_before.clone());
        target_git.get_commit(&selected_target_head)?;
        let selected_source_heads = vec![selected_source_head.clone()];
        let mut store = Self::capture_evidence_at(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            &selected_target_head,
            &selected_source_heads,
        )?;
        let authority = store
            .repository_authority
            .as_mut()
            .ok_or_else(|| RailError::message("mapping authority capture returned no repository binding"))?;
        authority.source_head = selected_source_head;

        if source_repository_before != authority.source_repository
            || target_repository_before != authority.target_repository
            || source_head_before != source_git.head_commit()?
            || target_head_before != authority.target_head
            || selected_target_head != authority.target_selected_head
            || target_git.current_branch()? != actual_branch
        {
            return Err(mapping_authority_drift_error());
        }
        let snapshot = store.mapping_authority_snapshot(direction, target_root, branch)?;
        Ok((store, snapshot))
    }

    /// Reconstruct the exact mapping authority before a prepared old/result ref transition.
    #[expect(
        clippy::too_many_arguments,
        reason = "prepared target capture binds the exact journaled transition"
    )]
    pub(crate) fn capture_prepared_authority_at(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        expected_target_head: Option<&str>,
        result_target_head: &str,
    ) -> RailResult<(Self, MappingAuthoritySnapshot)> {
        let canonical_target = utils::canonicalize_existing(target_repo)?;
        if canonical_target != target_root {
            return Err(RailError::message(
                "prepared mapping target root changed during authority capture",
            ));
        }
        let target = SystemGit::open(target_repo)?;
        let actual_branch = target.current_branch()?;
        if actual_branch != branch {
            return Err(RailError::with_help(
                format!("prepared mapping target is on branch '{actual_branch}', not '{branch}'"),
                format!("restore the exact prepared branch '{branch}' before retrying"),
            ));
        }
        let expected_target_head = expected_target_head
            .map(|head| normalize_object_id("prepared old target HEAD", head))
            .transpose()?;
        let result_target_head = normalize_object_id("prepared result target HEAD", result_target_head)?;
        let ref_name = format!("refs/heads/{branch}");
        let current_target_head = target.exact_branch_ref_oid(&ref_name)?;
        if current_target_head.as_deref() != expected_target_head.as_deref()
            && current_target_head.as_deref() != Some(result_target_head.as_str())
        {
            return Err(RailError::with_help(
                "prepared mapping target branch is in a third ref state",
                "restore the exact journaled old or result ref before retrying",
            ));
        }

        let Some(expected_target_head) = expected_target_head else {
            let mut store = Self::new(source_context.owner.clone());
            store.expected_ownership_snapshot = Some(source_context.ownership_snapshot.clone());
            let snapshot = MappingAuthoritySnapshot::empty_initialized_bound(
                source_repo,
                source_context,
                expected_target_repository,
                target_root,
                branch,
                direction,
            )?;
            return Ok((store, snapshot));
        };
        if repository_identity(target_repo)? != expected_target_repository {
            return Err(RailError::message(
                "prepared mapping target repository identity changed",
            ));
        }
        let selected_source_heads = selected_source_heads(source_repo)?;
        let mut store = Self::capture_evidence_at(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            &expected_target_head,
            &selected_source_heads,
        )?;
        let authority = store
            .repository_authority
            .as_mut()
            .ok_or_else(|| RailError::message("prepared mapping capture returned no repository authority"))?;
        if current_target_head.as_deref() != Some(authority.target_head.as_str())
            || authority.target_selected_head != expected_target_head
        {
            return Err(mapping_authority_drift_error());
        }
        authority.target_head = expected_target_head;
        let snapshot = store.mapping_authority_snapshot(direction, target_root, branch)?;
        Ok((store, snapshot))
    }

    /// Reconstruct mapping authority before a prepared monorepo ref transition.
    #[expect(
        clippy::too_many_arguments,
        reason = "prepared source capture binds the exact journaled transition"
    )]
    pub(crate) fn capture_prepared_source_authority_at(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        source_ref_name: &str,
        expected_source_head: &str,
        result_source_head: &str,
        selected_target_head: &str,
    ) -> RailResult<(Self, MappingAuthoritySnapshot)> {
        let source = SystemGit::open(source_repo)?;
        let expected_source_head = normalize_object_id("prepared old source HEAD", expected_source_head)?;
        let result_source_head = normalize_object_id("prepared result source HEAD", result_source_head)?;
        let current_source_head = source.exact_branch_ref_oid(source_ref_name)?;
        if current_source_head.as_deref() != Some(expected_source_head.as_str())
            && current_source_head.as_deref() != Some(result_source_head.as_str())
        {
            return Err(RailError::with_help(
                "prepared mapping source branch is in a third ref state",
                "restore the exact journaled old or result source ref before retrying",
            ));
        }
        let selected_source_heads = vec![expected_source_head.clone()];
        let mut store = Self::capture_evidence_at(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            selected_target_head,
            &selected_source_heads,
        )?;
        let authority = store
            .repository_authority
            .as_mut()
            .ok_or_else(|| RailError::message("prepared source mapping capture returned no repository authority"))?;
        if current_source_head.as_deref() != Some(authority.source_head.as_str())
            || authority.source_selected_heads != selected_source_heads
        {
            return Err(mapping_authority_drift_error());
        }
        authority.source_head = expected_source_head;
        let snapshot = store.mapping_authority_snapshot(direction, target_root, branch)?;
        Ok((store, snapshot))
    }

    pub(crate) fn capture_evidence(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
    ) -> RailResult<Self> {
        let selected_target_head = SystemGit::open(target_repo)?.head_commit()?;
        let selected_source_heads = selected_source_heads(source_repo)?;
        Self::capture_evidence_at(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            &selected_target_head,
            &selected_source_heads,
        )
    }

    pub(crate) fn capture_evidence_at(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        selected_target_head: &str,
        selected_source_heads: &[String],
    ) -> RailResult<Self> {
        let mut evidence = Self::new(source_context.owner.clone());
        evidence.expected_ownership_snapshot = Some(source_context.ownership_snapshot.clone());
        for selected_source_head in selected_source_heads {
            evidence.load_history_at(
                source_repo,
                HistorySide::Source,
                expected_target_repository,
                selected_source_head,
            )?;
        }
        evidence.load_history_at(
            target_repo,
            HistorySide::Target,
            source_context.source_repository(),
            selected_target_head,
        )?;
        Self::reject_mapping_notes(source_repo, &evidence.owner)?;
        Self::reject_mapping_notes(target_repo, &evidence.owner)?;
        evidence.repository_authority = Some(evidence.validate_evidence(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            selected_target_head,
            selected_source_heads,
        )?);
        Ok(evidence)
    }

    fn record_current_trailer(
        &mut self,
        repo_path: &Path,
        parsed: ParsedTrailer,
        containing_commit: &str,
        side: HistorySide,
        expected_source_repository: &str,
    ) -> RailResult<()> {
        if parsed.owner != self.owner || parsed.source_repository != expected_source_repository {
            return Ok(());
        }
        if parsed.transform_version != TRANSFORM_VERSION {
            return Err(RailError::message(format!(
                "unsupported Rail-Origin transform version {} for '{}'",
                parsed.transform_version, self.owner
            )));
        }
        validate_token("ownership snapshot", &parsed.ownership_snapshot)?;
        if self
            .expected_ownership_snapshot
            .as_ref()
            .is_some_and(|expected| expected != &parsed.ownership_snapshot)
        {
            return Err(RailError::with_help(
                format!(
                    "Rail-Origin ownership snapshot '{}' does not match current snapshot '{}' for '{}'",
                    parsed.ownership_snapshot,
                    self.expected_ownership_snapshot.as_deref().unwrap_or_default(),
                    self.owner
                ),
                "re-run split or sync from provenance created for the current ownership configuration; stale frontiers cannot authorize expanded paths",
            ));
        }
        if !parsed.mapping {
            match (parsed.evidence_commit, parsed.evidence_side, side) {
                (None, None, HistorySide::Source) => {
                    self.source_evidence.insert(containing_commit.to_string());
                    self.source_evidence_pairs
                        .insert((containing_commit.to_string(), parsed.source_commit));
                }
                (None, None, HistorySide::Target) => {
                    self.target_evidence.insert(containing_commit.to_string());
                    self.target_evidence_pairs
                        .insert((parsed.source_commit, containing_commit.to_string()));
                }
                (Some(endpoint), Some(HistorySide::Target), HistorySide::Target) => {
                    if !is_ancestor(repo_path, &endpoint, containing_commit)? {
                        return Err(mapping_resolution_error(
                            &endpoint,
                            "explicit target evidence is not an ancestor of its containing commit",
                        ));
                    }
                    self.target_evidence.insert(endpoint.clone());
                    self.target_evidence.insert(containing_commit.to_string());
                    self.target_evidence_pairs.insert((parsed.source_commit, endpoint));
                    self.explicit_pair_commits.insert(containing_commit.to_string());
                }
                _ => {
                    return Err(mapping_resolution_error(
                        containing_commit,
                        "explicit Rail-Origin evidence has an invalid history side",
                    ));
                }
            }
            return Ok(());
        }
        let (mapping, frontier) = match (side, parsed.target_commit) {
            (HistorySide::Source, Some(_)) => {
                return Err(mapping_resolution_error(
                    containing_commit,
                    "an explicit-pair trailer is valid only in target history",
                ));
            }
            (HistorySide::Target, Some(target_commit)) => {
                if !is_ancestor(repo_path, &target_commit, containing_commit)? {
                    return Err(mapping_resolution_error(
                        &parsed.source_commit,
                        &format!(
                            "explicit-pair target '{}' is not an ancestor of its trailer commit",
                            target_commit
                        ),
                    ));
                }
                self.explicit_pair_commits.insert(containing_commit.to_string());
                self.target_evidence.insert(containing_commit.to_string());
                (
                    CommitMapping::new(&parsed.source_commit, &target_commit)?,
                    parsed.frontier,
                )
            }
            (HistorySide::Source, None) => (
                CommitMapping::new(containing_commit, &parsed.source_commit)?,
                Some(MappingFrontier::Target),
            ),
            (HistorySide::Target, None) => (
                CommitMapping::new(&parsed.source_commit, containing_commit)?,
                Some(MappingFrontier::Source),
            ),
        };
        self.record_mapping(&mapping.source, &mapping.target)?;
        if let Some(frontier) = frontier {
            self.record_frontier(&mapping, frontier);
        }
        Ok(())
    }

    fn record_frontier(&mut self, mapping: &CommitMapping, frontier: MappingFrontier) {
        if frontier.proves_source() {
            self.source_frontiers.insert(mapping.source.clone());
        }
        if frontier.proves_target() {
            self.target_frontiers.insert(mapping.target.clone());
        }
    }

    pub(crate) fn mapping_authority_snapshot(
        &self,
        direction: &str,
        target_root: &Path,
        branch: &str,
    ) -> RailResult<MappingAuthoritySnapshot> {
        let authority = self
            .repository_authority
            .as_ref()
            .ok_or_else(|| RailError::message("mapping authority snapshot requires validated repository evidence"))?;
        let mut mappings = self
            .mappings
            .iter()
            .map(|(source, target)| (source.clone(), target.clone()))
            .collect::<Vec<_>>();
        mappings.sort();
        let mut source_frontiers = self.source_frontiers.iter().cloned().collect::<Vec<_>>();
        source_frontiers.sort();
        let mut target_frontiers = self.target_frontiers.iter().cloned().collect::<Vec<_>>();
        target_frontiers.sort();
        let mut source_evidence = self
            .source_evidence
            .iter()
            .map(|commit| format!("endpoint:{commit}"))
            .chain(
                self.source_evidence_pairs
                    .iter()
                    .map(|(source, target)| format!("pair:{source}:{target}")),
            )
            .collect::<Vec<_>>();
        source_evidence.sort();
        let mut target_evidence = self
            .target_evidence
            .iter()
            .map(|commit| format!("endpoint:{commit}"))
            .chain(
                self.target_evidence_pairs
                    .iter()
                    .map(|(source, target)| format!("pair:{source}:{target}")),
            )
            .collect::<Vec<_>>();
        target_evidence.sort();
        MappingAuthoritySnapshot::from_authority(
            direction,
            target_root,
            branch,
            authority.source_repository.clone(),
            authority.source_head.clone(),
            authority.source_selected_heads.clone(),
            Some(authority.target_repository.clone()),
            Some(authority.target_head.clone()),
            Some(authority.target_selected_head.clone()),
            self.owner.clone(),
            authority.ownership_snapshot.clone(),
            mappings,
            source_frontiers,
            target_frontiers,
            source_evidence,
            target_evidence,
        )
    }

    pub(crate) fn update_authority_heads(
        &mut self,
        source_head: Option<&str>,
        target_head: Option<&str>,
    ) -> RailResult<()> {
        let authority = self
            .repository_authority
            .as_mut()
            .ok_or_else(|| RailError::message("mapping authority heads require validated repository evidence"))?;
        if let Some(source_head) = source_head {
            authority.source_head = normalize_object_id("source HEAD", source_head)?;
            authority.source_selected_heads = vec![authority.source_head.clone()];
        }
        if let Some(target_head) = target_head {
            authority.target_head = normalize_object_id("target HEAD", target_head)?;
            authority.target_selected_head.clone_from(&authority.target_head);
        }
        Ok(())
    }

    /// Record one proven source-to-target mapping.
    pub fn record_mapping(&mut self, from_sha: &str, to_sha: &str) -> RailResult<()> {
        let source = normalize_object_id("source", from_sha)?;
        let target = normalize_object_id("target", to_sha)?;
        if let Some(existing) = self.mappings.get(&source)
            && existing != &target
        {
            return Err(mapping_resolution_error(
                &source,
                &format!("it maps to both '{}' and '{}'", existing, target),
            ));
        }
        if let Some(existing) = self.reverse_mappings.get(&target)
            && existing != &source
        {
            return Err(mapping_resolution_error(
                &source,
                &format!("target '{}' is already mapped from source '{}'", target, existing),
            ));
        }
        self.reverse_mappings.insert(target.clone(), source.clone());
        self.mappings.insert(source, target);
        Ok(())
    }

    pub(crate) fn record_source_frontier_mapping(&mut self, source: &str, target: &str) -> RailResult<()> {
        let mapping = CommitMapping::new(source, target)?;
        self.record_mapping(&mapping.source, &mapping.target)?;
        self.record_frontier(&mapping, MappingFrontier::Source);
        Ok(())
    }

    pub(crate) fn record_target_frontier_mapping(&mut self, source: &str, target: &str) -> RailResult<()> {
        let mapping = CommitMapping::new(source, target)?;
        self.record_mapping(&mapping.source, &mapping.target)?;
        self.record_frontier(&mapping, MappingFrontier::Target);
        Ok(())
    }

    /// Return the mapped target commit, when known.
    pub fn get_mapping(&self, sha: &str) -> Option<String> {
        let sha = normalize_object_id("source", sha).ok()?;
        self.mappings.get(&sha).cloned()
    }

    /// Return the source commit mapped to a target commit, when known.
    pub fn get_reverse_mapping(&self, sha: &str) -> Option<String> {
        let sha = normalize_object_id("target", sha).ok()?;
        self.reverse_mappings.get(&sha).cloned()
    }

    pub(crate) fn source_frontier_commits(&self) -> Vec<&str> {
        let mut commits = self.source_frontiers.iter().map(String::as_str).collect::<Vec<_>>();
        commits.sort_unstable();
        commits
    }

    pub(crate) fn target_frontier_commits(&self) -> Vec<&str> {
        let mut commits = self.target_frontiers.iter().map(String::as_str).collect::<Vec<_>>();
        commits.sort_unstable();
        commits
    }

    /// Exact pairs whose evidence proves neither directional
    /// ancestry frontier. These endpoints may suppress exact replay, but an
    /// unmatched relevant ancestor below either endpoint is ambiguous and
    /// must fail closed rather than be reordered after the mapped endpoint.
    pub(crate) fn unproven_mapping_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = self
            .mappings
            .iter()
            .filter(|(source, target)| {
                !self.source_frontiers.contains(*source) && !self.target_frontiers.contains(*target)
            })
            .map(|(source, target)| (source.clone(), target.clone()))
            .collect::<Vec<_>>();
        pairs.sort();
        pairs
    }

    /// Whether a source commit has a target mapping.
    pub fn has_mapping(&self, sha: &str) -> bool {
        normalize_object_id("source", sha)
            .is_ok_and(|sha| self.mappings.contains_key(&sha) || self.source_evidence.contains(&sha))
    }

    /// Whether a target commit has a source mapping.
    pub fn has_reverse_mapping(&self, sha: &str) -> bool {
        normalize_object_id("target", sha)
            .is_ok_and(|sha| self.reverse_mappings.contains_key(&sha) || self.target_evidence.contains(&sha))
    }

    /// Number of mappings recovered from all accepted evidence.
    pub fn count(&self) -> usize {
        self.mappings.len()
    }

    fn owns_target_commit(&self, commit: &str) -> bool {
        normalize_object_id("target publication commit", commit)
            .is_ok_and(|commit| self.reverse_mappings.contains_key(&commit) || self.target_evidence.contains(&commit))
    }
}

fn restore_snapshot_evidence(
    encoded: &[String],
    endpoints: &mut FxHashSet<String>,
    pairs: &mut FxHashSet<(String, String)>,
) -> RailResult<()> {
    for evidence in encoded {
        if let Some(endpoint) = evidence.strip_prefix("endpoint:") {
            endpoints.insert(normalize_object_id("mapping evidence endpoint", endpoint)?);
            continue;
        }
        let Some(pair) = evidence.strip_prefix("pair:") else {
            return Err(RailError::message(
                "mapping snapshot contains an unsupported evidence projection",
            ));
        };
        let (source, target) = pair
            .split_once(':')
            .ok_or_else(|| RailError::message("mapping snapshot contains a malformed evidence pair"))?;
        pairs.insert((
            normalize_object_id("mapping evidence source", source)?,
            normalize_object_id("mapping evidence target", target)?,
        ));
    }
    Ok(())
}

pub(crate) fn is_ancestor(repo_path: &Path, ancestor: &str, descendant: &str) -> RailResult<bool> {
    validate_object_id("ancestor", ancestor)?;
    validate_object_id("descendant", descendant)?;
    let output = git_cmd_for_path(repo_path)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .context("Failed to validate mapping ancestry")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(RailError::Git(GitError::CommandFailed {
            command: "git merge-base --is-ancestor <ancestor> <descendant>".to_string(),
            stderr: git_command_diagnostics(&output.stdout, &output.stderr),
        })),
    }
}

fn is_ancestor_of_any(repo_path: &Path, ancestor: &str, descendants: &[String]) -> RailResult<bool> {
    for descendant in descendants {
        if is_ancestor(repo_path, ancestor, descendant)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn selected_source_heads(source_repo: &Path) -> RailResult<Vec<String>> {
    let git = SystemGit::open(source_repo)?;
    Ok(vec![git.head_commit()?])
}

fn revision_range(repo_path: &Path, from: Option<&str>, to: &str) -> RailResult<Vec<String>> {
    validate_object_id("local publication head", to)?;
    let revision = if let Some(from) = from {
        validate_object_id("remote publication head", from)?;
        format!("{from}..{to}")
    } else {
        to.to_string()
    };
    let output = git_cmd_for_path(repo_path)
        .args(["rev-list", "--reverse", &revision])
        .output()
        .context("Failed to enumerate local split publication range")?;
    if !output.status.success() {
        return Err(RailError::Git(GitError::CommandFailed {
            command: "git rev-list --reverse <remote>..<local>".to_string(),
            stderr: git_command_diagnostics(&output.stdout, &output.stderr),
        }));
    }
    String::from_utf8(output.stdout)?
        .lines()
        .map(|commit| normalize_object_id("publication commit", commit.trim()))
        .collect()
}

fn origin_trailer_values(message: &str) -> Vec<&str> {
    let lines = message.lines().collect::<Vec<_>>();
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let start = lines[..end]
        .iter()
        .rposition(|line| line.trim().is_empty())
        .map_or(0, |index| index + 1);
    lines[start..end]
        .iter()
        .filter_map(|line| line.strip_prefix(TRAILER_PREFIX))
        .collect()
}

fn validate_object_id(field: &str, value: &str) -> RailResult<()> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(RailError::message(format!(
        "invalid {} commit object ID '{}': expected a 40- or 64-digit hexadecimal Git object ID",
        field, value
    )))
}

fn normalize_object_id(field: &str, value: &str) -> RailResult<String> {
    validate_object_id(field, value)?;
    Ok(value.to_ascii_lowercase())
}

fn validate_repository_identity(identity: &str) -> RailResult<()> {
    let digest = identity
        .strip_prefix("sha256-")
        .ok_or_else(|| RailError::message("repository identity must use sha256"))?;
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(RailError::message("repository identity has an invalid SHA-256 digest"))
    }
}

fn validate_token(field: &str, value: &str) -> RailResult<()> {
    if !value.is_empty() && !value.chars().any(char::is_whitespace) && !value.contains(['\0', '\n', '\r']) {
        Ok(())
    } else {
        Err(RailError::message(format!("{} must be one non-empty token", field)))
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn decode_hex(value: &str) -> RailResult<String> {
    if value.is_empty() || !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RailError::message("Rail-Origin owner has invalid hexadecimal encoding"));
    }
    let bytes = value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|_| RailError::message("Rail-Origin owner is not UTF-8"))?;
            u8::from_str_radix(pair, 16).map_err(|_| RailError::message("Rail-Origin owner has invalid hex"))
        })
        .collect::<RailResult<Vec<_>>>()?;
    String::from_utf8(bytes).map_err(|_| RailError::message("Rail-Origin owner is not UTF-8"))
}

fn mapping_resolution_error(source: &str, reason: &str) -> RailError {
    RailError::with_help(
        format!(
            "invalid or divergent mapping for source commit '{}': {}",
            source, reason
        ),
        "inspect ordinary Rail-Origin trailers, then choose one target commit; cargo-rail never merges divergent mappings automatically",
    )
}

fn mapping_authority_drift_error() -> RailError {
    RailError::with_help(
        "mapping evidence changed after it was checked",
        "retry after the ordinary histories and refs/notes/rail mapping refs stop changing",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explicit_pair_trailer(context: &OriginContext, source_commit: &str, target_commit: &str) -> RailResult<String> {
        let source_commit = normalize_object_id("source", source_commit)?;
        let target_commit = normalize_object_id("target", target_commit)?;
        Ok(format!(
            "{TRAILER_PREFIX}{TRAILER_SCHEMA} source={} commit={} owner={} snapshot={} transform={TRANSFORM_VERSION} target={}",
            context.source_repository,
            source_commit,
            encode_hex(context.owner.as_bytes()),
            context.ownership_snapshot,
            target_commit,
        ))
    }

    fn oid(digit: char) -> String {
        std::iter::repeat_n(digit, 40).collect()
    }

    fn repository_id(digit: char) -> String {
        format!("sha256-{}", std::iter::repeat_n(digit, 64).collect::<String>())
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = git_cmd_for_path(repo).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn repository() -> tempfile::TempDir {
        let repo = tempfile::TempDir::new().unwrap();
        git(repo.path(), &["init", "-b", "main"]);
        git(repo.path(), &["config", "user.name", "Test User"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        git(repo.path(), &["config", "commit.gpgsign", "false"]);
        repo
    }

    fn commit(repo: &Path, message: &str) -> String {
        git(repo, &["commit", "--allow-empty", "-m", message]);
        git(repo, &["rev-parse", "HEAD"])
    }

    #[test]
    fn final_split_revalidation_rejects_notes_added_after_capture() {
        let source = repository();
        let source_head = commit(source.path(), "source");
        let target = repository();
        let origin = OriginContext::discover(source.path(), "demo", "policy").unwrap();
        let target_head = commit(
            target.path(),
            &format!("split\n\n{}", origin.trailer(&source_head).unwrap()),
        );
        let target_root = utils::canonicalize_existing(target.path()).unwrap();
        let (_, captured) = MappingStore::capture_authority(
            source.path(),
            target.path(),
            &origin,
            &repository_identity(target.path()).unwrap(),
            &target_root,
            "main",
            "mono_to_remote",
        )
        .unwrap();
        captured
            .revalidate_split_repository_state(source.path(), target.path())
            .unwrap();
        git(
            target.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/demo",
                "add",
                "-m",
                "unsupported authority",
                &target_head,
            ],
        );
        let error = captured
            .revalidate_split_repository_state(source.path(), target.path())
            .unwrap_err();
        assert!(
            error.to_string().contains("unsupported Cargo-Rail mapping notes"),
            "{error}"
        );
        assert_eq!(git(source.path(), &["rev-parse", "HEAD"]), source_head);
        assert_eq!(git(target.path(), &["rev-parse", "HEAD"]), target_head);
    }

    #[test]
    fn origin_trailer_round_trips_required_identity() {
        let context = OriginContext::new(repository_id('a'), "demo", "v1-sha256-snapshot").unwrap();
        let source = oid('b');
        let trailer = context.trailer(&source).unwrap();
        let parsed = ParsedTrailer::parse(trailer.strip_prefix(TRAILER_PREFIX).unwrap()).unwrap();
        assert_eq!(
            parsed,
            ParsedTrailer {
                source_repository: repository_id('a'),
                source_commit: source,
                owner: "demo".to_string(),
                ownership_snapshot: "v1-sha256-snapshot".to_string(),
                transform_version: TRANSFORM_VERSION,
                mapping: true,
                target_commit: None,
                frontier: None,
                evidence_commit: None,
                evidence_side: None,
            }
        );
    }

    #[test]
    fn unannotated_explicit_pair_is_exact_evidence_without_ancestry_authority() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let target = commit(target_repo.path(), "target");
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let trailer = explicit_pair_trailer(&source_context, &source, &target).unwrap();
        commit(target_repo.path(), &format!("persistent pair\n\n{trailer}"));

        let mut mappings = MappingStore::new("demo".to_string());
        mappings
            .load_history(
                target_repo.path(),
                HistorySide::Target,
                source_context.source_repository(),
            )
            .unwrap();

        assert_eq!(mappings.get_mapping(&source), Some(target));
        assert!(mappings.source_frontier_commits().is_empty());
        assert!(mappings.target_frontier_commits().is_empty());
    }

    #[test]
    fn current_explicit_pair_enforces_transform_direction_and_ancestry() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();

        let transform_repo = repository();
        let transform_target = commit(transform_repo.path(), "target");
        let invalid_transform = explicit_pair_trailer(&source_context, &source, &transform_target)
            .unwrap()
            .replace(" transform=1 ", " transform=2 ");
        commit(transform_repo.path(), &format!("explicit pair\n\n{invalid_transform}"));
        let mut transform_mappings = MappingStore::new("demo".to_string());
        let transform_error = transform_mappings
            .load_history(
                transform_repo.path(),
                HistorySide::Target,
                source_context.source_repository(),
            )
            .unwrap_err();
        assert!(transform_error.to_string().contains("transform version 2"));

        let ancestry_repo = repository();
        commit(ancestry_repo.path(), "main root");
        git(ancestry_repo.path(), &["checkout", "--orphan", "unrelated"]);
        let unrelated = commit(ancestry_repo.path(), "unrelated root");
        git(ancestry_repo.path(), &["checkout", "main"]);
        let invalid_ancestry = explicit_pair_trailer(&source_context, &source, &unrelated).unwrap();
        commit(ancestry_repo.path(), &format!("explicit pair\n\n{invalid_ancestry}"));
        let mut ancestry_mappings = MappingStore::new("demo".to_string());
        let ancestry_error = ancestry_mappings
            .load_history(
                ancestry_repo.path(),
                HistorySide::Target,
                source_context.source_repository(),
            )
            .unwrap_err();
        assert!(ancestry_error.to_string().contains("not an ancestor"));

        let direction_repo = repository();
        let direction_target = commit(direction_repo.path(), "direction target");
        let target_identity = repository_identity(direction_repo.path()).unwrap();
        let target_context = OriginContext::new(target_identity.clone(), "demo", "v1-sha256-current").unwrap();
        commit(
            source_repo.path(),
            &format!(
                "wrong side\n\n{}",
                explicit_pair_trailer(&target_context, &source, &direction_target).unwrap()
            ),
        );
        let mut direction_mappings = MappingStore::new("demo".to_string());
        let direction_error = direction_mappings
            .load_history(source_repo.path(), HistorySide::Source, &target_identity)
            .unwrap_err();
        assert!(
            direction_error
                .to_string()
                .contains("explicit-pair trailer is valid only in target history")
        );
    }

    #[test]
    fn appending_origin_preserves_the_original_message_prefix() {
        let original = "subject\n\nbody with trailing space \n\n\n";
        let message = append_origin_trailers(original, &["Rail-Origin: evidence".to_string()]);
        assert!(message.starts_with(original));
        assert!(message.ends_with("Rail-Origin: evidence"));
    }

    #[test]
    fn origin_parser_ignores_body_lines_outside_the_trailer_block() {
        let message = format!(
            "subject\n\nRail-Origin: not-a-real-trailer\nbody continues\n\nSigned-off-by: Example <example.invalid>\n{}",
            OriginContext::new(repository_id('a'), "demo", "v1-sha256-snapshot")
                .unwrap()
                .trailer(&oid('b'))
                .unwrap()
        );
        let trailers = origin_trailer_values(&message);
        assert_eq!(trailers.len(), 1);
        assert!(trailers[0].starts_with("v2 "));
    }

    #[test]
    fn mapping_store_rejects_divergence_and_non_bijective_values() {
        let mut store = MappingStore::new("demo".to_string());
        let source = oid('a');
        let other_source = oid('b');
        let target = oid('c');
        let other_target = oid('d');
        store.record_mapping(&source, &target).unwrap();
        assert!(
            store
                .record_mapping(&source, &other_target)
                .unwrap_err()
                .to_string()
                .contains("maps to both")
        );
        assert!(
            store
                .record_mapping(&other_source, &target)
                .unwrap_err()
                .to_string()
                .contains("already mapped")
        );
    }

    #[test]
    fn remote_normalization_removes_http_credentials_and_query() {
        assert_eq!(
            normalize_remote_url("https://token@example.com/Org/repo.git?secret=value").unwrap(),
            "https://example.com/Org/repo"
        );
    }

    #[test]
    fn remote_aliases_share_logical_identity_but_not_publication_endpoint_authority() {
        let credentialed = "HTTPS://token@example.com/Org/repo.git?secret=value";
        let canonical = "https://example.com/Org/repo#ignored-fragment";

        assert_eq!(
            remote_repository_identity(credentialed).unwrap(),
            remote_repository_identity(canonical).unwrap(),
            "harmless URL aliases must identify one logical repository"
        );
        assert_ne!(
            remote_endpoint_identity(credentialed).unwrap(),
            remote_endpoint_identity(canonical).unwrap(),
            "publication retries must remain bound to the exact configured endpoint"
        );
    }
}
