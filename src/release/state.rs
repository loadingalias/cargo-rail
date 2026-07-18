//! Durable, idempotent release execution state.

use crate::config::ReleaseConfig;
use crate::error::{RailError, RailResult};
use crate::release::planner::ReleasePlan;
use crate::utils::canonicalize_existing;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReleaseState {
  pub schema_version: u32,
  pub status: ReleaseStatus,
  pub mode: ReleaseMode,
  pub plan: ReleasePlan,
  pub release_config: ReleaseConfig,
  pub skip_publish: bool,
  pub skip_tag: bool,
  pub initial_head: String,
  pub branch: String,
  pub planned_paths: Vec<PathBuf>,
  pub control_paths: Vec<PathBuf>,
  pub crates: Vec<CrateReleaseState>,
  pub push: Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReleaseStatus {
  Active,
  Complete,
  Aborted,
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
  #[allow(clippy::too_many_arguments)]
  pub fn create(
    root: &Path,
    mode: ReleaseMode,
    plan: ReleasePlan,
    release_config: ReleaseConfig,
    skip_publish: bool,
    skip_tag: bool,
    initial_head: String,
    branch: String,
    planned_paths: Vec<PathBuf>,
    control_paths: Vec<PathBuf>,
  ) -> RailResult<(Self, PathBuf)> {
    let complete_local = mode == ReleaseMode::Finalize;
    let crates = plan
      .crates
      .iter()
      .map(|crate_plan| CrateReleaseState {
        name: crate_plan.name.clone(),
        commit: if complete_local {
          complete_step(Some(initial_head.clone()))
        } else {
          Step::default()
        },
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
      schema_version: 1,
      status: ReleaseStatus::Active,
      mode,
      plan,
      release_config,
      skip_publish,
      skip_tag,
      initial_head,
      branch,
      planned_paths,
      control_paths,
      crates,
      push: Step::default(),
    };
    let json = serde_json::to_vec(&state)
      .map_err(|error| RailError::message(format!("failed to identify release state: {}", error)))?;
    let path = state_dir(root).join(format!("release-{}.json", short_hash(&json)));
    if path.exists() {
      let existing = Self::load(&path)?;
      if existing.status == ReleaseStatus::Active {
        return Err(RailError::with_help(
          format!("release execution is already active at '{}'", path.display()),
          format!("resume it with 'cargo rail release resume {}'", path.display()),
        ));
      }
    }
    state.save(&path)?;
    Ok((state, path))
  }

  pub fn load(path: &Path) -> RailResult<Self> {
    let state: Self = serde_json::from_slice(&std::fs::read(path)?)
      .map_err(|error| RailError::message(format!("invalid release state '{}': {}", path.display(), error)))?;
    if state.schema_version != 1 {
      return Err(RailError::message(format!(
        "unsupported release state version {}",
        state.schema_version
      )));
    }
    Ok(state)
  }

  pub fn save(&self, path: &Path) -> RailResult<()> {
    let parent = path
      .parent()
      .ok_or_else(|| RailError::message("release state path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(self)
      .map_err(|error| RailError::message(format!("failed to serialize release state: {}", error)))?;
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
  }

  pub fn crate_index(&self, name: &str) -> RailResult<usize> {
    self
      .crates
      .iter()
      .position(|state| state.name == name)
      .ok_or_else(|| RailError::message(format!("release state has no crate '{}'", name)))
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

fn state_dir(root: &Path) -> PathBuf {
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
