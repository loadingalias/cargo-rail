//! Run/execution profile configuration.

use crate::error::ConfigError;
use crate::source::RepositoryPath;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::path::Path;

const BUILTIN_PROFILE_NAMES: &[&str] = &["local", "ci", "nightly"];
const PLANNER_SURFACE_NAMES: &[&str] = &["build", "test", "bench", "docs", "infra"];
pub(crate) const BUILTIN_ACTION_NAMES: &[&str] = &[
  "build",
  "test",
  "bench",
  "docs",
  "format",
  "lint",
  "msrv",
  "package",
  "audit",
  "distribution",
];
const ALLOWED_RUN_ARG_TOKENS: &[&str] = &["workspace_root", "base_ref", "cargo_args"];
const ALLOWED_SINCE_TOKENS: &[&str] = &["workspace_root", "base_ref"];
const ACTION_ARG_TOKENS: &[&str] = &["workspace_root", "base_ref", "packages", "targets", "features"];
const SHELL_PROGRAMS: &[&str] = &[
  "bash",
  "cmd",
  "command.com",
  "csh",
  "dash",
  "fish",
  "ksh",
  "nu",
  "powershell",
  "pwsh",
  "sh",
  "tcsh",
  "xonsh",
  "zsh",
];

pub(crate) const MAX_ACTIONS: usize = 64;
const MAX_ACTION_ARGUMENTS: usize = 256;
const MAX_ACTION_DEPENDENCIES: usize = 64;
const MAX_ACTION_PATHS: usize = 256;
const MAX_ACTION_ENVIRONMENT_ENTRIES: usize = 128;
const MAX_ACTION_STRING_BYTES: usize = 16 * 1024;
const MAX_ACTION_TOTAL_ARGV_BYTES: usize = 64 * 1024;

/// Executor profile configuration for `cargo rail run`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunConfig {
  /// Optional default profile when `cargo rail run` is invoked without `--action` or `--profile`.
  #[serde(default)]
  pub default_profile: Option<String>,
  /// User-defined profiles keyed by profile name.
  #[serde(default, rename = "profile")]
  pub profiles: FxHashMap<String, RunProfile>,
  /// Bounded repository actions keyed by action ID.
  #[serde(default, rename = "action")]
  pub actions: FxHashMap<String, RepositoryAction>,
  /// Optional workflow-to-profile mapping (for CI wrappers and conventions).
  #[serde(default)]
  pub workflow: FxHashMap<String, String>,
}

/// One direct-argv repository action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepositoryAction {
  /// Whether this task performs ordinary work or owns generated outputs.
  pub kind: RepositoryActionKind,
  /// Program followed by literal arguments and whole-token substitutions.
  pub argv: Vec<String>,
  /// Read-only staleness check for generated outputs.
  pub check_argv: Vec<String>,
  /// Action IDs that must complete first.
  pub dependencies: Vec<String>,
  /// Planner surfaces that enable this action when `--all` is absent.
  pub when: Vec<String>,
  /// Repository-relative working directory; `.` means the workspace root.
  pub working_directory: String,
  /// How planner-selected packages enter `argv` at `{packages}`.
  pub packages: RepositoryPackageSelection,
  /// Explicit compilation targets inserted at `{targets}`.
  pub targets: Vec<String>,
  /// Explicit Cargo features declared by this action.
  pub features: Vec<String>,
  /// Repository-relative input roots. `.` declares the workspace tree.
  pub inputs: Vec<String>,
  /// Repository-relative output roots owned by this action.
  pub outputs: Vec<String>,
  /// Environment inheritance and typed entries.
  pub environment: RepositoryEnvironment,
}

impl Default for RepositoryAction {
  fn default() -> Self {
    Self {
      kind: RepositoryActionKind::Task,
      argv: Vec::new(),
      check_argv: Vec::new(),
      dependencies: Vec::new(),
      when: Vec::new(),
      working_directory: ".".to_string(),
      packages: RepositoryPackageSelection::None,
      targets: Vec::new(),
      features: Vec::new(),
      inputs: Vec::new(),
      outputs: Vec::new(),
      environment: RepositoryEnvironment::default(),
    }
  }
}

/// Repository task behavior.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryActionKind {
  /// A task with no generated-file ownership contract.
  #[default]
  Task,
  /// A deterministic generator that must declare at least one output.
  Generated,
}

/// Package insertion policy for a repository action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryPackageSelection {
  /// Do not insert package arguments.
  #[default]
  None,
  /// Insert each planner-selected package as `-p <name>`.
  Selected,
  /// Insert `--workspace` for workspace scope, otherwise `-p <name>` pairs.
  WorkspaceOrSelected,
}

/// Environment policy for a repository action.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RepositoryEnvironment {
  /// Inherit the caller's complete ambient environment before applying entries.
  pub inherit: bool,
  /// Fixed, pass-through, Cargo-derived, or secret-capability entries.
  pub entries: Vec<RepositoryEnvironmentEntry>,
}

/// One typed environment entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RepositoryEnvironmentEntry {
  /// Set a non-secret literal value.
  Fixed {
    /// Environment variable name.
    name: String,
    /// Literal non-secret value.
    value: String,
  },
  /// Pass through a non-secret value by name when present.
  Pass {
    /// Environment variable name.
    name: String,
  },
  /// Set a value derived from the captured Cargo workspace.
  Cargo {
    /// Environment variable name.
    name: String,
    /// Cargo-derived value source.
    value: CargoEnvironmentValue,
  },
  /// Pass a secret by capability name without serializing its value.
  Secret {
    /// Secret environment variable capability name.
    name: String,
  },
}

impl RepositoryEnvironmentEntry {
  fn name(&self) -> &str {
    match self {
      Self::Fixed { name, .. } | Self::Pass { name } | Self::Cargo { name, .. } | Self::Secret { name } => name,
    }
  }
}

/// Cargo-derived environment values available to repository actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CargoEnvironmentValue {
  /// Captured workspace root.
  WorkspaceRoot,
  /// Cargo metadata target directory.
  TargetDirectory,
}

/// A named run profile.
#[derive(Debug, Clone, Serialize, Default)]
pub struct RunProfile {
  /// Ordered action IDs selected by this profile.
  #[serde(default)]
  pub actions: Vec<String>,
  /// Arguments prepended to command-line `RUN_ARGS`.
  #[serde(default)]
  pub run_args: Vec<String>,
  /// Optional baseline policy if the CLI does not provide one.
  #[serde(default)]
  pub baseline: Option<RunBaseline>,
}

/// One valid baseline policy for a run profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RunBaseline {
  /// Use one explicit Git reference or supported placeholder.
  Since {
    /// Git reference or `{base_ref}` placeholder.
    reference: String,
  },
  /// Resolve the merge base against the default branch.
  MergeBase,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RunProfileInput {
  actions: Vec<String>,
  surfaces: Vec<String>,
  run_args: Vec<String>,
  baseline: Option<RunBaseline>,
  since: Option<String>,
  merge_base: Option<bool>,
}

impl<'de> Deserialize<'de> for RunProfile {
  fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
  where
    D: Deserializer<'de>,
  {
    let input = RunProfileInput::deserialize(deserializer)?;
    if !input.actions.is_empty() && !input.surfaces.is_empty() {
      return Err(de::Error::custom(
        "run profile actions cannot be combined with deprecated surfaces; run `cargo rail config migrate`",
      ));
    }
    if input.baseline.is_some() && (input.since.is_some() || input.merge_base.is_some()) {
      return Err(de::Error::custom(
        "run profile baseline cannot be combined with deprecated since or merge_base; run `cargo rail config migrate`",
      ));
    }
    if input.since.is_some() && input.merge_base == Some(true) {
      return Err(de::Error::custom(
        "deprecated run profile since and merge_base = true are mutually exclusive",
      ));
    }
    let baseline = input.baseline.or_else(|| {
      input
        .since
        .map(|reference| RunBaseline::Since { reference })
        .or_else(|| (input.merge_base == Some(true)).then_some(RunBaseline::MergeBase))
    });
    let actions = if input.actions.is_empty() {
      input.surfaces
    } else {
      input.actions
    };
    Ok(Self {
      actions,
      run_args: input.run_args,
      baseline,
    })
  }
}

impl RunConfig {
  /// Validate run profile configuration.
  pub fn validate(&self) -> Result<(), ConfigError> {
    if self.actions.len() > MAX_ACTIONS {
      return Err(invalid(
        "run.action",
        format!(
          "defines {} actions; at most {MAX_ACTIONS} are allowed",
          self.actions.len()
        ),
      ));
    }

    let mut action_names = self.actions.keys().collect::<Vec<_>>();
    action_names.sort_unstable();
    for name in action_names {
      validate_action_id(name, &format!("run.action.{name}"))?;
      if is_builtin_action(name) {
        return Err(invalid(
          format!("run.action.{name}"),
          "cannot override a built-in action ID",
        ));
      }
      self.actions[name].validate(name)?;
    }
    validate_action_dependencies(&self.actions)?;
    validate_action_output_ownership(&self.actions)?;

    if let Some(default_profile) = self.default_profile.as_deref()
      && !self.profiles.contains_key(default_profile)
      && !is_builtin_profile(default_profile)
    {
      return Err(ConfigError::InvalidField {
        field: "run.default_profile".to_string(),
        reason: format!(
          "unknown profile '{}'; define [run.profile.{}] or use one of: {}",
          default_profile,
          default_profile,
          BUILTIN_PROFILE_NAMES.join(", ")
        ),
      });
    }

    let mut profile_names = self.profiles.keys().collect::<Vec<_>>();
    profile_names.sort_unstable();
    for name in profile_names {
      self.profiles[name].validate(name, &self.actions)?;
    }

    for (workflow_name, profile_name) in &self.workflow {
      if self.profiles.contains_key(profile_name) || is_builtin_profile(profile_name) {
        continue;
      }

      return Err(ConfigError::InvalidField {
        field: format!("run.workflow.{}", workflow_name),
        reason: format!(
          "unknown profile '{}'; define [run.profile.{}] or use one of: {}",
          profile_name,
          profile_name,
          BUILTIN_PROFILE_NAMES.join(", ")
        ),
      });
    }

    Ok(())
  }
}

impl RunProfile {
  fn validate(
    &self,
    profile_name: &str,
    repository_actions: &FxHashMap<String, RepositoryAction>,
  ) -> Result<(), ConfigError> {
    if self.actions.is_empty() {
      return Err(ConfigError::InvalidField {
        field: format!("run.profile.{}.actions", profile_name),
        reason: "must contain at least one action".to_string(),
      });
    }
    if self.actions.len() > MAX_ACTIONS {
      return Err(invalid(
        format!("run.profile.{profile_name}.actions"),
        format!(
          "contains {} actions; at most {MAX_ACTIONS} are allowed",
          self.actions.len()
        ),
      ));
    }

    let mut unique_actions = std::collections::BTreeSet::new();
    for action in &self.actions {
      if !unique_actions.insert(action) {
        return Err(invalid(
          format!("run.profile.{profile_name}.actions"),
          format!("repeats action '{action}'"),
        ));
      }
      if action == "infra" {
        return Err(ConfigError::InvalidField {
          field: format!("run.profile.{}.actions", profile_name),
          reason: format!(
            "invalid action '{}'\n\n\
             `infra` is a planner output, not an executable action ID.\n\
             Use it in run.action.<name>.when or select a built-in action: {}",
            action,
            BUILTIN_ACTION_NAMES.join(", ")
          ),
        });
      }

      // custom:* surfaces are planner outputs, not profile inputs.
      if action.starts_with("custom:") {
        return Err(ConfigError::InvalidField {
          field: format!("run.profile.{}.actions", profile_name),
          reason: format!(
            "invalid action '{}'\n\n\
             Custom surfaces are planner outputs, not executable action IDs.\n\
             Use one in run.action.<name>.when or select a built-in action: {}",
            action,
            BUILTIN_ACTION_NAMES.join(", ")
          ),
        });
      }

      if !is_builtin_action(action) && !repository_actions.contains_key(action) {
        return Err(ConfigError::InvalidField {
          field: format!("run.profile.{}.actions", profile_name),
          reason: format!(
            "unknown action '{}'; define [run.action.{}] or use a built-in action: {}",
            action,
            action,
            BUILTIN_ACTION_NAMES.join(", ")
          ),
        });
      }
    }

    if let Some(RunBaseline::Since { reference }) = &self.baseline {
      validate_tokens(
        reference,
        ALLOWED_SINCE_TOKENS,
        &format!("run.profile.{}.baseline.reference", profile_name),
      )?;
    }

    for (index, arg) in self.run_args.iter().enumerate() {
      validate_tokens(
        arg,
        ALLOWED_RUN_ARG_TOKENS,
        &format!("run.profile.{}.run_args[{}]", profile_name, index),
      )?;
    }

    Ok(())
  }
}

impl RepositoryAction {
  fn validate(&self, action_name: &str) -> Result<(), ConfigError> {
    let prefix = format!("run.action.{action_name}");
    validate_action_argv(&self.argv, &format!("{prefix}.argv"))?;
    match self.kind {
      RepositoryActionKind::Task if !self.check_argv.is_empty() => {
        return Err(invalid(
          format!("{prefix}.check_argv"),
          "is only valid for kind = \"generated\"",
        ));
      }
      RepositoryActionKind::Generated => {
        validate_action_argv(&self.check_argv, &format!("{prefix}.check_argv"))?;
      }
      RepositoryActionKind::Task => {}
    }

    if self.dependencies.len() > MAX_ACTION_DEPENDENCIES {
      return Err(invalid(
        format!("{prefix}.dependencies"),
        format!(
          "contains {} values; at most {MAX_ACTION_DEPENDENCIES} are allowed",
          self.dependencies.len()
        ),
      ));
    }
    let mut unique_dependencies = std::collections::BTreeSet::new();
    for (index, dependency) in self.dependencies.iter().enumerate() {
      validate_action_id(dependency, &format!("{prefix}.dependencies[{index}]"))?;
      if !unique_dependencies.insert(dependency) {
        return Err(invalid(
          format!("{prefix}.dependencies[{index}]"),
          format!("repeats dependency '{dependency}'"),
        ));
      }
    }
    if self.when.is_empty() {
      return Err(invalid(
        format!("{prefix}.when"),
        "must name at least one planner surface",
      ));
    }
    if self.when.len() > MAX_ACTION_DEPENDENCIES {
      return Err(invalid(
        format!("{prefix}.when"),
        format!(
          "contains {} values; at most {MAX_ACTION_DEPENDENCIES} are allowed",
          self.when.len()
        ),
      ));
    }
    let mut unique_surfaces = std::collections::BTreeSet::new();
    for (index, surface) in self.when.iter().enumerate() {
      validate_surface_name(surface, &format!("{prefix}.when[{index}]"))?;
      if !unique_surfaces.insert(surface) {
        return Err(invalid(
          format!("{prefix}.when[{index}]"),
          format!("repeats planner surface '{surface}'"),
        ));
      }
    }

    validate_working_directory(&self.working_directory, &format!("{prefix}.working_directory"))?;
    validate_path_list(&self.inputs, &format!("{prefix}.inputs"), true)?;
    validate_path_list(&self.outputs, &format!("{prefix}.outputs"), false)?;
    if self.kind == RepositoryActionKind::Generated && self.outputs.is_empty() {
      return Err(invalid(
        format!("{prefix}.outputs"),
        "generated actions must declare at least one owned output",
      ));
    }
    if self.kind == RepositoryActionKind::Task && !self.outputs.is_empty() {
      return Err(invalid(
        format!("{prefix}.kind"),
        "actions with owned outputs must use kind = \"generated\"",
      ));
    }

    validate_selection_tokens(self, &self.argv, &format!("{prefix}.argv"))?;
    if self.kind == RepositoryActionKind::Generated {
      validate_selection_tokens(self, &self.check_argv, &format!("{prefix}.check_argv"))?;
    }
    validate_unique_strings(&self.targets, &format!("{prefix}.targets"))?;
    validate_unique_strings(&self.features, &format!("{prefix}.features"))?;

    if self.environment.entries.len() > MAX_ACTION_ENVIRONMENT_ENTRIES {
      return Err(invalid(
        format!("{prefix}.environment.entries"),
        format!(
          "contains {} entries; at most {MAX_ACTION_ENVIRONMENT_ENTRIES} are allowed",
          self.environment.entries.len()
        ),
      ));
    }
    let mut environment_names = std::collections::BTreeSet::new();
    for (index, entry) in self.environment.entries.iter().enumerate() {
      let field = format!("{prefix}.environment.entries[{index}]");
      validate_environment_name(entry.name(), &format!("{field}.name"))?;
      if !environment_names.insert(entry.name().to_ascii_uppercase()) {
        return Err(invalid(
          field,
          format!("duplicates environment name '{}'", entry.name()),
        ));
      }
      if !matches!(entry, RepositoryEnvironmentEntry::Secret { .. }) && secret_environment_name(entry.name()) {
        return Err(invalid(
          format!("{field}.kind"),
          format!(
            "secret-shaped environment name '{}' must use kind = \"secret\"",
            entry.name()
          ),
        ));
      }
      if let RepositoryEnvironmentEntry::Fixed { value, .. } = entry {
        validate_bounded_string(value, &format!("{field}.value"))?;
      }
    }
    Ok(())
  }
}

fn secret_environment_name(name: &str) -> bool {
  let normalized = name.to_ascii_lowercase().replace('_', "-");
  normalized == "token"
    || normalized.ends_with("-token")
    || normalized.contains("password")
    || normalized.contains("secret")
    || normalized.contains("credential")
    || normalized.contains("private-key")
}

fn validate_action_argv(argv: &[String], field: &str) -> Result<(), ConfigError> {
  if argv.is_empty() {
    return Err(invalid(field, "must contain a program"));
  }
  if argv.len() > MAX_ACTION_ARGUMENTS {
    return Err(invalid(
      field,
      format!(
        "contains {} values; at most {MAX_ACTION_ARGUMENTS} are allowed",
        argv.len()
      ),
    ));
  }
  let total_argv_bytes = argv.iter().map(String::len).sum::<usize>();
  if total_argv_bytes > MAX_ACTION_TOTAL_ARGV_BYTES {
    return Err(invalid(
      field,
      format!("contains {total_argv_bytes} bytes; at most {MAX_ACTION_TOTAL_ARGV_BYTES} are allowed"),
    ));
  }
  for (index, argument) in argv.iter().enumerate() {
    validate_bounded_string(argument, &format!("{field}[{index}]"))?;
    validate_action_argument(argument, &format!("{field}[{index}]"))?;
  }
  validate_program(&argv[0], &format!("{field}[0]"))
}

fn validate_selection_tokens(action: &RepositoryAction, argv: &[String], field: &str) -> Result<(), ConfigError> {
  let count = |token: &str| argv.iter().filter(|argument| argument.as_str() == token).count();
  match (action.packages, count("{packages}")) {
    (RepositoryPackageSelection::None, 0) => {}
    (RepositoryPackageSelection::None, _) => {
      return Err(invalid(
        field,
        "must not contain {packages} when no package policy is configured",
      ));
    }
    (_, 1) => {}
    (_, 0) => {
      return Err(invalid(
        field,
        "must contain exactly one {packages} token for the configured package policy",
      ));
    }
    (_, _) => return Err(invalid(field, "must not repeat the {packages} token")),
  }

  for (values, token) in [(&action.targets, "{targets}"), (&action.features, "{features}")] {
    match (values.is_empty(), count(token)) {
      (true, 0) | (false, 1) => {}
      (true, _) => {
        return Err(invalid(
          field,
          format!("must not contain {token} when no values are configured"),
        ));
      }
      (false, 0) => {
        return Err(invalid(
          field,
          format!("must contain exactly one {token} when values are configured"),
        ));
      }
      (false, _) => return Err(invalid(field, format!("must not repeat the {token} token"))),
    }
  }
  Ok(())
}

fn validate_action_dependencies(actions: &FxHashMap<String, RepositoryAction>) -> Result<(), ConfigError> {
  let mut indegree = std::collections::BTreeMap::new();
  let mut dependents = std::collections::BTreeMap::<&str, Vec<&str>>::new();
  for name in actions.keys() {
    indegree.insert(name.as_str(), 0usize);
  }
  for (name, action) in actions {
    for dependency in &action.dependencies {
      if is_builtin_action(dependency) {
        continue;
      }
      if !actions.contains_key(dependency) {
        return Err(invalid(
          format!("run.action.{name}.dependencies"),
          format!("depends on unknown action '{dependency}'"),
        ));
      }
      *indegree
        .get_mut(name.as_str())
        .ok_or_else(|| invalid("run.action", "internal dependency index mismatch"))? += 1;
      dependents.entry(dependency).or_default().push(name);
    }
  }

  let mut ready = indegree
    .iter()
    .filter_map(|(name, count)| (*count == 0).then_some(*name))
    .collect::<std::collections::BTreeSet<_>>();
  let mut visited = 0usize;
  while let Some(name) = ready.pop_first() {
    visited += 1;
    for dependent in dependents.get(name).into_iter().flatten() {
      let count = indegree
        .get_mut(dependent)
        .ok_or_else(|| invalid("run.action", "internal dependent index mismatch"))?;
      *count -= 1;
      if *count == 0 {
        ready.insert(dependent);
      }
    }
  }
  if visited != actions.len() {
    let cycle = indegree
      .into_iter()
      .filter_map(|(name, count)| (count > 0).then_some(name))
      .collect::<Vec<_>>()
      .join(", ");
    return Err(invalid("run.action", format!("dependency cycle contains: {cycle}")));
  }
  Ok(())
}

fn validate_action_output_ownership(actions: &FxHashMap<String, RepositoryAction>) -> Result<(), ConfigError> {
  let owners = actions
    .iter()
    .flat_map(|(owner, action)| action.outputs.iter().map(move |path| (path.as_str(), owner.as_str())))
    .collect::<Vec<_>>();
  if let Some((path, owner, other_path, other_owner)) = first_repository_output_overlap(owners) {
    return Err(invalid(
      "run.action",
      format!("output '{path}' owned by '{owner}' overlaps '{other_path}' owned by '{other_owner}'"),
    ));
  }
  Ok(())
}

pub(crate) fn first_repository_output_overlap<'a>(
  owners: Vec<(&'a str, &'a str)>,
) -> Option<(&'a str, &'a str, &'a str, &'a str)> {
  let mut portable = owners
    .into_iter()
    .map(|(path, owner)| (path.to_lowercase(), path, owner))
    .collect::<Vec<_>>();
  portable.sort_unstable();
  let mut seen = std::collections::BTreeMap::<&str, (&str, &str)>::new();
  for (comparison, path, owner) in &portable {
    if let Some((existing_path, existing_owner)) = seen.get(comparison.as_str()) {
      return Some((existing_path, existing_owner, path, owner));
    }
    for (separator, _) in comparison.match_indices('/') {
      let ancestor = &comparison[..separator];
      if let Some((ancestor_path, ancestor_owner)) = seen.get(ancestor) {
        return Some((ancestor_path, ancestor_owner, path, owner));
      }
    }
    seen.insert(comparison, (path, owner));
  }
  None
}

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> ConfigError {
  ConfigError::InvalidField {
    field: field.into(),
    reason: reason.into(),
  }
}

fn validate_action_id(value: &str, field: &str) -> Result<(), ConfigError> {
  if value.is_empty()
    || value.len() > 64
    || !value.as_bytes()[0].is_ascii_lowercase()
    || !value
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
  {
    return Err(invalid(
      field,
      "must start with a lowercase ASCII letter and contain only lowercase letters, digits, '-' or '_' (64 bytes max)",
    ));
  }
  Ok(())
}

fn validate_surface_name(value: &str, field: &str) -> Result<(), ConfigError> {
  let custom = value
    .strip_prefix("custom:")
    .is_some_and(|name| validate_action_id(name, field).is_ok());
  if !PLANNER_SURFACE_NAMES.contains(&value) && !custom {
    return Err(invalid(
      field,
      "must name build, test, bench, docs, infra, or a valid custom:<name> planner surface",
    ));
  }
  Ok(())
}

fn validate_program(program: &str, field: &str) -> Result<(), ConfigError> {
  if program.is_empty() {
    return Err(invalid(field, "program must not be empty"));
  }
  if program.contains('{') || program.contains('}') {
    return Err(invalid(
      field,
      "the program must be a literal executable, not a substitution",
    ));
  }
  let basename = Path::new(program)
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or(program)
    .to_ascii_lowercase();
  let basename = basename.strip_suffix(".exe").unwrap_or(&basename);
  let script_extension = Path::new(program)
    .extension()
    .and_then(|extension| extension.to_str())
    .map(str::to_ascii_lowercase)
    .is_some_and(|extension| matches!(extension.as_str(), "bat" | "cmd" | "ps1"));
  if SHELL_PROGRAMS.contains(&basename) || script_extension {
    return Err(invalid(
      field,
      format!("shell program '{program}' is not allowed; configure a direct executable and literal argv"),
    ));
  }
  Ok(())
}

fn validate_action_argument(value: &str, field: &str) -> Result<(), ConfigError> {
  if value.contains('{') || value.contains('}') {
    let Some(token) = value.strip_prefix('{').and_then(|value| value.strip_suffix('}')) else {
      return Err(invalid(field, "substitutions must occupy the complete argv value"));
    };
    if !ACTION_ARG_TOKENS.contains(&token) {
      return Err(invalid(
        field,
        format!(
          "unknown action token '{{{token}}}'; allowed tokens: {}",
          ACTION_ARG_TOKENS.join(", ")
        ),
      ));
    }
  }
  Ok(())
}

fn validate_environment_name(value: &str, field: &str) -> Result<(), ConfigError> {
  let mut bytes = value.bytes();
  let valid_start = bytes
    .next()
    .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
  if !valid_start || value.len() > 255 || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
    return Err(invalid(
      field,
      "must be a portable environment name: ASCII letter or '_' followed by letters, digits, or '_' (255 bytes max)",
    ));
  }
  Ok(())
}

fn validate_working_directory(value: &str, field: &str) -> Result<(), ConfigError> {
  validate_bounded_string(value, field)?;
  if value == "." {
    return Ok(());
  }
  validate_repository_path(value, field)
}

fn validate_path_list(values: &[String], field: &str, allow_workspace: bool) -> Result<(), ConfigError> {
  if values.len() > MAX_ACTION_PATHS {
    return Err(invalid(
      field,
      format!(
        "contains {} paths; at most {MAX_ACTION_PATHS} are allowed",
        values.len()
      ),
    ));
  }
  let mut unique = std::collections::BTreeSet::new();
  for (index, value) in values.iter().enumerate() {
    let item_field = format!("{field}[{index}]");
    validate_bounded_string(value, &item_field)?;
    if value == "." {
      if !allow_workspace {
        return Err(invalid(item_field, "the workspace root cannot be owned as an output"));
      }
    } else {
      validate_repository_path(value, &item_field)?;
    }
    if !unique.insert(value) {
      return Err(invalid(item_field, format!("duplicates path '{value}'")));
    }
  }
  Ok(())
}

fn validate_repository_path(value: &str, field: &str) -> Result<(), ConfigError> {
  if value.contains('\\') {
    return Err(invalid(field, "must use portable '/' path separators"));
  }
  let normalized = RepositoryPath::new(Path::new(value)).map_err(|error| invalid(field, error.to_string()))?;
  if normalized.as_str() != value {
    return Err(invalid(
      field,
      format!("must use canonical repository path '{}'", normalized.as_str()),
    ));
  }
  Ok(())
}

fn validate_unique_strings(values: &[String], field: &str) -> Result<(), ConfigError> {
  if values.len() > MAX_ACTION_PATHS {
    return Err(invalid(
      field,
      format!(
        "contains {} values; at most {MAX_ACTION_PATHS} are allowed",
        values.len()
      ),
    ));
  }
  let mut unique = std::collections::BTreeSet::new();
  for (index, value) in values.iter().enumerate() {
    let item_field = format!("{field}[{index}]");
    validate_bounded_string(value, &item_field)?;
    if value.is_empty() {
      return Err(invalid(item_field, "must not be empty"));
    }
    if !unique.insert(value) {
      return Err(invalid(item_field, format!("duplicates value '{value}'")));
    }
  }
  Ok(())
}

fn validate_bounded_string(value: &str, field: &str) -> Result<(), ConfigError> {
  if value.len() > MAX_ACTION_STRING_BYTES {
    return Err(invalid(
      field,
      format!(
        "contains {} bytes; at most {MAX_ACTION_STRING_BYTES} are allowed",
        value.len()
      ),
    ));
  }
  if value.contains('\0') {
    return Err(invalid(field, "must not contain a NUL byte"));
  }
  Ok(())
}

/// Returns true when name is one of the built-in profiles.
pub fn is_builtin_profile(name: &str) -> bool {
  BUILTIN_PROFILE_NAMES.contains(&name)
}

fn is_builtin_action(name: &str) -> bool {
  BUILTIN_ACTION_NAMES.contains(&name)
}

fn validate_tokens(value: &str, allowed: &[&str], field: &str) -> Result<(), ConfigError> {
  for token in extract_tokens(value) {
    if !allowed.contains(&token.as_str()) {
      return Err(ConfigError::InvalidField {
        field: field.to_string(),
        reason: format!("unknown token '{{{}}}'; allowed tokens: {}", token, allowed.join(", ")),
      });
    }
  }
  Ok(())
}

fn extract_tokens(value: &str) -> Vec<String> {
  let mut tokens = Vec::new();
  let bytes = value.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'{' {
      let start = i + 1;
      if let Some(end_rel) = bytes[start..].iter().position(|b| *b == b'}') {
        let end = start + end_rel;
        if end > start {
          let token = &value[start..end];
          if token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
          {
            tokens.push(token.to_string());
          }
        }
        i = end + 1;
        continue;
      }
    }
    i += 1;
  }
  tokens
}

#[cfg(test)]
mod tests {
  use super::*;

  fn valid_action(argv: &[&str]) -> RepositoryAction {
    RepositoryAction {
      argv: argv.iter().map(|value| (*value).to_string()).collect(),
      when: vec!["build".to_string()],
      inputs: vec!["Cargo.toml".to_string()],
      ..RepositoryAction::default()
    }
  }

  #[test]
  fn validate_rejects_empty_actions() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert("custom".to_string(), RunProfile::default());

    let err = cfg.validate().expect_err("profile without actions should fail");
    assert!(err.to_string().contains("must contain at least one action"));
  }

  #[test]
  fn validate_accepts_builtin_default_profile() {
    let cfg = RunConfig {
      default_profile: Some("local".to_string()),
      ..RunConfig::default()
    };

    assert!(cfg.validate().is_ok());
  }

  #[test]
  fn validate_rejects_unknown_default_profile() {
    let cfg = RunConfig {
      default_profile: Some("missing".to_string()),
      ..RunConfig::default()
    };

    let err = cfg.validate().expect_err("unknown default profile should fail");
    assert!(err.to_string().contains("unknown profile 'missing'"));
  }

  #[test]
  fn validate_rejects_unknown_run_arg_token() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "custom".to_string(),
      RunProfile {
        actions: vec!["test".to_string()],
        run_args: vec!["--manifest-path".to_string(), "{unknown}".to_string()],
        ..RunProfile::default()
      },
    );
    let err = cfg
      .validate()
      .expect_err("unknown run_args token should fail validation");
    assert!(err.to_string().contains("unknown token '{unknown}'"));
  }

  #[test]
  fn validate_rejects_unknown_since_token() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "custom".to_string(),
      RunProfile {
        actions: vec!["test".to_string()],
        baseline: Some(RunBaseline::Since {
          reference: "{cargo_args}".to_string(),
        }),
        ..RunProfile::default()
      },
    );
    let err = cfg.validate().expect_err("unknown since token should fail validation");
    assert!(err.to_string().contains("unknown token '{cargo_args}'"));
  }

  #[test]
  fn baseline_type_cannot_represent_conflicting_modes() {
    let err = toml_edit::de::from_str::<RunProfile>("surfaces = [\"test\"]\nsince = \"HEAD~1\"\nmerge_base = true\n")
      .unwrap_err();
    assert!(err.to_string().contains("mutually exclusive"));
  }

  #[test]
  fn valid_legacy_baselines_map_to_typed_policy() {
    let profile: RunProfile = toml_edit::de::from_str("surfaces = [\"test\"]\nmerge_base = true\n").unwrap();
    assert_eq!(profile.baseline, Some(RunBaseline::MergeBase));

    let profile: RunProfile = toml_edit::de::from_str("surfaces = [\"test\"]\nsince = \"HEAD~1\"\n").unwrap();
    assert_eq!(
      profile.baseline,
      Some(RunBaseline::Since {
        reference: "HEAD~1".to_string()
      })
    );
  }

  #[test]
  fn validate_rejects_custom_surface_in_profile() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "ci".to_string(),
      RunProfile {
        actions: vec!["custom:workloads".to_string()],
        ..RunProfile::default()
      },
    );
    let err = cfg.validate().expect_err("custom surface in profile should fail");
    let msg = err.to_string();
    assert!(msg.contains("invalid action 'custom:workloads'"));
    assert!(msg.contains("planner outputs"));
  }

  #[test]
  fn validate_rejects_infra_surface_in_profile() {
    let mut cfg = RunConfig::default();
    cfg.profiles.insert(
      "ci".to_string(),
      RunProfile {
        actions: vec!["infra".to_string()],
        ..RunProfile::default()
      },
    );
    let err = cfg.validate().expect_err("infra surface in profile should fail");
    let msg = err.to_string();
    assert!(msg.contains("invalid action 'infra'"));
    assert!(msg.contains("planner output"));
  }

  #[test]
  fn validate_rejects_workflow_mapping_to_missing_profile() {
    let mut cfg = RunConfig::default();
    cfg.workflow.insert("commit".to_string(), "missing".to_string());
    let err = cfg.validate().expect_err("missing profile mapping should fail");
    assert!(err.to_string().contains("unknown profile 'missing'"));
  }

  #[test]
  fn repository_action_parses_typed_environment_and_substitutions() {
    let config: RunConfig = toml_edit::de::from_str(
      r#"
[action.codegen]
kind = "generated"
argv = ["tool", "--root", "{workspace_root}", "{packages}", "{targets}", "{features}"]
check_argv = ["tool", "--check", "--root", "{workspace_root}", "{packages}", "{targets}", "{features}"]
when = ["build", "custom:schema"]
packages = "selected"
targets = ["x86_64-unknown-linux-gnu"]
features = ["serde"]
inputs = ["Cargo.toml", "src"]
outputs = ["generated/schema.rs"]

[action.codegen.environment]
entries = [
  { kind = "fixed", name = "MODE", value = "check" },
  { kind = "pass", name = "CI" },
  { kind = "cargo", name = "CARGO_TARGET_DIR", value = "target-directory" },
  { kind = "secret", name = "PUBLISH_TOKEN" },
]

[profile.ci]
actions = ["codegen"]
"#,
    )
    .expect("typed repository action should parse");

    config.validate().expect("typed repository action should validate");
    assert_eq!(config.actions["codegen"].kind, RepositoryActionKind::Generated);
    assert_eq!(config.actions["codegen"].environment.entries.len(), 4);
  }

  #[test]
  fn repository_action_rejects_shells_and_partial_or_unknown_tokens() {
    for (argv, expected) in [
      (vec!["sh", "-c", "cargo check"], "shell program 'sh' is not allowed"),
      (
        vec!["tools/check.cmd", "cargo check"],
        "shell program 'tools/check.cmd' is not allowed",
      ),
      (vec!["tool", "prefix-{workspace_root}"], "complete argv value"),
      (vec!["tool", "{expression}"], "unknown action token '{expression}'"),
    ] {
      let mut config = RunConfig::default();
      config.actions.insert("task".to_string(), valid_action(&argv));
      let error = config.validate().expect_err("unsafe action argv must fail closed");
      assert!(error.to_string().contains(expected), "unexpected error: {error}");
    }

    let mut config = RunConfig::default();
    let mut action = valid_action(&["tool"]);
    action.when = vec!["lint".to_string()];
    config.actions.insert("task".to_string(), action);
    let error = config
      .validate()
      .expect_err("action IDs must not be accepted as planner surfaces");
    assert!(error.to_string().contains("must name build, test, bench, docs, infra"));
  }

  #[test]
  fn repository_action_rejects_path_escape_and_unowned_generator() {
    let mut config = RunConfig::default();
    let mut escaped = valid_action(&["tool"]);
    escaped.inputs = vec!["../outside".to_string()];
    config.actions.insert("escaped".to_string(), escaped);
    let error = config.validate().expect_err("path traversal must fail closed");
    assert!(error.to_string().contains("must not contain '..'"));

    let mut config = RunConfig::default();
    let mut generator = valid_action(&["tool"]);
    generator.kind = RepositoryActionKind::Generated;
    generator.check_argv = vec!["tool".to_string(), "--check".to_string()];
    config.actions.insert("generator".to_string(), generator);
    let error = config
      .validate()
      .expect_err("generator without outputs must fail closed");
    assert!(error.to_string().contains("must declare at least one owned output"));

    let mut config = RunConfig::default();
    let mut generator = valid_action(&["tool"]);
    generator.kind = RepositoryActionKind::Generated;
    generator.outputs = vec!["generated/output.rs".to_string()];
    config.actions.insert("generator".to_string(), generator);
    let error = config
      .validate()
      .expect_err("generator without a read-only check must fail closed");
    assert!(error.to_string().contains("check_argv") && error.to_string().contains("must contain a program"));
  }

  #[test]
  fn repository_action_rejects_nonportable_or_aliased_paths() {
    for path in ["generated\\api", "generated//api", "generated/api/"] {
      let mut config = RunConfig::default();
      let mut action = valid_action(&["tool"]);
      action.kind = RepositoryActionKind::Generated;
      action.check_argv = vec!["tool".to_string(), "--check".to_string()];
      action.outputs = vec![path.to_string()];
      config.actions.insert("generator".to_string(), action);

      let error = config.validate().expect_err("non-canonical paths must fail closed");
      assert!(
        error.to_string().contains("portable '/' path separators")
          || error.to_string().contains("canonical repository path"),
        "unexpected error for {path}: {error}"
      );
    }
  }

  #[test]
  fn repository_action_rejects_case_insensitive_environment_duplicates() {
    let mut config = RunConfig::default();
    let mut action = valid_action(&["tool"]);
    action.environment.entries = vec![
      RepositoryEnvironmentEntry::Pass {
        name: "Path".to_string(),
      },
      RepositoryEnvironmentEntry::Secret {
        name: "PATH".to_string(),
      },
    ];
    config.actions.insert("task".to_string(), action);

    let error = config
      .validate()
      .expect_err("portable environment collisions must fail closed");
    assert!(error.to_string().contains("duplicates environment name 'PATH'"));
  }

  #[test]
  fn repository_action_requires_secret_capability_for_secret_shaped_names() {
    let mut config = RunConfig::default();
    let mut action = valid_action(&["tool"]);
    action.environment.entries = vec![RepositoryEnvironmentEntry::Pass {
      name: "PUBLISH_TOKEN".to_string(),
    }];
    config.actions.insert("release".to_string(), action);

    let error = config
      .validate()
      .expect_err("secret-shaped pass-through must fail closed");
    assert!(error.to_string().contains("must use kind = \"secret\""), "{error}");
  }

  #[test]
  fn repository_action_rejects_unknown_dependencies_and_cycles() {
    let mut config = RunConfig::default();
    let mut action = valid_action(&["tool"]);
    action.dependencies = vec!["missing".to_string()];
    config.actions.insert("task".to_string(), action);
    let error = config.validate().expect_err("unknown dependency must fail closed");
    assert!(error.to_string().contains("depends on unknown action 'missing'"));

    let mut config = RunConfig::default();
    let mut first = valid_action(&["first"]);
    first.dependencies = vec!["second".to_string()];
    let mut second = valid_action(&["second"]);
    second.dependencies = vec!["first".to_string()];
    config.actions.insert("first".to_string(), first);
    config.actions.insert("second".to_string(), second);
    let error = config.validate().expect_err("dependency cycle must fail closed");
    assert!(error.to_string().contains("dependency cycle contains: first, second"));
  }

  #[test]
  fn repository_generated_outputs_have_one_global_owner() {
    let mut first = valid_action(&["first"]);
    first.kind = RepositoryActionKind::Generated;
    first.check_argv = vec!["first".to_string(), "--check".to_string()];
    first.outputs = vec!["generated".to_string()];
    let mut lexical_sibling = valid_action(&["lexical-sibling"]);
    lexical_sibling.kind = RepositoryActionKind::Generated;
    lexical_sibling.check_argv = vec!["lexical-sibling".to_string(), "--check".to_string()];
    lexical_sibling.outputs = vec!["generated-other".to_string()];
    let mut nested = valid_action(&["nested"]);
    nested.kind = RepositoryActionKind::Generated;
    nested.check_argv = vec!["nested".to_string(), "--check".to_string()];
    nested.outputs = vec!["generated/api".to_string()];
    let mut config = RunConfig::default();
    config.actions.insert("first".to_string(), first);
    config.actions.insert("lexical-sibling".to_string(), lexical_sibling);
    config.actions.insert("nested".to_string(), nested);

    let error = config
      .validate()
      .expect_err("generated output ownership must be repository-global");
    assert!(error.to_string().contains("overlaps"));

    let mut first = valid_action(&["first"]);
    first.kind = RepositoryActionKind::Generated;
    first.check_argv = vec!["first".to_string(), "--check".to_string()];
    first.outputs = vec!["Generated/API".to_string()];
    let mut second = valid_action(&["second"]);
    second.kind = RepositoryActionKind::Generated;
    second.check_argv = vec!["second".to_string(), "--check".to_string()];
    second.outputs = vec!["generated/api".to_string()];
    let mut config = RunConfig::default();
    config.actions.insert("first".to_string(), first);
    config.actions.insert("second".to_string(), second);

    let error = config
      .validate()
      .expect_err("case-insensitive generated output ownership must be portable");
    assert!(error.to_string().contains("overlaps"));
  }

  #[test]
  fn repository_output_ownership_scales_to_configured_limits() {
    let mut actions = FxHashMap::default();
    for action_index in 0..MAX_ACTIONS {
      let action = RepositoryAction {
        outputs: (0..MAX_ACTION_PATHS)
          .map(|path_index| format!("generated/action-{action_index:02}/path-{path_index:03}"))
          .collect(),
        ..RepositoryAction::default()
      };
      actions.insert(format!("action-{action_index:02}"), action);
    }

    validate_action_output_ownership(&actions).expect("maximum bounded ownership set should be collision-free");
  }

  #[test]
  fn profile_accepts_configured_actions_and_rejects_repetition() {
    let mut config = RunConfig::default();
    config.actions.insert("task".to_string(), valid_action(&["tool"]));
    config.profiles.insert(
      "custom".to_string(),
      RunProfile {
        actions: vec!["task".to_string()],
        ..RunProfile::default()
      },
    );
    config.validate().expect("configured profile action should validate");

    config
      .profiles
      .get_mut("custom")
      .unwrap()
      .actions
      .push("task".to_string());
    let error = config.validate().expect_err("repeated profile action must fail closed");
    assert!(error.to_string().contains("repeats action 'task'"));
  }
}
