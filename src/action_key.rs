//! Versioned identities and fail-closed eligibility for expanded actions.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use cargo_metadata::{DependencyKind, Package, TargetKind};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::action::{
  ActionEnvironmentEntry, ActionInput, ActionOutput, ActionResolutionBinding, ActionWorkingDirectory, ExpandedAction,
};
use crate::cargo::{ResolutionPackages, ResolutionView};
use crate::error::{RailError, RailResult};
use crate::executable::{ExecutableIdentity, ToolchainExecutableScope};
use crate::source::{ContentDigest, RepositoryPath, SourceEntryKind};
use crate::workspace::WorkspaceSnapshot;

const ACTION_KEY_VERSION: u32 = 2;
const ACTION_SEMANTICS_VERSION: u32 = 2;

/// Exact identity evidence derived for one expanded action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ActionKeyAnalysis {
  version: u32,
  status: ActionKeyStatus,
  #[serde(skip_serializing_if = "Option::is_none")]
  key: Option<String>,
  declared_inputs: DeclaredInputSummary,
  reasons: Vec<ActionKeyReason>,
}

impl ActionKeyAnalysis {
  pub(crate) fn reason_codes(&self) -> impl Iterator<Item = &str> {
    self.reasons.iter().map(|reason| reason.code.as_str())
  }
}

/// Whether complete pre-execution evidence exists for an action key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ActionKeyStatus {
  Eligible,
  Uncacheable,
}

/// One stable, redaction-safe reason an action key cannot authorize reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ActionKeyReason {
  code: String,
  detail: String,
}

/// Compact exact-input summary; individual paths remain internal key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DeclaredInputSummary {
  entries: usize,
  root_digest: String,
}

/// Portable resolved-graph evidence bound into an action.
pub(crate) struct ResolutionIdentity {
  pub(crate) digest: String,
  pub(crate) resolved_node_count: usize,
  pub(crate) local_package_roots: Vec<PathBuf>,
  pub(crate) has_build_scripts: bool,
  pub(crate) has_proc_macros: bool,
  pub(crate) has_unverified_external_sources: bool,
}

#[derive(Clone)]
enum InputEntryKind {
  RegularFile { digest: ContentDigest, executable: bool },
  Symlink { target: String },
}

#[derive(Clone)]
struct InputEntry {
  path: RepositoryPath,
  kind: InputEntryKind,
}

struct ReasonSet(BTreeMap<&'static str, BTreeSet<String>>);

impl ReasonSet {
  fn new() -> Self {
    Self(BTreeMap::new())
  }

  fn add(&mut self, code: &'static str, detail: impl Into<String>) {
    self.0.entry(code).or_default().insert(detail.into());
  }

  fn is_empty(&self) -> bool {
    self.0.is_empty()
  }

  fn finish(self) -> Vec<ActionKeyReason> {
    self
      .0
      .into_iter()
      .map(|(code, details)| ActionKeyReason {
        code: code.to_string(),
        detail: details.into_iter().collect::<Vec<_>>().join(", "),
      })
      .collect()
  }
}

/// Derive exact declared inputs and issue an action key only when evidence is complete.
pub(crate) fn analyze(action: &ExpandedAction, snapshot: &WorkspaceSnapshot) -> RailResult<ActionKeyAnalysis> {
  let mut reasons = ReasonSet::new();
  let input_entries = declared_input_entries(action, snapshot, &mut reasons)?;
  let input_root = input_root_digest(&input_entries);
  let mut identity = FramedHasher::new(b"cargo-rail-action-key\0");
  identity.frame(b"key-version", &ACTION_KEY_VERSION.to_le_bytes());
  identity.frame(b"semantics-version", &ACTION_SEMANTICS_VERSION.to_le_bytes());
  identity.frame(b"kind", action.kind().as_str().as_bytes());
  for argument in action.argv() {
    identity.frame(b"argv", argument.as_bytes());
    if argument == "--config" || argument.starts_with("--config=") {
      reasons.add(
        "cargo_cli_configuration_unmodeled",
        "Cargo --config overrides are present in argv but have not been parsed into the sanitized configuration contract",
      );
    }
    if Path::new(argument).is_absolute() && Path::new(argument).starts_with(snapshot.source_root()) {
      reasons.add(
        "absolute_workspace_argument",
        "an argv value exposes the physical workspace root without verified path remapping",
      );
    }
  }
  match action.working_directory() {
    ActionWorkingDirectory::Workspace => identity.frame(b"working-directory", b"repository:"),
    ActionWorkingDirectory::Repository(path) => {
      identity.frame(b"working-directory", format!("repository:{}", path.as_str()).as_bytes());
    }
  }
  for package in action.selected_packages() {
    identity.frame(b"selected-package", package.as_bytes());
  }
  for target in action.selected_targets() {
    identity.frame(b"selected-target", target.as_bytes());
  }
  identity.frame(b"all-features", &[u8::from(action.selected_features().all_features())]);
  identity.frame(
    b"default-features",
    &[u8::from(action.selected_features().default_features())],
  );
  for feature in action.selected_features().named() {
    identity.frame(b"feature", feature.as_bytes());
  }
  identity.frame(b"platform", action.platform().as_bytes());
  identity.frame(b"generated-mode", action.generated_mode().unwrap_or("none").as_bytes());
  identity.frame(b"declared-input-root", input_root.as_bytes());

  for input in action.inputs() {
    match input {
      ActionInput::WorkspaceSnapshot => identity.frame(b"input", b"selected-workspace-source"),
      ActionInput::AmbientHost => reasons.add(
        "ambient_host",
        "the action declares host state outside the captured workspace",
      ),
      ActionInput::Repository { path } => identity.frame(b"input", path.as_str().as_bytes()),
    }
  }
  for output in action.key_outputs() {
    match output {
      ActionOutput::AmbientProcess => reasons.add(
        "ambient_outputs",
        "the process is not confined to its declared output roots",
      ),
      ActionOutput::Repository { path } => identity.frame(b"output", path.as_str().as_bytes()),
    }
  }

  bind_environment(action, snapshot, &mut identity, &mut reasons)?;
  bind_resolution_and_tools(action, snapshot, &mut identity, &mut reasons)?;

  if !action.key_dependencies().is_empty() {
    for dependency in action.key_dependencies() {
      identity.frame(b"dependency", dependency.as_bytes());
      reasons.add(
        "dependency_results_unavailable",
        format!("dependency action '{dependency}' has no verified result digest"),
      );
    }
  }

  if matches!(
    action.kind(),
    crate::action::ActionKind::Repository | crate::action::ActionKind::GeneratedArtifact
  ) {
    reasons.add(
      "filesystem_observation_incomplete",
      "repository process reads are not sandboxed or traced",
    );
  }

  let action_key = reasons.is_empty().then(|| ActionKey {
    version: ACTION_KEY_VERSION,
    digest: identity.finish(),
  });
  let status = if action_key.is_some() {
    ActionKeyStatus::Eligible
  } else {
    ActionKeyStatus::Uncacheable
  };
  Ok(ActionKeyAnalysis {
    version: ACTION_KEY_VERSION,
    status,
    key: action_key.map(|key| key.to_string()),
    declared_inputs: DeclaredInputSummary {
      entries: input_entries.len(),
      root_digest: format!("sha256:{input_root}"),
    },
    reasons: reasons.finish(),
  })
}

fn bind_environment(
  action: &ExpandedAction,
  snapshot: &WorkspaceSnapshot,
  identity: &mut FramedHasher,
  reasons: &mut ReasonSet,
) -> RailResult<()> {
  if action.environment().inherit() {
    reasons.add(
      "ambient_environment",
      "the action inherits environment variables outside its typed allowlist",
    );
  }

  let mut entries = action.environment().entries().iter().collect::<Vec<_>>();
  entries.sort_unstable_by_key(|entry| match entry {
    ActionEnvironmentEntry::Fixed { name, .. }
    | ActionEnvironmentEntry::Pass { name }
    | ActionEnvironmentEntry::Cargo { name, .. }
    | ActionEnvironmentEntry::Secret { name } => name,
  });
  for entry in entries {
    let mut framed = Vec::new();
    let entry_name = match entry {
      ActionEnvironmentEntry::Fixed { name, .. }
      | ActionEnvironmentEntry::Pass { name }
      | ActionEnvironmentEntry::Cargo { name, .. }
      | ActionEnvironmentEntry::Secret { name } => name,
    };
    if !matches!(entry, ActionEnvironmentEntry::Secret { .. }) && is_secret_environment_name(entry_name) {
      append_frame(&mut framed, b"kind", b"secret-capability-misclassified");
      append_frame(&mut framed, b"name", entry_name.as_bytes());
      identity.frame(b"environment", &framed);
      reasons.add(
        "secret_environment_misclassified",
        format!("secret-shaped environment name '{entry_name}' must use kind = \"secret\""),
      );
      continue;
    }
    match entry {
      ActionEnvironmentEntry::Fixed { name, value } => {
        append_frame(&mut framed, b"kind", b"fixed");
        append_frame(&mut framed, b"name", name.as_bytes());
        append_frame(&mut framed, b"value", value.as_bytes());
      }
      ActionEnvironmentEntry::Pass { name } => {
        append_frame(&mut framed, b"kind", b"pass");
        append_frame(&mut framed, b"name", name.as_bytes());
        match std::env::var_os(name) {
          Some(value) => append_frame(&mut framed, b"value", value.as_encoded_bytes()),
          None => append_frame(&mut framed, b"value", b"absent"),
        }
      }
      ActionEnvironmentEntry::Cargo { name, value } => {
        append_frame(&mut framed, b"kind", b"cargo");
        append_frame(&mut framed, b"name", name.as_bytes());
        let value = match value {
          crate::config::CargoEnvironmentValue::WorkspaceRoot => "repository:",
          crate::config::CargoEnvironmentValue::TargetDirectory => "cargo:target-directory",
        };
        append_frame(&mut framed, b"value", value.as_bytes());
      }
      ActionEnvironmentEntry::Secret { name } => {
        append_frame(&mut framed, b"kind", b"secret-capability");
        append_frame(&mut framed, b"name", name.as_bytes());
        reasons.add(
          "secret_environment",
          format!("secret capability '{name}' cannot authorize reusable output"),
        );
      }
    }
    identity.frame(b"environment", &framed);
  }

  if action.argv().first().is_some_and(|program| program == "cargo") {
    identity.frame(
      b"cargo-config",
      &snapshot
        .cargo_config()
        .portable_snapshot_identity(snapshot.source_root())?,
    );
    if snapshot.cargo_config().has_credential_capability() {
      reasons.add(
        "cargo_secret_capability",
        "Cargo configuration contains a credential capability whose value is intentionally excluded",
      );
    }
    for setting in snapshot.cargo_config().unmodeled_settings() {
      reasons.add(
        "cargo_configuration_unmodeled",
        format!("Cargo setting '{setting}' is outside the hermetic allowlist"),
      );
    }
  }
  Ok(())
}

fn is_secret_environment_name(name: &str) -> bool {
  let normalized = name.to_ascii_lowercase().replace('_', "-");
  normalized == "token"
    || normalized.ends_with("-token")
    || normalized.contains("password")
    || normalized.contains("secret")
    || normalized.contains("credential")
    || normalized.contains("private-key")
}

fn bind_resolution_and_tools(
  action: &ExpandedAction,
  snapshot: &WorkspaceSnapshot,
  identity: &mut FramedHasher,
  reasons: &mut ReasonSet,
) -> RailResult<()> {
  let Some(program) = action.argv().first() else {
    reasons.add("program_identity_unavailable", "the expanded argv has no program");
    return Ok(());
  };
  if program == "cargo" {
    identity.frame(
      b"toolchain",
      &snapshot
        .toolchain()
        .portable_snapshot_identity(snapshot.source_root())?,
    );
    reasons.add(
      "cargo_units_unmodeled",
      "stable Cargo does not expose a complete pre-execution compilation-unit graph",
    );
    if action.kind() == crate::action::ActionKind::Docs {
      reasons.add(
        "rustdoc_invocation_observations_unavailable",
        "stable Cargo exposes rustdoc artifacts after execution but has no rustdoc-wrapper boundary",
      );
    }
    let scope = if matches!(
      action.kind(),
      crate::action::ActionKind::Docs | crate::action::ActionKind::Test
    ) {
      ToolchainExecutableScope::Documentation
    } else {
      ToolchainExecutableScope::Compilation
    };
    let executables = snapshot.executable_identities(scope)?;
    identity.frame(b"toolchain-executables", &executables.identity_bytes()?);
    for limitation in executables.limitations() {
      reasons.add(
        "executable_identity_unavailable",
        format!("toolchain executable limitation: {limitation}"),
      );
    }
    for (role, executable) in [
      ("cargo", Some(executables.cargo())),
      ("rustc", Some(executables.rustc())),
      ("rustdoc", executables.rustdoc()),
      ("rustc_wrapper", executables.rustc_wrapper()),
      ("rustc_workspace_wrapper", executables.rustc_workspace_wrapper()),
    ] {
      if let Some(executable) = executable {
        bind_captured_executable(role, executable, identity, reasons)?;
      }
    }
    if executables.rustc_wrapper().is_some() || executables.rustc_workspace_wrapper().is_some() {
      match std::env::current_exe()
        .map_err(|error| RailError::message(format!("failed to locate cargo-rail executable: {error}")))
        .and_then(|program| {
          ExecutableIdentity::capture(program.as_os_str(), snapshot.source_root(), snapshot.source_root())
        }) {
        Ok(cargo_rail) => {
          if [executables.rustc_wrapper(), executables.rustc_workspace_wrapper()]
            .into_iter()
            .flatten()
            .any(|wrapper| wrapper.same_resolved_file(&cargo_rail))
          {
            reasons.add(
              "recursive_wrapper_chain",
              "cargo-rail is configured as an existing rustc wrapper; diagnostics injection would recurse",
            );
          }
        }
        Err(error) => reasons.add(
          "executable_identity_unavailable",
          format!("cargo-rail wrapper executable cannot be content-addressed: {error}"),
        ),
      }
    }
  } else {
    bind_executable("external_program", OsStr::new(program), snapshot, identity, reasons)?;
  }
  identity.frame(b"platform-family", std::env::consts::FAMILY.as_bytes());
  identity.frame(b"platform-os", std::env::consts::OS.as_bytes());
  identity.frame(b"platform-arch", std::env::consts::ARCH.as_bytes());
  reasons.add(
    "platform_runtime_identity_incomplete",
    "the host kernel, loader, system libraries, and SDK are not captured by an immutable platform image",
  );

  for resolution in action.resolution_views() {
    identity.frame(b"resolution", resolution.resolution_digest().as_bytes());
    if resolution.has_build_scripts() {
      reasons.add(
        "build_script_observations_unavailable",
        "the resolved graph contains a build script without a verified result digest",
      );
    }
    if resolution.has_proc_macros() {
      reasons.add(
        "proc_macro_observations_unavailable",
        "the resolved graph contains a proc macro with unobserved ambient reads",
      );
    }
    if resolution.has_unverified_external_sources() {
      reasons.add(
        "external_source_digest_unavailable",
        "at least one resolved external package lacks a Cargo.lock checksum",
      );
    }
  }

  let mut unmatched_targets = action
    .resolution_views()
    .iter()
    .filter_map(ActionResolutionBinding::target)
    .collect::<BTreeSet<_>>();
  let mut unmatched_default = action
    .resolution_views()
    .iter()
    .any(|resolution| resolution.target().is_none());
  let has_configured_build_target = snapshot.targets().iter().any(|target| target.is_build_target());
  for target in snapshot.targets() {
    let target_name = resolution_target_name(target);
    let explicitly_selected = action
      .resolution_views()
      .iter()
      .filter_map(ActionResolutionBinding::target)
      .any(|selected| target_name.is_some_and(|name| selected == name));
    let selected_as_default =
      unmatched_default && (target.is_build_target() || (!has_configured_build_target && target.is_host()));
    if explicitly_selected || selected_as_default {
      if selected_as_default {
        unmatched_default = false;
      }
      if explicitly_selected && let Some(name) = target_name {
        unmatched_targets.remove(name);
      }
      identity.frame(
        b"target-identity",
        &target.portable_snapshot_identity(snapshot.source_root())?,
      );
      if let Some(linker) = target.linker() {
        bind_executable("linker", linker, snapshot, identity, reasons)?;
        reasons.add(
          "linker_sdk_inputs_unavailable",
          "the selected linker can read an SDK and native inputs outside the captured workspace",
        );
      } else if matches!(
        action.kind(),
        crate::action::ActionKind::Build
          | crate::action::ActionKind::Test
          | crate::action::ActionKind::Bench
          | crate::action::ActionKind::Package
          | crate::action::ActionKind::Distribution
      ) {
        reasons.add(
          "linker_executable_identity_unavailable",
          "rustc may select a default linker that Cargo did not expose as an executable path",
        );
      }
      if let Some(runner) = target.runner()
        && let Some(program) = runner.first()
      {
        bind_executable("runner", program, snapshot, identity, reasons)?;
      }
    }
  }
  for target in unmatched_targets {
    reasons.add(
      "target_identity_unavailable",
      format!("target '{target}' has no captured cfg, linker, runner, or flag identity"),
    );
  }
  if unmatched_default {
    reasons.add(
      "target_identity_unavailable",
      "the default build target has no captured cfg, linker, runner, or flag identity",
    );
  }
  Ok(())
}

fn bind_executable(
  role: &'static str,
  program: &OsStr,
  snapshot: &WorkspaceSnapshot,
  identity: &mut FramedHasher,
  reasons: &mut ReasonSet,
) -> RailResult<()> {
  match ExecutableIdentity::capture(program, snapshot.source_root(), snapshot.source_root()) {
    Ok(executable) => bind_captured_executable(role, &executable, identity, reasons)?,
    Err(error) => reasons.add(
      "executable_identity_unavailable",
      format!("{role} executable cannot be content-addressed: {error}"),
    ),
  }
  Ok(())
}

fn bind_captured_executable(
  role: &'static str,
  executable: &ExecutableIdentity,
  identity: &mut FramedHasher,
  reasons: &mut ReasonSet,
) -> RailResult<()> {
  identity.frame(format!("{role}-executable").as_bytes(), &executable.identity_bytes()?);
  for limitation in executable.limitations() {
    reasons.add(
      "executable_runtime_inputs_unavailable",
      format!("{role} executable limitation: {limitation}"),
    );
  }
  Ok(())
}

fn resolution_target_name(target: &crate::cargo::resolution::TargetIdentity) -> Option<&str> {
  match target.specification() {
    crate::cargo::resolution::TargetSpecificationIdentity::BuiltIn(name) => Some(name),
    crate::cargo::resolution::TargetSpecificationIdentity::Custom(specification) => Some(specification.name()),
  }
}

fn declared_input_entries(
  action: &ExpandedAction,
  snapshot: &WorkspaceSnapshot,
  reasons: &mut ReasonSet,
) -> RailResult<Vec<InputEntry>> {
  let mut all_source = false;
  let mut roots = BTreeSet::new();
  for input in action.inputs() {
    match input {
      ActionInput::AmbientHost => {}
      ActionInput::Repository { path } => {
        roots.insert(path.as_path().to_path_buf());
      }
      ActionInput::WorkspaceSnapshot if action.resolution_views().is_empty() => all_source = true,
      ActionInput::WorkspaceSnapshot => {
        roots.insert(PathBuf::from("Cargo.toml"));
        for root in action
          .resolution_views()
          .iter()
          .flat_map(ActionResolutionBinding::resolved_local_roots)
        {
          if root.as_os_str().is_empty() {
            all_source = true;
          } else {
            roots.insert(root.clone());
          }
        }
      }
    }
  }

  let selected = |path: &Path| all_source || roots.iter().any(|root| path == root || path.starts_with(root));
  let mut entries = BTreeMap::new();
  for entry in snapshot.source().tree().entries() {
    if !selected(entry.path.as_path()) {
      continue;
    }
    let kind = match &entry.kind {
      SourceEntryKind::RegularFile { digest, executable } => InputEntryKind::RegularFile {
        digest: *digest,
        executable: *executable,
      },
      SourceEntryKind::Symlink { target } => InputEntryKind::Symlink { target: target.clone() },
      SourceEntryKind::Deleted => {
        return Err(RailError::message(format!(
          "action input tree contains deleted entry '{}'",
          entry.path
        )));
      }
    };
    entries.insert(
      entry.path.clone(),
      InputEntry {
        path: entry.path.clone(),
        kind,
      },
    );
  }
  for file in snapshot.manifests() {
    insert_snapshot_file(&mut entries, file, &selected);
  }
  if let Some(lockfile) = snapshot.lockfile() {
    insert_snapshot_file(&mut entries, lockfile.file(), &selected);
  }
  if let Some(config) = snapshot.rail_config() {
    insert_snapshot_file(&mut entries, config, &selected);
  }

  if !all_source {
    for root in roots {
      let found = entries
        .keys()
        .any(|path| path.as_path() == root || path.as_path().starts_with(&root));
      if !found {
        reasons.add(
          "declared_input_unavailable",
          format!(
            "declared input '{}' is absent from the authoritative snapshot",
            root.display()
          ),
        );
      }
    }
  }
  Ok(entries.into_values().collect())
}

fn insert_snapshot_file(
  entries: &mut BTreeMap<RepositoryPath, InputEntry>,
  file: &crate::workspace::SnapshotFile,
  selected: &impl Fn(&Path) -> bool,
) {
  if selected(file.path().as_path()) {
    entries.entry(file.path().clone()).or_insert_with(|| InputEntry {
      path: file.path().clone(),
      kind: InputEntryKind::RegularFile {
        digest: file.digest(),
        executable: false,
      },
    });
  }
}

fn input_root_digest(entries: &[InputEntry]) -> ContentDigest {
  let mut identity = FramedHasher::new(b"cargo-rail-declared-input-root\0");
  identity.frame(b"version", &1_u32.to_le_bytes());
  for entry in entries {
    let mut framed = Vec::new();
    append_frame(&mut framed, b"path", entry.path.as_str().as_bytes());
    match &entry.kind {
      InputEntryKind::RegularFile { digest, executable } => {
        append_frame(&mut framed, b"kind", b"regular-file");
        append_frame(&mut framed, b"content", digest.as_bytes());
        append_frame(&mut framed, b"executable", &[u8::from(*executable)]);
      }
      InputEntryKind::Symlink { target } => {
        append_frame(&mut framed, b"kind", b"symlink");
        append_frame(&mut framed, b"target", target.as_bytes());
      }
    }
    identity.frame(b"entry", &framed);
  }
  identity.finish()
}

/// Build a portable identity for the exact Cargo resolution graph.
pub(crate) fn resolution_identity(
  snapshot: &WorkspaceSnapshot,
  view: &ResolutionView,
) -> RailResult<ResolutionIdentity> {
  let metadata = view.metadata();
  let packages = metadata
    .packages
    .iter()
    .map(|package| (&package.id, package))
    .collect::<HashMap<_, _>>();
  let Some(resolve) = metadata.resolve.as_ref() else {
    return Err(RailError::message("Cargo resolution view contains no resolve graph"));
  };

  let nodes_by_id = resolve
    .nodes
    .iter()
    .map(|node| (&node.id, node))
    .collect::<HashMap<_, _>>();
  let mut pending = match view.request().packages() {
    ResolutionPackages::Workspace => metadata.workspace_members.iter().collect::<Vec<_>>(),
    ResolutionPackages::Selected(selected) => selected.iter().collect::<Vec<_>>(),
  };
  let mut selected = BTreeSet::new();
  while let Some(package_id) = pending.pop() {
    if !selected.insert(package_id) {
      continue;
    }
    let node = nodes_by_id.get(package_id).ok_or_else(|| {
      RailError::message(format!(
        "selected package '{package_id}' is absent from the Cargo resolve graph"
      ))
    })?;
    pending.extend(node.dependencies.iter());
  }

  let mut package_identities = HashMap::with_capacity(selected.len());
  let mut has_unverified_external_sources = false;
  for package_id in &selected {
    let package = packages
      .get(*package_id)
      .ok_or_else(|| RailError::message(format!("selected package '{package_id}' is absent from Cargo metadata")))?;
    let (identity, verified) = portable_package_identity(snapshot, package)?;
    has_unverified_external_sources |= !verified;
    package_identities.insert(*package_id, identity);
  }

  let mut nodes = selected
    .iter()
    .map(|package_id| nodes_by_id[package_id])
    .collect::<Vec<_>>();
  nodes.sort_unstable_by(|left, right| package_identities[&left.id].cmp(&package_identities[&right.id]));
  let mut identity = FramedHasher::new(b"cargo-rail-resolution-identity\0");
  identity.frame(b"version", &1_u32.to_le_bytes());
  let mut local_package_roots = BTreeSet::new();
  let mut has_build_scripts = false;
  let mut has_proc_macros = false;

  for node in &nodes {
    let package = packages
      .get(&node.id)
      .ok_or_else(|| RailError::message(format!("resolved package '{}' is absent from Cargo metadata", node.id)))?;
    if package.source.is_none() {
      local_package_roots.insert(local_package_root(snapshot, package)?);
    }
    has_build_scripts |= package
      .targets
      .iter()
      .flat_map(|target| target.kind.iter())
      .any(|kind| *kind == TargetKind::CustomBuild);
    has_proc_macros |= package
      .targets
      .iter()
      .flat_map(|target| target.kind.iter())
      .any(|kind| *kind == TargetKind::ProcMacro);

    let mut framed = Vec::new();
    append_frame(&mut framed, b"package", &package_identities[&node.id]);
    let mut features = node.features.clone();
    features.sort_unstable();
    for feature in features {
      append_frame(&mut framed, b"feature", feature.as_bytes());
    }
    let mut dependencies = node.deps.iter().collect::<Vec<_>>();
    dependencies.sort_unstable_by(|left, right| {
      left
        .name
        .cmp(&right.name)
        .then_with(|| package_identities[&left.pkg].cmp(&package_identities[&right.pkg]))
    });
    for dependency in dependencies {
      let mut dependency_frame = Vec::new();
      append_frame(&mut dependency_frame, b"name", dependency.name.as_bytes());
      append_frame(
        &mut dependency_frame,
        b"package",
        package_identities.get(&dependency.pkg).ok_or_else(|| {
          RailError::message(format!(
            "dependency package '{}' is absent from Cargo metadata",
            dependency.pkg
          ))
        })?,
      );
      let mut kinds = dependency.dep_kinds.iter().collect::<Vec<_>>();
      kinds.sort_unstable_by_key(|kind| {
        (
          dependency_kind_name(kind.kind),
          kind.target.as_ref().map(ToString::to_string),
        )
      });
      for kind in kinds {
        let mut kind_frame = Vec::new();
        append_frame(&mut kind_frame, b"kind", dependency_kind_name(kind.kind).as_bytes());
        append_frame(
          &mut kind_frame,
          b"target",
          kind
            .target
            .as_ref()
            .map(ToString::to_string)
            .as_deref()
            .unwrap_or("all")
            .as_bytes(),
        );
        append_frame(&mut dependency_frame, b"domain", &kind_frame);
      }
      append_frame(&mut framed, b"dependency", &dependency_frame);
    }
    identity.frame(b"node", &framed);
  }

  Ok(ResolutionIdentity {
    digest: format!("sha256:{}", identity.finish()),
    resolved_node_count: nodes.len(),
    local_package_roots: local_package_roots.into_iter().collect(),
    has_build_scripts,
    has_proc_macros,
    has_unverified_external_sources,
  })
}

fn portable_package_identity(snapshot: &WorkspaceSnapshot, package: &Package) -> RailResult<(Vec<u8>, bool)> {
  let mut identity = Vec::from(&b"cargo-package-identity-v1\0"[..]);
  append_frame(&mut identity, b"name", package.name.as_bytes());
  append_frame(&mut identity, b"version", package.version.to_string().as_bytes());
  let Some(source) = package.source.as_ref() else {
    append_frame(&mut identity, b"kind", b"local");
    let manifest = snapshot_package(snapshot, package)?.manifest_path().ok_or_else(|| {
      RailError::message(format!(
        "local package '{}' has no logical manifest identity",
        package.id
      ))
    })?;
    append_frame(&mut identity, b"manifest", manifest.as_str().as_bytes());
    return Ok((identity, true));
  };

  append_frame(&mut identity, b"kind", b"external");
  append_frame(&mut identity, b"source", source.repr.as_bytes());
  let checksum = snapshot.lockfile().and_then(|lockfile| {
    lockfile.packages().iter().find_map(|locked| {
      (locked.name() == package.name.as_str()
        && locked.version() == package.version.to_string()
        && locked.source() == Some(source.repr.as_str()))
      .then(|| locked.checksum())
      .flatten()
    })
  });
  match checksum {
    Some(checksum) => append_frame(&mut identity, b"checksum", checksum.as_bytes()),
    None => append_frame(&mut identity, b"checksum", b"unverified"),
  }
  Ok((identity, checksum.is_some()))
}

fn local_package_root(snapshot: &WorkspaceSnapshot, package: &Package) -> RailResult<PathBuf> {
  let manifest = snapshot_package(snapshot, package)?.manifest_path().ok_or_else(|| {
    RailError::message(format!(
      "local package '{}' has no logical manifest identity",
      package.id
    ))
  })?;
  Ok(
    manifest
      .as_path()
      .parent()
      .unwrap_or_else(|| Path::new(""))
      .to_path_buf(),
  )
}

fn snapshot_package<'a>(
  snapshot: &'a WorkspaceSnapshot,
  package: &Package,
) -> RailResult<&'a crate::workspace::SnapshotPackage> {
  snapshot
    .packages()
    .iter()
    .find(|candidate| candidate.id() == &package.id)
    .ok_or_else(|| RailError::message(format!("snapshot is missing local package '{}'", package.id)))
}

fn dependency_kind_name(kind: DependencyKind) -> &'static str {
  match kind {
    DependencyKind::Normal => "normal",
    DependencyKind::Development => "development",
    DependencyKind::Build => "build",
    _ => "unknown",
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActionKey {
  version: u32,
  digest: ContentDigest,
}

impl fmt::Display for ActionKey {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "v{}-sha256-{}", self.version, self.digest)
  }
}

struct FramedHasher {
  hasher: Sha256,
  input_bytes: usize,
}

impl FramedHasher {
  fn new(domain: &[u8]) -> Self {
    let mut identity = Self {
      hasher: Sha256::new(),
      input_bytes: 0,
    };
    identity.update(domain);
    identity
  }

  fn frame(&mut self, tag: &[u8], value: &[u8]) {
    self.update(&(tag.len() as u64).to_le_bytes());
    self.update(tag);
    self.update(&(value.len() as u64).to_le_bytes());
    self.update(value);
  }

  fn update(&mut self, bytes: &[u8]) {
    self.hasher.update(bytes);
    self.input_bytes = self.input_bytes.saturating_add(bytes.len());
  }

  fn finish(self) -> ContentDigest {
    crate::instrumentation::record_hash_operation();
    crate::instrumentation::record_hash_input_bytes(self.input_bytes);
    ContentDigest::from_sha256_bytes(self.hasher.finalize().into())
  }
}

fn append_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
  output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
  output.extend_from_slice(tag);
  output.extend_from_slice(&(value.len() as u64).to_le_bytes());
  output.extend_from_slice(value);
}
