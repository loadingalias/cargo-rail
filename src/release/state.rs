//! Durable, idempotent release execution state.

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::git::SystemGit;
use crate::release::planner::ReleasePlan;
use crate::utils::canonicalize_existing;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReleaseState {
    pub schema_version: u32,
    #[serde(default)]
    pub transaction_id: String,
    pub status: ReleaseStatus,
    #[serde(default)]
    pub phase: ReleasePhase,
    pub mode: ReleaseMode,
    pub plan: ReleasePlan,
    pub release_config: ReleaseConfig,
    pub skip_publish: bool,
    pub skip_tag: bool,
    pub initial_head: String,
    pub branch: String,
    pub planned_paths: Vec<PathBuf>,
    pub control_paths: Vec<PathBuf>,
    #[serde(default, alias = "change_file_backups")]
    pub local_input_backups: Vec<LocalInputBackup>,
    pub crates: Vec<CrateReleaseState>,
    #[serde(default)]
    pub release_commit: Option<String>,
    #[serde(default, alias = "push")]
    pub commit_push: Step,
    #[serde(default)]
    pub readiness: Step,
    #[serde(default)]
    pub tag_push: Step,
    #[serde(default)]
    pub abort: Step,
}

pub(crate) struct ReleaseStateCreate<'a> {
    pub(crate) root: &'a Path,
    pub(crate) transaction_id: String,
    pub(crate) mode: ReleaseMode,
    pub(crate) plan: ReleasePlan,
    pub(crate) release_config: ReleaseConfig,
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
            skip_publish,
            skip_tag,
            initial_head,
            branch,
            planned_paths,
            control_paths,
            reconstructed,
        } = request;
        let git = SystemGit::open(root)?;
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
            schema_version: 2,
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
        };
        let path = state_dir(root).join(format!("{}.json", transaction_id));
        if path.exists() {
            let existing = Self::load(&path)?;
            let help = match existing.status {
        ReleaseStatus::Active => format!("resume it with 'cargo rail release resume {}'", path.display()),
        ReleaseStatus::Complete | ReleaseStatus::Aborted => {
          "inspect it with 'cargo rail release status'; clean terminal journals before repeating the same transaction"
            .to_string()
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
        let mut state: Self = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|error| RailError::message(format!("invalid release state '{}': {}", path.display(), error)))?;
        if state.schema_version == 1 {
            state.migrate_v1();
        }
        if state.schema_version != 2 {
            return Err(RailError::message(format!(
                "unsupported release state version {}",
                state.schema_version
            )));
        }
        Ok(state)
    }

    pub fn save(&self, path: &Path, checkpoint: &str) -> RailResult<()> {
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
        for path in self
            .planned_paths
            .iter()
            .chain(self.local_input_backups.iter().map(|backup| &backup.path))
        {
            if path.as_os_str().is_empty() {
                return Err(RailError::message("release state contains an empty recovery path"));
            }
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                worktree_root.join(path)
            };
            crate::utils::path_relative_to(worktree_root, &absolute).map_err(|error| {
                RailError::message(format!(
                    "release recovery path '{}' escapes Git worktree '{}': {}",
                    path.display(),
                    worktree_root.display(),
                    error
                ))
            })?;
        }
        Ok(())
    }

    fn migrate_v1(&mut self) {
        let legacy = serde_json::to_vec(&(&self.plan, &self.initial_head, &self.branch)).unwrap_or_default();
        self.transaction_id = format!("release-legacy-{}", short_hash(&legacy));
        self.release_commit = self.crates.iter().rev().find_map(|state| state.commit.object.clone());
        self.phase = if self.status == ReleaseStatus::Complete {
            ReleasePhase::Released
        } else if self
            .crates
            .iter()
            .any(|state| state.publication.status != StepStatus::Pending)
        {
            ReleasePhase::Publishing
        } else if self.commit_push.is_complete() {
            ReleasePhase::Ready
        } else if self.release_commit.is_some() {
            ReleasePhase::Prepared
        } else {
            ReleasePhase::Planned
        };
        if self.commit_push.is_complete() {
            self.readiness = complete_step(self.commit_push.object.clone());
            if self.crates.iter().all(|state| state.tag.is_complete()) {
                self.tag_push = complete_step(self.commit_push.object.clone());
            }
        }
        self.schema_version = 2;
    }
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
    let mut state = ReleaseState::load(&path)?;
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

    let mut allowed = state
        .planned_paths
        .iter()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for control in &state.control_paths {
        if control.is_absolute() {
            if let Ok(relative) = control.strip_prefix(&git.worktree_root) {
                allowed.insert(relative.to_path_buf());
            }
        } else if !control.as_os_str().is_empty() {
            allowed.insert(control.clone());
        }
    }
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
    for planned in &state.planned_paths {
        if planned.as_os_str().is_empty() {
            return Err(RailError::message("release state contains an empty planned path"));
        }
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
        crate::utils::write_file_atomic(&backup.path, backup.content.as_bytes())?;
    }
    state.save(&path, "pre_context_local_restore")
}

pub(crate) fn state_dir(root: &Path) -> PathBuf {
    crate::workspace::cargo_rail_state_root(root).join("releases")
}

fn complete_step(object: Option<String>) -> Step {
    Step {
        status: StepStatus::Complete,
        object,
    }
}

fn short_hash(bytes: &[u8]) -> String {
    let hash = crate::utils::fnv1a64(bytes);
    format!("{:016x}", hash)
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
