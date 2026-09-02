//! Common utilities and builders for command implementations

use crate::config::{RailConfig, SplitConfig as ConfigSplitConfig};
use crate::error::{ConfigError, RailError, RailResult};
use crate::git::mappings::{
    MappingAuthoritySnapshot, MappingStore, OriginContext, TargetPublicationSnapshot, observe_target_branch,
    remote_endpoint_identity, remote_repository_identity, repository_identity,
};
use crate::mutation::git_effect::{GitEffectJournal, GitEffectStore, ordered_mapping_effect_indices};
use crate::split::{ReleaseBoundary, SplitOwnership, SplitParams};
use crate::sync::SyncConfig;
use crate::workspace::WorkspaceContext;
use clap::ValueEnum;
use glob::Pattern;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn stable_split_ownership_digest(
    workspace_root: &std::path::Path,
    split_config: &ConfigSplitConfig,
    crate_paths: &[PathBuf],
    ownership: &SplitOwnership,
    transform_policy_digest: &str,
) -> RailResult<String> {
    fn frame(bytes: &mut Vec<u8>, label: &[u8], value: &[u8]) {
        bytes.extend_from_slice(&(label.len() as u64).to_be_bytes());
        bytes.extend_from_slice(label);
        bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
        bytes.extend_from_slice(value);
    }
    fn relative_git_path(workspace_root: &std::path::Path, path: &std::path::Path) -> RailResult<String> {
        let relative = path.strip_prefix(workspace_root).unwrap_or(path);
        Ok(crate::utils::path_to_git_format(relative))
    }

    let mut canonical = b"cargo-rail-split-ownership-v1".to_vec();
    frame(&mut canonical, b"name", split_config.name.as_bytes());
    frame(
        &mut canonical,
        b"mode",
        match split_config.mode {
            crate::config::SplitMode::Single => b"single",
            crate::config::SplitMode::Combined => b"combined",
        },
    );
    frame(
        &mut canonical,
        b"workspace-mode",
        match split_config.workspace_mode {
            crate::config::WorkspaceMode::Standalone => b"standalone",
            crate::config::WorkspaceMode::Workspace => b"workspace",
        },
    );
    frame(&mut canonical, b"transform", b"1");
    frame(&mut canonical, b"transform-policy", transform_policy_digest.as_bytes());
    for member in &ownership.members {
        frame(&mut canonical, b"member", member.as_bytes());
    }
    for dependency in &ownership.dependency_closure {
        frame(&mut canonical, b"dependency", dependency.as_bytes());
    }
    for boundary in &ownership.release_boundaries {
        frame(&mut canonical, b"release-boundary", boundary.name.as_bytes());
        for member in &boundary.members {
            frame(&mut canonical, b"release-member", member.as_bytes());
        }
    }
    let mut roots = crate_paths
        .iter()
        .map(|path| relative_git_path(workspace_root, path))
        .collect::<RailResult<Vec<_>>>()?;
    roots.sort();
    for root in roots {
        frame(&mut canonical, b"root", root.as_bytes());
    }
    let mut includes = split_config.include.clone();
    includes.sort();
    for include in includes {
        frame(&mut canonical, b"include", include.as_bytes());
    }
    let mut excludes = split_config.exclude.clone();
    excludes.sort();
    for exclude in excludes {
        frame(&mut canonical, b"exclude", exclude.as_bytes());
    }
    Ok(format!("sha256-{}", crate::source::ContentDigest::sha256(&canonical)))
}

/// Render a deterministic preview for a potentially large list.
///
/// Keeps small lists intact and truncates large lists with a `+N more` suffix.
pub(crate) fn format_preview_list<T: AsRef<str>>(items: &[T], preview_limit: usize) -> String {
    if items.is_empty() {
        return "none".to_string();
    }

    let preview_limit = preview_limit.max(1);
    let preview = items
        .iter()
        .take(preview_limit)
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ");

    if items.len() <= preview_limit {
        preview
    } else {
        format!("{preview}, ... +{} more", items.len() - preview_limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SplitMappingSnapshot {
    pub(crate) mapping_count: usize,
    pub(crate) origin_migration: MappingAuthoritySnapshot,
    pub(crate) publication: Option<TargetPublicationSnapshot>,
    pub(crate) prepared_effects: Vec<serde_json::Value>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the snapshot binds each independent repository and publication authority explicitly"
)]
pub(crate) fn split_mapping_snapshot(
    workspace_root: &std::path::Path,
    crate_name: &str,
    ownership_snapshot: &str,
    target_repo_path: &std::path::Path,
    branch: &str,
    direction: &str,
    effect_family: &str,
    remote_url: Option<&str>,
) -> RailResult<SplitMappingSnapshot> {
    let source_git = crate::git::SystemGit::open(workspace_root)?;
    let source_head = source_git.head_commit()?;
    let source_identity = crate::git::mappings::repository_identity_from_git(&source_git, Some(&source_head))?;
    let source_origin = OriginContext::new(source_identity, crate_name, ownership_snapshot)?;
    let publication_observation = remote_url
        .filter(|remote| !crate::utils::is_local_path(remote))
        .map(|remote| observe_target_branch(workspace_root, target_repo_path, remote, branch))
        .transpose()?;
    if !target_repo_path.join(".git").exists() {
        let help = if publication_observation
            .as_ref()
            .and_then(crate::git::mappings::TargetBranchObservation::remote_head)
            .is_some()
        {
            "clone or fetch the configured remote branch into that exact target directory, then rerun check".to_string()
        } else if remote_url.is_some_and(|remote| !crate::utils::is_local_path(remote)) {
            format!(
                "initialize that exact existing empty directory with the configured branch and identity, then add the configured endpoint: git init -b {branch} '{}' && git -C '{}' remote add origin '{}'",
                target_repo_path.display(),
                target_repo_path.display(),
                remote_url.unwrap_or_default(),
            )
        } else {
            format!(
                "initialize that exact existing empty directory first, for example: git init -b {branch} '{}'; cargo-rail never creates or initializes configured targets",
                target_repo_path.display(),
            )
        };
        return Err(RailError::with_help(
            format!(
                "split target '{}' is not an initialized Git repository",
                target_repo_path.display()
            ),
            help,
        ));
    }
    let target_git = crate::git::SystemGit::open(target_repo_path)?;
    let dirty_target_paths = target_git.obstructing_worktree_paths()?;
    if !dirty_target_paths.is_empty() && !permits_prepared_target_recovery(&target_git)? {
        return Err(RailError::with_help(
            format!(
                "split target has staged, unstaged, or untracked work: {}",
                dirty_target_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "commit or restore target work before split/sync planning; source --allow-dirty never authorizes target resets",
        ));
    }
    let ref_name = format!("refs/heads/{branch}");
    let prepared_journals = GitEffectStore::discover_unacknowledged_read_only(&target_git)?
        .into_iter()
        .filter(|journal| journal.repository().ref_name == ref_name)
        .collect::<Vec<_>>();
    for journal in &prepared_journals {
        let mapping_operation_matches = match effect_family {
            "split" => journal.operation_id().starts_with("split-chain-sha256-"),
            "sync" => journal
                .operation_id()
                .starts_with(&format!("sync-to-remote-{crate_name}-")),
            _ => false,
        };
        let publication_operation_matches = match effect_family {
            "split" => journal
                .operation_id()
                .starts_with(&format!("split-publication-{crate_name}-sha256-")),
            "sync" => journal
                .operation_id()
                .starts_with(&format!("sync-publication-{crate_name}-sha256-")),
            _ => false,
        };
        let mapping_matches = journal.mapping().is_some_and(|mapping| {
            mapping.owner() == crate_name
                && mapping.ownership_snapshot() == ownership_snapshot
                && (mapping_operation_matches || journal.operation_id().starts_with("origin-migration-"))
        });
        let publication_matches =
            journal.mapping().is_none() && journal.publication().is_some() && publication_operation_matches;
        if !mapping_matches && !publication_matches {
            return Err(RailError::with_help(
                format!(
                    "split target branch '{ref_name}' has unrelated unacknowledged effect '{}'",
                    journal.effect_id()
                ),
                "finish or reconcile that exact prepared effect before planning split",
            ));
        }
        if journal.mapping().is_none() && !journal.permits_local_recovery_state(&target_git)? {
            return Err(RailError::with_help(
                format!(
                    "prepared split effect '{}' no longer matches its exact old/result path and ref authority",
                    journal.effect_id()
                ),
                "restore the exact journaled branch, index, and worktree images before retrying",
            ));
        }
    }
    let mapping_journals = prepared_journals
        .iter()
        .filter(|journal| journal.mapping().is_some())
        .cloned()
        .collect::<Vec<_>>();
    let mapping_order = ordered_mapping_effect_indices(&mapping_journals)?;
    let ordered_mapping_journals = mapping_order
        .iter()
        .map(|index| &mapping_journals[*index])
        .collect::<Vec<_>>();
    for pair in ordered_mapping_journals.windows(2) {
        let previous = pair[0];
        let next = pair[1];
        let previous_repository = previous.repository();
        let next_repository = next.repository();
        if !previous.is_terminal()
            || previous_repository.common_dir_identity != next_repository.common_dir_identity
            || previous_repository.worktree_identity != next_repository.worktree_identity
            || previous_repository.logical_repository != next_repository.logical_repository
            || previous_repository.object_format != next_repository.object_format
            || previous_repository.ref_name != next_repository.ref_name
            || previous_repository.symbolic_head != next_repository.symbolic_head
            || next_repository.expected_oid.as_deref() != Some(previous_repository.result_oid.as_str())
        {
            return Err(RailError::message(format!(
                "split target branch '{ref_name}' has a broken prepared mapping-effect chain"
            )));
        }
    }
    if let Some(terminal) = ordered_mapping_journals.last()
        && !terminal.permits_local_recovery_state(&target_git)?
    {
        return Err(RailError::with_help(
            format!(
                "prepared split effect '{}' no longer matches the terminal old/result path and ref authority",
                terminal.effect_id()
            ),
            "restore the exact journaled branch, index, and worktree images before retrying",
        ));
    }
    let prepared_effects = ordered_mapping_journals
        .iter()
        .map(|journal| prepared_effect_projection(journal))
        .chain(
            prepared_journals
                .iter()
                .filter(|journal| journal.mapping().is_none())
                .map(prepared_effect_projection),
        )
        .collect::<Vec<_>>();
    if let (Some(journal), Some(terminal)) = (ordered_mapping_journals.first(), ordered_mapping_journals.last()) {
        let mapping = journal.mapping().expect("filtered mapping journal");
        let repository = journal.repository();
        let (store, origin_migration) = MappingStore::capture_prepared_authority_at(
            workspace_root,
            target_repo_path,
            &source_origin,
            &repository.logical_repository,
            target_repo_path,
            branch,
            direction,
            repository.expected_oid.as_deref(),
            &terminal.repository().result_oid,
        )?;
        if origin_migration.digest() != mapping.pre_authority() {
            return Err(RailError::with_help(
                format!(
                    "prepared split effect '{}' pre-authority changed: expected '{}', found '{}'",
                    journal.effect_id(),
                    mapping.pre_authority(),
                    origin_migration.digest()
                ),
                "restore the exact journaled predecessor histories before retrying",
            ));
        }
        return Ok(SplitMappingSnapshot {
            mapping_count: store.count(),
            origin_migration,
            publication: split_prepared_publication_snapshot(
                publication_observation.as_ref(),
                target_repo_path,
                Some(&store),
                remote_url,
                &prepared_journals,
            )?,
            prepared_effects,
        });
    }
    let target_head = target_git.head_commit().ok();
    let target_identity = crate::git::mappings::repository_identity_from_git(&target_git, target_head.as_deref())?;
    if publication_observation
        .as_ref()
        .is_some_and(|observation| observation.remote_repository() != target_identity)
    {
        return Err(RailError::with_help(
            "configured split remote does not match the target repository identity",
            "restore the configured remote URL or use the matching target repository",
        ));
    }
    if let Some(remote_head) = publication_observation
        .as_ref()
        .and_then(crate::git::mappings::TargetBranchObservation::remote_head)
        && crate::git::SystemGit::open(target_repo_path)?
            .get_commit(remote_head)
            .is_err()
    {
        return Err(RailError::with_help(
            format!(
                "configured remote branch commit '{}' is absent from the local target object view",
                remote_head
            ),
            format!(
                "fetch it explicitly, for example: git -C '{}' fetch --no-tags <configured-url> refs/heads/{branch}",
                target_repo_path.display()
            ),
        ));
    }
    let target_git = crate::git::SystemGit::open(target_repo_path)?;
    if target_head.is_none() {
        if publication_observation
            .as_ref()
            .and_then(crate::git::mappings::TargetBranchObservation::remote_head)
            .is_some()
        {
            return Err(RailError::with_help(
                "initialized split target is unborn while the configured remote branch has history",
                "fetch and check out the configured remote branch before planning split",
            ));
        }
        if target_git.current_branch()? != branch {
            return Err(RailError::with_help(
                format!("unborn split target is not on configured branch '{branch}'"),
                format!("reinitialize the empty target with: git init -b {branch}"),
            ));
        }
        let origin_migration = MappingAuthoritySnapshot::empty_initialized_from_observed(
            &source_origin,
            source_head,
            target_identity,
            target_repo_path,
            branch,
            direction,
        )?;
        let publication = split_prepared_publication_snapshot(
            publication_observation.as_ref(),
            target_repo_path,
            None,
            remote_url,
            &prepared_journals,
        )?;
        return Ok(SplitMappingSnapshot {
            mapping_count: 0,
            origin_migration,
            publication,
            prepared_effects,
        });
    }
    let (store, origin_migration) = capture_current_mapping_authority(
        workspace_root,
        target_repo_path,
        &source_origin,
        &target_identity,
        branch,
        direction,
        effect_family,
        crate_name,
        publication_observation.as_ref(),
    )?;
    let publication = split_prepared_publication_snapshot(
        publication_observation.as_ref(),
        target_repo_path,
        Some(&store),
        remote_url,
        &prepared_journals,
    )?;
    Ok(SplitMappingSnapshot {
        mapping_count: store.count(),
        origin_migration,
        publication,
        prepared_effects,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the capture boundary keeps each independent authority visible"
)]
fn capture_current_mapping_authority(
    workspace_root: &std::path::Path,
    target_repo_path: &std::path::Path,
    source_origin: &OriginContext,
    target_identity: &str,
    branch: &str,
    direction: &str,
    effect_family: &str,
    crate_name: &str,
    publication_observation: Option<&crate::git::mappings::TargetBranchObservation>,
) -> RailResult<(MappingStore, MappingAuthoritySnapshot)> {
    let selected_target_head = publication_observation.and_then(|observation| observation.effective_head());
    let selected_source_head = if effect_family == "sync" {
        crate::sync::engine::selected_sync_source_head(workspace_root, crate_name, direction)?
    } else {
        None
    };
    if let Some(selected_source_head) = selected_source_head.as_deref() {
        MappingStore::capture_v025_authority_at_source(
            workspace_root,
            target_repo_path,
            source_origin,
            target_identity,
            target_repo_path,
            branch,
            direction,
            selected_source_head,
            selected_target_head,
        )
    } else if let Some(selected_target_head) = selected_target_head {
        MappingStore::capture_v025_authority_at(
            workspace_root,
            target_repo_path,
            source_origin,
            target_identity,
            target_repo_path,
            branch,
            direction,
            selected_target_head,
        )
    } else {
        MappingStore::capture_v025_authority(
            workspace_root,
            target_repo_path,
            source_origin,
            target_identity,
            target_repo_path,
            branch,
            direction,
        )
    }
}

fn split_prepared_publication_snapshot(
    observation: Option<&crate::git::mappings::TargetBranchObservation>,
    target_repo_path: &std::path::Path,
    mappings: Option<&MappingStore>,
    remote_url: Option<&str>,
    journals: &[GitEffectJournal],
) -> RailResult<Option<TargetPublicationSnapshot>> {
    let publication_journals = journals
        .iter()
        .filter(|journal| journal.publication().is_some())
        .collect::<Vec<_>>();
    if publication_journals.len() > 1 {
        return Err(RailError::message(
            "split target has multiple unacknowledged publication effects",
        ));
    }
    let Some(journal) = publication_journals.first() else {
        return observation
            .cloned()
            .map(|observation| TargetPublicationSnapshot::capture(observation, target_repo_path, mappings))
            .transpose();
    };
    let publication = journal.publication().expect("filtered publication journal");
    let observation = observation
        .ok_or_else(|| RailError::message("prepared split publication lost its configured remote observation"))?;
    let remote_url = remote_url
        .filter(|remote| !crate::utils::is_local_path(remote))
        .ok_or_else(|| RailError::message("prepared split publication lost its exact configured endpoint"))?;
    if remote_endpoint_identity(remote_url)? != publication.exact_endpoint_digest() {
        return Err(RailError::with_help(
            "prepared split publication endpoint changed",
            "restore the exact configured remote endpoint before retrying",
        ));
    }
    let mapping_journals = journals
        .iter()
        .filter(|journal| journal.mapping().is_some())
        .cloned()
        .collect::<Vec<_>>();
    let mapping_order = ordered_mapping_effect_indices(&mapping_journals)?;
    let expected_local_head = mapping_order.first().map_or_else(
        || journal.repository().expected_oid.as_deref(),
        |index| mapping_journals[*index].repository().expected_oid.as_deref(),
    );
    let result_local_head = mapping_order.last().map_or_else(
        || journal.repository().result_oid.as_str(),
        |index| mapping_journals[*index].repository().result_oid.as_str(),
    );
    if result_local_head != publication.desired_oid() {
        return Err(RailError::message(
            "prepared publication does not follow the terminal local mapping effect",
        ));
    }
    TargetPublicationSnapshot::capture_prepared_authority(
        observation,
        target_repo_path,
        mappings,
        publication.logical_remote(),
        publication.expected_oid(),
        publication.desired_oid(),
        expected_local_head,
        result_local_head,
    )
    .map(Some)
}

pub(crate) fn prepared_effect_projection(journal: &GitEffectJournal) -> serde_json::Value {
    let repository = journal.repository();
    serde_json::json!({
        "effect_id": journal.effect_id(),
        "operation_id": journal.operation_id(),
        "payload_digest": journal.payload_digest(),
        "repository": {
            "logical_repository": repository.logical_repository,
            "common_dir_identity": repository.common_dir_identity,
            "worktree_identity": repository.worktree_identity,
            "object_format": repository.object_format,
            "ref_name": repository.ref_name,
            "symbolic_head": repository.symbolic_head,
            "expected_oid": repository.expected_oid,
            "result_oid": repository.result_oid,
        },
        "mapping": journal.mapping().map(|mapping| serde_json::json!({
            "owner": mapping.owner(),
            "ownership_snapshot": mapping.ownership_snapshot(),
            "pre_authority": mapping.pre_authority(),
            "post_authority": mapping.post_authority(),
            "migration_digest": mapping.migration_digest(),
            "migration_count": mapping.migration_count(),
        })),
        "path_transition_count": journal.paths().len(),
        "publication": journal.publication().map(|publication| serde_json::json!({
            "logical_remote": publication.logical_remote(),
            "exact_endpoint_digest": publication.exact_endpoint_digest(),
            "ref_name": publication.ref_name(),
            "expected_oid": publication.expected_oid(),
            "desired_oid": publication.desired_oid(),
        })),
    })
}

pub(crate) fn validate_existing_target_before_remote_refresh(
    target_repo_path: &std::path::Path,
    remote_url: Option<&str>,
) -> RailResult<()> {
    if !target_repo_path.join(".git").exists() {
        return Ok(());
    }
    let target_git = crate::git::SystemGit::open(target_repo_path)?;
    let dirty_paths = target_git.obstructing_worktree_paths()?;
    if !dirty_paths.is_empty() && !permits_prepared_target_recovery(&target_git)? {
        return Err(RailError::with_help(
            format!(
                "split/sync target has staged, unstaged, or untracked work: {}",
                dirty_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "commit or restore target work before remote observation; cargo-rail will not fetch or mutate a dirty target",
        ));
    }
    if let Some(remote_url) = remote_url.filter(|remote| !crate::utils::is_local_path(remote))
        && repository_identity(target_repo_path)? != remote_repository_identity(remote_url)?
    {
        return Err(RailError::with_help(
            "configured split remote does not match the target repository identity",
            "restore remote.origin.url to the configured remote before retrying; cargo-rail will not fetch through a drifted remote",
        ));
    }
    Ok(())
}

fn permits_prepared_target_recovery(target: &crate::git::SystemGit) -> RailResult<bool> {
    let active = crate::mutation::git_effect::GitEffectStore::discover_active_read_only(target)?;
    let mut matching = 0usize;
    for journal in active {
        if journal.permits_local_recovery_state(target)? {
            matching += 1;
        }
    }
    match matching {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(RailError::message(
            "target has multiple prepared effects that claim its current local state",
        )),
    }
}

pub(crate) fn origin_authority_json(snapshot: &MappingAuthoritySnapshot) -> serde_json::Value {
    serde_json::json!({
        "digest": snapshot.digest(),
        "migration_digest": snapshot.migration_digest(),
        "direction": snapshot.direction(),
        "target_root": snapshot.target_root(),
        "branch": snapshot.branch(),
        "source_repository": snapshot.source_repository(),
        "source_head": snapshot.source_head(),
        "source_selected_head_count": snapshot.source_selected_head_count(),
        "target_repository": snapshot.target_repository(),
        "target_head": snapshot.target_head(),
        "target_selected_head": snapshot.target_selected_head(),
        "owner": snapshot.owner(),
        "ownership_snapshot": snapshot.ownership_snapshot(),
        "transform_version": snapshot.transform_version(),
        "mapping_count": snapshot.mappings().len(),
        "source_frontier_count": snapshot.source_frontier_count(),
        "target_frontier_count": snapshot.target_frontier_count(),
        "source_evidence_count": snapshot.source_evidence().len(),
        "target_evidence_count": snapshot.target_evidence().len(),
        "pending_candidate_count": snapshot.count(),
    })
}

pub(crate) fn publication_authority_json(snapshot: Option<&TargetPublicationSnapshot>) -> serde_json::Value {
    snapshot.map_or(serde_json::Value::Null, |snapshot| {
        serde_json::json!({
            "digest": snapshot.digest(),
            "relation": snapshot.relation(),
            "remote_repository": snapshot.remote_repository(),
            "remote_head": snapshot.remote_head(),
            "local_head": snapshot.local_head(),
            "pending_owned_commits": snapshot.count(),
        })
    })
}

/// Output format for commands that support only human text and JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum TextJsonOutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// Machine-readable JSON output
    Json,
}

impl TextJsonOutputFormat {
    /// Check if this format is JSON.
    pub fn is_json(&self) -> bool {
        matches!(self, Self::Json)
    }

    /// Check if this format is structured output.
    pub fn is_json_like(&self) -> bool {
        self.is_json()
    }
}

/// Output format for `cargo rail split run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SplitOutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// Machine-readable JSON output
    Json,
    /// Names only, one per line
    #[value(name = "names-only")]
    NamesOnly,
    /// JSON Lines format (one object per line)
    #[value(name = "jsonl")]
    JsonLines,
}

impl SplitOutputFormat {
    /// Check if this format is JSON.
    pub fn is_json(&self) -> bool {
        matches!(self, Self::Json)
    }

    /// Check if this format is structured output.
    pub fn is_json_like(&self) -> bool {
        matches!(self, Self::Json | Self::JsonLines)
    }
}

/// Output format for `cargo rail change`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ChangeOutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// Machine-readable JSON output
    Json,
    /// Names only, one per line
    #[value(name = "names-only")]
    NamesOnly,
}

impl ChangeOutputFormat {
    /// Check if this format is JSON.
    pub fn is_json(&self) -> bool {
        matches!(self, Self::Json)
    }

    /// Check if this format is JSON-like structured output.
    pub fn is_json_like(&self) -> bool {
        self.is_json()
    }
}

/// Output format for `cargo rail unify`.
///
/// `unify` currently supports only text and JSON output. Unlike planner/executor surfaces,
/// it does not produce list-like formats (cargo-args, github, jsonl, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum UnifyOutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// Machine-readable JSON output
    Json,
}

impl UnifyOutputFormat {
    /// Check if this format is JSON
    pub fn is_json(&self) -> bool {
        matches!(self, Self::Json)
    }

    /// Check if this format is a JSON-like structured format
    pub fn is_json_like(&self) -> bool {
        self.is_json()
    }
}

/// Output format for `cargo rail surface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SurfaceOutputFormat {
    /// Human-readable text output (default)
    #[default]
    Text,
    /// Machine-readable JSON output
    Json,
    /// GitHub Actions key/value output
    #[value(name = "github")]
    GitHub,
}

impl SurfaceOutputFormat {
    /// Check if this format is JSON.
    pub fn is_json(&self) -> bool {
        matches!(self, Self::Json)
    }

    /// Check if this format is structured output.
    pub fn is_json_like(&self) -> bool {
        self.is_json()
    }
}

/// Builder for split/sync configurations
///
/// Centralizes the logic for selecting crates (by name, --all, etc.)
/// and building engine-specific configs. Eliminates duplication between
/// split.rs and sync.rs command handlers.
#[derive(Debug)]
pub struct SplitSyncConfigBuilder<'a> {
    ctx: &'a WorkspaceContext,
    config: &'a RailConfig,
    split_configs: Vec<ConfigSplitConfig>,
    remote_override: Option<String>,
}

/// Enforce explicit confirmation for destructive operations in non-interactive contexts.
///
/// Allowed confirmation paths:
/// - interactive prompt is available (`prompt_possible`)
/// - explicit `--yes`
/// - `--plan <path>` (fingerprint-validated mutation plan)
pub fn enforce_safety_gate(
    operation: &str,
    yes: bool,
    plan_path: Option<&std::path::Path>,
    prompt_possible: bool,
) -> RailResult<()> {
    if yes || plan_path.is_some() || prompt_possible {
        return Ok(());
    }

    Err(RailError::with_help(
        format!("{} requires explicit confirmation in non-interactive mode", operation),
        "use --yes to confirm explicitly, or pass --plan <PATH> from a prior --check JSON plan",
    ))
}

impl<'a> SplitSyncConfigBuilder<'a> {
    fn resolve_ownership(&self, split_config: &ConfigSplitConfig) -> RailResult<(Vec<PathBuf>, SplitOwnership)> {
        let snapshot = self.ctx.snapshot()?;
        let graph = snapshot.base_resolution().graph();
        let mut members = split_config.members.clone();
        members.sort();
        let original_len = members.len();
        members.dedup();
        if members.len() != original_len {
            return Err(RailError::Config(ConfigError::InvalidField {
                field: format!("crates.{}.split.members", split_config.name),
                reason: "Cargo member names must be unique".to_string(),
            }));
        }

        let mut crate_paths = Vec::with_capacity(members.len());
        for member in &members {
            let package = graph.workspace_package_by_name(member)?;
            let snapshot_package = snapshot
                .packages()
                .iter()
                .find(|candidate| candidate.id() == &package.id && candidate.is_workspace_member())
                .ok_or_else(|| {
                    RailError::message(format!(
                        "workspace snapshot has no exact package identity for split member '{}'",
                        member
                    ))
                })?;
            let root = snapshot_package.package_root().ok_or_else(|| {
                RailError::message(format!(
                    "split member '{}' has no source root in the workspace snapshot",
                    member
                ))
            })?;
            crate_paths.push(root.to_path_buf());
        }

        let dependency_closure = graph.workspace_dependency_closure(&members)?;
        let selected = members
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let mut release_boundaries = snapshot
            .config()
            .into_iter()
            .flat_map(|config| &config.release.version_groups)
            .filter(|(_, boundary_members)| boundary_members.iter().any(|member| selected.contains(member.as_str())))
            .map(|(name, boundary_members)| {
                let mut boundary_members = boundary_members.clone();
                boundary_members.sort();
                ReleaseBoundary {
                    name: name.clone(),
                    members: boundary_members,
                }
            })
            .collect::<Vec<_>>();
        release_boundaries.sort_by(|left, right| left.name.cmp(&right.name));

        Ok((
            crate_paths,
            SplitOwnership {
                snapshot_id: String::new(),
                members,
                dependency_closure,
                release_boundaries,
            },
        ))
    }

    fn resolve_asset_paths(
        &self,
        split_config: &ConfigSplitConfig,
        crate_paths: &[PathBuf],
    ) -> RailResult<Vec<PathBuf>> {
        let compile = |field: &str, values: &[String]| {
            values
                .iter()
                .map(|value| {
                    Pattern::new(value).map_err(|error| {
                        RailError::Config(ConfigError::InvalidField {
                            field: format!("crates.{}.split.{field}", split_config.name),
                            reason: format!("invalid glob '{value}': {error}"),
                        })
                    })
                })
                .collect::<RailResult<Vec<_>>>()
        };
        let includes = compile("include", &split_config.include)?;
        let excludes = compile("exclude", &split_config.exclude)?;
        let mut assets = self
            .ctx
            .snapshot()?
            .source()
            .tree()
            .entries()
            .iter()
            .filter(|entry| includes.iter().any(|pattern| pattern.matches(entry.path.as_str())))
            .filter(|entry| !excludes.iter().any(|pattern| pattern.matches(entry.path.as_str())))
            .map(|entry| entry.path.as_path().to_path_buf())
            .collect::<Vec<_>>();
        assets.sort();
        assets.dedup();

        if let Some(path) = assets
            .iter()
            .find(|path| crate_paths.iter().any(|crate_root| path.starts_with(crate_root)))
        {
            return Err(RailError::Config(ConfigError::InvalidField {
                field: format!("crates.{}.split.include", split_config.name),
                reason: format!(
                    "'{}' is Cargo-owned; member roots are included automatically",
                    path.display()
                ),
            }));
        }

        if split_config.mode == crate::config::SplitMode::Single {
            let crate_root = crate_paths
                .first()
                .ok_or_else(|| RailError::message("single split has no Cargo member root"))?;
            let mut targets = BTreeMap::<PathBuf, PathBuf>::new();
            for entry in self.ctx.snapshot()?.source().tree().entries() {
                let source = entry.path.as_path();
                let target = if let Ok(relative) = source.strip_prefix(crate_root) {
                    Some(relative.to_path_buf())
                } else if assets.iter().any(|asset| asset == source) {
                    Some(source.to_path_buf())
                } else {
                    None
                };
                let Some(target) = target else { continue };
                if let Some(previous) = targets.insert(target.clone(), source.to_path_buf())
                    && previous != source
                {
                    return Err(RailError::Config(ConfigError::InvalidField {
                        field: format!("crates.{}.split.include", split_config.name),
                        reason: format!(
                            "'{}' and '{}' both map to target path '{}'",
                            previous.display(),
                            source.display(),
                            target.display()
                        ),
                    }));
                }
            }
        }

        Ok(assets)
    }

    /// Create a new builder from workspace context
    pub fn new(ctx: &'a WorkspaceContext) -> RailResult<Self> {
        let config = ctx.require_config()?.as_ref();
        Ok(Self {
            ctx,
            config,
            split_configs: Vec::new(),
            remote_override: None,
        })
    }

    /// Select a single crate by name
    pub fn with_crate(mut self, crate_name: &str) -> RailResult<Self> {
        // Use the helper method to get all splits from unified config
        let all_splits = self.config.build_split_configs();
        let split_config = all_splits.iter().find(|s| s.name == crate_name).ok_or_else(|| {
            RailError::Config(ConfigError::CrateNotFound {
                name: crate_name.to_string(),
            })
        })?;

        self.split_configs = vec![split_config.clone()];
        Ok(self)
    }

    /// Select all configured crates
    pub fn with_all_crates(mut self) -> Self {
        self.split_configs = self.config.build_split_configs();
        self
    }

    /// Select crates based on optional name or --all flag
    pub fn with_crate_or_all(self, crate_name: Option<String>, all: bool) -> RailResult<Self> {
        if all {
            Ok(self.with_all_crates())
        } else if let Some(name) = crate_name {
            self.with_crate(&name)
        } else {
            Err(RailError::with_help(
                "must specify a crate name or use --all",
                "Try: cargo rail <command> --all OR cargo rail <command> <crate-name>",
            ))
        }
    }

    /// Override remote URL for all selected crates
    pub fn with_remote_override(mut self, remote: Option<String>) -> Self {
        self.remote_override = remote;
        self
    }

    /// Validate all selected configurations
    pub fn validate(self) -> RailResult<Self> {
        for split_config in &self.split_configs {
            split_config.validate()?;
        }
        Ok(self)
    }

    /// Check if all remotes are local paths (testing mode)
    pub fn all_local(&self) -> bool {
        self.split_configs.iter().all(|s| {
            self.remote_override
                .as_ref()
                .map(|r| crate::utils::is_local_path(r))
                .unwrap_or_else(|| s.is_local_testing())
        })
    }

    /// Get the number of selected crates
    pub fn count(&self) -> usize {
        self.split_configs.len()
    }

    /// Build SplitParams instances for the split engine
    pub fn build_split_configs(self) -> RailResult<Vec<SplitParams>> {
        let mut configs = Vec::new();
        let mut target_roots = BTreeMap::new();
        let mut common_dirs = BTreeMap::new();
        let mut publications = BTreeMap::new();
        let transform_policy_digest = crate::cargo::ManifestTransformPolicy::capture(self.ctx)?.authority_digest();

        for split_config in &self.split_configs {
            let (crate_paths, mut ownership) = self.resolve_ownership(split_config)?;
            let asset_paths = self.resolve_asset_paths(split_config, &crate_paths)?;
            ownership.snapshot_id = stable_split_ownership_digest(
                self.ctx.workspace_root(),
                split_config,
                &crate_paths,
                &ownership,
                &transform_policy_digest,
            )?;

            // Apply remote override if provided
            let remote = self
                .remote_override
                .clone()
                .unwrap_or_else(|| split_config.remote.clone());

            let target_repo_path = split_config.target_repo_path_for_remote(self.ctx.workspace_root(), &remote);
            let path_capabilities = crate::split::SplitPathCapabilities::new(
                self.ctx.workspace_root(),
                &self.ctx.git()?.git().worktree_root,
                &crate_paths,
                &target_repo_path,
            )?
            .with_asset_paths(&asset_paths)?
            .with_asset_policy(&split_config.include, &split_config.exclude)?;
            let target_repo_path = path_capabilities.target_root().to_path_buf();
            reject_selected_target_overlap(&target_roots, &split_config.name, &target_repo_path)?;
            target_roots.insert(target_repo_path.clone(), split_config.name.clone());
            reject_selected_common_dir(&common_dirs, &split_config.name, &target_repo_path)?;
            if target_repo_path.join(".git").exists() {
                common_dirs.insert(
                    crate::git::SystemGit::open(&target_repo_path)?.common_dir()?,
                    split_config.name.clone(),
                );
            }
            reject_selected_publication(&publications, &split_config.name, &remote, &split_config.branch)?;
            publications.insert(
                publication_key(&remote, &split_config.branch)?,
                split_config.name.clone(),
            );

            configs.push(SplitParams {
                crate_name: split_config.name.clone(),
                crate_paths,
                asset_paths,
                ownership,
                mode: split_config.mode.clone(),
                workspace_mode: split_config.workspace_mode.clone(),
                target_repo_path,
                branch: split_config.branch.clone(),
                remote_url: Some(remote),
                path_capabilities,
            });
        }

        Ok(configs)
    }

    /// Build (SyncConfig, target_exists) tuples for the sync engine
    pub fn build_sync_configs(self) -> RailResult<Vec<(SyncConfig, bool)>> {
        let mut configs = Vec::new();
        let mut target_roots = BTreeMap::new();
        let mut common_dirs = BTreeMap::new();
        let mut publications = BTreeMap::new();
        let transform_policy_digest = crate::cargo::ManifestTransformPolicy::capture(self.ctx)?.authority_digest();

        for split_config in &self.split_configs {
            let (crate_paths, mut ownership) = self.resolve_ownership(split_config)?;
            let asset_paths = self.resolve_asset_paths(split_config, &crate_paths)?;
            ownership.snapshot_id = stable_split_ownership_digest(
                self.ctx.workspace_root(),
                split_config,
                &crate_paths,
                &ownership,
                &transform_policy_digest,
            )?;

            // Apply remote override if provided
            let remote = self
                .remote_override
                .clone()
                .unwrap_or_else(|| split_config.remote.clone());

            let target_repo_path = split_config.target_repo_path_for_remote(self.ctx.workspace_root(), &remote);
            let path_capabilities = crate::split::SplitPathCapabilities::new(
                self.ctx.workspace_root(),
                &self.ctx.git()?.git().worktree_root,
                &crate_paths,
                &target_repo_path,
            )?
            .with_asset_paths(&asset_paths)?
            .with_asset_policy(&split_config.include, &split_config.exclude)?;
            let target_repo_path = path_capabilities.target_root().to_path_buf();
            reject_selected_target_overlap(&target_roots, &split_config.name, &target_repo_path)?;
            target_roots.insert(target_repo_path.clone(), split_config.name.clone());
            reject_selected_common_dir(&common_dirs, &split_config.name, &target_repo_path)?;
            if target_repo_path.join(".git").exists() {
                common_dirs.insert(
                    crate::git::SystemGit::open(&target_repo_path)?.common_dir()?,
                    split_config.name.clone(),
                );
            }
            reject_selected_publication(&publications, &split_config.name, &remote, &split_config.branch)?;
            publications.insert(
                publication_key(&remote, &split_config.branch)?,
                split_config.name.clone(),
            );

            let target_exists = target_repo_path.exists();

            configs.push((
                SyncConfig {
                    crate_name: split_config.name.clone(),
                    crate_paths,
                    asset_paths,
                    ownership,
                    mode: split_config.mode.clone(),
                    workspace_mode: split_config.workspace_mode.clone(),
                    target_repo_path,
                    branch: split_config.branch.clone(),
                    remote_url: remote,
                    path_capabilities,
                },
                target_exists,
            ));
        }

        Ok(configs)
    }
}

fn reject_selected_target_overlap(
    selected: &BTreeMap<PathBuf, String>,
    crate_name: &str,
    target_root: &std::path::Path,
) -> RailResult<()> {
    let Some((existing_root, existing_name)) = selected
        .iter()
        .find(|(existing_root, _)| target_roots_overlap(existing_root, target_root))
    else {
        return Ok(());
    };
    Err(RailError::with_help(
        format!(
            "selected split configurations '{}' and '{}' resolve to overlapping target repositories '{}' and '{}'",
            existing_name,
            crate_name,
            existing_root.display(),
            target_root.display()
        ),
        "assign every selected split a distinct, non-nested target repository; --remote applies to every --all selection",
    ))
}

fn reject_selected_common_dir(
    selected: &BTreeMap<PathBuf, String>,
    crate_name: &str,
    target_root: &std::path::Path,
) -> RailResult<()> {
    if !target_root.join(".git").exists() {
        return Ok(());
    }
    let common = crate::git::SystemGit::open(target_root)?.common_dir()?;
    let Some(existing) = selected.get(&common) else {
        return Ok(());
    };
    Err(RailError::with_help(
        format!(
            "selected split configurations '{}' and '{}' share Git common directory '{}'",
            existing,
            crate_name,
            common.display()
        ),
        "use independent repositories; linked worktrees share refs and objects and cannot be selected together",
    ))
}

fn publication_key(remote: &str, branch: &str) -> RailResult<(String, String)> {
    let repository = if crate::utils::is_local_path(remote) {
        format!(
            "path:{}",
            crate::utils::canonicalize_allow_missing(std::path::Path::new(remote))?.display()
        )
    } else {
        remote_repository_identity(remote)?
    };
    Ok((repository, branch.to_string()))
}

fn reject_selected_publication(
    selected: &BTreeMap<(String, String), String>,
    crate_name: &str,
    remote: &str,
    branch: &str,
) -> RailResult<()> {
    let key = publication_key(remote, branch)?;
    let Some(existing) = selected.get(&key) else {
        return Ok(());
    };
    Err(RailError::with_help(
        format!(
            "selected split configurations '{}' and '{}' publish the same repository branch '{}@{}'",
            existing, crate_name, key.0, key.1
        ),
        "assign every selected split a distinct normalized remote repository and branch",
    ))
}

fn target_roots_overlap(left: &std::path::Path, right: &std::path::Path) -> bool {
    target_path_starts_with(left, right) || target_path_starts_with(right, left)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn target_path_starts_with(path: &std::path::Path, base: &std::path::Path) -> bool {
    path.starts_with(base)
}

#[cfg(any(windows, target_os = "macos"))]
fn target_path_starts_with(path: &std::path::Path, base: &std::path::Path) -> bool {
    let mut path_components = path.components();
    base.components().all(|base_component| {
        path_components
            .next()
            .is_some_and(|path_component| case_folded_path_components_equal(path_component, base_component))
    })
}

#[cfg(windows)]
fn case_folded_path_components_equal(left: std::path::Component<'_>, right: std::path::Component<'_>) -> bool {
    // Canonicalization resolves every existing ancestor, but a missing suffix retains its
    // spelling. Fold those components so case aliases cannot authorize two writers on default
    // case-insensitive Windows filesystems. Over-rejection on a case-sensitive directory is
    // intentional: selection must fail closed when the target does not exist to prove its
    // filesystem semantics.
    left.as_os_str()
        .to_string_lossy()
        .chars()
        .flat_map(char::to_uppercase)
        .eq(right.as_os_str().to_string_lossy().chars().flat_map(char::to_uppercase))
}

#[cfg(target_os = "macos")]
fn case_folded_path_components_equal(left: std::path::Component<'_>, right: std::path::Component<'_>) -> bool {
    // APFS/HFS+ aliases canonically equivalent Unicode spellings as well as case by default.
    // Normalize a missing suffix to NFD before folding so composed/decomposed names cannot
    // authorize parallel writers for one repository.
    let normalizer = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd();
    let left = left.as_os_str().to_string_lossy();
    let right = right.as_os_str().to_string_lossy();
    let left = normalizer.normalize(left.as_ref());
    let right = normalizer.normalize(right.as_ref());
    left.chars()
        .flat_map(char::to_uppercase)
        .eq(right.chars().flat_map(char::to_uppercase))
}

#[cfg(test)]
mod tests {
    use super::format_preview_list;
    use super::{publication_key, reject_selected_publication};
    use std::collections::BTreeMap;

    #[cfg(any(windows, target_os = "macos"))]
    use super::reject_selected_target_overlap;
    #[cfg(any(windows, target_os = "macos"))]
    use std::path::PathBuf;

    #[test]
    fn preview_list_keeps_short_lists() {
        let items = vec!["rail-a".to_string(), "rail-b".to_string(), "rail-c".to_string()];
        assert_eq!(format_preview_list(&items, 5), "rail-a, rail-b, rail-c");
    }

    #[test]
    fn preview_list_truncates_large_lists() {
        let items = vec![
            "rail-a".to_string(),
            "rail-b".to_string(),
            "rail-c".to_string(),
            "rail-d".to_string(),
        ];
        assert_eq!(format_preview_list(&items, 2), "rail-a, rail-b, ... +2 more");
    }

    #[test]
    fn selected_publication_rejects_normalized_remote_aliases_on_one_branch() {
        let first = "https://user:secret@example.com/org/demo.git?token=one";
        let alias = "https://example.com/org/demo#different-fragment";
        let selected = BTreeMap::from([(publication_key(first, "main").unwrap(), "crate-a".to_string())]);

        assert!(reject_selected_publication(&selected, "crate-b", alias, "main").is_err());
        reject_selected_publication(&selected, "crate-b", alias, "release").unwrap();
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn selected_target_overlap_rejects_missing_case_aliases() {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\rail-targets")
        } else {
            PathBuf::from("/tmp/rail-targets")
        };
        let selected = BTreeMap::from([(root.join("Crate-A"), "crate-a".to_string())]);

        let alias = reject_selected_target_overlap(&selected, "crate-b", &root.join("crate-a"));
        assert!(alias.is_err());

        let nested = reject_selected_target_overlap(&selected, "crate-b", &root.join("crate-a/nested"));
        assert!(nested.is_err());

        #[cfg(target_os = "macos")]
        {
            let composed = BTreeMap::from([(root.join("Caf\u{e9}"), "crate-a".to_string())]);
            let decomposed = root.join("Cafe\u{301}");
            assert!(reject_selected_target_overlap(&composed, "crate-b", &decomposed).is_err());
        }
    }
}
