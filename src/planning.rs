//! Typed semantic index shared by planner evaluators.
//!
//! This module owns the lossless handoff from captured source/config facts to
//! planning policy. It deliberately contains no renderer or command invocation.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::config::RailConfig;
use crate::error::{RailError, RailResult};
use crate::source::{ChangeSet, SourceContentIdentity};
use crate::workspace::WorkspaceContext;

mod evidence;
mod work;

pub(crate) use work::{WorkPlan, WorkPlanAuthority, build_work_plan, format_work_plan};

const CONFIG_CANDIDATES: &[&str] = &["rail.toml", ".rail.toml", ".cargo/rail.toml", ".config/rail.toml"];

/// One exact effective configuration leaf whose value changed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ConfigDelta {
    pub(crate) path: String,
    pub(crate) before: Value,
    pub(crate) after: Value,
}

/// One portable changed-path fact retained from the captured source authority.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct IndexedFileChange {
    pub(crate) path: String,
    pub(crate) kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) relation: Option<String>,
    pub(crate) provenance: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) after: Option<String>,
}

/// Rich captured source state retained alongside its workspace path projection.
#[derive(Debug, Clone)]
enum PlanningSourceChanges {
    Worktree {
        changes: ChangeSet,
        identities: Vec<SourceIdentityPair>,
    },
    Objects {
        changes: ChangeSet,
        identities: Vec<SourceIdentityPair>,
    },
}

#[derive(Debug, Clone)]
struct SourceIdentityPair {
    before: Option<SourceContentIdentity>,
    after: Option<SourceContentIdentity>,
}

/// One deterministic semantic view consumed by every planner evaluator.
#[derive(Debug, Clone)]
pub(crate) struct PlanningIndex {
    source: PlanningSourceChanges,
    files: Vec<String>,
    file_changes: Vec<IndexedFileChange>,
    config: Vec<ConfigDelta>,
}

impl PlanningIndex {
    pub(crate) fn from_worktree(ctx: &WorkspaceContext, changes: ChangeSet, base_ref: &str) -> RailResult<Self> {
        let files = changes
            .entries()
            .iter()
            .filter_map(|change| ctx.to_workspace_path(change.path.as_path()))
            .map(|path| crate::utils::path_to_git_format(&path))
            .collect::<Vec<_>>();
        let config = effective_config_deltas(ctx, &files, base_ref, None)?;
        let repository_paths = changes
            .entries()
            .iter()
            .map(|change| change.path.as_path().to_path_buf())
            .collect::<Vec<_>>();
        let before = content_identities_at_ref(ctx, base_ref, &repository_paths)?;
        let capture = ctx
            .planning_source_capture()
            .ok_or_else(|| RailError::message("worktree planning index requires one sparse planning source capture"))?;
        let identities: Vec<SourceIdentityPair> = changes
            .entries()
            .iter()
            .map(|change| SourceIdentityPair {
                before: before.get(change.path.as_path()).cloned(),
                after: capture.current_content_identity(&change.path),
            })
            .collect();
        let file_changes = changes
            .entries()
            .iter()
            .zip(&identities)
            .filter_map(|(change, identity)| {
                let path = ctx.to_workspace_path(change.path.as_path())?;
                Some(IndexedFileChange {
                    path: crate::utils::path_to_git_format(&path),
                    kind: source_change_kind(change.kind).to_string(),
                    relation: change.relation.as_ref().map(change_relation),
                    provenance: change_provenance(change.provenance),
                    before: identity.before.as_ref().map(content_identity),
                    after: identity.after.as_ref().map(content_identity),
                })
            })
            .collect();
        let index = Self {
            source: PlanningSourceChanges::Worktree { changes, identities },
            files: sorted_unique(files),
            file_changes,
            config,
        };
        index.validate()?;
        Ok(index)
    }

    pub(crate) fn from_objects(
        ctx: &WorkspaceContext,
        changes: ChangeSet,
        base_ref: &str,
        head_ref: &str,
    ) -> RailResult<Self> {
        let files = changes
            .entries()
            .iter()
            .filter_map(|change| ctx.to_workspace_path(change.path.as_path()))
            .map(|path| crate::utils::path_to_git_format(&path))
            .collect::<Vec<_>>();
        let config = effective_config_deltas(ctx, &files, base_ref, Some(head_ref))?;
        let repository_paths = changes
            .entries()
            .iter()
            .map(|change| change.path.as_path().to_path_buf())
            .collect::<Vec<_>>();
        let before = content_identities_at_ref(ctx, base_ref, &repository_paths)?;
        let after = content_identities_at_ref(ctx, head_ref, &repository_paths)?;
        let identities = changes
            .entries()
            .iter()
            .map(|change| SourceIdentityPair {
                before: before.get(change.path.as_path()).cloned(),
                after: after.get(change.path.as_path()).cloned(),
            })
            .collect::<Vec<_>>();
        let file_changes = changes
            .entries()
            .iter()
            .zip(&identities)
            .filter_map(|(change, identity)| {
                let path = ctx.to_workspace_path(change.path.as_path())?;
                Some(IndexedFileChange {
                    path: crate::utils::path_to_git_format(&path),
                    kind: source_change_kind(change.kind).to_string(),
                    relation: change.relation.as_ref().map(change_relation),
                    provenance: change_provenance(change.provenance),
                    before: identity.before.as_ref().map(content_identity),
                    after: identity.after.as_ref().map(content_identity),
                })
            })
            .collect();
        let index = Self {
            source: PlanningSourceChanges::Objects { changes, identities },
            files: sorted_unique(files),
            file_changes,
            config,
        };
        index.validate()?;
        Ok(index)
    }

    pub(crate) fn files(&self) -> &[String] {
        &self.files
    }

    pub(crate) fn config_deltas(&self) -> &[ConfigDelta] {
        &self.config
    }

    pub(crate) fn file_changes(&self) -> &[IndexedFileChange] {
        &self.file_changes
    }

    fn validate(&self) -> RailResult<()> {
        if !self.files.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(RailError::message("planning source paths are not strictly sorted"));
        }
        if !self.config.windows(2).all(|pair| pair[0].path < pair[1].path) {
            return Err(RailError::message(
                "planning configuration deltas are not strictly sorted",
            ));
        }
        if !self.file_changes.windows(2).all(|pair| pair[0].path < pair[1].path) {
            return Err(RailError::message("planning file changes are not strictly sorted"));
        }
        let source_count = match &self.source {
            PlanningSourceChanges::Worktree { changes, identities } => {
                if changes.entries().len() != identities.len() {
                    return Err(RailError::message(
                        "captured source changes and content identities have different lengths",
                    ));
                }
                for identity in identities {
                    let _ = (&identity.before, &identity.after);
                }
                changes.entries().len()
            }
            PlanningSourceChanges::Objects { changes, identities } => {
                if changes.entries().len() != identities.len() {
                    return Err(RailError::message(
                        "captured object changes and content identities have different lengths",
                    ));
                }
                changes.entries().len()
            }
        };
        if source_count < self.files.len() {
            return Err(RailError::message(
                "planning path projection contains more paths than its captured source evidence",
            ));
        }
        Ok(())
    }
}

fn source_change_kind(kind: crate::source::SourceChangeKind) -> &'static str {
    match kind {
        crate::source::SourceChangeKind::Added => "added",
        crate::source::SourceChangeKind::Modified => "modified",
        crate::source::SourceChangeKind::TypeChanged => "type_changed",
        crate::source::SourceChangeKind::Deleted => "deleted",
    }
}

fn change_relation(relation: &crate::source::ChangeRelation) -> String {
    match relation {
        crate::source::ChangeRelation::RenamedFrom(path) => format!("renamed_from:{path}"),
        crate::source::ChangeRelation::RenamedTo(path) => format!("renamed_to:{path}"),
        crate::source::ChangeRelation::CopiedFrom(path) => format!("copied_from:{path}"),
    }
}

fn change_provenance(provenance: crate::source::ChangeProvenance) -> Vec<&'static str> {
    [
        (crate::source::ChangeLayer::Committed, "committed"),
        (crate::source::ChangeLayer::Staged, "staged"),
        (crate::source::ChangeLayer::Unstaged, "unstaged"),
        (crate::source::ChangeLayer::Untracked, "untracked"),
    ]
    .into_iter()
    .filter_map(|(layer, name)| provenance.contains(layer).then_some(name))
    .collect()
}

fn content_identity(identity: &SourceContentIdentity) -> String {
    match identity {
        SourceContentIdentity::GitObject { object_id, mode } => format!("git:{mode}:{object_id}"),
        SourceContentIdentity::Sha256 { digest, executable } => {
            format!("sha256:{digest}:executable={executable}")
        }
        SourceContentIdentity::Symlink { target } => format!("symlink:{target}"),
    }
}

fn content_identities_at_ref(
    ctx: &WorkspaceContext,
    revision: &str,
    paths: &[PathBuf],
) -> RailResult<std::collections::BTreeMap<PathBuf, SourceContentIdentity>> {
    if paths.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    let entries = ctx.git()?.git().collect_tree_entries_for_paths(revision, paths)?;
    Ok(entries
        .into_iter()
        .map(|entry| {
            (
                entry.path,
                SourceContentIdentity::GitObject {
                    object_id: entry.object_id,
                    mode: entry.mode,
                },
            )
        })
        .collect())
}

fn sorted_unique(values: Vec<String>) -> Vec<String> {
    values.into_iter().collect::<BTreeSet<_>>().into_iter().collect()
}

fn effective_config_deltas(
    ctx: &WorkspaceContext,
    changed_files: &[String],
    base_ref: &str,
    head_ref: Option<&str>,
) -> RailResult<Vec<ConfigDelta>> {
    let candidates = config_candidates(ctx)?;
    if !changed_files
        .iter()
        .any(|path| candidates.iter().any(|candidate| candidate == path))
    {
        return Ok(Vec::new());
    }

    let before = config_at_ref(ctx, base_ref, &candidates)?;
    let after = match head_ref {
        Some(head) => config_at_ref(ctx, head, &candidates)?,
        None => ctx.config().map(|config| config.as_ref().clone()).unwrap_or_default(),
    };
    config_deltas(&before, &after)
}

fn config_candidates(ctx: &WorkspaceContext) -> RailResult<Vec<String>> {
    let standard = CONFIG_CANDIDATES
        .iter()
        .map(|path| (*path).to_string())
        .collect::<Vec<_>>();
    let Some(path) = ctx.config_path() else {
        return Ok(standard);
    };
    let Ok(relative) = path.strip_prefix(ctx.workspace_root()) else {
        return Ok(standard);
    };
    let relative = crate::utils::path_to_git_format(relative);
    if standard.contains(&relative) {
        Ok(standard)
    } else {
        Ok(vec![relative])
    }
}

fn config_at_ref(ctx: &WorkspaceContext, revision: &str, candidates: &[String]) -> RailResult<RailConfig> {
    let absolute = candidates
        .iter()
        .map(|candidate| ctx.workspace_root().join(candidate))
        .collect::<Vec<_>>();
    let entries = ctx.git()?.git().collect_tree_entries_for_paths(revision, &absolute)?;
    let selected = candidates.iter().find(|candidate| {
        entries.iter().any(|entry| {
            ctx.to_workspace_path(&entry.path)
                .is_some_and(|path| crate::utils::path_to_git_format(&path) == candidate.as_str())
        })
    });
    let Some(selected) = selected else {
        return Ok(RailConfig::default());
    };
    let path = ctx.workspace_root().join(selected);
    let bytes = ctx.git()?.git().read_files_bulk(&[(revision, path.as_path())])?;
    RailConfig::parse_historical_planning_bytes(&bytes[0]).map_err(|message| {
        RailError::with_help(
            format!("failed to parse historical planning configuration '{selected}': {message}"),
            "migrate the selected base configuration or choose a comparison ref using the current schema",
        )
    })
}

fn config_deltas(before: &RailConfig, after: &RailConfig) -> RailResult<Vec<ConfigDelta>> {
    let before = serde_json::to_value(before)
        .map_err(|error| RailError::message(format!("failed to serialize base configuration: {error}")))?;
    let after = serde_json::to_value(after)
        .map_err(|error| RailError::message(format!("failed to serialize current configuration: {error}")))?;
    let mut deltas = Vec::new();
    collect_config_deltas("", &before, &after, &mut deltas);
    Ok(deltas)
}

fn collect_config_deltas(path: &str, before: &Value, after: &Value, output: &mut Vec<ConfigDelta>) {
    if before == after {
        return;
    }
    if let (Some(before), Some(after)) = (before.as_object(), after.as_object()) {
        let keys = before.keys().chain(after.keys()).collect::<BTreeSet<_>>();
        for key in keys {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            collect_config_deltas(
                &child,
                before.get(key).unwrap_or(&Value::Null),
                after.get(key).unwrap_or(&Value::Null),
                output,
            );
        }
        return;
    }
    output.push(ConfigDelta {
        path: path.to_string(),
        before: before.clone(),
        after: after.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_config_delta_names_only_changed_schema_leaves() {
        let before =
            RailConfig::parse_bytes(b"[release]\nsemver_check = \"warn\"\n\n[surface]\nenabled = true\n").unwrap();
        let after =
            RailConfig::parse_bytes(b"[release]\nsemver_check = \"off\"\n\n[surface]\nenabled = true\n").unwrap();
        let deltas = config_deltas(&before, &after).unwrap();
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].path, "release.semver_check");
        assert_eq!(deltas[0].before, "warn");
        assert_eq!(deltas[0].after, "off");
    }
}
