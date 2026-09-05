//! Durable, idempotent release execution state.

mod v0_25;

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::release::planner::{RELEASE_PLAN_CONTRACT_VERSION, RELEASE_REGISTRY, ReleasePlan};
use crate::release::remote::RemoteRepository;
use crate::utils::canonicalize_existing;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const RELEASE_STATE_SCHEMA_VERSION: u32 = 7;

#[derive(Deserialize)]
struct ReleaseStateSchema {
    schema_version: u32,
    transaction_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseState {
    pub schema_version: u32,
    pub transaction_id: String,
    pub status: ReleaseStatus,
    pub phase: ReleasePhase,
    pub mode: ReleaseMode,
    pub plan: ReleasePlan,
    pub release_config: ReleaseConfig,
    #[serde(default)]
    pub remote_repository: Option<RemoteRepository>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_registry: Option<String>,
    pub skip_publish: bool,
    pub skip_tag: bool,
    pub initial_head: String,
    pub branch: String,
    #[serde(serialize_with = "super::path_serde::serialize_vec")]
    pub planned_paths: Vec<PathBuf>,
    #[serde(serialize_with = "super::path_serde::serialize_vec")]
    pub control_paths: Vec<PathBuf>,
    pub local_input_backups: Vec<LocalInputBackup>,
    pub crates: Vec<CrateReleaseState>,
    pub release_commit: Option<String>,
    pub commit_push: Step,
    pub readiness: Step,
    pub tag_push: Step,
    pub abort: Step,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_execution: Option<V0_25ExecutionInputs>,
}

pub(crate) struct ReleaseStateCreate<'a> {
    pub(crate) root: &'a Path,
    pub(crate) transaction_id: String,
    pub(crate) mode: ReleaseMode,
    pub(crate) plan: ReleasePlan,
    pub(crate) release_config: ReleaseConfig,
    pub(crate) remote_repository: Option<RemoteRepository>,
    pub(crate) skip_publish: bool,
    pub(crate) skip_tag: bool,
    pub(crate) initial_head: String,
    pub(crate) branch: String,
    pub(crate) planned_paths: Vec<PathBuf>,
    pub(crate) control_paths: Vec<PathBuf>,
    pub(crate) reconstructed: Option<ReconstructedRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalInputBackup {
    #[serde(serialize_with = "super::path_serde::serialize")]
    pub path: PathBuf,
    pub content: String,
    #[serde(default)]
    pub restore: BackupRestorePolicy,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackupRestorePolicy {
    #[default]
    BeforeFirstCommit,
    Always,
}

pub(crate) struct ReconstructedRelease {
    pub release_commit: String,
    pub commit_targets: BTreeMap<String, String>,
    pub remote_repository: Option<RemoteRepository>,
}

/// Execution inputs removed after v0.25.0 but still required to finish one
/// already-authorized predecessor transaction without changing its effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct V0_25ExecutionInputs {
    pub(crate) require_changelog_entries: bool,
    pub(crate) release_note_bodies: BTreeMap<String, String>,
}

impl V0_25ExecutionInputs {
    pub(crate) fn release_note_body(&self, crate_name: &str) -> Option<&str> {
        self.release_note_bodies.get(crate_name).map(String::as_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReleaseStatus {
    Active,
    Complete,
    Aborted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReleasePhase {
    #[default]
    Planned,
    Prepared,
    AwaitingChecks,
    Ready,
    Publishing,
    Released,
}

impl ReleasePhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Prepared => "prepared",
            Self::AwaitingChecks => "awaiting_checks",
            Self::Ready => "ready",
            Self::Publishing => "publishing",
            Self::Released => "released",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReleaseMode {
    Run,
    Finalize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CrateReleaseState {
    pub name: String,
    pub commit: Step,
    pub tag: Step,
    pub forge_draft: Step,
    pub publication: Step,
    pub forge_publication: Step,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Step {
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Complete,
}

impl Step {
    pub fn is_complete(&self) -> bool {
        self.status == StepStatus::Complete
    }
}

impl ReleaseState {
    pub fn create(request: ReleaseStateCreate<'_>) -> RailResult<(Self, PathBuf)> {
        let ReleaseStateCreate {
            root,
            transaction_id,
            mode,
            plan,
            release_config,
            remote_repository,
            skip_publish,
            skip_tag,
            initial_head,
            branch,
            planned_paths,
            control_paths,
            reconstructed,
        } = request;
        let git = SystemGit::open(root)?;
        for planned in &plan.crates {
            crate::release::presentation::validate_inputs(root, planned)?;
        }
        let publish_registry = if skip_publish {
            None
        } else {
            let registry = release_config
                .registry_publication
                .registry()
                .ok_or_else(|| RailError::message("release state has no configured registry publication authority"))?;
            if registry != RELEASE_REGISTRY {
                return Err(RailError::message(format!(
                    "release state selected unsupported registry '{registry}'"
                )));
            }
            Some(registry.to_string())
        };
        let mut local_input_paths = plan
            .change_files_to_delete
            .iter()
            .cloned()
            .chain(plan.change_files_to_update.iter().map(|update| update.path.clone()))
            .map(|path| (path, BackupRestorePolicy::BeforeFirstCommit))
            .collect::<BTreeMap<_, _>>();
        for (path, restore) in control_paths.iter().filter_map(|path| {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                git.worktree_root.join(path)
            };
            (absolute.is_file() && crate::utils::path_relative_to(&git.worktree_root, &absolute).is_ok())
                .then_some((absolute, BackupRestorePolicy::Always))
        }) {
            local_input_paths.entry(path).or_insert(restore);
        }
        let local_input_backups = local_input_paths
            .into_iter()
            .map(|(path, restore)| {
                let content = std::fs::read_to_string(&path)
                    .map_err(|error| RailError::message(format!("failed to preserve {}: {}", path.display(), error)))?;
                Ok(LocalInputBackup { path, content, restore })
            })
            .collect::<RailResult<Vec<_>>>()?;
        let crates = plan
            .crates
            .iter()
            .map(|crate_plan| CrateReleaseState {
                name: crate_plan.name.clone(),
                commit: reconstructed
                    .as_ref()
                    .and_then(|release| release.commit_targets.get(&crate_plan.name))
                    .map(|commit| complete_step(Some(commit.clone())))
                    .unwrap_or_default(),
                tag: if skip_tag { complete_step(None) } else { Step::default() },
                forge_draft: Step::default(),
                publication: if skip_publish || !crate_plan.publish {
                    complete_step(None)
                } else {
                    Step::default()
                },
                forge_publication: Step::default(),
            })
            .collect();
        let state = Self {
            schema_version: RELEASE_STATE_SCHEMA_VERSION,
            transaction_id: transaction_id.clone(),
            status: ReleaseStatus::Active,
            phase: if reconstructed.is_some() {
                ReleasePhase::Prepared
            } else {
                ReleasePhase::Planned
            },
            mode,
            plan,
            release_config,
            remote_repository,
            publish_registry,
            skip_publish,
            skip_tag,
            initial_head,
            branch,
            planned_paths,
            control_paths,
            local_input_backups,
            crates,
            release_commit: reconstructed.map(|release| release.release_commit),
            commit_push: Step::default(),
            readiness: Step::default(),
            tag_push: Step::default(),
            abort: Step::default(),
            predecessor_execution: None,
        };
        state.validate_contract()?;
        state.validate_recovery_paths(&git.worktree_root)?;
        let path = state_dir(root).join(format!("{}.json", transaction_id));
        if path.exists() {
            let existing = Self::load(&path)?;
            let help = match existing.status {
                ReleaseStatus::Active => format!("resume it with 'cargo rail release resume {}'", path.display()),
                ReleaseStatus::Complete | ReleaseStatus::Aborted => {
                    format!(
                        "delete only this terminal journal with 'cargo rail clean --release-journal {}'",
                        state.transaction_id
                    )
                }
            };
            return Err(RailError::with_help(
                format!(
                    "release transaction '{}' already exists at '{}'",
                    transaction_id,
                    path.display()
                ),
                help,
            ));
        }
        let checkpoint = if state.phase == ReleasePhase::Prepared {
            "reconstructed"
        } else {
            "planned"
        };
        if let Err(error) = state.save(&path, checkpoint) {
            if path.exists() {
                return Err(error.context(format!(
                    "release journal may have been persisted at '{}'; inspect it with: cargo rail release status {}",
                    path.display(),
                    path.display()
                )));
            }
            return Err(error);
        }
        Ok((state, path))
    }

    pub fn load(path: &Path) -> RailResult<Self> {
        let bytes = std::fs::read(path)?;
        let (state, _) = Self::decode(&bytes, path, release_root(path), false)?;
        state.validate_journal_path(path)?;
        Ok(state)
    }

    /// Load one journal for a command that may mutate recovery state. A valid
    /// v0.25 journal is atomically upgraded before any recovery mutation or
    /// external reconciliation can occur.
    pub(crate) fn load_for_recovery(path: &Path) -> RailResult<Self> {
        let bytes = std::fs::read(path)?;
        let root = release_root(path);
        let (mut state, predecessor) = Self::decode(&bytes, path, root, true)?;
        state.validate_journal_path(path)?;
        if state.status == ReleaseStatus::Active {
            state.validate_recovery_paths(root)?;
            if predecessor {
                let date = chrono::Local::now().format("%Y-%m-%d").to_string();
                let github = crate::release::changelog::detect_github_repo(root);
                for plan in &mut state.plan.crates {
                    if plan.presentation.is_some() {
                        continue;
                    }
                    let mut input = plan.clone();
                    let existing = crate::release::presentation::read_optional(root, &plan.changelog_path)?;
                    if existing.as_deref().is_some_and(|text| {
                        crate::release::presentation::extract_section(text, &plan.new_version.to_string()).is_some()
                    }) {
                        input.changelog_body.clear();
                        input.current_version = input.new_version.clone();
                    }
                    plan.presentation = Some(crate::release::presentation::capture(
                        root,
                        &state.release_config,
                        &input,
                        &date,
                        github.as_ref(),
                    )?);
                }
                state.save(path, "migrated_v0_25")?;
            }
        }
        Ok(state)
    }

    fn decode(bytes: &[u8], path: &Path, root: &Path, capture_predecessor_inputs: bool) -> RailResult<(Self, bool)> {
        let schema: ReleaseStateSchema = serde_json::from_slice(bytes)
            .map_err(|error| RailError::message(format!("invalid release state '{}': {error}", path.display())))?;
        validate_transaction_id(&schema.transaction_id)?;
        validate_journal_path(&schema.transaction_id, path)?;
        if schema.schema_version == 5 {
            return v0_25::ReleaseStateV5::decode(bytes, path, root, capture_predecessor_inputs)
                .map(|state| (state, true));
        }
        if schema.schema_version == 6 {
            let mut state: Self = serde_json::from_slice(bytes)
                .map_err(|error| RailError::message(format!("invalid release state '{}': {error}", path.display())))?;
            if state.plan.plan_contract_version != 6 || state.plan.crates.iter().any(|plan| plan.presentation.is_some())
            {
                return Err(RailError::message(
                    "release state version 6 requires an unchanged version 6 plan",
                ));
            }
            state.schema_version = RELEASE_STATE_SCHEMA_VERSION;
            state.plan.plan_contract_version = RELEASE_PLAN_CONTRACT_VERSION;
            state.validate_contract()?;
            return Ok((state, true));
        }
        if schema.schema_version != RELEASE_STATE_SCHEMA_VERSION {
            return Err(RailError::message(format!(
                "unsupported release state version {}",
                schema.schema_version
            )));
        }
        let state: Self = serde_json::from_slice(bytes)
            .map_err(|error| RailError::message(format!("invalid release state '{}': {error}", path.display())))?;
        if state.schema_version != RELEASE_STATE_SCHEMA_VERSION {
            return Err(RailError::message(format!(
                "unsupported release state version {}",
                state.schema_version
            )));
        }
        state.validate_contract()?;
        if state.skip_publish != state.publish_registry.is_none()
            || state
                .publish_registry
                .as_deref()
                .is_some_and(|registry| registry != RELEASE_REGISTRY)
        {
            return Err(RailError::message(
                "release state contains inconsistent or unsupported registry publication authority",
            ));
        }
        Ok((state, false))
    }

    pub fn save(&self, path: &Path, checkpoint: &str) -> RailResult<()> {
        self.validate_contract()?;
        self.validate_journal_path(path)?;
        self.validate_recovery_paths(release_root(path))?;
        let parent = path
            .parent()
            .ok_or_else(|| RailError::message("release state path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        journal_fault("before", checkpoint)?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| RailError::message(format!("failed to serialize release state: {}", error)))?;
        crate::utils::write_file_atomic(path, &bytes)?;
        journal_fault("after", checkpoint)?;
        Ok(())
    }

    pub fn crate_index(&self, name: &str) -> RailResult<usize> {
        self.crates
            .iter()
            .position(|state| state.name == name)
            .ok_or_else(|| RailError::message(format!("release state has no crate '{}'", name)))
    }

    pub fn validate_recovery_paths(&self, worktree_root: &Path) -> RailResult<()> {
        normalize_release_paths(worktree_root, &self.planned_paths, "planned")?;
        normalize_release_paths(worktree_root, &self.control_paths, "control")?;
        for backup in &self.local_input_backups {
            normalize_release_path(worktree_root, &backup.path, "backup")?;
        }
        Ok(())
    }

    fn validate_contract(&self) -> RailResult<()> {
        if self.schema_version != RELEASE_STATE_SCHEMA_VERSION {
            return Err(RailError::message(format!(
                "cannot persist unsupported release state version {}",
                self.schema_version
            )));
        }
        if self.plan.plan_contract_version != RELEASE_PLAN_CONTRACT_VERSION {
            return Err(RailError::with_help(
                format!(
                    "release state version {} requires embedded release plan contract {}, found {}",
                    RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION, self.plan.plan_contract_version
                ),
                "resume with the cargo-rail version that created this state, or safely abort and replan",
            ));
        }
        validate_transaction_id(&self.transaction_id)?;
        Ok(())
    }

    fn validate_journal_path(&self, path: &Path) -> RailResult<()> {
        validate_journal_path(&self.transaction_id, path)
    }
}

fn validate_journal_path(transaction_id: &str, path: &Path) -> RailResult<()> {
    let expected = format!("{transaction_id}.json");
    if path.file_name().is_none_or(|name| name != expected.as_str()) {
        return Err(RailError::message(format!(
            "release journal '{}' does not match transaction identity '{}'",
            path.display(),
            transaction_id
        )));
    }
    Ok(())
}

fn validate_transaction_id(transaction_id: &str) -> RailResult<()> {
    if transaction_id.len() > "release-".len()
        && transaction_id.starts_with("release-")
        && transaction_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Ok(());
    }
    Err(RailError::message(format!(
        "release state contains invalid transaction identity '{transaction_id}'"
    )))
}

pub(crate) fn normalize_release_paths(
    worktree_root: &Path,
    paths: &[PathBuf],
    kind: &str,
) -> RailResult<BTreeSet<PathBuf>> {
    paths
        .iter()
        .map(|path| normalize_release_path(worktree_root, path, kind))
        .collect()
}

pub(crate) fn normalize_release_path(worktree_root: &Path, path: &Path, kind: &str) -> RailResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(RailError::message(format!(
            "release state contains an empty {kind} path"
        )));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree_root.join(path)
    };
    let relative = crate::utils::path_relative_to(worktree_root, &absolute).map_err(|error| {
        RailError::message(format!(
            "release {kind} path '{}' escapes Git worktree '{}': {error}",
            path.display(),
            worktree_root.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        return Err(RailError::message(format!(
            "release {kind} path '{}' names the Git worktree root",
            path.display()
        )));
    }
    Ok(relative)
}

pub(crate) fn validate_state_path(root: &Path, path: &Path) -> RailResult<PathBuf> {
    let canonical = canonicalize_existing(path)?;
    let dir = canonicalize_existing(&state_dir(root))?;
    if canonical.parent().is_none_or(|parent| parent != dir) {
        return Err(RailError::message(format!(
            "release state '{}' is outside the workspace release-state directory",
            path.display()
        )));
    }
    Ok(canonical)
}

pub(crate) fn prepare_recovery(root: &Path, path: &Path) -> RailResult<()> {
    let path = validate_state_path(root, path)?;
    let mut state = ReleaseState::load_for_recovery(&path)?;
    if state.status != ReleaseStatus::Active
        || !state
            .crates
            .iter()
            .any(|crate_state| crate_state.commit.status == StepStatus::InProgress)
    {
        return Ok(());
    }
    let git = SystemGit::open(root)?;
    state.validate_recovery_paths(&git.worktree_root)?;
    let head = git.head_commit()?;
    let message = git.run_git_stdout(&["log", "-1", "--format=%B"])?;
    let transaction = format!("Rail-Release: {}", state.transaction_id);
    let transaction_matches = message.lines().any(|line| line.trim() == transaction);
    if transaction_matches {
        if state.mode == ReleaseMode::Finalize
            && message.lines().any(|line| line.trim() == "Rail-Release-Mode: finalize")
        {
            for crate_state in &mut state.crates {
                crate_state.commit = complete_step(Some(head.clone()));
            }
            state.release_commit = Some(head);
            state.save(&path, "pre_context_finalize_commit_observed")?;
            return Ok(());
        }
        if let Some(index) = state
            .crates
            .iter()
            .position(|crate_state| crate_state.commit.status == StepStatus::InProgress)
        {
            let crate_name = state.crates[index].name.clone();
            let expected_parent = state.crates[index]
                .commit
                .object
                .as_deref()
                .ok_or_else(|| RailError::message("in-progress release commit has no recorded parent"))?;
            let parent = git.run_git_stdout(&["rev-parse", "HEAD^"]).unwrap_or_default();
            let subject = git.run_git_stdout(&["log", "-1", "--format=%s"])?;
            let expected_subject = state
                .plan
                .crates
                .iter()
                .find(|crate_plan| crate_plan.name == crate_name)
                .map(|crate_plan| format!("chore(release): {} v{}", crate_plan.name, crate_plan.new_version))
                .ok_or_else(|| RailError::message(format!("release state has no plan for '{}'", crate_name)))?;
            if parent == expected_parent && subject == expected_subject {
                state.crates[index].commit = complete_step(Some(head));
                state.save(&path, &format!("pre_context_commit_observed:{}", crate_name))?;
                return Ok(());
            }
        }
    }

    let mut allowed = normalize_release_paths(&git.worktree_root, &state.planned_paths, "planned")?;
    allowed.extend(normalize_release_paths(
        &git.worktree_root,
        &state.control_paths,
        "control",
    )?);
    let unexpected = git
        .changed_paths()?
        .into_iter()
        .filter(|changed| !allowed.contains(changed))
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(RailError::with_help(
            format!(
                "release recovery found unrelated changes: {}",
                unexpected
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "restore unrelated work before resuming or aborting the release",
        ));
    }
    git.run_git(&["reset", "--hard", "HEAD"])?;
    for planned in normalize_release_paths(&git.worktree_root, &state.planned_paths, "planned")? {
        let planned = planned
            .to_str()
            .ok_or_else(|| RailError::message(format!("release path '{}' is not valid UTF-8", planned.display())))?;
        git.run_git(&["clean", "-f", "--", planned])?;
    }
    let before_first_commit = !state.crates.iter().any(|crate_state| crate_state.commit.is_complete());
    for backup in &state.local_input_backups {
        if !before_first_commit && !matches!(backup.restore, BackupRestorePolicy::Always) {
            continue;
        }
        let relative = normalize_release_path(&git.worktree_root, &backup.path, "backup")?;
        crate::utils::write_file_atomic(&git.worktree_root.join(relative), backup.content.as_bytes())?;
    }
    state.save(&path, "pre_context_local_restore")
}

pub(crate) fn state_dir(root: &Path) -> PathBuf {
    crate::workspace::cargo_rail_state_root(root).join("releases")
}

fn release_root(path: &Path) -> &Path {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap_or_else(|| path.parent().unwrap_or(path))
}

fn complete_step(object: Option<String>) -> Step {
    Step {
        status: StepStatus::Complete,
        object,
    }
}

fn journal_fault(boundary: &str, checkpoint: &str) -> RailResult<()> {
    let variable = match boundary {
        "before" => "CARGO_RAIL_RELEASE_FAIL_BEFORE",
        _ => "CARGO_RAIL_RELEASE_FAIL_AFTER",
    };
    let Ok(requested) = std::env::var(variable) else {
        return Ok(());
    };
    let point = format!("journal:{}", checkpoint);
    if requested == "journal" || requested == point {
        return Err(RailError::message(format!(
            "injected release failure {} {}",
            boundary, point
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::planner::ReleaseSummary;

    fn fixture(schema_version: u32, plan_contract_version: u32) -> ReleaseState {
        ReleaseState {
            schema_version,
            transaction_id: "release-test".to_string(),
            status: ReleaseStatus::Active,
            phase: ReleasePhase::Planned,
            mode: ReleaseMode::Run,
            plan: ReleasePlan {
                plan_contract_version,
                snapshot_id: String::new(),
                source: Default::default(),
                canonical_crate_order: Vec::new(),
                crates: Vec::new(),
                summary: ReleaseSummary {
                    total_crates: 0,
                    crates_to_publish: 0,
                    crates_to_tag: 0,
                },
                change_files_to_delete: Vec::new(),
                change_files_to_update: Vec::new(),
                auxiliary_lockfiles: Vec::new(),
                skipped: Vec::new(),
            },
            release_config: ReleaseConfig::default(),
            remote_repository: None,
            publish_registry: None,
            skip_publish: true,
            skip_tag: true,
            initial_head: "head".to_string(),
            branch: "main".to_string(),
            planned_paths: Vec::new(),
            control_paths: Vec::new(),
            local_input_backups: Vec::new(),
            crates: Vec::new(),
            release_commit: None,
            commit_push: Step::default(),
            readiness: Step::default(),
            tag_push: Step::default(),
            abort: Step::default(),
            predecessor_execution: None,
        }
    }

    fn load_fixture(state: &ReleaseState) -> RailResult<ReleaseState> {
        load_value(serde_json::to_value(state)?)
    }

    fn load_value(value: serde_json::Value) -> RailResult<ReleaseState> {
        let directory = tempfile::tempdir()?;
        let transaction_id = value["transaction_id"].as_str().unwrap_or("release-test");
        let path = directory.path().join(format!("{transaction_id}.json"));
        std::fs::write(&path, serde_json::to_vec(&value)?)?;
        ReleaseState::load(&path)
    }

    #[test]
    fn current_state_requires_exact_embedded_plan_contract() {
        load_fixture(&fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION)).unwrap();
        for plan_contract in [4, RELEASE_PLAN_CONTRACT_VERSION + 1] {
            let error = load_fixture(&fixture(RELEASE_STATE_SCHEMA_VERSION, plan_contract)).unwrap_err();
            assert!(
                error.to_string().contains(&format!(
                    "requires embedded release plan contract {}",
                    RELEASE_PLAN_CONTRACT_VERSION
                )),
                "{error}"
            );
        }
    }

    #[test]
    fn current_state_requires_auxiliary_projection_field() {
        let mut state =
            serde_json::to_value(fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION)).unwrap();
        state["plan"].as_object_mut().unwrap().remove("auxiliary_lockfiles");
        let error = load_value(state).unwrap_err();
        assert!(error.to_string().contains("auxiliary_lockfiles"), "{error}");
    }

    #[test]
    fn current_state_serializes_release_paths_portably() {
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.planned_paths = vec![PathBuf::from(r"crates\fixture\Cargo.toml")];
        state.control_paths = vec![PathBuf::from(r"release-notes\fixture-v0.1.1.md")];
        state.local_input_backups = vec![LocalInputBackup {
            path: PathBuf::from(r".changes\fixture.md"),
            content: String::new(),
            restore: BackupRestorePolicy::BeforeFirstCommit,
        }];
        state.plan.change_files_to_delete = vec![PathBuf::from(r".changes\fixture.md")];

        let document = serde_json::to_value(state).unwrap();
        assert_eq!(
            document["planned_paths"],
            serde_json::json!(["crates/fixture/Cargo.toml"])
        );
        assert_eq!(
            document["control_paths"],
            serde_json::json!(["release-notes/fixture-v0.1.1.md"])
        );
        assert_eq!(document["local_input_backups"][0]["path"], ".changes/fixture.md");
        assert_eq!(
            document["plan"]["change_files_to_delete"],
            serde_json::json!([".changes/fixture.md"])
        );
    }

    #[test]
    fn future_state_schema_is_rejected() {
        let error = load_fixture(&fixture(
            RELEASE_STATE_SCHEMA_VERSION + 1,
            RELEASE_PLAN_CONTRACT_VERSION,
        ))
        .unwrap_err();
        assert!(
            error.to_string().contains(&format!(
                "unsupported release state version {}",
                RELEASE_STATE_SCHEMA_VERSION + 1
            )),
            "{error}"
        );
    }

    #[test]
    fn current_state_binds_transaction_identity_to_the_journal_filename() {
        let root = tempfile::tempdir().unwrap();
        let state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        let mismatched = root.path().join("release-renamed.json");
        std::fs::write(&mismatched, serde_json::to_vec(&state).unwrap()).unwrap();

        let error = ReleaseState::load(&mismatched).unwrap_err();
        assert!(
            error.to_string().contains("does not match transaction identity"),
            "{error}"
        );
        let save_path = root.path().join("release-also-renamed.json");
        let error = state.save(&save_path, "test").unwrap_err();
        assert!(
            error.to_string().contains("does not match transaction identity"),
            "{error}"
        );
        assert!(!save_path.exists());
    }

    #[test]
    fn current_state_rejects_invalid_transaction_identity_spelling() {
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.transaction_id = "release_bad".to_string();
        let error = state
            .save(&tempfile::tempdir().unwrap().path().join("release_bad.json"), "test")
            .unwrap_err();
        assert!(error.to_string().contains("invalid transaction identity"), "{error}");
    }

    #[test]
    fn v0_25_state_preserves_execution_authority_and_ambiguous_steps() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("release-v025-fixture.json");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/release/v0.25.0/state-v5.json"
            )),
        )
        .unwrap();

        let state = ReleaseState::load(&path).unwrap();
        assert_eq!(state.schema_version, RELEASE_STATE_SCHEMA_VERSION);
        assert_eq!(state.plan.plan_contract_version, RELEASE_PLAN_CONTRACT_VERSION);
        assert_eq!(state.transaction_id, "release-v025-fixture");
        assert_eq!(state.crates[0].commit.status, StepStatus::Complete);
        assert_eq!(state.crates[0].tag.status, StepStatus::InProgress);
        assert_eq!(state.crates[0].publication.status, StepStatus::InProgress);
        assert_eq!(
            state.commit_push.object.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );
        let predecessor = state.predecessor_execution.as_ref().unwrap();
        assert!(predecessor.require_changelog_entries);
        assert_eq!(predecessor.release_note_body("fixture-crate"), None);
        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["schema_version"], 5, "inspection must not rewrite a journal");
    }

    #[test]
    fn v0_25_recovery_is_atomically_rewritten_as_current_state() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("release-v025-fixture.json");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/release/v0.25.0/state-v5.json"
            )),
        )
        .unwrap();
        let release_notes = root.path().join("release-notes");
        std::fs::create_dir_all(&release_notes).unwrap();
        std::fs::write(
            release_notes.join("fixture-v0.1.1.md"),
            "Tag fallback must not replace the version override.\n",
        )
        .unwrap();
        std::fs::write(
            release_notes.join("v0.1.1.md"),
            "Exact live predecessor release body.\n",
        )
        .unwrap();

        ReleaseState::load_for_recovery(&path).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["schema_version"], RELEASE_STATE_SCHEMA_VERSION);
        assert_eq!(document["plan"]["plan_contract_version"], RELEASE_PLAN_CONTRACT_VERSION);
        assert_eq!(document["plan"]["source"], "changes");
        assert_eq!(
            document["predecessor_execution"]["release_note_bodies"]["fixture-crate"],
            "Exact live predecessor release body.\n"
        );
        assert_eq!(
            document["control_paths"],
            serde_json::json!(["release-notes/fixture-v0.1.1.md", "release-notes/v0.1.1.md"])
        );
    }

    #[test]
    fn v0_25_state_binds_transaction_identity_before_recovery_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("release-v025-renamed.json");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/release/v0.25.0/state-v5.json"
            )),
        )
        .unwrap();
        // A renamed journal must fail identity binding before predecessor
        // recovery inspects any live release-note input.
        std::fs::create_dir_all(root.path().join("release-notes/v0.1.1.md")).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(
            error.to_string().contains("does not match transaction identity"),
            "{error}"
        );
        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["schema_version"], 5, "identity failure must precede migration");
    }

    #[test]
    fn current_recovery_rejects_an_escaping_control_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.control_paths = vec![PathBuf::from("../outside-plan.json")];
        let path = directory.join("release-test.json");
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(error.to_string().contains("control path"), "{error}");
        assert!(error.to_string().contains("escapes Git worktree"), "{error}");
    }

    #[test]
    fn v0_25_recovery_rejects_an_escaping_control_path_before_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("release-v025-fixture.json");
        let mut document: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/release/v0.25.0/state-v5.json"
        )))
        .unwrap();
        document["control_paths"] = serde_json::json!(["../outside-plan.json"]);
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(error.to_string().contains("control path"), "{error}");
        assert!(error.to_string().contains("escapes Git worktree"), "{error}");
        let unchanged: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(unchanged["schema_version"], 5);
    }

    #[test]
    fn current_recovery_accepts_an_absolute_in_worktree_control_path() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let control = root.path().join("release-plan.json");
        std::fs::write(&control, "{}\n").unwrap();
        let mut state = fixture(RELEASE_STATE_SCHEMA_VERSION, RELEASE_PLAN_CONTRACT_VERSION);
        state.control_paths = vec![control];
        let path = directory.join("release-test.json");
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let loaded = ReleaseState::load_for_recovery(&path).unwrap();
        assert_eq!(
            normalize_release_paths(root.path(), &loaded.control_paths, "control").unwrap(),
            BTreeSet::from([PathBuf::from("release-plan.json")])
        );
    }

    #[test]
    fn v0_25_recovery_persists_a_missing_live_override_as_absent() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("release-v025-fixture.json");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/release/v0.25.0/state-v5.json"
            )),
        )
        .unwrap();

        ReleaseState::load_for_recovery(&path).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["schema_version"], RELEASE_STATE_SCHEMA_VERSION);
        assert_eq!(
            document["predecessor_execution"]["release_note_bodies"],
            serde_json::json!({})
        );
    }

    #[test]
    fn v0_25_recovery_uses_the_tag_fallback_when_the_version_override_is_missing() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        let release_notes = root.path().join("release-notes");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::create_dir_all(&release_notes).unwrap();
        let path = directory.join("release-v025-fixture.json");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/release/v0.25.0/state-v5.json"
            )),
        )
        .unwrap();
        std::fs::write(release_notes.join("fixture-v0.1.1.md"), "Exact tag fallback body.\n").unwrap();

        ReleaseState::load_for_recovery(&path).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            document["predecessor_execution"]["release_note_bodies"]["fixture-crate"],
            "Exact tag fallback body.\n"
        );
        assert_eq!(
            document["control_paths"],
            serde_json::json!(["release-notes/fixture-v0.1.1.md"])
        );
    }

    #[test]
    fn v0_25_recovery_rejects_a_non_utf8_live_override_before_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        let release_notes = root.path().join("release-notes");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::create_dir_all(&release_notes).unwrap();
        let path = directory.join("release-v025-fixture.json");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/release/v0.25.0/state-v5.json"
            )),
        )
        .unwrap();
        std::fs::write(release_notes.join("v0.1.1.md"), [0xff_u8, 0xfe]).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(error.to_string().contains("is not UTF-8"), "{error}");
        let unchanged: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(unchanged["schema_version"], 5);
    }

    #[test]
    fn v0_25_recovery_rejects_an_escaping_release_note_directory_before_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("release-v025-fixture.json");
        let mut document: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/release/v0.25.0/state-v5.json"
        )))
        .unwrap();
        document["release_config"]["release_notes_dir"] = serde_json::json!("../outside");
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(error.to_string().contains("escapes workspace"), "{error}");
        let unchanged: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(unchanged["schema_version"], 5);
    }

    #[cfg(unix)]
    #[test]
    fn v0_25_recovery_rejects_a_symlinked_release_note_before_rewrite() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let directory = state_dir(root.path());
        let release_notes = root.path().join("release-notes");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::create_dir_all(&release_notes).unwrap();
        let path = directory.join("release-v025-fixture.json");
        std::fs::write(
            &path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/release/v0.25.0/state-v5.json"
            )),
        )
        .unwrap();
        let target = root.path().join("real-note.md");
        std::fs::write(&target, "live body\n").unwrap();
        symlink(&target, release_notes.join("v0.1.1.md")).unwrap();

        let error = ReleaseState::load_for_recovery(&path).unwrap_err();
        assert!(error.to_string().contains("not a regular file"), "{error}");
        let unchanged: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(unchanged["schema_version"], 5);
    }
}
