//! Git-native split/sync origin mapping.
//!
//! Synthesized commits carry a versioned `Rail-Origin` trailer. Ordinary clone
//! history is therefore sufficient to recover source/target mappings. Legacy
//! `refs/notes/rail/*` values are read only as migration evidence and are never
//! fetched, pushed, or written by normal operation.

use std::collections::BTreeSet;
use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::error::{GitError, RailError, RailResult, ResultExt, git_command_diagnostics};
use crate::git::{SystemGit, git_cmd_for_path};
use crate::source::ContentDigest;
use crate::utils;

const NOTE_SCHEMA: &str = "cargo-rail-mapping-v1";
const TRAILER_PREFIX: &str = "Rail-Origin: ";
const TRAILER_SCHEMA: &str = "v1";

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
    /// Bind a source repository, split owner, and workspace snapshot.
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
        self.format_trailer(source_commit, None, true)
    }

    /// Format provenance for a synthesized commit that does not define a new mapping.
    pub fn evidence_trailer(&self, source_commit: &str) -> RailResult<String> {
        self.format_trailer(source_commit, None, false)
    }

    fn migration_trailer(&self, source_commit: &str, target_commit: &str) -> RailResult<String> {
        self.format_trailer(source_commit, Some(target_commit), true)
    }

    fn format_trailer(&self, source_commit: &str, target_commit: Option<&str>, mapping: bool) -> RailResult<String> {
        validate_object_id("source", source_commit)?;
        if let Some(target) = target_commit {
            validate_object_id("target", target)?;
        }
        let mut trailer = format!(
            "{TRAILER_PREFIX}{TRAILER_SCHEMA} source={} commit={} owner={} snapshot={} transform={TRANSFORM_VERSION}",
            self.source_repository,
            source_commit,
            encode_hex(self.owner.as_bytes()),
            self.ownership_snapshot,
        );
        if let Some(target) = target_commit {
            trailer.push_str(" target=");
            trailer.push_str(target);
        } else if !mapping {
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
    let identity_input = match git.get_config("remote.origin.url")? {
        Some(url) if !utils::is_local_path(&url) => format!("remote\0{}", normalize_remote_url(&url)?),
        _ => {
            let output = git_cmd_for_path(repo_path)
                .args(["rev-list", "--max-parents=0", "HEAD"])
                .output()
                .context("Failed to discover Git root commits")?;
            if !output.status.success() {
                return Err(RailError::message(format!(
                    "failed to discover repository identity: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            let roots_output = String::from_utf8_lossy(&output.stdout);
            let mut roots = roots_output
                .lines()
                .map(str::trim)
                .filter(|root| !root.is_empty())
                .collect::<Vec<_>>();
            roots.sort_unstable();
            if roots.is_empty() {
                return Err(RailError::message(
                    "cannot identify a repository without a remote or root commit",
                ));
            }
            for root in &roots {
                validate_object_id("root", root)?;
            }
            format!("roots\0{}", roots.join("\n"))
        }
    };
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitMapping {
    source: String,
    target: String,
}

impl CommitMapping {
    fn new(source: &str, target: &str) -> RailResult<Self> {
        validate_object_id("source", source)?;
        validate_object_id("target", target)?;
        Ok(Self {
            source: source.to_string(),
            target: target.to_string(),
        })
    }

    fn decode_note(note_target: &str, content: &str) -> RailResult<Self> {
        let lines = content.lines().collect::<Vec<_>>();
        if lines.first().copied() != Some(NOTE_SCHEMA) {
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
        if source != note_target {
            return Err(mapping_resolution_error(
                note_target,
                "the note attachment and declared source commit differ",
            ));
        }
        Self::new(source, target)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedTrailer {
    Versioned {
        source_repository: String,
        source_commit: String,
        owner: String,
        ownership_snapshot: String,
        transform_version: u32,
        target_commit: Option<String>,
        mapping: bool,
    },
    Legacy {
        side: HistorySide,
        source_commit: String,
    },
}

impl ParsedTrailer {
    fn parse(value: &str) -> RailResult<Self> {
        if let Some(source_commit) = value.strip_prefix("mono@") {
            validate_object_id("legacy mono origin", source_commit)?;
            return Ok(Self::Legacy {
                side: HistorySide::Target,
                source_commit: source_commit.to_string(),
            });
        }
        if let Some(source_commit) = value.strip_prefix("remote@") {
            validate_object_id("legacy remote origin", source_commit)?;
            return Ok(Self::Legacy {
                side: HistorySide::Source,
                source_commit: source_commit.to_string(),
            });
        }

        let mut fields = value.split_whitespace();
        if fields.next() != Some(TRAILER_SCHEMA) {
            return Err(RailError::message(format!(
                "unsupported Rail-Origin trailer '{}'",
                value
            )));
        }
        let source_repository = parse_field(fields.next(), "source")?.to_string();
        validate_repository_identity(&source_repository)?;
        let source_commit = parse_field(fields.next(), "commit")?.to_string();
        validate_object_id("source", &source_commit)?;
        let owner = decode_hex(parse_field(fields.next(), "owner")?)?;
        let ownership_snapshot = parse_field(fields.next(), "snapshot")?.to_string();
        validate_token("ownership snapshot", &ownership_snapshot)?;
        let transform_version = parse_field(fields.next(), "transform")?
            .parse::<u32>()
            .map_err(|_| RailError::message("Rail-Origin transform must be an unsigned integer"))?;
        let optional = fields.next();
        let mapping = optional != Some("mapping=evidence");
        let target_commit = match optional {
            Some("mapping=evidence") | None => None,
            Some(field) => Some(parse_field(Some(field), "target")?.to_string()),
        };
        if let Some(target) = &target_commit {
            validate_object_id("target", target)?;
        }
        if fields.next().is_some() {
            return Err(RailError::message("Rail-Origin trailer has unknown fields"));
        }
        Ok(Self::Versioned {
            source_repository,
            source_commit,
            owner,
            ownership_snapshot,
            transform_version,
            target_commit,
            mapping,
        })
    }
}

fn parse_field<'a>(field: Option<&'a str>, name: &str) -> RailResult<&'a str> {
    field
        .and_then(|field| field.strip_prefix(name).and_then(|value| value.strip_prefix('=')))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RailError::message(format!("Rail-Origin trailer is missing {name}")))
}

/// One-to-one source/target mapping recovered from history and legacy notes.
#[derive(Debug)]
pub struct MappingStore {
    owner: String,
    mappings: FxHashMap<String, String>,
    reverse_mappings: FxHashMap<String, String>,
    history_mappings: FxHashSet<(String, String)>,
    note_mappings: BTreeSet<(String, String)>,
    source_evidence: FxHashSet<String>,
    target_evidence: FxHashSet<String>,
}

impl MappingStore {
    /// Create an empty store scoped to one split owner.
    pub fn new(owner: String) -> Self {
        Self {
            owner,
            mappings: FxHashMap::default(),
            reverse_mappings: FxHashMap::default(),
            history_mappings: FxHashSet::default(),
            note_mappings: BTreeSet::new(),
            source_evidence: FxHashSet::default(),
            target_evidence: FxHashSet::default(),
        }
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

        for commit in &commits {
            for value in origin_trailer_values(&commit.message) {
                match ParsedTrailer::parse(value)? {
                    ParsedTrailer::Legacy {
                        side: trailer_side,
                        source_commit,
                    } if side == trailer_side => {
                        let mapping = match side {
                            HistorySide::Source => CommitMapping::new(&commit.sha, &source_commit)?,
                            HistorySide::Target => CommitMapping::new(&source_commit, &commit.sha)?,
                        };
                        self.record_history(mapping)?;
                    }
                    ParsedTrailer::Legacy { .. } => {}
                    ParsedTrailer::Versioned {
                        source_repository,
                        source_commit,
                        owner,
                        ownership_snapshot,
                        transform_version,
                        target_commit,
                        mapping,
                    } => {
                        if owner != self.owner || source_repository != expected_source_repository {
                            continue;
                        }
                        if transform_version != TRANSFORM_VERSION {
                            return Err(RailError::message(format!(
                                "unsupported Rail-Origin transform version {} for '{}'",
                                transform_version, self.owner
                            )));
                        }
                        validate_token("ownership snapshot", &ownership_snapshot)?;
                        if !mapping {
                            match side {
                                HistorySide::Source => self.source_evidence.insert(commit.sha.clone()),
                                HistorySide::Target => self.target_evidence.insert(commit.sha.clone()),
                            };
                            continue;
                        }
                        let mapping = match side {
                            HistorySide::Source => {
                                if target_commit.is_some() {
                                    return Err(mapping_resolution_error(
                                        &commit.sha,
                                        "a source-history trailer cannot override its target commit",
                                    ));
                                }
                                CommitMapping::new(&commit.sha, &source_commit)?
                            }
                            HistorySide::Target => {
                                let target = target_commit.as_deref().unwrap_or(&commit.sha);
                                if !git.is_ancestor(target, &commit.sha) {
                                    return Err(mapping_resolution_error(
                                        &source_commit,
                                        &format!(
                                            "migration target '{}' is not an ancestor of its trailer commit",
                                            target
                                        ),
                                    ));
                                }
                                CommitMapping::new(&source_commit, target)?
                            }
                        };
                        self.record_history(mapping)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Load the legacy notes ref without fetching or mutating it.
    pub fn load_legacy_notes(&mut self, repo_path: &Path) -> RailResult<()> {
        let notes_ref = format!("refs/notes/rail/{}", self.owner);
        let output = git_cmd_for_path(repo_path)
            .args(["notes", "--ref", &notes_ref, "list"])
            .output()
            .context("Failed to list legacy mapping notes")?;
        if !output.status.success() {
            return Ok(());
        }
        let entries = String::from_utf8_lossy(&output.stdout)
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
            let mapping = CommitMapping::decode_note(&source, content.trim())?;
            self.record_mapping(&mapping.source, &mapping.target)?;
            self.note_mappings.insert((mapping.source, mapping.target));
        }
        Ok(())
    }

    fn record_history(&mut self, mapping: CommitMapping) -> RailResult<()> {
        self.record_mapping(&mapping.source, &mapping.target)?;
        self.history_mappings.insert((mapping.source, mapping.target));
        Ok(())
    }

    /// Record one proven source-to-target mapping.
    pub fn record_mapping(&mut self, from_sha: &str, to_sha: &str) -> RailResult<()> {
        let mapping = CommitMapping::new(from_sha, to_sha)?;
        if let Some(existing) = self.mappings.get(&mapping.source)
            && existing != &mapping.target
        {
            return Err(mapping_resolution_error(
                &mapping.source,
                &format!("it maps to both '{}' and '{}'", existing, mapping.target),
            ));
        }
        if let Some(existing) = self.reverse_mappings.get(&mapping.target)
            && existing != &mapping.source
        {
            return Err(mapping_resolution_error(
                &mapping.source,
                &format!(
                    "target '{}' is already mapped from source '{}'",
                    mapping.target, existing
                ),
            ));
        }
        self.reverse_mappings
            .insert(mapping.target.clone(), mapping.source.clone());
        self.mappings.insert(mapping.source, mapping.target);
        Ok(())
    }

    /// Return the mapped target commit, when known.
    pub fn get_mapping(&self, sha: &str) -> Option<String> {
        self.mappings.get(sha).cloned()
    }

    /// Return the source commit mapped to a target commit, when known.
    pub fn get_reverse_mapping(&self, sha: &str) -> Option<String> {
        self.reverse_mappings.get(sha).cloned()
    }

    /// Whether a source commit has a target mapping.
    pub fn has_mapping(&self, sha: &str) -> bool {
        self.mappings.contains_key(sha) || self.source_evidence.contains(sha)
    }

    /// Whether a target commit has a source mapping.
    pub fn has_reverse_mapping(&self, sha: &str) -> bool {
        self.reverse_mappings.contains_key(sha) || self.target_evidence.contains(sha)
    }

    /// Number of mappings recovered from all accepted evidence.
    pub fn count(&self) -> usize {
        self.mappings.len()
    }

    /// Legacy mappings that ordinary history does not yet carry.
    pub fn legacy_mappings(&self) -> Vec<(String, String)> {
        self.note_mappings
            .iter()
            .filter(|mapping| !self.history_mappings.contains(*mapping))
            .cloned()
            .collect()
    }

    /// Format deterministic migration trailers for every notes-only mapping.
    pub fn legacy_migration_trailers(&self, context: &OriginContext) -> RailResult<Vec<String>> {
        self.legacy_mappings()
            .into_iter()
            .map(|(source, target)| context.migration_trailer(&source, &target))
            .collect()
    }

    /// Persist every notes-only mapping in one deterministic empty history commit.
    ///
    /// The commit reuses the exact `HEAD` tree and identity metadata. `update-ref`
    /// is compare-and-swap guarded, and the worktree/index are not rewritten.
    pub fn migrate_legacy_mappings(&mut self, repo_path: &Path, context: &OriginContext) -> RailResult<Option<String>> {
        let legacy = self.legacy_mappings();
        if legacy.is_empty() {
            return Ok(None);
        }
        let git = SystemGit::open(repo_path)?;
        let head = git.head_commit()?;
        let parent = git.get_commit(&head)?;
        const HEAD_TREE: &str = "HEAD^\u{7b}tree\u{7d}";
        let tree = git_cmd_for_path(repo_path)
            .args(["rev-parse", HEAD_TREE])
            .output()
            .context("Failed to resolve migration tree")?;
        if !tree.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: format!("git rev-parse {HEAD_TREE}"),
                stderr: git_command_diagnostics(&tree.stdout, &tree.stderr),
            }));
        }
        let tree = String::from_utf8(tree.stdout)?.trim().to_string();
        validate_object_id("tree", &tree)?;
        let trailers = legacy
            .iter()
            .map(|(source, target)| context.migration_trailer(source, target))
            .collect::<RailResult<Vec<_>>>()?;
        let message = append_origin_trailers("chore: migrate cargo-rail origin mappings", &trailers);
        let metadata = parent.metadata();
        let author_date = format!("{} {}", metadata.author_timestamp, metadata.author_timezone);
        let committer_date = format!("{} {}", metadata.committer_timestamp, metadata.committer_timezone);
        let output = git_cmd_for_path(repo_path)
            .env("GIT_AUTHOR_NAME", &metadata.author)
            .env("GIT_AUTHOR_EMAIL", &metadata.author_email)
            .env("GIT_AUTHOR_DATE", &author_date)
            .env("GIT_COMMITTER_NAME", &metadata.committer)
            .env("GIT_COMMITTER_EMAIL", &metadata.committer_email)
            .env("GIT_COMMITTER_DATE", &committer_date)
            .args(["commit-tree", &tree, "-p", &head, "-m", &message])
            .output()
            .context("Failed to create legacy mapping migration commit")?;
        if !output.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git commit-tree".to_string(),
                stderr: git_command_diagnostics(&output.stdout, &output.stderr),
            }));
        }
        let commit = String::from_utf8(output.stdout)?.trim().to_string();
        validate_object_id("migration", &commit)?;
        let update = git_cmd_for_path(repo_path)
            .args(["update-ref", "HEAD", &commit, &head])
            .output()
            .context("Failed to publish legacy mapping migration commit")?;
        if !update.status.success() {
            return Err(RailError::Git(GitError::CommandFailed {
                command: "git update-ref HEAD".to_string(),
                stderr: git_command_diagnostics(&update.stdout, &update.stderr),
            }));
        }
        self.history_mappings.extend(legacy);
        Ok(Some(commit))
    }
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
        "inspect ordinary Rail-Origin trailers and any legacy refs/notes/rail mapping, then choose one target commit; cargo-rail never merges divergent mappings automatically",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(digit: char) -> String {
        std::iter::repeat_n(digit, 40).collect()
    }

    fn repository_id(digit: char) -> String {
        format!("sha256-{}", std::iter::repeat_n(digit, 64).collect::<String>())
    }

    #[test]
    fn origin_trailer_round_trips_required_identity() {
        let context = OriginContext::new(repository_id('a'), "demo", "v1-sha256-snapshot").unwrap();
        let source = oid('b');
        let trailer = context.trailer(&source).unwrap();
        let parsed = ParsedTrailer::parse(trailer.strip_prefix(TRAILER_PREFIX).unwrap()).unwrap();
        assert_eq!(
            parsed,
            ParsedTrailer::Versioned {
                source_repository: repository_id('a'),
                source_commit: source,
                owner: "demo".to_string(),
                ownership_snapshot: "v1-sha256-snapshot".to_string(),
                transform_version: TRANSFORM_VERSION,
                target_commit: None,
                mapping: true,
            }
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
        assert!(trailers[0].starts_with("v1 "));
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
    fn legacy_migration_trailers_preserve_exact_note_mapping() {
        let mut store = MappingStore::new("demo".to_string());
        let source = oid('a');
        let target = oid('b');
        store.note_mappings.insert((source.clone(), target.clone()));
        store.record_mapping(&source, &target).unwrap();
        let context = OriginContext::new(repository_id('c'), "demo", "v1-sha256-snapshot").unwrap();
        let trailers = store.legacy_migration_trailers(&context).unwrap();
        assert_eq!(trailers.len(), 1);
        assert!(trailers[0].contains(&format!("commit={source}")));
        assert!(trailers[0].contains(&format!("target={target}")));
    }

    #[test]
    fn remote_normalization_removes_http_credentials_and_query() {
        assert_eq!(
            normalize_remote_url("https://token@example.com/Org/repo.git?secret=value").unwrap(),
            "https://example.com/Org/repo"
        );
    }
}
