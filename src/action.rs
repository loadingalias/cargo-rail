//! Deterministic declarations and expansions for executable repository actions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::action_key::{ActionKeyAnalysis, ResolutionIdentity};
use crate::config::{
  BUILTIN_ACTION_NAMES, CargoEnvironmentValue, MAX_ACTIONS, RepositoryAction, RepositoryActionKind,
  RepositoryEnvironmentEntry, RepositoryPackageSelection, first_repository_output_overlap,
};
use crate::error::{RailError, RailResult};
use crate::source::RepositoryPath;
use crate::utils;

/// Built-in action behavior currently supported by `cargo rail run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionKind {
  Build,
  Test,
  Bench,
  Docs,
  Format,
  Lint,
  Msrv,
  Package,
  Audit,
  Distribution,
  GeneratedArtifact,
  Repository,
}

impl ActionKind {
  pub(crate) const fn as_str(self) -> &'static str {
    match self {
      Self::Build => "build",
      Self::Test => "test",
      Self::Bench => "bench",
      Self::Docs => "docs",
      Self::Format => "format",
      Self::Lint => "lint",
      Self::Msrv => "msrv",
      Self::Package => "package",
      Self::Audit => "audit",
      Self::Distribution => "distribution",
      Self::GeneratedArtifact => "generated_artifact",
      Self::Repository => "repository",
    }
  }

  pub(crate) fn from_name(name: &str) -> Option<Self> {
    match name {
      "build" => Some(Self::Build),
      "test" => Some(Self::Test),
      "bench" => Some(Self::Bench),
      "docs" => Some(Self::Docs),
      "format" => Some(Self::Format),
      "lint" => Some(Self::Lint),
      "msrv" => Some(Self::Msrv),
      "package" => Some(Self::Package),
      "audit" => Some(Self::Audit),
      "distribution" => Some(Self::Distribution),
      _ => None,
    }
  }

  pub(crate) const fn planner_surface(self) -> Option<&'static str> {
    match self {
      Self::Test => Some("test"),
      Self::Bench => Some("bench"),
      Self::Docs => Some("docs"),
      Self::Build | Self::Format | Self::Lint | Self::Msrv | Self::Package | Self::Audit | Self::Distribution => {
        Some("build")
      }
      Self::GeneratedArtifact | Self::Repository => None,
    }
  }
}

/// Logical working directory resolved only at the process boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActionWorkingDirectory {
  Workspace,
  Repository(ActionPath),
}

/// One validated repository-relative action path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ActionPath(RepositoryPath);

impl ActionPath {
  fn new(path: &str) -> RailResult<Self> {
    RepositoryPath::new(Path::new(path)).map(Self)
  }

  pub(crate) fn as_path(&self) -> &Path {
    self.0.as_path()
  }

  pub(crate) fn as_str(&self) -> &str {
    self.0.as_str()
  }
}

impl Serialize for ActionPath {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: serde::Serializer,
  {
    serializer.serialize_str(self.as_str())
  }
}

/// Why one action is present in an expanded request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ActionReason {
  /// The caller bypassed planner narrowing with `--all`.
  All,
  /// The planner enabled the action through one trace reason.
  Planner { surface: String, trace_id: u32 },
  /// A selected action requires this prerequisite.
  Dependency { action_id: String },
}

/// How selected packages are represented in an argv template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PackageArguments {
  None,
  Selected,
  WorkspaceOrSelected,
  AllOrSelected,
}

/// Which package domain an action declaration selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackageSelector {
  None,
  Selected,
  WorkspaceOrSelected,
}

/// Which platform domain an action declaration selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TargetSelector {
  None,
  CargoResolution,
  Explicit,
}

/// Exact Cargo feature domain represented by one expanded action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActionFeatureSelection {
  all_features: bool,
  default_features: bool,
  named: Vec<String>,
}

impl Default for ActionFeatureSelection {
  fn default() -> Self {
    Self {
      all_features: false,
      default_features: true,
      named: Vec::new(),
    }
  }
}

impl ActionFeatureSelection {
  pub(crate) fn requested(all_features: bool, default_features: bool, named: Vec<String>) -> Self {
    Self {
      all_features,
      default_features,
      named,
    }
  }

  fn configured(named: Vec<String>) -> Self {
    Self {
      named,
      ..Self::default()
    }
  }

  pub(crate) fn all_features(&self) -> bool {
    self.all_features
  }

  pub(crate) fn default_features(&self) -> bool {
    self.default_features
  }

  pub(crate) fn named(&self) -> &[String] {
    &self.named
  }
}

/// Exact Cargo resolution view loaded for one action target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActionResolutionBinding {
  root_package_ids: Vec<String>,
  target: Option<String>,
  features: ActionFeatureSelection,
  resolution_digest: String,
  resolved_node_count: usize,
  #[serde(skip)]
  resolved_local_roots: Vec<PathBuf>,
  #[serde(skip)]
  has_build_scripts: bool,
  #[serde(skip)]
  has_proc_macros: bool,
  #[serde(skip)]
  has_unverified_external_sources: bool,
}

impl ActionResolutionBinding {
  pub(crate) fn new(
    root_package_ids: Vec<String>,
    target: Option<String>,
    features: ActionFeatureSelection,
    resolution: ResolutionIdentity,
  ) -> Self {
    Self {
      root_package_ids,
      target,
      features,
      resolution_digest: resolution.digest,
      resolved_node_count: resolution.resolved_node_count,
      resolved_local_roots: resolution.local_package_roots,
      has_build_scripts: resolution.has_build_scripts,
      has_proc_macros: resolution.has_proc_macros,
      has_unverified_external_sources: resolution.has_unverified_external_sources,
    }
  }

  pub(crate) fn resolution_digest(&self) -> &str {
    &self.resolution_digest
  }

  pub(crate) fn target(&self) -> Option<&str> {
    self.target.as_deref()
  }

  pub(crate) fn resolved_local_roots(&self) -> &[PathBuf] {
    &self.resolved_local_roots
  }

  pub(crate) fn has_build_scripts(&self) -> bool {
    self.has_build_scripts
  }

  pub(crate) fn has_proc_macros(&self) -> bool {
    self.has_proc_macros
  }

  pub(crate) fn has_unverified_external_sources(&self) -> bool {
    self.has_unverified_external_sources
  }
}

/// Authoritative input domain declared by an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ActionInput {
  WorkspaceSnapshot,
  AmbientHost,
  Repository { path: ActionPath },
}

/// Output domain declared by an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ActionOutput {
  /// The process is not sandboxed and may write anywhere its caller can.
  AmbientProcess,
  /// A repository path owned by one generated action.
  Repository { path: ActionPath },
}

/// Environment boundary for an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActionEnvironment {
  inherit: bool,
  entries: Vec<ActionEnvironmentEntry>,
}

impl ActionEnvironment {
  fn ambient() -> Self {
    Self {
      inherit: true,
      entries: Vec::new(),
    }
  }

  pub(crate) fn inherit(&self) -> bool {
    self.inherit
  }

  pub(crate) fn entries(&self) -> &[ActionEnvironmentEntry] {
    &self.entries
  }
}

/// One redaction-safe expanded environment entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ActionEnvironmentEntry {
  Fixed { name: String, value: String },
  Pass { name: String },
  Cargo { name: String, value: CargoEnvironmentValue },
  Secret { name: String },
}

/// A shell-free argv template with one typed package insertion point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArgvTemplate {
  program: String,
  before_packages: Vec<String>,
  packages: PackageArguments,
  after_packages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActionCommandTemplate {
  Builtin(ArgvTemplate),
  Repository {
    regenerate: Vec<String>,
    check: Option<Vec<String>>,
  },
}

/// Operation represented by an expanded generated action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExpandedGeneratedMode {
  Check,
  Regenerate,
}

impl ExpandedGeneratedMode {
  const fn as_str(self) -> &'static str {
    match self {
      Self::Check => "check",
      Self::Regenerate => "regenerate",
    }
  }
}

/// Request-specific values used to expand one action declaration.
pub(crate) struct ActionExpansion<'a> {
  pub(crate) selected_packages: Vec<String>,
  pub(crate) use_workspace: bool,
  pub(crate) selected_targets: Vec<String>,
  pub(crate) selected_features: ActionFeatureSelection,
  pub(crate) platform: String,
  pub(crate) workspace_root: &'a Path,
  pub(crate) base_ref: &'a str,
  pub(crate) check_generated: bool,
  pub(crate) reasons: Vec<ActionReason>,
}

impl ArgvTemplate {
  pub(crate) fn new(
    program: impl Into<String>,
    before_packages: Vec<String>,
    packages: PackageArguments,
    after_packages: Vec<String>,
  ) -> Self {
    Self {
      program: program.into(),
      before_packages,
      packages,
      after_packages,
    }
  }

  fn expand(&self, selected_packages: &[String], use_workspace: bool) -> RailResult<Vec<String>> {
    if self.program.is_empty() {
      return Err(RailError::message("action program cannot be empty"));
    }
    if use_workspace
      && !matches!(
        self.packages,
        PackageArguments::WorkspaceOrSelected | PackageArguments::AllOrSelected
      )
    {
      return Err(RailError::message(
        "only a workspace-or-selected package template can use workspace expansion",
      ));
    }
    if self.packages == PackageArguments::None && !selected_packages.is_empty() {
      return Err(RailError::message(
        "an action without package arguments cannot select packages",
      ));
    }

    let package_arg_count = match self.packages {
      PackageArguments::None => 0,
      PackageArguments::Selected => selected_packages.len() * 2,
      PackageArguments::WorkspaceOrSelected if use_workspace => 1,
      PackageArguments::WorkspaceOrSelected => selected_packages.len() * 2,
      PackageArguments::AllOrSelected if use_workspace => 1,
      PackageArguments::AllOrSelected => selected_packages.len() * 2,
    };
    let mut argv = Vec::with_capacity(1 + self.before_packages.len() + package_arg_count + self.after_packages.len());
    argv.push(self.program.clone());
    argv.extend(self.before_packages.iter().cloned());
    match self.packages {
      PackageArguments::None => {}
      PackageArguments::Selected => push_selected_packages(&mut argv, selected_packages),
      PackageArguments::WorkspaceOrSelected if use_workspace => argv.push("--workspace".to_string()),
      PackageArguments::WorkspaceOrSelected => push_selected_packages(&mut argv, selected_packages),
      PackageArguments::AllOrSelected if use_workspace => argv.push("--all".to_string()),
      PackageArguments::AllOrSelected => push_selected_packages(&mut argv, selected_packages),
    }
    argv.extend(self.after_packages.iter().cloned());
    Ok(argv)
  }
}

fn push_selected_packages(argv: &mut Vec<String>, selected_packages: &[String]) {
  for package in selected_packages {
    argv.push("-p".to_string());
    argv.push(package.clone());
  }
}

/// A small action declaration before request-specific package expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionSpec {
  id: String,
  kind: ActionKind,
  command: ActionCommandTemplate,
  dependencies: Vec<String>,
  working_directory: ActionWorkingDirectory,
  package_selector: PackageSelector,
  target_selector: TargetSelector,
  inputs: Vec<ActionInput>,
  outputs: Vec<ActionOutput>,
  environment: ActionEnvironment,
  configured_targets: Vec<String>,
  configured_features: Vec<String>,
}

impl ActionSpec {
  pub(crate) fn builtin(kind: ActionKind, argv: ArgvTemplate) -> Self {
    let package_selector = match kind {
      ActionKind::Test => PackageSelector::Selected,
      ActionKind::Build
      | ActionKind::Bench
      | ActionKind::Format
      | ActionKind::Lint
      | ActionKind::Msrv
      | ActionKind::Package
      | ActionKind::Distribution => PackageSelector::WorkspaceOrSelected,
      ActionKind::Docs | ActionKind::Audit => PackageSelector::None,
      ActionKind::GeneratedArtifact | ActionKind::Repository => PackageSelector::None,
    };
    Self {
      id: kind.as_str().to_string(),
      kind,
      command: ActionCommandTemplate::Builtin(argv),
      dependencies: Vec::new(),
      working_directory: ActionWorkingDirectory::Workspace,
      package_selector,
      target_selector: match kind {
        ActionKind::Format | ActionKind::Package | ActionKind::Audit => TargetSelector::None,
        _ => TargetSelector::CargoResolution,
      },
      inputs: vec![ActionInput::WorkspaceSnapshot, ActionInput::AmbientHost],
      outputs: vec![ActionOutput::AmbientProcess],
      environment: ActionEnvironment::ambient(),
      configured_targets: Vec::new(),
      configured_features: Vec::new(),
    }
  }

  pub(crate) fn repository(id: &str, action: &RepositoryAction, workspace_root: &Path) -> RailResult<Self> {
    let working_directory = if action.working_directory == "." {
      ActionWorkingDirectory::Workspace
    } else {
      let configured = ActionPath::new(&action.working_directory)?;
      let (path, resolved) = authorize_action_path(workspace_root, &configured, true, "working directory")?;
      if !resolved.is_dir() {
        return Err(RailError::message(format!(
          "action '{}' working directory '{}' is not a directory",
          id,
          path.as_str()
        )));
      }
      ActionWorkingDirectory::Repository(path)
    };
    let mut inputs = Vec::with_capacity(action.inputs.len() + 1);
    for input in &action.inputs {
      if input == "." {
        inputs.push(ActionInput::WorkspaceSnapshot);
      } else {
        let configured = ActionPath::new(input)?;
        let (path, _) = authorize_action_path(workspace_root, &configured, true, "input")?;
        inputs.push(ActionInput::Repository { path });
      }
    }
    inputs.push(ActionInput::AmbientHost);

    let mut outputs = Vec::with_capacity(action.outputs.len() + 1);
    for output in &action.outputs {
      let configured = ActionPath::new(output)?;
      let (path, _) = authorize_action_path(workspace_root, &configured, false, "output")?;
      outputs.push(ActionOutput::Repository { path });
    }
    outputs.push(ActionOutput::AmbientProcess);

    let entries = action
      .environment
      .entries
      .iter()
      .map(|entry| match entry {
        RepositoryEnvironmentEntry::Fixed { name, value } => ActionEnvironmentEntry::Fixed {
          name: name.clone(),
          value: value.clone(),
        },
        RepositoryEnvironmentEntry::Pass { name } => ActionEnvironmentEntry::Pass { name: name.clone() },
        RepositoryEnvironmentEntry::Cargo { name, value } => ActionEnvironmentEntry::Cargo {
          name: name.clone(),
          value: *value,
        },
        RepositoryEnvironmentEntry::Secret { name } => ActionEnvironmentEntry::Secret { name: name.clone() },
      })
      .collect();
    let package_selector = match action.packages {
      RepositoryPackageSelection::None => PackageSelector::None,
      RepositoryPackageSelection::Selected => PackageSelector::Selected,
      RepositoryPackageSelection::WorkspaceOrSelected => PackageSelector::WorkspaceOrSelected,
    };
    Ok(Self {
      id: id.to_string(),
      kind: match action.kind {
        RepositoryActionKind::Task => ActionKind::Repository,
        RepositoryActionKind::Generated => ActionKind::GeneratedArtifact,
      },
      command: ActionCommandTemplate::Repository {
        regenerate: action.argv.clone(),
        check: (action.kind == RepositoryActionKind::Generated).then(|| action.check_argv.clone()),
      },
      dependencies: action.dependencies.clone(),
      working_directory,
      package_selector,
      target_selector: if action.targets.is_empty() {
        TargetSelector::None
      } else {
        TargetSelector::Explicit
      },
      inputs,
      outputs,
      environment: ActionEnvironment {
        inherit: action.environment.inherit,
        entries,
      },
      configured_targets: action.targets.clone(),
      configured_features: action.features.clone(),
    })
  }

  pub(crate) fn expand(&self, expansion: ActionExpansion<'_>) -> RailResult<ExpandedAction> {
    if expansion.reasons.is_empty() {
      return Err(RailError::message(format!(
        "action '{}' cannot be expanded without a reason",
        self.id
      )));
    }
    match self.package_selector {
      PackageSelector::None if !expansion.selected_packages.is_empty() => {
        return Err(RailError::message(format!(
          "action '{}' does not select packages",
          self.id
        )));
      }
      PackageSelector::Selected | PackageSelector::WorkspaceOrSelected if expansion.selected_packages.is_empty() => {
        return Err(RailError::message(format!(
          "action '{}' requires at least one selected package",
          self.id
        )));
      }
      PackageSelector::None | PackageSelector::Selected | PackageSelector::WorkspaceOrSelected => {}
    }
    let selected_targets = match self.target_selector {
      TargetSelector::None => Vec::new(),
      TargetSelector::CargoResolution => expansion.selected_targets,
      TargetSelector::Explicit => self.configured_targets.clone(),
    };
    let selected_features = if self.configured_features.is_empty() {
      expansion.selected_features
    } else {
      ActionFeatureSelection::configured(self.configured_features.clone())
    };
    let generated_mode = match self.kind {
      ActionKind::GeneratedArtifact if expansion.check_generated => Some(ExpandedGeneratedMode::Check),
      ActionKind::GeneratedArtifact => Some(ExpandedGeneratedMode::Regenerate),
      _ => None,
    };
    let argv = match &self.command {
      ActionCommandTemplate::Builtin(argv) => argv.expand(&expansion.selected_packages, expansion.use_workspace)?,
      ActionCommandTemplate::Repository { regenerate, check } => {
        let template = if expansion.check_generated && self.kind == ActionKind::GeneratedArtifact {
          check
            .as_deref()
            .ok_or_else(|| RailError::message(format!("generated action '{}' has no check argv", self.id)))?
        } else {
          regenerate
        };
        expand_repository_argv(
          template,
          RepositoryExpansion {
            packages: self.package_selector,
            selected_packages: &expansion.selected_packages,
            use_workspace: expansion.use_workspace,
            selected_targets: &selected_targets,
            selected_features: selected_features.named(),
            workspace_root: expansion.workspace_root,
            base_ref: expansion.base_ref,
          },
        )?
      }
    };
    Ok(ExpandedAction {
      id: self.id.clone(),
      kind: self.kind,
      argv,
      working_directory: self.working_directory.clone(),
      selected_packages: expansion.selected_packages,
      selected_targets,
      selected_features,
      resolution_views: Vec::new(),
      platform: expansion.platform,
      generated_mode,
      dependencies: self.dependencies.clone(),
      reasons: expansion.reasons,
      package_selector: self.package_selector,
      target_selector: self.target_selector,
      inputs: self.inputs.clone(),
      outputs: self.outputs.clone(),
      environment: self.environment.clone(),
      action_key: None,
    })
  }
}

fn authorize_action_path(
  workspace_root: &Path,
  path: &ActionPath,
  must_exist: bool,
  label: &str,
) -> RailResult<(ActionPath, PathBuf)> {
  let candidate = workspace_root.join(path.as_path());
  let relative = utils::path_relative_to(workspace_root, &candidate).map_err(|error| {
    RailError::with_help(
      format!("action {label} '{}' escapes the workspace: {error}", path.as_str()),
      "use a repository-relative path that does not traverse an external symlink",
    )
  })?;
  if relative.as_os_str().is_empty() {
    return Err(RailError::message(format!(
      "action {label} must not resolve to the workspace root"
    )));
  }
  let path = ActionPath::new(
    relative
      .to_str()
      .ok_or_else(|| RailError::message(format!("action {label} path is not valid UTF-8")))?,
  )?;
  let resolved = workspace_root.join(path.as_path());
  if must_exist {
    std::fs::symlink_metadata(&resolved).map_err(|error| {
      RailError::message(format!(
        "action {label} '{}' cannot be inspected: {error}",
        path.as_str()
      ))
    })?;
  }
  Ok((path, resolved))
}

struct RepositoryExpansion<'a> {
  packages: PackageSelector,
  selected_packages: &'a [String],
  use_workspace: bool,
  selected_targets: &'a [String],
  selected_features: &'a [String],
  workspace_root: &'a Path,
  base_ref: &'a str,
}

fn expand_repository_argv(template: &[String], expansion: RepositoryExpansion<'_>) -> RailResult<Vec<String>> {
  let mut argv = Vec::with_capacity(
    template.len() + expansion.selected_packages.len() * 2 + expansion.selected_targets.len() * 2 + 1,
  );
  let Some(program) = template.first() else {
    return Err(RailError::message("repository action argv cannot be empty"));
  };
  argv.push(program.clone());
  for argument in &template[1..] {
    match argument.as_str() {
      "{workspace_root}" => argv.push(expansion.workspace_root.display().to_string()),
      "{base_ref}" => {
        if expansion.base_ref.is_empty() {
          return Err(RailError::with_help(
            "repository action requires {base_ref}, but no planner or profile baseline resolved it",
            "run with a planner baseline or remove the {base_ref} argument token",
          ));
        }
        argv.push(expansion.base_ref.to_string());
      }
      "{packages}" => match expansion.packages {
        PackageSelector::None => {
          return Err(RailError::message(
            "repository action contains {packages} without a package selector",
          ));
        }
        PackageSelector::WorkspaceOrSelected if expansion.use_workspace => argv.push("--workspace".to_string()),
        PackageSelector::Selected | PackageSelector::WorkspaceOrSelected => {
          push_selected_packages(&mut argv, expansion.selected_packages);
        }
      },
      "{targets}" => {
        for target in expansion.selected_targets {
          argv.push("--target".to_string());
          argv.push(target.clone());
        }
      }
      "{features}" => {
        if !expansion.selected_features.is_empty() {
          argv.push("--features".to_string());
          argv.push(expansion.selected_features.join(","));
        }
      }
      _ => argv.push(argument.clone()),
    }
  }
  Ok(argv)
}

/// One exact, ordered action ready for preview or direct process execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExpandedAction {
  id: String,
  kind: ActionKind,
  argv: Vec<String>,
  working_directory: ActionWorkingDirectory,
  selected_packages: Vec<String>,
  selected_targets: Vec<String>,
  selected_features: ActionFeatureSelection,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  resolution_views: Vec<ActionResolutionBinding>,
  platform: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  generated_mode: Option<ExpandedGeneratedMode>,
  dependencies: Vec<String>,
  reasons: Vec<ActionReason>,
  package_selector: PackageSelector,
  target_selector: TargetSelector,
  inputs: Vec<ActionInput>,
  outputs: Vec<ActionOutput>,
  environment: ActionEnvironment,
  #[serde(skip_serializing_if = "Option::is_none")]
  action_key: Option<ActionKeyAnalysis>,
}

impl ExpandedAction {
  pub(crate) fn kind(&self) -> ActionKind {
    self.kind
  }

  pub(crate) fn argv(&self) -> &[String] {
    &self.argv
  }

  pub(crate) fn selected_packages(&self) -> &[String] {
    &self.selected_packages
  }

  pub(crate) fn selected_targets(&self) -> &[String] {
    &self.selected_targets
  }

  pub(crate) fn selected_features(&self) -> &ActionFeatureSelection {
    &self.selected_features
  }

  pub(crate) fn bind_resolution_views(&mut self, bindings: Vec<ActionResolutionBinding>) {
    self.resolution_views = bindings;
  }

  pub(crate) fn bind_action_key(&mut self, action_key: ActionKeyAnalysis) {
    self.action_key = Some(action_key);
  }

  pub(crate) fn id(&self) -> &str {
    &self.id
  }

  fn dependencies(&self) -> &[String] {
    &self.dependencies
  }

  pub(crate) fn key_dependencies(&self) -> &[String] {
    &self.dependencies
  }

  pub(crate) fn working_directory(&self) -> &ActionWorkingDirectory {
    &self.working_directory
  }

  pub(crate) fn resolution_views(&self) -> &[ActionResolutionBinding] {
    &self.resolution_views
  }

  pub(crate) fn platform(&self) -> &str {
    &self.platform
  }

  pub(crate) fn generated_mode(&self) -> Option<&'static str> {
    self.generated_mode.map(ExpandedGeneratedMode::as_str)
  }

  pub(crate) fn inputs(&self) -> &[ActionInput] {
    &self.inputs
  }

  pub(crate) fn key_outputs(&self) -> &[ActionOutput] {
    &self.outputs
  }

  pub(crate) fn action_key(&self) -> Option<&ActionKeyAnalysis> {
    self.action_key.as_ref()
  }

  fn outputs(&self) -> &[ActionOutput] {
    &self.outputs
  }

  pub(crate) fn environment(&self) -> &ActionEnvironment {
    &self.environment
  }

  pub(crate) fn is_generated(&self) -> bool {
    self.kind == ActionKind::GeneratedArtifact
  }

  pub(crate) fn repository_outputs(&self) -> impl Iterator<Item = &str> {
    self.outputs.iter().filter_map(|output| match output {
      ActionOutput::Repository { path } => Some(path.as_str()),
      ActionOutput::AmbientProcess => None,
    })
  }

  pub(crate) fn validate_runtime_environment(&self) -> RailResult<()> {
    for entry in self.environment.entries() {
      if let ActionEnvironmentEntry::Secret { name } = entry
        && std::env::var_os(name).is_none()
      {
        return Err(RailError::with_help(
          format!("action '{}' requires secret environment capability '{name}'", self.id),
          format!("set {name} in the process environment before running this action graph"),
        ));
      }
    }
    Ok(())
  }

  pub(crate) fn validate_paths(&self, workspace_root: &Path) -> RailResult<PathBuf> {
    let working_directory = match &self.working_directory {
      ActionWorkingDirectory::Workspace => utils::canonicalize_existing(workspace_root).map_err(|error| {
        RailError::message(format!(
          "workspace root '{}' cannot be resolved: {error}",
          workspace_root.display()
        ))
      })?,
      ActionWorkingDirectory::Repository(path) => {
        let (current, resolved) = authorize_action_path(workspace_root, path, true, "working directory")?;
        if current != *path {
          return Err(RailError::message(format!(
            "action '{}' working directory changed resolution from '{}' to '{}'",
            self.id,
            path.as_str(),
            current.as_str()
          )));
        }
        resolved
      }
    };
    for input in &self.inputs {
      if let ActionInput::Repository { path } = input {
        let (current, _) = authorize_action_path(workspace_root, path, true, "input")?;
        if current != *path {
          return Err(RailError::message(format!(
            "action '{}' input changed resolution from '{}' to '{}'",
            self.id,
            path.as_str(),
            current.as_str()
          )));
        }
      }
    }
    for output in &self.outputs {
      if let ActionOutput::Repository { path } = output {
        let (current, _) = authorize_action_path(workspace_root, path, false, "output")?;
        if current != *path {
          return Err(RailError::message(format!(
            "action '{}' output changed resolution from '{}' to '{}'",
            self.id,
            path.as_str(),
            current.as_str()
          )));
        }
      }
    }
    Ok(working_directory)
  }
}

/// A snapshot-bound action DAG in deterministic topological order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActionGraph {
  snapshot_id: String,
  actions: Vec<ExpandedAction>,
}

impl ActionGraph {
  pub(crate) fn new(snapshot_id: String, actions: Vec<ExpandedAction>) -> RailResult<Self> {
    if snapshot_id.is_empty() {
      return Err(RailError::message("action graph snapshot id cannot be empty"));
    }
    let max_expanded_actions = MAX_ACTIONS + BUILTIN_ACTION_NAMES.len();
    if actions.len() > max_expanded_actions {
      return Err(RailError::message(format!(
        "action graph contains {} actions; at most {max_expanded_actions} are allowed",
        actions.len()
      )));
    }
    let mut indices = BTreeMap::new();
    for (index, action) in actions.iter().enumerate() {
      if indices.insert(action.id().to_string(), index).is_some() {
        return Err(RailError::message(format!("duplicate action id '{}'", action.id())));
      }
    }

    let mut indegree = vec![0usize; actions.len()];
    let mut dependents = vec![Vec::new(); actions.len()];
    for (index, action) in actions.iter().enumerate() {
      let mut unique_dependencies = BTreeSet::new();
      for dependency in action.dependencies() {
        if !unique_dependencies.insert(dependency) {
          return Err(RailError::message(format!(
            "action '{}' repeats dependency '{}'",
            action.id(),
            dependency
          )));
        }
        let Some(&dependency_index) = indices.get(dependency) else {
          return Err(RailError::message(format!(
            "action '{}' depends on unknown action '{}'",
            action.id(),
            dependency
          )));
        };
        indegree[index] += 1;
        dependents[dependency_index].push(index);
      }
    }

    validate_repository_output_ownership(&actions)?;

    let mut ready = indegree
      .iter()
      .enumerate()
      .filter_map(|(index, count)| (*count == 0).then_some(index))
      .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(actions.len());
    while let Some(index) = ready.pop_first() {
      order.push(index);
      for dependent in &dependents[index] {
        indegree[*dependent] -= 1;
        if indegree[*dependent] == 0 {
          ready.insert(*dependent);
        }
      }
    }
    if order.len() != actions.len() {
      let cyclic = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count > 0).then_some(actions[index].id()))
        .collect::<Vec<_>>()
        .join(", ");
      return Err(RailError::message(format!(
        "action dependency cycle contains: {cyclic}"
      )));
    }

    let mut slots = actions.into_iter().map(Some).collect::<Vec<_>>();
    let actions = order
      .into_iter()
      .map(|index| {
        slots[index]
          .take()
          .ok_or_else(|| RailError::message("action ordered twice"))
      })
      .collect::<RailResult<Vec<_>>>()?;
    Ok(Self { snapshot_id, actions })
  }

  pub(crate) fn snapshot_id(&self) -> &str {
    &self.snapshot_id
  }

  pub(crate) fn actions(&self) -> &[ExpandedAction] {
    &self.actions
  }
}

fn validate_repository_output_ownership(actions: &[ExpandedAction]) -> RailResult<()> {
  let mut owners = Vec::new();
  for action in actions {
    for output in action.outputs() {
      if let ActionOutput::Repository { path } = output {
        owners.push((path.as_str(), action.id()));
      }
    }
  }
  if let Some((path, owner, other_path, other_owner)) = first_repository_output_overlap(owners) {
    return Err(RailError::message(format!(
      "action output '{path}' owned by '{owner}' overlaps '{other_path}' owned by '{other_owner}'"
    )));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn built_in_action_inventory_matches_expansion_kinds() {
    for name in BUILTIN_ACTION_NAMES {
      let kind = ActionKind::from_name(name).expect("configured built-in must expand");
      assert_eq!(kind.as_str(), *name);
    }
  }

  fn expansion(
    selected_packages: Vec<String>,
    use_workspace: bool,
    reasons: Vec<ActionReason>,
  ) -> ActionExpansion<'static> {
    ActionExpansion {
      selected_packages,
      use_workspace,
      selected_targets: vec!["x86_64-unknown-linux-gnu".to_string()],
      selected_features: ActionFeatureSelection::default(),
      platform: "x86_64-unknown-linux-gnu".to_string(),
      workspace_root: Path::new("."),
      base_ref: "main",
      check_generated: false,
      reasons,
    }
  }

  fn action(id: &str, dependencies: &[&str]) -> ExpandedAction {
    let mut spec = ActionSpec::builtin(
      ActionKind::Build,
      ArgvTemplate::new(
        "cargo",
        vec!["check".to_string()],
        PackageArguments::Selected,
        Vec::new(),
      ),
    );
    spec.id = id.to_string();
    spec.dependencies = dependencies
      .iter()
      .map(|dependency| (*dependency).to_string())
      .collect();
    spec
      .expand(expansion(vec!["crate-a".to_string()], false, vec![ActionReason::All]))
      .expect("test action should expand")
  }

  #[test]
  fn workspace_or_selected_expansion_is_exact() {
    let spec = ActionSpec::builtin(
      ActionKind::Build,
      ArgvTemplate::new(
        "cargo",
        vec!["check".to_string()],
        PackageArguments::WorkspaceOrSelected,
        vec!["--locked".to_string()],
      ),
    );
    let packages = vec!["crate-a".to_string(), "crate-b".to_string()];

    let workspace = spec
      .expand(expansion(packages.clone(), true, vec![ActionReason::All]))
      .expect("workspace action should expand");
    assert_eq!(workspace.argv(), ["cargo", "check", "--workspace", "--locked"]);

    let selected = spec
      .expand(expansion(
        packages,
        false,
        vec![ActionReason::Planner {
          surface: "build".to_string(),
          trace_id: 7,
        }],
      ))
      .expect("selected action should expand");
    assert_eq!(
      selected.argv(),
      ["cargo", "check", "-p", "crate-a", "-p", "crate-b", "--locked"]
    );
  }

  #[test]
  fn expansion_rejects_an_action_without_authorizing_reasons() {
    let spec = ActionSpec::builtin(
      ActionKind::Docs,
      ArgvTemplate::new("cargo", vec!["doc".to_string()], PackageArguments::None, Vec::new()),
    );

    let error = spec
      .expand(expansion(Vec::new(), false, Vec::new()))
      .expect_err("reasonless actions must fail closed");
    assert!(error.to_string().contains("without a reason"));
  }

  #[test]
  fn graph_orders_dependencies_stably() {
    let graph = ActionGraph::new(
      "snapshot".to_string(),
      vec![action("lint", &["build"]), action("docs", &[]), action("build", &[])],
    )
    .expect("valid graph should order");

    assert_eq!(
      graph.actions().iter().map(ExpandedAction::id).collect::<Vec<_>>(),
      ["docs", "build", "lint"]
    );
  }

  #[test]
  fn graph_rejects_duplicate_ids() {
    let error = ActionGraph::new("snapshot".to_string(), vec![action("build", &[]), action("build", &[])])
      .expect_err("duplicate ids must fail closed");

    assert!(error.to_string().contains("duplicate action id 'build'"));
  }

  #[test]
  fn graph_rejects_unknown_and_repeated_dependencies() {
    let unknown = ActionGraph::new("snapshot".to_string(), vec![action("lint", &["missing"])])
      .expect_err("unknown dependencies must fail closed");
    assert!(unknown.to_string().contains("depends on unknown action 'missing'"));

    let repeated = ActionGraph::new(
      "snapshot".to_string(),
      vec![action("build", &[]), action("lint", &["build", "build"])],
    )
    .expect_err("repeated dependencies must fail closed");
    assert!(repeated.to_string().contains("repeats dependency 'build'"));
  }

  #[test]
  fn graph_rejects_cycles() {
    let error = ActionGraph::new(
      "snapshot".to_string(),
      vec![action("build", &["lint"]), action("lint", &["build"])],
    )
    .expect_err("dependency cycles must fail closed");

    assert!(
      error
        .to_string()
        .contains("action dependency cycle contains: build, lint")
    );
  }

  #[test]
  fn graph_rejects_an_empty_snapshot_id() {
    let error =
      ActionGraph::new(String::new(), vec![action("build", &[])]).expect_err("unbound graphs must fail closed");

    assert!(error.to_string().contains("snapshot id cannot be empty"));
  }

  #[test]
  fn graph_rejects_unbounded_action_sets() {
    let actions = (0..(MAX_ACTIONS + BUILTIN_ACTION_NAMES.len() + 1))
      .map(|index| {
        let mut action = action("build", &[]);
        action.id = format!("action-{index}");
        action
      })
      .collect();
    let error = ActionGraph::new("snapshot".to_string(), actions).expect_err("oversized graph must fail closed");
    assert!(error.to_string().contains("at most"));
  }

  #[test]
  fn repository_action_expands_typed_tokens_and_redacts_secret_values() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    std::fs::write(workspace.path().join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
    let action = RepositoryAction {
      argv: vec![
        "tool".to_string(),
        "{packages}".to_string(),
        "{targets}".to_string(),
        "{features}".to_string(),
        "{base_ref}".to_string(),
      ],
      when: vec!["build".to_string()],
      packages: RepositoryPackageSelection::Selected,
      targets: vec!["wasm32-wasip2".to_string()],
      features: vec!["serde".to_string()],
      inputs: vec!["Cargo.toml".to_string()],
      environment: crate::config::RepositoryEnvironment {
        inherit: false,
        entries: vec![RepositoryEnvironmentEntry::Secret {
          name: "_CARGO_RAIL_TEST_MISSING_SECRET_CAPABILITY_".to_string(),
        }],
      },
      ..RepositoryAction::default()
    };
    let spec = ActionSpec::repository("verify", &action, workspace.path()).expect("repository action declaration");
    let expanded = spec
      .expand(ActionExpansion {
        selected_packages: vec!["crate-a".to_string()],
        use_workspace: false,
        selected_targets: Vec::new(),
        selected_features: ActionFeatureSelection::default(),
        platform: "aarch64-apple-darwin".to_string(),
        workspace_root: workspace.path(),
        base_ref: "origin/main",
        check_generated: false,
        reasons: vec![ActionReason::All],
      })
      .expect("repository action expansion");

    assert_eq!(
      expanded.argv(),
      [
        "tool",
        "-p",
        "crate-a",
        "--target",
        "wasm32-wasip2",
        "--features",
        "serde",
        "origin/main"
      ]
    );
    let json = serde_json::to_string(&expanded).expect("expanded action JSON");
    assert!(json.contains("_CARGO_RAIL_TEST_MISSING_SECRET_CAPABILITY_"));
    assert!(!json.contains("secret_value"));
    let error = expanded
      .validate_runtime_environment()
      .expect_err("missing secret capability must fail before execution");
    assert!(error.to_string().contains("requires secret environment capability"));
  }

  #[test]
  fn graph_rejects_overlapping_generated_outputs() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let first_config = RepositoryAction {
      kind: RepositoryActionKind::Generated,
      argv: vec!["first".to_string()],
      check_argv: vec!["first".to_string(), "--check".to_string()],
      when: vec!["docs".to_string()],
      outputs: vec!["generated".to_string()],
      ..RepositoryAction::default()
    };
    let first = ActionSpec::repository("first", &first_config, workspace.path())
      .expect("first declaration")
      .expand(ActionExpansion {
        selected_packages: Vec::new(),
        use_workspace: false,
        selected_targets: Vec::new(),
        selected_features: ActionFeatureSelection::default(),
        platform: "host".to_string(),
        workspace_root: workspace.path(),
        base_ref: "main",
        check_generated: false,
        reasons: vec![ActionReason::All],
      })
      .expect("first expansion");
    let second_config = RepositoryAction {
      kind: RepositoryActionKind::Generated,
      argv: vec!["second".to_string()],
      check_argv: vec!["second".to_string(), "--check".to_string()],
      when: vec!["docs".to_string()],
      outputs: vec!["generated/api".to_string()],
      ..RepositoryAction::default()
    };
    let second = ActionSpec::repository("second", &second_config, workspace.path())
      .expect("second declaration")
      .expand(ActionExpansion {
        selected_packages: Vec::new(),
        use_workspace: false,
        selected_targets: Vec::new(),
        selected_features: ActionFeatureSelection::default(),
        platform: "host".to_string(),
        workspace_root: workspace.path(),
        base_ref: "main",
        check_generated: false,
        reasons: vec![ActionReason::All],
      })
      .expect("second expansion");

    let error = ActionGraph::new("snapshot".to_string(), vec![first, second])
      .expect_err("overlapping output ownership must fail closed");
    assert!(error.to_string().contains("overlaps"));
  }

  #[test]
  fn generated_action_expands_one_deterministic_mode_specific_argv() {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let action = RepositoryAction {
      kind: RepositoryActionKind::Generated,
      argv: vec!["generator".to_string(), "regenerate".to_string()],
      check_argv: vec!["generator".to_string(), "check".to_string()],
      when: vec!["build".to_string()],
      outputs: vec!["generated/output.rs".to_string()],
      ..RepositoryAction::default()
    };
    let spec = ActionSpec::repository("codegen", &action, workspace.path()).expect("generated declaration");

    let expanded = spec
      .expand(ActionExpansion {
        selected_packages: Vec::new(),
        use_workspace: false,
        selected_targets: Vec::new(),
        selected_features: ActionFeatureSelection::default(),
        platform: "host".to_string(),
        workspace_root: workspace.path(),
        base_ref: "main",
        check_generated: true,
        reasons: vec![ActionReason::All],
      })
      .expect("generated check expansion");

    assert_eq!(expanded.argv(), ["generator", "check"]);
    assert!(
      serde_json::to_string(&expanded)
        .unwrap()
        .contains("\"generated_mode\":\"check\"")
    );
  }

  #[cfg(unix)]
  #[test]
  fn repository_action_rejects_symlink_path_escape() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("temporary workspace");
    let outside = tempfile::tempdir().expect("outside directory");
    symlink(outside.path(), workspace.path().join("escape")).expect("escape symlink");
    let action = RepositoryAction {
      argv: vec!["tool".to_string()],
      when: vec!["build".to_string()],
      inputs: vec!["escape/input".to_string()],
      ..RepositoryAction::default()
    };

    let error =
      ActionSpec::repository("escaped", &action, workspace.path()).expect_err("symlink escape must fail closed");
    assert!(error.to_string().contains("escapes the workspace"));
  }
}
