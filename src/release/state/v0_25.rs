//! Frozen reader for release journals written by Cargo-Rail v0.25.0.
//!
//! This is one bounded predecessor transition, not a general legacy mode. It
//! can be removed when the supported recovery predecessor advances beyond
//! v0.25.0; the exact fixture must remain as historical contract evidence.

use super::{
    BackupRestorePolicy, CrateReleaseState, LocalInputBackup, ReleaseMode, ReleasePhase, ReleaseState, ReleaseStatus,
    Step, StepStatus, V0_25ExecutionInputs,
};
use crate::config::{
    ChangelogRelativeTo, ChangelogShape, Pre1BreakingBump, ReleaseConfig, ReleaseRegistryPublication,
    ReleaseRemoteEffects, SemverCheckPolicy,
};
use crate::error::{RailError, RailResult};
use crate::release::planner::{RELEASE_PLAN_CONTRACT_VERSION, ReleasePlan};
use crate::release::remote::RemoteRepository;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

const STATE_SCHEMA_VERSION: u32 = 5;
const PLAN_CONTRACT_VERSION: u32 = 5;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleaseStateV5 {
    schema_version: u32,
    transaction_id: String,
    status: ReleaseStatus,
    phase: ReleasePhase,
    mode: ReleaseMode,
    plan: ReleasePlanV5,
    release_config: ReleaseConfigV5,
    remote_repository: Option<RemoteRepositoryV5>,
    #[serde(default)]
    publish_registry: Option<String>,
    skip_publish: bool,
    skip_tag: bool,
    initial_head: String,
    branch: String,
    planned_paths: Vec<PathBuf>,
    control_paths: Vec<PathBuf>,
    local_input_backups: Vec<LocalInputBackupV5>,
    crates: Vec<CrateReleaseStateV5>,
    release_commit: Option<String>,
    commit_push: StepV5,
    readiness: StepV5,
    tag_push: StepV5,
    abort: StepV5,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasePlanV5 {
    plan_contract_version: u32,
    #[serde(default)]
    snapshot_id: String,
    source: String,
    canonical_crate_order: Vec<String>,
    crates: Vec<CrateReleasePlanV5>,
    summary: ReleaseSummaryV5,
    change_files_to_delete: Vec<PathBuf>,
    change_files_to_update: Vec<PlannedChangeFileUpdateV5>,
    auxiliary_lockfiles: Vec<PlannedAuxiliaryLockfileV5>,
    skipped: Vec<SkippedCrateV5>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrateReleasePlanV5 {
    name: String,
    current_version: semver::Version,
    new_version: semver::Version,
    manifest_path: PathBuf,
    changelog_path: PathBuf,
    tag_name: String,
    previous_tag: Option<String>,
    changelog_range_start: Option<String>,
    changelog_range_end: String,
    publish: bool,
    publish_intent: String,
    generate_changelog: bool,
    bump: String,
    bump_reason: String,
    commits: Vec<serde_json::Value>,
    commit_diagnostics: Vec<serde_json::Value>,
    changelog_body: String,
    changelog_entries: Vec<serde_json::Value>,
    dependency_updates: Vec<DependencyUpdateV5>,
    change_entries: Vec<PlannedChangeEntryV5>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version_group: Option<String>,
    affected_dependents: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyUpdateV5 {
    name: String,
    version: semver::Version,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedChangeEntryV5 {
    path: PathBuf,
    bump: crate::release::change_files::ChangeBump,
    body: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedChangeFileUpdateV5 {
    path: PathBuf,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedAuxiliaryLockfileV5 {
    manifest_path: PathBuf,
    lockfile_path: PathBuf,
    before_digest: String,
    after_digest: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkippedCrateV5 {
    name: String,
    reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseSummaryV5 {
    total_crates: usize,
    crates_to_publish: usize,
    crates_to_tag: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseConfigV5 {
    source: String,
    tag_prefix: String,
    tag_format: String,
    remote_effects: ReleaseRemoteEffects,
    registry_publication: ReleaseRegistryPublication,
    sign_tags: bool,
    require_changelog_entries: bool,
    require_release_notes: bool,
    release_notes_dir: String,
    change_dir: String,
    pre_1_breaking_bump: Pre1BreakingBump,
    unconventional_commits: String,
    semver_check: SemverCheckPolicy,
    require_change_files: serde_json::Value,
    version_groups: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    auxiliary_cargo_manifests: Vec<PathBuf>,
    changelog: ChangelogShapeV5,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangelogShapeV5 {
    path: String,
    relative_to: ChangelogRelativeTo,
    entry_format: String,
    emoji: bool,
    group_order: Vec<String>,
    fallback: String,
    groups: Vec<GroupSpecV5>,
    filters: ChangelogFiltersV5,
    commit_url: Option<String>,
    pr_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupSpecV5 {
    types: Vec<String>,
    title: String,
    emoji: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangelogFiltersV5 {
    skip_types: Vec<String>,
    skip_scopes: Vec<String>,
    include_paths: Vec<String>,
    exclude_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteRepositoryV5 {
    host: Option<String>,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalInputBackupV5 {
    path: PathBuf,
    content: String,
    restore: BackupRestorePolicy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrateReleaseStateV5 {
    name: String,
    commit: StepV5,
    tag: StepV5,
    forge_draft: StepV5,
    publication: StepV5,
    forge_publication: StepV5,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepV5 {
    status: StepStatus,
    #[serde(default)]
    object: Option<String>,
}

impl ReleaseStateV5 {
    pub(super) fn decode(
        bytes: &[u8],
        path: &Path,
        root: &Path,
        capture_predecessor_inputs: bool,
    ) -> RailResult<ReleaseState> {
        let predecessor: Self = serde_json::from_slice(bytes)
            .map_err(|error| RailError::message(format!("invalid release state '{}': {error}", path.display())))?;
        predecessor.into_current(root, capture_predecessor_inputs)
    }

    fn into_current(self, root: &Path, capture_predecessor_inputs: bool) -> RailResult<ReleaseState> {
        if self.schema_version != STATE_SCHEMA_VERSION {
            return Err(RailError::message(format!(
                "unsupported release state version {}",
                self.schema_version
            )));
        }
        if self.plan.plan_contract_version != PLAN_CONTRACT_VERSION {
            return Err(RailError::with_help(
                format!(
                    "release state version {STATE_SCHEMA_VERSION} requires embedded release plan contract {PLAN_CONTRACT_VERSION}, found {}",
                    self.plan.plan_contract_version
                ),
                "resume with Cargo-Rail v0.25.0 or safely abort that transaction",
            ));
        }
        validate_v5_spelling(
            &self.plan.source,
            "release plan source",
            &["changes", "commits", "both"],
        )?;
        validate_v5_spelling(
            &self.release_config.source,
            "persisted release source",
            &["changes", "commits", "both"],
        )?;
        validate_v5_spelling(
            &self.release_config.unconventional_commits,
            "persisted unconventional-commit policy",
            &["allow", "warn", "deny"],
        )?;
        validate_require_change_files(&self.release_config.require_change_files)?;

        let plan_names = self
            .plan
            .crates
            .iter()
            .map(|planned| planned.name.as_str())
            .collect::<Vec<_>>();
        let state_names = self.crates.iter().map(|state| state.name.as_str()).collect::<Vec<_>>();
        if plan_names != state_names || plan_names.iter().copied().collect::<BTreeSet<_>>().len() != plan_names.len() {
            return Err(RailError::message(
                "release state version 5 has inconsistent or duplicate crate execution state",
            ));
        }
        if self.skip_publish != self.publish_registry.is_none()
            || self
                .publish_registry
                .as_deref()
                .is_some_and(|registry| registry != "crates-io")
        {
            return Err(RailError::message(
                "release state contains inconsistent or unsupported registry publication authority",
            ));
        }

        let (release_note_bodies, predecessor_control_paths) = if capture_predecessor_inputs {
            capture_predecessor_release_note_bodies(root, &self.release_config.release_notes_dir, &self.plan.crates)?
        } else {
            (BTreeMap::new(), Vec::new())
        };
        let predecessor_execution = (self.release_config.require_changelog_entries || !release_note_bodies.is_empty())
            .then_some(V0_25ExecutionInputs {
                require_changelog_entries: self.release_config.require_changelog_entries,
                release_note_bodies,
            });

        let plan = self.plan.into_current()?;
        let release_config = self.release_config.into_current();
        let remote_repository = self
            .remote_repository
            .map(RemoteRepositoryV5::into_current)
            .transpose()?;
        let mut control_paths = self.control_paths;
        control_paths.extend(predecessor_control_paths);
        control_paths.sort();
        control_paths.dedup();
        let state = ReleaseState {
            schema_version: super::RELEASE_STATE_SCHEMA_VERSION,
            transaction_id: self.transaction_id,
            status: self.status,
            phase: self.phase,
            mode: self.mode,
            plan,
            release_config,
            remote_repository,
            publish_registry: self.publish_registry,
            skip_publish: self.skip_publish,
            skip_tag: self.skip_tag,
            initial_head: self.initial_head,
            branch: self.branch,
            planned_paths: self.planned_paths,
            control_paths,
            local_input_backups: self
                .local_input_backups
                .into_iter()
                .map(LocalInputBackupV5::into_current)
                .collect(),
            crates: self.crates.into_iter().map(CrateReleaseStateV5::into_current).collect(),
            release_commit: self.release_commit,
            commit_push: self.commit_push.into_current(),
            readiness: self.readiness.into_current(),
            tag_push: self.tag_push.into_current(),
            abort: self.abort.into_current(),
            predecessor_execution,
        };
        state.validate_contract()?;
        Ok(state)
    }
}

impl ReleasePlanV5 {
    fn into_current(self) -> RailResult<ReleasePlan> {
        let mut value = serde_json::to_value(self)
            .map_err(|error| RailError::message(format!("failed to normalize release plan version 5: {error}")))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| RailError::message("release plan version 5 is not an object"))?;
        object.remove("source");
        object.insert(
            "plan_contract_version".to_string(),
            serde_json::Value::from(RELEASE_PLAN_CONTRACT_VERSION),
        );
        let crates = object
            .get_mut("crates")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| RailError::message("release plan version 5 has no crate array"))?;
        for planned in crates {
            let planned = planned
                .as_object_mut()
                .ok_or_else(|| RailError::message("release plan version 5 contains a non-object crate"))?;
            planned.remove("commits");
            planned.remove("commit_diagnostics");
            planned.remove("changelog_entries");
        }
        serde_json::from_value(value)
            .map_err(|error| RailError::message(format!("invalid converted release plan version 5: {error}")))
    }
}

impl ReleaseConfigV5 {
    fn into_current(self) -> ReleaseConfig {
        let retired_groups = self
            .changelog
            .groups
            .into_iter()
            .map(|group| (group.types, group.title, group.emoji))
            .collect::<Vec<_>>();
        let retired_filters = (
            self.changelog.filters.skip_types,
            self.changelog.filters.skip_scopes,
            self.changelog.filters.include_paths,
            self.changelog.filters.exclude_paths,
        );
        let _retired_planning_fields = (
            self.source,
            self.require_release_notes,
            self.release_notes_dir,
            self.unconventional_commits,
            self.require_change_files,
            self.changelog.entry_format,
            self.changelog.emoji,
            self.changelog.group_order,
            self.changelog.fallback,
            retired_groups,
            retired_filters,
            self.changelog.commit_url,
            self.changelog.pr_url,
        );
        ReleaseConfig {
            tag_prefix: self.tag_prefix,
            tag_format: self.tag_format,
            remote_effects: self.remote_effects,
            registry_publication: self.registry_publication,
            sign_tags: self.sign_tags,
            change_dir: self.change_dir,
            pre_1_breaking_bump: self.pre_1_breaking_bump,
            semver_check: self.semver_check,
            version_groups: self.version_groups,
            auxiliary_cargo_manifests: self.auxiliary_cargo_manifests,
            changelog: ChangelogShape {
                path: self.changelog.path,
                relative_to: self.changelog.relative_to,
            },
        }
    }
}

impl RemoteRepositoryV5 {
    fn into_current(self) -> RailResult<RemoteRepository> {
        serde_json::from_value(serde_json::json!({ "host": self.host, "path": self.path }))
            .map_err(|error| RailError::message(format!("invalid v0.25.0 release repository identity: {error}")))
    }
}

impl LocalInputBackupV5 {
    fn into_current(self) -> LocalInputBackup {
        LocalInputBackup {
            path: self.path,
            content: self.content,
            restore: self.restore,
        }
    }
}

impl CrateReleaseStateV5 {
    fn into_current(self) -> CrateReleaseState {
        CrateReleaseState {
            name: self.name,
            commit: self.commit.into_current(),
            tag: self.tag.into_current(),
            forge_draft: self.forge_draft.into_current(),
            publication: self.publication.into_current(),
            forge_publication: self.forge_publication.into_current(),
        }
    }
}

impl StepV5 {
    fn into_current(self) -> Step {
        Step {
            status: self.status,
            object: self.object,
        }
    }
}

fn capture_predecessor_release_note_bodies(
    root: &Path,
    release_notes_dir: &str,
    plans: &[CrateReleasePlanV5],
) -> RailResult<(BTreeMap<String, String>, Vec<PathBuf>)> {
    let configured_dir = Path::new(release_notes_dir);
    let notes_dir = if configured_dir.is_absolute() {
        configured_dir.to_path_buf()
    } else {
        root.join(configured_dir)
    };
    let mut bodies = BTreeMap::new();
    let mut control_paths = Vec::new();
    for plan in plans {
        let version_candidate = notes_dir.join(format!("v{}.md", plan.new_version));
        let tag_candidate = notes_dir.join(format!("{}.md", plan.tag_name));
        if let Some((content, path)) = capture_release_note_candidate(root, &version_candidate, true)? {
            let content = content.ok_or_else(|| RailError::message("captured v0.25 release note has no body"))?;
            bodies.insert(plan.name.clone(), content);
            control_paths.push(path);
            if tag_candidate != version_candidate
                && let Some((_, path)) = capture_release_note_candidate(root, &tag_candidate, false)?
            {
                control_paths.push(path);
            }
        } else if tag_candidate != version_candidate
            && let Some((content, path)) = capture_release_note_candidate(root, &tag_candidate, true)?
        {
            let content = content.ok_or_else(|| RailError::message("captured v0.25 release note has no body"))?;
            bodies.insert(plan.name.clone(), content);
            control_paths.push(path);
        }
    }
    Ok((bodies, control_paths))
}

fn capture_release_note_candidate(
    root: &Path,
    candidate: &Path,
    capture_body: bool,
) -> RailResult<Option<(Option<String>, PathBuf)>> {
    let relative = crate::utils::path_relative_to(root, candidate).map_err(|error| {
        RailError::message(format!(
            "v0.25 release-note override '{}' escapes workspace '{}': {error}",
            candidate.display(),
            root.display()
        ))
    })?;
    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(RailError::message(format!(
                "failed to inspect v0.25 release-note override '{}': {error}",
                candidate.display()
            )));
        }
    };
    if !metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "v0.25 release-note override '{}' is not a regular file",
            candidate.display()
        )));
    }
    if !capture_body {
        return Ok(Some((None, relative)));
    }
    let mut file = fs::File::open(candidate).map_err(|error| {
        RailError::message(format!(
            "failed to open v0.25 release-note override '{}': {error}",
            candidate.display()
        ))
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        RailError::message(format!(
            "failed to read v0.25 release-note override '{}': {error}",
            candidate.display()
        ))
    })?;
    let stable = crate::utils::private_file_matches_path(&file, candidate, bytes.len() as u64).map_err(|error| {
        RailError::message(format!(
            "failed to verify v0.25 release-note override '{}': {error}",
            candidate.display()
        ))
    })?;
    if !stable {
        return Err(RailError::message(format!(
            "v0.25 release-note override '{}' changed identity while it was captured",
            candidate.display()
        )));
    }
    String::from_utf8(bytes)
        .map(|content| Some((Some(content), relative)))
        .map_err(|error| {
            RailError::message(format!(
                "v0.25 release-note override '{}' is not UTF-8: {error}",
                candidate.display()
            ))
        })
}

fn validate_v5_spelling(value: &str, field: &str, accepted: &[&str]) -> RailResult<()> {
    if accepted.contains(&value) {
        return Ok(());
    }
    Err(RailError::message(format!(
        "release state version 5 contains invalid {field} '{value}'"
    )))
}

fn validate_require_change_files(value: &serde_json::Value) -> RailResult<()> {
    if value.is_boolean()
        || value
            .as_array()
            .is_some_and(|items| items.iter().all(serde_json::Value::is_string))
    {
        return Ok(());
    }
    Err(RailError::message(
        "release state version 5 contains invalid require_change_files policy",
    ))
}
