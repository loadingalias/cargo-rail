//! Git-native split/sync origin mapping.
//!
//! Synthesized commits carry a versioned `Rail-Origin` trailer, so ordinary
//! clone history is sufficient to recover source/target mappings.

use std::collections::BTreeSet;
use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::git::{CommitMetadata, SystemGit, git_cmd_for_path};
use crate::mutation::git_effect::{
    GitCommitEffect, GitEffectCommitMetadata, GitEffectIntent, GitEffectJournal, GitEffectRecord, GitEffectStore,
    GitMappingBinding,
};
use crate::source::ContentDigest;
use crate::utils;

const TRAILER_PREFIX: &str = "Rail-Origin: ";
const TRAILER_SCHEMA: &str = "v2";
const V025_TRAILER_SCHEMA: &str = "v1";
const V025_NOTE_SCHEMA: &str = "cargo-rail-mapping-v1";
const V025_MIGRATION_SUBJECT: &str = "chore: migrate cargo-rail origin mappings";

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

/// Convert the exact weak trailer written into a v0.25 conflict receipt into
/// the current stable-ownership trailer before the receipt can authorize a
/// commit. The weak trailer must be the receipt's entire final trailer block;
/// accepting it anywhere else would let mutable receipt text supply mapping
/// authority that did not come from the predecessor writer.
pub(crate) fn migrate_v025_receipt_message(
    message: &str,
    context: &OriginContext,
    remote_commit: &str,
) -> RailResult<String> {
    let remote_commit = normalize_object_id("v0.25 receipt remote", remote_commit)?;
    let lines = message.lines().collect::<Vec<_>>();
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let start = lines[..end]
        .iter()
        .rposition(|line| line.trim().is_empty())
        .map_or(0, |index| index + 1);
    let Some(value) = lines
        .get(start)
        .and_then(|line| line.strip_prefix(TRAILER_PREFIX))
        .filter(|_| end.saturating_sub(start) == 1)
    else {
        return Err(RailError::with_help(
            "v0.25 sync receipt has an invalid predecessor origin trailer",
            "restart sync; cargo-rail will not reinterpret modified predecessor receipt text",
        ));
    };
    let parsed = ParsedTrailer::parse_v025(value).map_err(|_| {
        RailError::with_help(
            "v0.25 sync receipt has an invalid predecessor origin trailer",
            "restart sync; cargo-rail will not reinterpret modified predecessor receipt text",
        )
    })?;
    if !parsed.mapping
        || parsed.target_commit.is_some()
        || parsed.frontier.is_some()
        || parsed.source_commit != remote_commit
        || parsed.source_repository != context.source_repository
        || parsed.owner != context.owner
        || parsed.transform_version != TRANSFORM_VERSION
    {
        return Err(RailError::with_help(
            "v0.25 sync receipt predecessor origin does not match its bound remote commit and owner",
            "restart sync; cargo-rail will not reinterpret modified predecessor receipt authority",
        ));
    }

    let body = lines[..start].to_vec().join("\n").trim_end().to_string();
    Ok(append_origin_trailers(&body, &[context.trailer(&remote_commit)?]))
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
struct V025CommitMapping {
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

    fn as_str(self) -> &'static str {
        match self {
            Self::Neither => "none",
            Self::Source => "source",
            Self::Target => "target",
            Self::Both => "both",
        }
    }

    fn proves_source(self) -> bool {
        matches!(self, Self::Source | Self::Both)
    }

    fn proves_target(self) -> bool {
        matches!(self, Self::Target | Self::Both)
    }

    fn from_proofs(source: bool, target: bool) -> Self {
        match (source, target) {
            (true, true) => Self::Both,
            (true, false) => Self::Source,
            (false, true) => Self::Target,
            (false, false) => Self::Neither,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MigrationCandidate {
    source: String,
    target: String,
    frontier: MappingFrontier,
    kind: MigrationCandidateKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MigrationCandidateKind {
    Mapping,
    TargetEvidence,
}

impl V025CommitMapping {
    fn new(source: &str, target: &str) -> RailResult<Self> {
        Ok(Self {
            source: normalize_object_id("source", source)?,
            target: normalize_object_id("target", target)?,
        })
    }

    fn decode_note(note_target: &str, content: &str) -> RailResult<Self> {
        let lines = content.lines().collect::<Vec<_>>();
        if lines.first().copied() != Some(V025_NOTE_SCHEMA) {
            if lines.len() != 1 {
                return Err(mapping_resolution_error(
                    note_target,
                    "the note contains multiple legacy mapping values",
                ));
            }
            return Self::new(note_target, lines[0].trim());
        }
        if lines.len() != 3 {
            return Err(mapping_resolution_error(
                note_target,
                "the v1 note has an invalid field count",
            ));
        }
        let source = lines[1]
            .strip_prefix("source=")
            .ok_or_else(|| mapping_resolution_error(note_target, "the v1 note is missing its source field"))?;
        let target = lines[2]
            .strip_prefix("target=")
            .ok_or_else(|| mapping_resolution_error(note_target, "the v1 note is missing its target field"))?;
        let normalized_note_target = normalize_object_id("note attachment", note_target)?;
        let normalized_source = normalize_object_id("note source", source)?;
        if normalized_source != normalized_note_target {
            return Err(mapping_resolution_error(
                note_target,
                "the note attachment and declared source commit differ",
            ));
        }
        Self::new(&normalized_source, target)
    }
}

/// Exact read-only decoder for the origin forms accepted by v0.25.0.
///
/// Current origin values never pass through this type. Compatibility callers
/// first use the strict current parser and fall back here only for a form that
/// is exclusive to the predecessor grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
enum V025ParsedTrailer {
    Legacy { side: HistorySide, source_commit: String },
}

impl V025ParsedTrailer {
    fn parse(value: &str) -> RailResult<Self> {
        if let Some(source_commit) = value.strip_prefix("mono@") {
            return Ok(Self::Legacy {
                side: HistorySide::Target,
                source_commit: normalize_object_id("legacy mono origin", source_commit)?,
            });
        }
        if let Some(source_commit) = value.strip_prefix("remote@") {
            return Ok(Self::Legacy {
                side: HistorySide::Source,
                source_commit: normalize_object_id("legacy remote origin", source_commit)?,
            });
        }
        Err(RailError::message(format!(
            "unsupported predecessor Rail-Origin trailer '{}'",
            value
        )))
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
        Self::parse_schema(value, TRAILER_SCHEMA, true)
    }

    fn parse_v025(value: &str) -> RailResult<Self> {
        Self::parse_schema(value, V025_TRAILER_SCHEMA, false)
    }

    fn parse_schema(value: &str, schema: &str, allow_frontier: bool) -> RailResult<Self> {
        let mut fields = value.split_whitespace();
        if fields.next() != Some(schema) {
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
                let evidence_commit = if allow_frontier {
                    fields
                        .next()
                        .map(|field| normalize_object_id("evidence", parse_field(Some(field), "evidence")?))
                        .transpose()?
                } else {
                    None
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
                let frontier = if allow_frontier {
                    fields
                        .next()
                        .map(|field| MappingFrontier::parse(parse_field(Some(field), "frontier")?))
                        .transpose()?
                } else {
                    None
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
    current_history_mappings: FxHashSet<(String, String)>,
    v025_mappings: BTreeSet<(String, String)>,
    source_frontiers: FxHashSet<String>,
    target_frontiers: FxHashSet<String>,
    current_source_frontiers: FxHashSet<String>,
    current_target_frontiers: FxHashSet<String>,
    v025_source_frontiers: FxHashSet<String>,
    v025_target_frontiers: FxHashSet<String>,
    v025_source_evidence: BTreeSet<(String, String)>,
    v025_target_evidence: BTreeSet<(String, String)>,
    current_explicit_target_evidence: FxHashSet<String>,
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
    candidates: Vec<MigrationCandidate>,
    migration_digest: ContentDigest,
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
        candidates: Vec<MigrationCandidate>,
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

        let migration_digest = ContentDigest::sha256(&canonical_migration_bytes(
            &source_repository,
            target_repository.as_deref(),
            &owner,
            &ownership_snapshot,
            &candidates,
        ));
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
            candidates,
            migration_digest,
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
            Vec::new(),
        )
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        let mut canonical = b"cargo-rail-mapping-authority-v2".to_vec();
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
        append_migration_candidates(&mut canonical, b"pending-migration", &self.candidates);
        canonical
    }

    pub(crate) fn count(&self) -> usize {
        self.candidates.len()
    }

    pub(crate) fn migration_candidate_pairs(&self) -> Vec<(String, String)> {
        self.candidates
            .iter()
            .filter(|candidate| candidate.kind == MigrationCandidateKind::Mapping)
            .map(|candidate| (candidate.source.clone(), candidate.target.clone()))
            .collect()
    }

    pub(crate) fn digest(&self) -> String {
        format!("sha256-{}", self.digest)
    }

    pub(crate) fn migration_digest(&self) -> String {
        format!("sha256-{}", self.migration_digest)
    }

    /// Derive the exact mapping authority that one deterministic predecessor
    /// migration commit must produce. This is pure plan authority: callers can
    /// bind the post-effect digest before writing the prepared commit or moving
    /// the target ref, then compare it with a full post-effect recapture.
    pub(crate) fn after_migration(&self, migration_commit: &str) -> RailResult<Self> {
        let migration_commit = normalize_object_id("migration commit", migration_commit)?;
        if self.candidates.is_empty() {
            return Err(RailError::message(
                "mapping authority has no predecessor candidates to migrate",
            ));
        }
        if self.target_head.is_none() || self.target_selected_head.is_none() {
            return Err(RailError::message(
                "predecessor mapping migration requires an existing selected target history",
            ));
        }

        let mut mappings = self.mappings.clone();
        let mut source_frontiers = self.source_frontiers.clone();
        let mut target_frontiers = self.target_frontiers.clone();
        let source_evidence = self.source_evidence.clone();
        let mut target_evidence = self.target_evidence.clone();
        for candidate in &self.candidates {
            match candidate.kind {
                MigrationCandidateKind::Mapping => {
                    mappings.push((candidate.source.clone(), candidate.target.clone()));
                    if candidate.frontier.proves_source() {
                        source_frontiers.push(candidate.source.clone());
                    }
                    if candidate.frontier.proves_target() {
                        target_frontiers.push(candidate.target.clone());
                    }
                }
                MigrationCandidateKind::TargetEvidence => {
                    target_evidence.push(format!("endpoint:{}", candidate.target));
                    target_evidence.push(format!("pair:{}:{}", candidate.source, candidate.target));
                }
            }
        }
        // Every persistent explicit-pair/evidence migration trailer makes the
        // containing commit exact target-side evidence as well.
        target_evidence.push(format!("endpoint:{migration_commit}"));
        for values in [&mut source_frontiers, &mut target_frontiers, &mut target_evidence] {
            values.sort();
            values.dedup();
        }
        mappings.sort();
        mappings.dedup();

        Self::from_authority(
            &self.direction,
            &self.target_root,
            &self.branch,
            self.source_repository.clone(),
            self.source_head.clone(),
            self.source_selected_heads.clone(),
            self.target_repository.clone(),
            Some(migration_commit.clone()),
            Some(migration_commit),
            self.owner.clone(),
            self.ownership_snapshot.clone(),
            mappings,
            source_frontiers,
            target_frontiers,
            source_evidence,
            target_evidence,
            Vec::new(),
        )
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
        if !self.candidates.is_empty() {
            return Err(RailError::message(
                "ordinary split authority cannot advance while predecessor migration is pending",
            ));
        }
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
            Vec::new(),
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

fn append_migration_candidates(output: &mut Vec<u8>, label: &[u8], candidates: &[MigrationCandidate]) {
    for candidate in candidates {
        let mut value = Vec::with_capacity(candidate.source.len() + candidate.target.len() + 32);
        append_authority_frame(&mut value, b"source", candidate.source.as_bytes());
        append_authority_frame(&mut value, b"target", candidate.target.as_bytes());
        append_authority_frame(&mut value, b"frontier", candidate.frontier.as_str().as_bytes());
        append_authority_frame(
            &mut value,
            b"kind",
            match candidate.kind {
                MigrationCandidateKind::Mapping => b"mapping",
                MigrationCandidateKind::TargetEvidence => b"target-evidence",
            },
        );
        append_authority_frame(output, label, &value);
    }
}

fn canonical_migration_bytes(
    source_repository: &str,
    target_repository: Option<&str>,
    owner: &str,
    ownership_snapshot: &str,
    candidates: &[MigrationCandidate],
) -> Vec<u8> {
    let mut canonical = b"cargo-rail-v0.25-origin-migration-v3".to_vec();
    append_authority_frame(&mut canonical, b"source-repository", source_repository.as_bytes());
    append_optional_authority_frame(
        &mut canonical,
        b"target-repository",
        target_repository.map(str::as_bytes),
    );
    append_authority_frame(&mut canonical, b"owner", owner.as_bytes());
    append_authority_frame(&mut canonical, b"ownership-snapshot", ownership_snapshot.as_bytes());
    append_authority_frame(&mut canonical, b"transform-version", &TRANSFORM_VERSION.to_be_bytes());
    append_migration_candidates(&mut canonical, b"pending-migration", candidates);
    canonical
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
            current_history_mappings: FxHashSet::default(),
            v025_mappings: BTreeSet::new(),
            source_frontiers: FxHashSet::default(),
            target_frontiers: FxHashSet::default(),
            current_source_frontiers: FxHashSet::default(),
            current_target_frontiers: FxHashSet::default(),
            v025_source_frontiers: FxHashSet::default(),
            v025_target_frontiers: FxHashSet::default(),
            v025_source_evidence: BTreeSet::new(),
            v025_target_evidence: BTreeSet::new(),
            current_explicit_target_evidence: FxHashSet::default(),
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
        if !snapshot.candidates.is_empty() {
            return Err(RailError::message(
                "current mapping snapshot still contains predecessor migration candidates",
            ));
        }
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
            store.current_history_mappings.insert((source.clone(), target.clone()));
        }
        store.source_frontiers.extend(snapshot.source_frontiers.iter().cloned());
        store
            .current_source_frontiers
            .extend(snapshot.source_frontiers.iter().cloned());
        store.target_frontiers.extend(snapshot.target_frontiers.iter().cloned());
        store
            .current_target_frontiers
            .extend(snapshot.target_frontiers.iter().cloned());
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
                if is_inert_predecessor_trailer(value) {
                    continue;
                }
                let parsed = ParsedTrailer::parse(value)?;
                self.record_current_trailer(repo_path, parsed, &commit.sha, side, expected_source_repository)?;
            }
        }
        Ok(())
    }

    fn load_v025_compatible_history_at(
        &mut self,
        repo_path: &Path,
        side: HistorySide,
        expected_source_repository: &str,
        revision: &str,
    ) -> RailResult<()> {
        validate_repository_identity(expected_source_repository)?;
        let commits = SystemGit::open(repo_path)?.ordinary_commit_history_at(revision)?;
        self.load_v025_compatible_commits(repo_path, side, expected_source_repository, &commits)
    }

    fn load_v025_compatible_commits(
        &mut self,
        repo_path: &Path,
        side: HistorySide,
        expected_source_repository: &str,
        commits: &[crate::git::CommitInfo],
    ) -> RailResult<()> {
        for commit in commits {
            for value in origin_trailer_values(&commit.message) {
                match ParsedTrailer::parse(value) {
                    Ok(parsed) => {
                        self.record_current_trailer(repo_path, parsed, &commit.sha, side, expected_source_repository)?;
                    }
                    Err(current_error) => match ParsedTrailer::parse_v025(value) {
                        Ok(parsed) => self.record_v025_current_trailer(
                            repo_path,
                            parsed,
                            &commit.sha,
                            side,
                            expected_source_repository,
                        )?,
                        Err(_) => match V025ParsedTrailer::parse(value) {
                            Ok(parsed) => self.record_v025_trailer(parsed, &commit.sha, side)?,
                            Err(_) => return Err(current_error),
                        },
                    },
                }
            }
        }
        Ok(())
    }

    /// Load the exact predecessor notes ref without fetching or mutating it.
    fn load_v025_notes(&mut self, repo_path: &Path, _side: HistorySide) -> RailResult<()> {
        let notes_ref = format!("refs/notes/rail/{}", self.owner);
        let exists = git_cmd_for_path(repo_path)
            .args(["show-ref", "--verify", "--quiet", &notes_ref])
            .output()
            .context("Failed to inspect predecessor mapping notes")?;
        if !exists.status.success() {
            if exists.status.code() == Some(1) {
                return Ok(());
            }
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git show-ref --verify --quiet <mapping-notes-ref>".to_string(),
                stderr: git_command_diagnostics(&exists.stdout, &exists.stderr),
            }));
        }

        let output = git_cmd_for_path(repo_path)
            .args(["notes", "--ref", &notes_ref, "list"])
            .output()
            .context("Failed to list predecessor mapping notes")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git notes --ref <mapping-notes-ref> list".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let entries = String::from_utf8(output.stdout)?
            .lines()
            .map(|line| {
                let mut fields = line.split_whitespace();
                let blob = fields
                    .next()
                    .ok_or_else(|| RailError::message("mapping note has no blob ID"))?;
                let source = fields
                    .next()
                    .ok_or_else(|| RailError::message("mapping note has no source commit"))?;
                if fields.next().is_some() {
                    return Err(RailError::message(format!("invalid mapping note entry '{}'", line)));
                }
                validate_object_id("note blob", blob)?;
                validate_object_id("note source", source)?;
                Ok((blob.to_string(), source.to_string()))
            })
            .collect::<RailResult<Vec<_>>>()?;
        let git = SystemGit::open(repo_path)?;
        let blob_ids = entries.iter().map(|(blob, _)| blob.as_str()).collect::<Vec<_>>();
        let contents = git.read_blobs_bulk(&blob_ids)?;
        for ((_, source), content) in entries.into_iter().zip(contents) {
            let content = std::str::from_utf8(&content)
                .map_err(|_| RailError::message(format!("mapping note for '{}' is not UTF-8", source)))?;
            let mapping = V025CommitMapping::decode_note(&source, content.trim())?;
            // Notes carry an exact pair but no trustworthy history-side
            // provenance. They suppress endpoint replay without proving that
            // either endpoint's ancestors have already synchronized.
            self.record_v025_mapping(mapping, MappingFrontier::Neither)?;
        }
        Ok(())
    }

    /// Validate weak predecessor evidence against the exact repository pair.
    fn validate_v025_evidence(
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
                "predecessor mapping owner does not match the current split owner",
            ));
        }
        validate_token("ownership snapshot", &source_context.ownership_snapshot)?;
        validate_repository_identity(expected_target_repository)?;
        if repository_identity(source_repo)? != source_context.source_repository {
            return Err(RailError::message(
                "predecessor mapping source repository identity changed during validation",
            ));
        }
        if repository_identity(target_repo)? != expected_target_repository {
            return Err(RailError::message(
                "predecessor mapping target repository identity changed during validation",
            ));
        }

        let source_head = SystemGit::open(source_repo)?.head_commit()?;
        let target_head = SystemGit::open(target_repo)?.head_commit()?;
        let selected_target_head = normalize_object_id("selected target HEAD", selected_target_head)?;
        for (source, target) in &self.v025_mappings {
            if !is_ancestor_of_any(source_repo, source, selected_source_heads)? {
                return Err(mapping_resolution_error(
                    source,
                    "the predecessor source commit is not an ancestor of the selected source HEAD",
                ));
            }
            if !is_ancestor(target_repo, target, &selected_target_head)? {
                return Err(mapping_resolution_error(
                    source,
                    &format!(
                        "predecessor target '{}' is not an ancestor of the selected target HEAD",
                        target
                    ),
                ));
            }
        }
        if !self.v025_source_evidence.is_empty() {
            return Err(RailError::with_help(
                "predecessor source-history evidence cannot be upgraded from target history",
                "recreate current v2 source-history provenance before retrying; cargo-rail will not guess its repository authority",
            ));
        }
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
                    "the predecessor migration commit is not an ancestor of the selected target HEAD",
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

    /// Persist predecessor evidence in one deterministic history commit only when
    /// it still matches the checked plan.
    #[expect(
        clippy::too_many_arguments,
        reason = "migration application binds each checked repository authority explicitly"
    )]
    pub(crate) fn migrate_v025_evidence_bound(
        &mut self,
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        expected: Option<&MappingAuthoritySnapshot>,
    ) -> RailResult<Option<String>> {
        if self.owner != source_context.owner {
            return Err(RailError::message(
                "predecessor mapping owner does not match the current split owner",
            ));
        }
        if let Some(commit) = self.resume_active_v025_migration(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            target_root,
            branch,
            direction,
            expected,
        )? {
            return Ok(Some(commit));
        }
        let (captured_store, captured) = Self::capture_v025_authority(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            target_root,
            branch,
            direction,
        )?;
        *self = captured_store;
        if expected.is_some_and(|expected| expected != &captured) {
            return Err(origin_migration_drift_error());
        }
        if expected.is_none() && captured.count() > 0 {
            return Err(RailError::with_help(
                "predecessor mapping migration has no checked authority binding",
                "run split or sync through its check/apply command boundary before migrating predecessor evidence",
            ));
        }
        let migrations = captured.candidates.clone();
        if migrations.is_empty() {
            return Ok(None);
        }

        let git = SystemGit::open(target_repo)?;
        let head = git.head_commit()?;
        let parent = git.get_commit(&head)?;
        let quarantine = git.object_quarantine()?;
        quarantine.import_object_closure(&git, &[&head])?;
        let head_tree = format!("{head}^{{tree}}");
        let tree = quarantine
            .git_cmd()
            .args(["rev-parse", &head_tree])
            .output()
            .context("Failed to resolve predecessor mapping migration tree")?;
        if !tree.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: format!("git rev-parse <migration-parent>^{}tree{} (quarantine)", '{', '}'),
                stderr: git_command_diagnostics(&tree.stdout, &tree.stderr),
            }));
        }
        let tree = String::from_utf8(tree.stdout)?.trim().to_string();
        validate_object_id("tree", &tree)?;
        let trailers = migrations
            .iter()
            .map(|candidate| match candidate.kind {
                MigrationCandidateKind::Mapping => explicit_pair_trailer_with_frontier(
                    source_context,
                    &candidate.source,
                    &candidate.target,
                    candidate.frontier,
                ),
                MigrationCandidateKind::TargetEvidence => {
                    explicit_target_evidence_trailer(source_context, &candidate.source, &candidate.target)
                }
            })
            .collect::<RailResult<Vec<_>>>()?;
        let message = append_origin_trailers(V025_MIGRATION_SUBJECT, &trailers);
        let metadata = parent.metadata();
        let commit = write_v025_migration_commit_with_command(
            quarantine.git_cmd(),
            &tree,
            &head,
            &message,
            &metadata,
            "git commit-tree <predecessor-migration> (quarantine)",
        )?;
        let post_authority = captured.after_migration(&commit)?;
        let store = GitEffectStore::open(&git)?;
        let ref_name = format!("refs/heads/{branch}");
        let repository = store.capture_repository_authority(
            &git,
            expected_target_repository.to_string(),
            ref_name,
            Some(head.clone()),
            commit.clone(),
        )?;
        let mapping = GitMappingBinding::new(
            captured.owner.clone(),
            captured.ownership_snapshot.clone(),
            captured.digest(),
            post_authority.digest(),
            Some(captured.migration_digest()),
            captured.count(),
        );
        let commit_effect = GitCommitEffect::new(
            commit.clone(),
            tree,
            vec![head.clone()],
            message,
            GitEffectCommitMetadata::from(&metadata),
        );
        let operation_id = format!("origin-migration-{}", captured.migration_digest());
        let mut bundle = store.create_object_bundle_temp()?;
        let bundle_digest = quarantine.write_pack(&commit, Some(&head), bundle.file_mut()?)?;
        let intent = GitEffectIntent::new(
            operation_id,
            repository,
            Some(commit_effect),
            Vec::new(),
            Some(mapping),
            None,
            Some(bundle_digest.clone()),
        )?;
        let effect_id = intent.effect_id()?;
        let persisted_bundle = bundle.persist(&effect_id, &bundle_digest)?;
        drop(persisted_bundle);

        let (final_evidence, final_snapshot) = Self::capture_v025_authority(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            target_root,
            branch,
            direction,
        )?;
        if final_snapshot != captured {
            return Err(origin_migration_drift_error());
        }
        *self = final_evidence;
        let record = store.prepare(intent)?;
        self.reconcile_v025_migration_record(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            target_root,
            branch,
            direction,
            expected,
            &store,
            record,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "migration preparation keeps exact source and target authority visible"
    )]
    fn resume_active_v025_migration(
        &mut self,
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        expected: Option<&MappingAuthoritySnapshot>,
    ) -> RailResult<Option<String>> {
        let git = SystemGit::open(target_repo)?;
        let ref_name = format!("refs/heads/{branch}");
        let mut matching = GitEffectStore::discover_unacknowledged_read_only(&git)?
            .into_iter()
            .filter(|journal| journal.repository().ref_name == ref_name)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Ok(None);
        }
        if matching.len() != 1 {
            return Err(RailError::message(format!(
                "target branch '{ref_name}' has multiple active prepared Git effects"
            )));
        }
        let journal = matching.pop().expect("one matching prepared effect");
        if journal.mapping().is_none_or(|mapping| mapping.migration_count() == 0) {
            return Err(RailError::with_help(
                format!(
                    "target branch '{ref_name}' has unrelated active prepared effect '{}'",
                    journal.effect_id()
                ),
                "finish or reconcile that exact target effect before starting predecessor mapping migration",
            ));
        }
        let effect_id = journal.effect_id().to_string();
        let store = GitEffectStore::open(&git)?;
        let record = store.resume(&effect_id)?;
        self.reconcile_v025_migration_record(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            target_root,
            branch,
            direction,
            expected,
            &store,
            record,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "migration commit creation binds every exact repository authority"
    )]
    fn reconcile_v025_migration_record(
        &mut self,
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        expected: Option<&MappingAuthoritySnapshot>,
        store: &GitEffectStore,
        record: GitEffectRecord,
    ) -> RailResult<Option<String>> {
        match record {
            GitEffectRecord::Active(mut active) => {
                let journal = active.journal().clone();
                let commit = self.reconcile_v025_migration_journal(
                    source_repo,
                    target_repo,
                    source_context,
                    expected_target_repository,
                    target_root,
                    branch,
                    direction,
                    expected,
                    store,
                    &journal,
                    true,
                )?;
                #[cfg(test)]
                {
                    fail_v025_migration_after_ref_cas()?;
                }
                active.mark_local_applied()?;
                let _completed = active.finish()?;
                Ok(Some(commit))
            }
            GitEffectRecord::Completed(completed) => {
                let journal = completed.journal().clone();
                let commit = self.reconcile_v025_migration_journal(
                    source_repo,
                    target_repo,
                    source_context,
                    expected_target_repository,
                    target_root,
                    branch,
                    direction,
                    expected,
                    store,
                    &journal,
                    false,
                )?;
                Ok(Some(commit))
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "migration recovery keeps persisted and live authority inputs distinct"
    )]
    fn reconcile_v025_migration_journal(
        &mut self,
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
        expected: Option<&MappingAuthoritySnapshot>,
        store: &GitEffectStore,
        journal: &GitEffectJournal,
        install: bool,
    ) -> RailResult<String> {
        if !journal.paths().is_empty() || journal.publication().is_some() {
            return Err(RailError::message(
                "predecessor mapping recovery journal contains unrelated path or publication effects",
            ));
        }
        let repository = journal.repository();
        let commit = journal
            .commit()
            .ok_or_else(|| RailError::message("predecessor mapping recovery journal has no prepared commit"))?;
        let mapping = journal
            .mapping()
            .ok_or_else(|| RailError::message("predecessor mapping recovery journal has no mapping authority"))?;
        let bundle_digest = journal
            .object_bundle_digest()
            .ok_or_else(|| RailError::message("predecessor mapping recovery journal has no prepared object bundle"))?;
        if mapping.migration_count() == 0 || mapping.migration_digest().is_none() {
            return Err(RailError::message(
                "predecessor mapping recovery journal has no bound migration candidates",
            ));
        }
        if mapping.owner() != source_context.owner
            || mapping.ownership_snapshot() != source_context.ownership_snapshot
            || repository.logical_repository != expected_target_repository
            || repository.ref_name != format!("refs/heads/{branch}")
            || repository.result_oid != commit.oid()
        {
            return Err(origin_migration_drift_error());
        }
        let expected_parent = repository
            .expected_oid
            .as_deref()
            .ok_or_else(|| RailError::message("predecessor mapping recovery requires an existing target parent"))?;
        if commit.parents() != [expected_parent] {
            return Err(origin_migration_drift_error());
        }
        if let Some(expected) = expected
            && (expected.digest() != mapping.pre_authority()
                || expected.migration_digest() != mapping.migration_digest().unwrap_or_default()
                || expected.count() != mapping.migration_count()
                || expected.owner() != mapping.owner()
                || expected.ownership_snapshot() != mapping.ownership_snapshot()
                || expected.after_migration(commit.oid())?.digest() != mapping.post_authority())
        {
            return Err(origin_migration_drift_error());
        }

        let git = SystemGit::open(target_repo)?;
        let current_head = git.head_commit()?;
        let observed_repository = store.capture_repository_authority(
            &git,
            expected_target_repository.to_string(),
            repository.ref_name.clone(),
            Some(current_head.clone()),
            commit.oid().to_string(),
        )?;
        if observed_repository.logical_repository != repository.logical_repository
            || observed_repository.common_dir_identity != repository.common_dir_identity
            || observed_repository.worktree_identity != repository.worktree_identity
            || observed_repository.object_format != repository.object_format
            || observed_repository.ref_name != repository.ref_name
            || observed_repository.symbolic_head != repository.symbolic_head
            || observed_repository.result_oid != repository.result_oid
        {
            return Err(origin_migration_drift_error());
        }
        if current_head == expected_parent {
            let (pre_store, pre_authority) = Self::capture_v025_authority(
                source_repo,
                target_repo,
                source_context,
                expected_target_repository,
                target_root,
                branch,
                direction,
            )?;
            if pre_authority.digest() != mapping.pre_authority()
                || expected.is_some_and(|expected| expected != &pre_authority)
            {
                return Err(origin_migration_drift_error());
            }
            *self = pre_store;
            let obstructing = git.obstructing_worktree_paths()?;
            if !obstructing.is_empty() {
                return Err(RailError::with_help(
                    format!(
                        "target repository became dirty before predecessor migration: {}",
                        obstructing
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    "commit, restore, or remove target work before retrying; the prepared migration remains recoverable",
                ));
            }
        } else if current_head != commit.oid() {
            return Err(origin_migration_drift_error());
        }

        if install {
            let bundle = store
                .open_object_bundle(journal.effect_id(), bundle_digest)?
                .ok_or_else(|| {
                    RailError::message(format!(
                        "prepared Git effect '{}' is missing its bound object bundle",
                        journal.effect_id()
                    ))
                })?;
            let bundle_path = bundle.path().to_path_buf();
            git.install_prepared_object_pack_and_update_ref(
                bundle.into_file(),
                &bundle_path,
                bundle_digest,
                commit,
                &repository.ref_name,
                repository.expected_oid.as_deref(),
                journal.effect_id(),
            )?;
            if !journal.matches_repository_authority(store, &git, Some(repository.result_oid.clone()))? {
                return Err(origin_migration_drift_error());
            }
        } else if current_head != commit.oid() {
            return Err(origin_migration_drift_error());
        }
        Self::validate_completed_v025_migration(
            target_repo,
            commit.oid(),
            expected_parent,
            source_context,
            expected_target_repository,
            mapping.migration_digest().unwrap_or_default(),
        )?;
        let (post_store, post_authority) = Self::capture_v025_authority(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            target_root,
            branch,
            direction,
        )?;
        if post_authority.digest() != mapping.post_authority() || post_authority.count() != 0 {
            return Err(origin_migration_drift_error());
        }
        *self = post_store;
        Ok(commit.oid().to_string())
    }

    /// Validate the deterministic migration commit left by a crash after the
    /// target-HEAD CAS but before a durable receipt was advanced. The receipt
    /// binds only the bounded migration digest; this reconstructs the exact
    /// candidate set from the current v2 trailers and proves the commit is a
    /// tree-preserving child of the previously bound target head.
    pub(crate) fn validate_completed_v025_migration(
        target_repo: &Path,
        commit: &str,
        expected_parent: &str,
        source_context: &OriginContext,
        expected_target_repository: &str,
        expected_migration_digest: &str,
    ) -> RailResult<()> {
        let git = SystemGit::open(target_repo)?;
        let migrated = git.get_commit(commit)?;
        if migrated.parent_shas != [expected_parent.to_string()]
            || migrated.message.lines().next() != Some(V025_MIGRATION_SUBJECT)
            || git.collect_tree_entries(commit, Path::new("."))?
                != git.collect_tree_entries(expected_parent, Path::new("."))?
        {
            return Err(origin_migration_drift_error());
        }

        let mut candidates = Vec::new();
        for value in origin_trailer_values(&migrated.message) {
            let parsed = ParsedTrailer::parse(value)?;
            if parsed.owner != source_context.owner
                || parsed.source_repository != source_context.source_repository
                || parsed.ownership_snapshot != source_context.ownership_snapshot
                || parsed.transform_version != TRANSFORM_VERSION
            {
                return Err(origin_migration_drift_error());
            }
            let candidate = if parsed.mapping {
                let target = parsed.target_commit.ok_or_else(origin_migration_drift_error)?;
                if !is_ancestor(target_repo, &target, commit)? {
                    return Err(origin_migration_drift_error());
                }
                MigrationCandidate {
                    source: parsed.source_commit,
                    target,
                    frontier: parsed.frontier.unwrap_or(MappingFrontier::Neither),
                    kind: MigrationCandidateKind::Mapping,
                }
            } else {
                let target = parsed.evidence_commit.ok_or_else(origin_migration_drift_error)?;
                if parsed.evidence_side != Some(HistorySide::Target) || !is_ancestor(target_repo, &target, commit)? {
                    return Err(origin_migration_drift_error());
                }
                MigrationCandidate {
                    source: parsed.source_commit,
                    target,
                    frontier: MappingFrontier::Neither,
                    kind: MigrationCandidateKind::TargetEvidence,
                }
            };
            candidates.push(candidate);
        }
        candidates.sort();
        if candidates.is_empty() {
            return Err(origin_migration_drift_error());
        }
        let actual = format!(
            "sha256-{}",
            ContentDigest::sha256(&canonical_migration_bytes(
                &source_context.source_repository,
                Some(expected_target_repository),
                &source_context.owner,
                &source_context.ownership_snapshot,
                &candidates,
            ))
        );
        if actual != expected_migration_digest {
            return Err(origin_migration_drift_error());
        }

        let trailers = candidates
            .iter()
            .map(|candidate| match candidate.kind {
                MigrationCandidateKind::Mapping => explicit_pair_trailer_with_frontier(
                    source_context,
                    &candidate.source,
                    &candidate.target,
                    candidate.frontier,
                ),
                MigrationCandidateKind::TargetEvidence => {
                    explicit_target_evidence_trailer(source_context, &candidate.source, &candidate.target)
                }
            })
            .collect::<RailResult<Vec<_>>>()?;
        let expected_message = append_origin_trailers(V025_MIGRATION_SUBJECT, &trailers);
        let parent = git.get_commit(expected_parent)?;
        if migrated.message != expected_message || migrated.metadata() != parent.metadata() {
            return Err(origin_migration_drift_error());
        }

        let quarantine = git.object_quarantine()?;
        quarantine.import_object_closure(&git, &[expected_parent])?;
        let tree = quarantine
            .git_cmd()
            .args(["rev-parse", &format!("{expected_parent}^{{tree}}")])
            .output()
            .context("Failed to resolve migration recovery tree")?;
        if !tree.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: format!("git rev-parse <parent>^{}tree{}", '{', '}'),
                stderr: git_command_diagnostics(&tree.stdout, &tree.stderr),
            }));
        }
        let tree = String::from_utf8(tree.stdout)?.trim().to_string();
        let metadata = parent.metadata();
        let reconstructed = write_v025_migration_commit_with_command(
            quarantine.git_cmd(),
            &tree,
            expected_parent,
            &expected_message,
            &metadata,
            "git commit-tree <migration-recovery> (quarantine)",
        )?;
        if reconstructed != commit {
            return Err(origin_migration_drift_error());
        }
        Ok(())
    }

    pub(crate) fn capture_v025_authority(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
        target_root: &Path,
        branch: &str,
        direction: &str,
    ) -> RailResult<(Self, MappingAuthoritySnapshot)> {
        let selected_target_head = SystemGit::open(target_repo)?.head_commit()?;
        Self::capture_v025_authority_at(
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
    pub(crate) fn capture_v025_authority_at(
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
        let store = Self::capture_v025_evidence_at(
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
            return Err(origin_migration_drift_error());
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
    pub(crate) fn capture_v025_authority_at_source(
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
        let mut store = Self::capture_v025_evidence_at(
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
            return Err(origin_migration_drift_error());
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
        let mut store = Self::capture_v025_evidence_at(
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
            return Err(origin_migration_drift_error());
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
        let mut store = Self::capture_v025_evidence_at(
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
            return Err(origin_migration_drift_error());
        }
        authority.source_head = expected_source_head;
        let snapshot = store.mapping_authority_snapshot(direction, target_root, branch)?;
        Ok((store, snapshot))
    }

    pub(crate) fn capture_v025_evidence(
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        expected_target_repository: &str,
    ) -> RailResult<Self> {
        let selected_target_head = SystemGit::open(target_repo)?.head_commit()?;
        let selected_source_heads = selected_source_heads(source_repo)?;
        Self::capture_v025_evidence_at(
            source_repo,
            target_repo,
            source_context,
            expected_target_repository,
            &selected_target_head,
            &selected_source_heads,
        )
    }

    pub(crate) fn capture_v025_evidence_at(
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
            evidence.load_v025_compatible_history_at(
                source_repo,
                HistorySide::Source,
                expected_target_repository,
                selected_source_head,
            )?;
        }
        evidence.load_v025_compatible_history_at(
            target_repo,
            HistorySide::Target,
            source_context.source_repository(),
            selected_target_head,
        )?;
        evidence.load_v025_notes(source_repo, HistorySide::Source)?;
        evidence.load_v025_notes(target_repo, HistorySide::Target)?;
        evidence.repository_authority = Some(evidence.validate_v025_evidence(
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
                            "explicit target evidence is not an ancestor of its migration commit",
                        ));
                    }
                    self.target_evidence.insert(endpoint.clone());
                    self.target_evidence.insert(containing_commit.to_string());
                    self.target_evidence_pairs
                        .insert((parsed.source_commit, endpoint.clone()));
                    self.current_explicit_target_evidence.insert(endpoint);
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
                    V025CommitMapping::new(&parsed.source_commit, &target_commit)?,
                    parsed.frontier,
                )
            }
            (HistorySide::Source, None) => (
                V025CommitMapping::new(containing_commit, &parsed.source_commit)?,
                Some(MappingFrontier::Target),
            ),
            (HistorySide::Target, None) => (
                V025CommitMapping::new(&parsed.source_commit, containing_commit)?,
                Some(MappingFrontier::Source),
            ),
        };
        self.record_mapping(&mapping.source, &mapping.target)?;
        if let Some(frontier) = frontier {
            self.record_frontier(&mapping, frontier, true);
        }
        self.current_history_mappings.insert((mapping.source, mapping.target));
        Ok(())
    }

    fn record_v025_trailer(
        &mut self,
        parsed: V025ParsedTrailer,
        containing_commit: &str,
        side: HistorySide,
    ) -> RailResult<()> {
        match parsed {
            V025ParsedTrailer::Legacy {
                side: trailer_side,
                source_commit,
            } if side == trailer_side => {
                let mapping = match side {
                    HistorySide::Source => V025CommitMapping::new(containing_commit, &source_commit)?,
                    HistorySide::Target => V025CommitMapping::new(&source_commit, containing_commit)?,
                };
                // Weak predecessor trailers bind neither repository identity
                // nor ownership/transform authority. Keep the exact pair, but
                // never infer an ancestry frontier from it.
                self.record_v025_mapping(mapping, MappingFrontier::Neither)
            }
            V025ParsedTrailer::Legacy { .. } => Ok(()),
        }
    }

    fn record_v025_current_trailer(
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
                "unsupported predecessor Rail-Origin transform version {} for '{}'",
                parsed.transform_version, self.owner
            )));
        }
        validate_token("predecessor workspace snapshot", &parsed.ownership_snapshot)?;
        if !parsed.mapping {
            match side {
                HistorySide::Source => {
                    self.source_evidence.insert(containing_commit.to_string());
                    self.source_evidence_pairs
                        .insert((containing_commit.to_string(), parsed.source_commit.clone()));
                    self.v025_source_evidence
                        .insert((containing_commit.to_string(), parsed.source_commit));
                }
                HistorySide::Target => {
                    self.target_evidence.insert(containing_commit.to_string());
                    self.target_evidence_pairs
                        .insert((parsed.source_commit.clone(), containing_commit.to_string()));
                    self.v025_target_evidence
                        .insert((parsed.source_commit, containing_commit.to_string()));
                }
            }
            return Ok(());
        }
        let mapping = match (side, parsed.target_commit) {
            (HistorySide::Source, Some(_)) => {
                return Err(mapping_resolution_error(
                    containing_commit,
                    "a predecessor explicit-pair trailer is valid only in target history",
                ));
            }
            (HistorySide::Target, Some(target_commit)) => {
                if !is_ancestor(repo_path, &target_commit, containing_commit)? {
                    return Err(mapping_resolution_error(
                        &parsed.source_commit,
                        &format!(
                            "predecessor explicit-pair target '{}' is not an ancestor of its trailer commit",
                            target_commit
                        ),
                    ));
                }
                V025CommitMapping::new(&parsed.source_commit, &target_commit)?
            }
            (HistorySide::Source, None) => V025CommitMapping::new(containing_commit, &parsed.source_commit)?,
            (HistorySide::Target, None) => V025CommitMapping::new(&parsed.source_commit, containing_commit)?,
        };
        // v1's `snapshot=` was the volatile full workspace-content ID. It
        // cannot prove current ownership ancestry, even when its exact pair is
        // still valid. Persist the pair as frontier=none and let the ambiguity
        // guard reject unmatched ancestors rather than guessing.
        self.record_v025_mapping(mapping, MappingFrontier::Neither)
    }

    fn record_v025_mapping(&mut self, mapping: V025CommitMapping, frontier: MappingFrontier) -> RailResult<()> {
        self.record_mapping(&mapping.source, &mapping.target)?;
        self.record_frontier(&mapping, frontier, false);
        let pair = (mapping.source, mapping.target);
        self.v025_mappings.insert(pair);
        Ok(())
    }

    fn record_frontier(&mut self, mapping: &V025CommitMapping, frontier: MappingFrontier, current: bool) {
        if frontier.proves_source() {
            self.source_frontiers.insert(mapping.source.clone());
            if current {
                self.current_source_frontiers.insert(mapping.source.clone());
            } else {
                self.v025_source_frontiers.insert(mapping.source.clone());
            }
        }
        if frontier.proves_target() {
            self.target_frontiers.insert(mapping.target.clone());
            if current {
                self.current_target_frontiers.insert(mapping.target.clone());
            } else {
                self.v025_target_frontiers.insert(mapping.target.clone());
            }
        }
    }

    fn v025_migration_candidates_with_frontiers(&self) -> Vec<MigrationCandidate> {
        let mut candidates = self
            .v025_mappings
            .iter()
            .filter_map(|(source, target)| {
                let mapping_missing = !self
                    .current_history_mappings
                    .contains(&(source.clone(), target.clone()));
                let source_missing =
                    self.v025_source_frontiers.contains(source) && !self.current_source_frontiers.contains(source);
                let target_missing =
                    self.v025_target_frontiers.contains(target) && !self.current_target_frontiers.contains(target);
                (mapping_missing || source_missing || target_missing).then(|| MigrationCandidate {
                    source: source.clone(),
                    target: target.clone(),
                    frontier: MappingFrontier::from_proofs(source_missing, target_missing),
                    kind: MigrationCandidateKind::Mapping,
                })
            })
            .collect::<Vec<_>>();
        candidates.extend(
            self.v025_target_evidence
                .iter()
                .filter(|(_, target)| !self.current_explicit_target_evidence.contains(target))
                .map(|(source, target)| MigrationCandidate {
                    source: source.clone(),
                    target: target.clone(),
                    frontier: MappingFrontier::Neither,
                    kind: MigrationCandidateKind::TargetEvidence,
                }),
        );
        candidates.sort();
        candidates
    }

    #[cfg(test)]
    fn v025_migration_candidates(&self) -> Vec<(String, String)> {
        self.v025_migration_candidates_with_frontiers()
            .into_iter()
            .map(|candidate| (candidate.source, candidate.target))
            .collect()
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
            self.v025_migration_candidates_with_frontiers(),
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
        let mapping = V025CommitMapping::new(source, target)?;
        self.record_mapping(&mapping.source, &mapping.target)?;
        self.record_frontier(&mapping, MappingFrontier::Source, true);
        self.current_history_mappings.insert((mapping.source, mapping.target));
        Ok(())
    }

    pub(crate) fn record_target_frontier_mapping(&mut self, source: &str, target: &str) -> RailResult<()> {
        let mapping = V025CommitMapping::new(source, target)?;
        self.record_mapping(&mapping.source, &mapping.target)?;
        self.record_frontier(&mapping, MappingFrontier::Target, true);
        self.current_history_mappings.insert((mapping.source, mapping.target));
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

    /// Exact pairs whose predecessor evidence proves neither directional
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

fn explicit_pair_trailer_with_frontier(
    context: &OriginContext,
    source_commit: &str,
    target_commit: &str,
    frontier: MappingFrontier,
) -> RailResult<String> {
    Ok(format!(
        "{} frontier={}",
        explicit_pair_trailer(context, source_commit, target_commit)?,
        frontier.as_str()
    ))
}

fn explicit_target_evidence_trailer(
    context: &OriginContext,
    source_commit: &str,
    target_commit: &str,
) -> RailResult<String> {
    let source_commit = normalize_object_id("evidence source", source_commit)?;
    let target_commit = normalize_object_id("evidence target", target_commit)?;
    Ok(format!(
        "{TRAILER_PREFIX}{TRAILER_SCHEMA} source={} commit={} owner={} snapshot={} transform={TRANSFORM_VERSION} mapping=evidence evidence={} side=target",
        context.source_repository,
        source_commit,
        encode_hex(context.owner.as_bytes()),
        context.ownership_snapshot,
        target_commit,
    ))
}

pub(crate) fn is_ancestor(repo_path: &Path, ancestor: &str, descendant: &str) -> RailResult<bool> {
    validate_object_id("ancestor", ancestor)?;
    validate_object_id("descendant", descendant)?;
    let output = git_cmd_for_path(repo_path)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .context("Failed to validate predecessor mapping ancestry")?;
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

fn is_inert_predecessor_trailer(value: &str) -> bool {
    ParsedTrailer::parse_v025(value).is_ok()
        || value
            .strip_prefix("mono@")
            .or_else(|| value.strip_prefix("remote@"))
            .is_some_and(|object_id| validate_object_id("retired predecessor origin", object_id).is_ok())
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

fn origin_migration_drift_error() -> RailError {
    RailError::with_help(
        "predecessor mapping evidence changed after it was checked",
        "retry after the ordinary histories and refs/notes/rail mapping refs stop changing",
    )
}

#[cfg(test)]
std::thread_local! {
    static FAIL_V025_MIGRATION_AFTER_REF_CAS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_v025_migration_after_ref_cas() -> RailResult<()> {
    if FAIL_V025_MIGRATION_AFTER_REF_CAS.replace(false) {
        Err(RailError::message(
            "injected interruption after predecessor migration ref CAS",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn write_v025_migration_commit(
    target_repo: &Path,
    tree: &str,
    parent: &str,
    message: &str,
    metadata: &CommitMetadata,
) -> RailResult<String> {
    write_v025_migration_commit_with_command(
        git_cmd_for_path(target_repo),
        tree,
        parent,
        message,
        metadata,
        "git commit-tree -F -",
    )
}

fn write_v025_migration_commit_with_command(
    mut command: std::process::Command,
    tree: &str,
    parent: &str,
    message: &str,
    metadata: &CommitMetadata,
    command_name: &str,
) -> RailResult<String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let author_date = format!("{} {}", metadata.author_timestamp, metadata.author_timezone);
    let committer_date = format!("{} {}", metadata.committer_timestamp, metadata.committer_timezone);
    command
        .env("GIT_AUTHOR_NAME", &metadata.author)
        .env("GIT_AUTHOR_EMAIL", &metadata.author_email)
        .env("GIT_AUTHOR_DATE", &author_date)
        .env("GIT_COMMITTER_NAME", &metadata.committer)
        .env("GIT_COMMITTER_EMAIL", &metadata.committer_email)
        .env("GIT_COMMITTER_DATE", &committer_date)
        .args(["commit-tree", tree, "-p", parent, "-F", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("Failed to start predecessor mapping migration commit")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| RailError::message("git commit-tree stdin was unavailable"))?;
    stdin
        .write_all(message.as_bytes())
        .context("Failed to write predecessor mapping migration message")?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .context("Failed to create predecessor mapping migration commit")?;
    if !output.status.success() {
        return Err(RailError::Git(GitError::CommandFailed {
            command: command_name.to_string(),
            stderr: git_command_diagnostics(&output.stdout, &output.stderr),
        }));
    }
    let commit = String::from_utf8(output.stdout)?.trim().to_string();
    validate_object_id("migration", &commit)?;
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V025_LEGACY_NOTE: &[u8] = include_bytes!("../../tests/fixtures/compat/v0.25.0/mappings/legacy.note");
    const V025_MAPPING_NOTE: &[u8] = include_bytes!("../../tests/fixtures/compat/v0.25.0/mappings/mapping-v1.note");
    const V025_MIGRATION_TRAILER: &[u8] =
        include_bytes!("../../tests/fixtures/compat/v0.25.0/mappings/migration-origin.trailer");
    const V025_EVIDENCE_TRAILER: &[u8] =
        include_bytes!("../../tests/fixtures/compat/v0.25.0/mappings/evidence-origin.trailer");
    const V025_MONO_TRAILER: &[u8] = include_bytes!("../../tests/fixtures/compat/v0.25.0/mappings/mono-origin.trailer");
    const V025_REMOTE_TRAILER: &[u8] =
        include_bytes!("../../tests/fixtures/compat/v0.25.0/mappings/remote-origin.trailer");

    fn fixture(bytes: &'static [u8]) -> &'static str {
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        std::str::from_utf8(bytes).unwrap()
    }

    fn oid(digit: char) -> String {
        std::iter::repeat_n(digit, 40).collect()
    }

    fn repository_id(digit: char) -> String {
        format!("sha256-{}", std::iter::repeat_n(digit, 64).collect::<String>())
    }

    #[test]
    fn v025_receipt_message_is_rewritten_to_current_ownership_trailer() {
        let remote = oid('b');
        let context = OriginContext::new(repository_id('a'), "demo", "sha256-stable-policy").unwrap();
        let legacy_context = OriginContext::new(repository_id('a'), "demo", "v1-volatile-content").unwrap();
        let legacy = format!(
            "remote change\n\n{}",
            legacy_context
                .format_trailer(&remote, true)
                .unwrap()
                .replacen("Rail-Origin: v2", "Rail-Origin: v1", 1)
        );

        let migrated = migrate_v025_receipt_message(&legacy, &context, &remote).unwrap();

        assert_eq!(origin_trailer_values(&migrated).len(), 1);
        assert!(origin_trailer_values(&migrated)[0].starts_with("v2 source="));
        assert!(!migrated.contains("Rail-Origin: v1"));
        ParsedTrailer::parse(origin_trailer_values(&migrated)[0]).unwrap();
    }

    #[test]
    fn v025_receipt_message_rejects_modified_predecessor_trailer_block() {
        let remote = oid('b');
        let context = OriginContext::new(repository_id('a'), "demo", "sha256-stable-policy").unwrap();
        let modified = format!(
            "remote change\n\n{TRAILER_PREFIX}v1 source={} commit={remote} owner=64656d6f snapshot=v1-volatile-content transform=1\nSigned-off-by: attacker",
            repository_id('a')
        );

        let error = migrate_v025_receipt_message(&modified, &context, &remote).unwrap_err();
        assert!(error.to_string().contains("invalid predecessor origin trailer"));
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

    fn import_source_and_add_note(source_repo: &Path, target_repo: &Path, source: &str, content: &str) {
        git(
            target_repo,
            &["fetch", "--quiet", source_repo.to_str().unwrap(), source],
        );
        git(
            target_repo,
            &["notes", "--ref", "refs/notes/rail/demo", "add", "-m", content, source],
        );
    }

    fn migrate_checked(
        mappings: &mut MappingStore,
        source_repo: &Path,
        target_repo: &Path,
        source_context: &OriginContext,
        target_identity: &str,
    ) -> RailResult<Option<String>> {
        let target_root = utils::canonicalize_existing(target_repo)?;
        let (_, expected) = MappingStore::capture_v025_authority(
            source_repo,
            &target_root,
            source_context,
            target_identity,
            &target_root,
            "main",
            "mono_to_remote",
        )?;
        mappings.migrate_v025_evidence_bound(
            source_repo,
            target_repo,
            source_context,
            target_identity,
            &target_root,
            "main",
            "mono_to_remote",
            Some(&expected),
        )
    }

    fn acknowledge_completed_test_effect(target_repo: &Path) {
        let git = SystemGit::open(target_repo).unwrap();
        let journals = GitEffectStore::discover_unacknowledged_read_only(&git).unwrap();
        assert_eq!(journals.len(), 1, "expected one completed migration effect");
        let store = GitEffectStore::open(&git).unwrap();
        match store.resume(journals[0].effect_id()).unwrap() {
            GitEffectRecord::Completed(completed) => completed.acknowledge().unwrap(),
            GitEffectRecord::Active(_) => panic!("migration effect was not terminal"),
        }
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
    fn v025_fixtures_separate_transitional_and_persistent_grammar() {
        let source = oid('a');
        let target = oid('b');
        let repository = repository_id('e');

        assert_eq!(fixture(V025_MONO_TRAILER), format!("mono@{source}"));
        assert_eq!(fixture(V025_REMOTE_TRAILER), format!("remote@{target}"));
        assert_eq!(
            fixture(V025_EVIDENCE_TRAILER),
            format!(
                "v1 source={repository} commit={source} owner=64656d6f snapshot=v1-sha256-snapshot transform=1 mapping=evidence"
            )
        );
        assert_eq!(
            fixture(V025_MIGRATION_TRAILER),
            format!(
                "v1 source={repository} commit={source} owner=64656d6f snapshot=v1-sha256-snapshot transform=1 target={target}"
            )
        );
        assert_eq!(fixture(V025_LEGACY_NOTE), target);
        assert_eq!(
            fixture(V025_MAPPING_NOTE),
            format!("{V025_NOTE_SCHEMA}\nsource={source}\ntarget={target}")
        );

        ParsedTrailer::parse(fixture(V025_MONO_TRAILER)).unwrap_err();
        ParsedTrailer::parse(fixture(V025_REMOTE_TRAILER)).unwrap_err();
        assert_eq!(
            ParsedTrailer::parse_v025(fixture(V025_MIGRATION_TRAILER)).unwrap(),
            ParsedTrailer {
                source_repository: repository,
                source_commit: source.clone(),
                owner: "demo".to_string(),
                ownership_snapshot: "v1-sha256-snapshot".to_string(),
                transform_version: TRANSFORM_VERSION,
                mapping: true,
                target_commit: Some(target.clone()),
                frontier: None,
                evidence_commit: None,
                evidence_side: None,
            }
        );
        assert_eq!(
            V025ParsedTrailer::parse(fixture(V025_MONO_TRAILER)).unwrap(),
            V025ParsedTrailer::Legacy {
                side: HistorySide::Target,
                source_commit: source.clone(),
            }
        );
        assert_eq!(
            V025ParsedTrailer::parse(fixture(V025_REMOTE_TRAILER)).unwrap(),
            V025ParsedTrailer::Legacy {
                side: HistorySide::Source,
                source_commit: target.clone(),
            }
        );
        V025ParsedTrailer::parse(fixture(V025_MIGRATION_TRAILER)).unwrap_err();
        assert_eq!(
            V025CommitMapping::decode_note(&source, fixture(V025_LEGACY_NOTE)).unwrap(),
            V025CommitMapping::new(&source, &target).unwrap()
        );
        assert_eq!(
            V025CommitMapping::decode_note(&source, fixture(V025_MAPPING_NOTE)).unwrap(),
            V025CommitMapping::new(&source, &target).unwrap()
        );
    }

    #[test]
    fn v025_decoder_rejects_near_miss_grammar() {
        let source = oid('a');
        let target = oid('b');
        let repository = repository_id('e');
        V025ParsedTrailer::parse(&format!("mono@{source} extra")).unwrap_err();
        V025ParsedTrailer::parse(&format!("remote@{target} extra")).unwrap_err();
        V025ParsedTrailer::parse(&format!(
            "v1 source={repository} commit={source} owner=64656d6f snapshot=v1-sha256-snapshot transform=1 target={target} extra=value"
        ))
        .unwrap_err();
        for frontier in ["neither", "source", "target", "both"] {
            V025ParsedTrailer::parse(&format!(
                "v1 source={repository} commit={source} owner=64656d6f snapshot=v1-sha256-snapshot transform=1 target={target} frontier={frontier}"
            ))
            .unwrap_err();
        }
        V025CommitMapping::decode_note(&source, &format!("{target}\n{target}")).unwrap_err();
        V025CommitMapping::decode_note(
            &source,
            &format!("{V025_NOTE_SCHEMA}\nsource={}\ntarget={target}", oid('c')),
        )
        .unwrap_err();
    }

    #[test]
    fn v025_notes_only_migrate_and_mixed_history_is_already_migrated() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let target = commit(target_repo.path(), "target");
        let note = format!("{V025_NOTE_SCHEMA}\nsource={source}\ntarget={target}");
        import_source_and_add_note(source_repo.path(), target_repo.path(), &source, &note);

        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();
        let before_head = git(target_repo.path(), &["rev-parse", "HEAD"]);
        let before_tree = git(target_repo.path(), &["rev-parse", "HEAD^{tree}"]);
        let mut mappings = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert_eq!(mappings.get_mapping(&source), Some(target.clone()));
        assert_eq!(
            mappings.v025_migration_candidates(),
            vec![(source.clone(), target.clone())]
        );
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD"]), before_head);
        let target_root = utils::canonicalize_existing(target_repo.path()).unwrap();
        let (_, checked_authority) = MappingStore::capture_v025_authority(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
            &target_root,
            "main",
            "mono_to_remote",
        )
        .unwrap();

        let migration = migrate_checked(
            &mut mappings,
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap()
        .unwrap();
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD"]), migration);
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD^{tree}"]), before_tree);
        let (_, migrated_authority) = MappingStore::capture_v025_authority(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
            &target_root,
            "main",
            "mono_to_remote",
        )
        .unwrap();
        assert_eq!(
            checked_authority.after_migration(&migration).unwrap(),
            migrated_authority,
            "the pre-effect authority must predict the exact post-migration store",
        );
        let message = git(target_repo.path(), &["log", "-1", "--format=%B"]);
        assert_eq!(
            message,
            format!(
                "{V025_MIGRATION_SUBJECT}\n\n{}",
                explicit_pair_trailer_with_frontier(&source_context, &source, &target, MappingFrontier::Neither,)
                    .unwrap()
            )
        );
        acknowledge_completed_test_effect(target_repo.path());

        let mut mixed = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert!(mixed.v025_migration_candidates().is_empty());
        assert!(mixed.has_reverse_mapping(&migration));
        assert_eq!(
            migrate_checked(
                &mut mixed,
                source_repo.path(),
                target_repo.path(),
                &source_context,
                &target_identity,
            )
            .unwrap(),
            None
        );
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD"]), migration);

        git(target_repo.path(), &["update-ref", "-d", "refs/notes/rail/demo"]);
        let ordinary_only = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert_eq!(ordinary_only.get_mapping(&source), Some(target.clone()));
        assert!(ordinary_only.v025_migration_candidates().is_empty());

        let mut current_only = MappingStore::new("demo".to_string());
        current_only
            .load_history(
                target_repo.path(),
                HistorySide::Target,
                source_context.source_repository(),
            )
            .unwrap();
        assert_eq!(current_only.get_mapping(&source), Some(target));
        assert!(current_only.has_reverse_mapping(&migration));
        assert!(current_only.source_frontier_commits().is_empty());
        assert!(current_only.target_frontier_commits().is_empty());
        assert!(current_only.v025_mappings.is_empty());
    }

    #[test]
    fn v025_evidence_only_endpoint_migrates_as_exact_current_evidence() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let source_context = OriginContext::discover(source_repo.path(), "demo", "sha256-stable-policy").unwrap();
        let predecessor_context = OriginContext::new(
            source_context.source_repository().to_string(),
            "demo",
            "v1-volatile-content",
        )
        .unwrap();
        let evidence = commit(
            target_repo.path(),
            &predecessor_context
                .evidence_trailer(&source)
                .unwrap()
                .replacen("Rail-Origin: v2", "Rail-Origin: v1", 1),
        );
        let target_identity = repository_identity(target_repo.path()).unwrap();

        let mut mappings = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert!(mappings.has_reverse_mapping(&evidence));
        assert_eq!(mappings.v025_migration_candidates_with_frontiers().len(), 1);
        migrate_checked(
            &mut mappings,
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap()
        .unwrap();

        let migrated = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert!(migrated.has_reverse_mapping(&evidence));
        assert!(migrated.v025_migration_candidates_with_frontiers().is_empty());
        let mut current_only = MappingStore::new("demo".to_string());
        current_only.expected_ownership_snapshot = Some("sha256-stable-policy".to_string());
        current_only
            .load_history(
                target_repo.path(),
                HistorySide::Target,
                source_context.source_repository(),
            )
            .unwrap();
        assert!(current_only.has_reverse_mapping(&evidence));
    }

    #[test]
    fn v025_note_migration_is_deterministic_across_equivalent_clones() {
        let source_repo = repository();
        let source_one = commit(source_repo.path(), "source one");
        let source_two = commit(source_repo.path(), "source two");
        let target_repo = repository();
        let target_one = commit(target_repo.path(), "target one");
        let target_two = commit(target_repo.path(), "target two");
        import_source_and_add_note(source_repo.path(), target_repo.path(), &source_one, &target_one);
        import_source_and_add_note(
            source_repo.path(),
            target_repo.path(),
            &source_two,
            &format!("{V025_NOTE_SCHEMA}\nsource={source_two}\ntarget={target_two}"),
        );

        let clone_parent = tempfile::TempDir::new().unwrap();
        git(
            clone_parent.path(),
            &["clone", "--quiet", target_repo.path().to_str().unwrap(), "copy"],
        );
        let clone = clone_parent.path().join("copy");
        git(
            &clone,
            &[
                "fetch",
                "--quiet",
                target_repo.path().to_str().unwrap(),
                "refs/notes/rail/demo:refs/notes/rail/demo",
            ],
        );

        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();
        assert_eq!(repository_identity(&clone).unwrap(), target_identity);
        let mut original = MappingStore::new("demo".to_string());
        let original_commit = migrate_checked(
            &mut original,
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap()
        .unwrap();
        let mut copied = MappingStore::new("demo".to_string());
        let copied_commit = migrate_checked(
            &mut copied,
            source_repo.path(),
            &clone,
            &source_context,
            &target_identity,
        )
        .unwrap()
        .unwrap();
        assert_eq!(original_commit, copied_commit);

        let message = git(target_repo.path(), &["log", "-1", "--format=%B"]);
        let mut encoded_sources = message
            .lines()
            .filter_map(|line| line.strip_prefix(TRAILER_PREFIX))
            .map(|value| {
                V025ParsedTrailer::parse(value).unwrap_err();
                let parsed = ParsedTrailer::parse(value).unwrap();
                assert!(parsed.target_commit.is_some());
                parsed.source_commit
            })
            .collect::<Vec<_>>();
        let observed = encoded_sources.clone();
        encoded_sources.sort();
        assert_eq!(observed, encoded_sources);
    }

    #[test]
    fn v025_predecessor_trailer_only_migrates_without_replay() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let legacy = format!("split\n\n{TRAILER_PREFIX}mono@{source}");
        let target = commit(target_repo.path(), &legacy);
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();

        let mut mappings = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert_eq!(mappings.get_mapping(&source), Some(target.clone()));
        let migration = migrate_checked(
            &mut mappings,
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap()
        .unwrap();

        let migrated = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert_eq!(migrated.get_mapping(&source), Some(target));
        assert!(migrated.has_reverse_mapping(&migration));
        assert!(migrated.v025_migration_candidates().is_empty());

        let mut current_only = MappingStore::new("demo".to_string());
        current_only
            .load_history(source_repo.path(), HistorySide::Source, &target_identity)
            .unwrap();
        current_only
            .load_history(
                target_repo.path(),
                HistorySide::Target,
                source_context.source_repository(),
            )
            .unwrap();
        assert_eq!(current_only.get_mapping(&source), migrated.get_mapping(&source));
        assert!(current_only.has_reverse_mapping(&migration));
        assert!(current_only.source_frontier_commits().is_empty());
        assert!(current_only.target_frontier_commits().is_empty());
        assert!(current_only.v025_mappings.is_empty());
    }

    #[test]
    fn v025_remote_trailer_on_source_side_migrates_in_the_declared_direction() {
        let target_repo = repository();
        let target = commit(target_repo.path(), "target");
        let source_repo = repository();
        let source = commit(
            source_repo.path(),
            &format!("source\n\n{TRAILER_PREFIX}remote@{target}"),
        );
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();

        let mut mappings = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert_eq!(mappings.get_mapping(&source), Some(target.clone()));
        assert!(
            migrate_checked(
                &mut mappings,
                source_repo.path(),
                target_repo.path(),
                &source_context,
                &target_identity,
            )
            .unwrap()
            .is_some()
        );
        assert_eq!(mappings.get_mapping(&source), Some(target));
        assert!(mappings.source_frontier_commits().is_empty());
        assert!(mappings.target_frontier_commits().is_empty());
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
        commit(transform_repo.path(), &format!("migration\n\n{invalid_transform}"));
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
        commit(ancestry_repo.path(), &format!("migration\n\n{invalid_ancestry}"));
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
    fn v025_conflicting_note_and_trailer_fail_before_head_mutation() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let legacy_target = commit(target_repo.path(), &format!("split\n\n{TRAILER_PREFIX}mono@{source}"));
        let note_target = commit(target_repo.path(), "different target");
        import_source_and_add_note(source_repo.path(), target_repo.path(), &source, &note_target);
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();
        let before_head = git(target_repo.path(), &["rev-parse", "HEAD"]);

        let error = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap_err();
        assert!(error.to_string().contains("maps to both"));
        assert_ne!(legacy_target, note_target);
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD"]), before_head);
    }

    #[test]
    fn mixed_case_object_ids_coalesce_and_still_reject_divergent_evidence() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let source_upper = source.to_ascii_uppercase();
        let target_repo = repository();
        let target = commit(
            target_repo.path(),
            &format!("split\n\n{TRAILER_PREFIX}mono@{source_upper}"),
        );
        let note = format!(
            "{V025_NOTE_SCHEMA}\nsource={}\ntarget={}",
            source_upper,
            target.to_ascii_uppercase()
        );
        import_source_and_add_note(source_repo.path(), target_repo.path(), &source, &note);
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();

        let mappings = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert_eq!(mappings.get_mapping(&source_upper), Some(target.clone()));
        assert_eq!(
            mappings.get_reverse_mapping(&target.to_ascii_uppercase()),
            Some(source.clone())
        );
        assert!(mappings.has_mapping(&source_upper));
        assert!(mappings.has_reverse_mapping(&target.to_ascii_uppercase()));
        assert_eq!(mappings.v025_migration_candidates(), vec![(source.clone(), target)]);

        let divergent_target = commit(target_repo.path(), "different target");
        git(
            target_repo.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/demo",
                "add",
                "-f",
                "-m",
                &format!(
                    "{V025_NOTE_SCHEMA}\nsource={}\ntarget={}",
                    source_upper,
                    divergent_target.to_ascii_uppercase()
                ),
                &source,
            ],
        );
        let before_head = git(target_repo.path(), &["rev-parse", "HEAD"]);
        let error = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap_err();
        assert!(error.to_string().contains("maps to both"));
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD"]), before_head);
    }

    #[test]
    fn v025_checked_migration_rejects_candidate_drift_before_head_cas() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let original_target = commit(target_repo.path(), "original target");
        let replacement_target = commit(target_repo.path(), "replacement target");
        import_source_and_add_note(source_repo.path(), target_repo.path(), &source, &original_target);
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();
        let target_root = utils::canonicalize_existing(target_repo.path()).unwrap();
        let (_, expected) = MappingStore::capture_v025_authority(
            source_repo.path(),
            &target_root,
            &source_context,
            &target_identity,
            &target_root,
            "main",
            "mono_to_remote",
        )
        .unwrap();

        git(
            target_repo.path(),
            &[
                "notes",
                "--ref",
                "refs/notes/rail/demo",
                "add",
                "-f",
                "-m",
                &replacement_target,
                &source,
            ],
        );
        let before_head = git(target_repo.path(), &["rev-parse", "HEAD"]);
        let mut mappings = MappingStore::new("demo".to_string());
        let error = mappings
            .migrate_v025_evidence_bound(
                source_repo.path(),
                &target_root,
                &source_context,
                &target_identity,
                &target_root,
                "main",
                "mono_to_remote",
                Some(&expected),
            )
            .unwrap_err();
        assert!(error.to_string().contains("changed after it was checked"));
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD"]), before_head);
    }

    #[test]
    fn v025_prepared_migration_recovers_after_ref_cas_exactly_once() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let target = commit(target_repo.path(), "target");
        let note = format!("{V025_NOTE_SCHEMA}\nsource={source}\ntarget={target}");
        import_source_and_add_note(source_repo.path(), target_repo.path(), &source, &note);
        let source_context = OriginContext::discover(source_repo.path(), "demo", "sha256-stable-policy").unwrap();
        let target_identity = repository_identity(target_repo.path()).unwrap();
        let target_root = utils::canonicalize_existing(target_repo.path()).unwrap();
        let (_, expected) = MappingStore::capture_v025_authority(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
            &target_root,
            "main",
            "mono_to_remote",
        )
        .unwrap();
        let before = git(target_repo.path(), &["rev-parse", "HEAD"]);
        FAIL_V025_MIGRATION_AFTER_REF_CAS.set(true);
        let mut mappings = MappingStore::new("demo".to_string());
        let error = mappings
            .migrate_v025_evidence_bound(
                source_repo.path(),
                target_repo.path(),
                &source_context,
                &target_identity,
                &target_root,
                "main",
                "mono_to_remote",
                Some(&expected),
            )
            .unwrap_err();
        assert!(error.to_string().contains("injected interruption"), "{error}");
        let migrated = git(target_repo.path(), &["rev-parse", "HEAD"]);
        assert_ne!(migrated, before);
        assert_eq!(
            git(
                target_repo.path(),
                &["rev-list", "--count", &format!("{before}..{migrated}")],
            ),
            "1"
        );
        let target_git = SystemGit::open(target_repo.path()).unwrap();
        assert_eq!(
            GitEffectStore::discover_active_read_only(&target_git).unwrap().len(),
            1,
            "the ref-CAS interruption must leave exact durable recovery authority",
        );

        let resumed = mappings
            .migrate_v025_evidence_bound(
                source_repo.path(),
                target_repo.path(),
                &source_context,
                &target_identity,
                &target_root,
                "main",
                "mono_to_remote",
                Some(&expected),
            )
            .unwrap();
        assert_eq!(resumed.as_deref(), Some(migrated.as_str()));
        assert_eq!(git(target_repo.path(), &["rev-parse", "HEAD"]), migrated);
        assert!(
            GitEffectStore::discover_active_read_only(&target_git)
                .unwrap()
                .is_empty()
        );
        acknowledge_completed_test_effect(target_repo.path());

        let (_, current) = MappingStore::capture_v025_authority(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
            &target_root,
            "main",
            "mono_to_remote",
        )
        .unwrap();
        assert_eq!(current, expected.after_migration(&migrated).unwrap());
        assert_eq!(
            mappings
                .migrate_v025_evidence_bound(
                    source_repo.path(),
                    target_repo.path(),
                    &source_context,
                    &target_identity,
                    &target_root,
                    "main",
                    "mono_to_remote",
                    Some(&current),
                )
                .unwrap(),
            None,
        );
    }

    #[test]
    fn v025_large_migration_message_bypasses_process_argument_limits() {
        const MAPPING_COUNT: usize = 4_096;

        let target_repo = repository();
        let head = commit(target_repo.path(), "target head");
        let git_backend = SystemGit::open(target_repo.path()).unwrap();
        let metadata = git_backend.get_commit(&head).unwrap().metadata();
        let tree_spec = format!("HEAD^{}tree{}", '{', '}');
        let tree = git(target_repo.path(), &["rev-parse", &tree_spec]);
        let source_context = OriginContext::new(repository_id('a'), "large-owner", "v1-sha256-large").unwrap();
        let trailers = (0..MAPPING_COUNT)
            .map(|index| {
                let source = format!("{:040x}", index + 1);
                let target = format!("{:040x}", MAPPING_COUNT + index + 1);
                explicit_pair_trailer(&source_context, &source, &target).unwrap()
            })
            .collect::<Vec<_>>();
        let message = append_origin_trailers(V025_MIGRATION_SUBJECT, &trailers);
        assert!(
            message.len() > 512 * 1_024,
            "fixture must exceed conservative single-argument process limits"
        );

        let migration = write_v025_migration_commit(target_repo.path(), &tree, &head, &message, &metadata).unwrap();
        let repeated = write_v025_migration_commit(target_repo.path(), &tree, &head, &message, &metadata).unwrap();
        assert_eq!(
            migration, repeated,
            "stdin transport must preserve deterministic commits"
        );
        let committed_message = git(target_repo.path(), &["show", "-s", "--format=%B", &migration]);
        assert_eq!(origin_trailer_values(&committed_message).len(), MAPPING_COUNT);
        assert_eq!(
            origin_trailer_values(&committed_message).last().copied(),
            trailers.last().map(|value| {
                value
                    .strip_prefix(TRAILER_PREFIX)
                    .expect("generated migration trailer must use the current prefix")
            })
        );
    }

    #[test]
    fn v025_wrong_side_and_unrelated_identity_never_grant_mapping_authority() {
        let source_repo = repository();
        let source = commit(source_repo.path(), "source");
        let target_repo = repository();
        let target = commit(
            target_repo.path(),
            &format!("wrong direction\n\n{TRAILER_PREFIX}remote@{source}"),
        );
        let target_identity = repository_identity(target_repo.path()).unwrap();
        let source_context = OriginContext::discover(source_repo.path(), "demo", "v1-sha256-current").unwrap();
        let unrelated = OriginContext::new(repository_id('f'), "demo", "v1-sha256-old").unwrap();
        commit(
            target_repo.path(),
            &format!(
                "unrelated\n\n{}",
                explicit_pair_trailer(&unrelated, &source, &target).unwrap()
            ),
        );

        let mappings = MappingStore::capture_v025_evidence(
            source_repo.path(),
            target_repo.path(),
            &source_context,
            &target_identity,
        )
        .unwrap();
        assert_eq!(mappings.count(), 0);
        assert!(mappings.v025_migration_candidates().is_empty());
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
