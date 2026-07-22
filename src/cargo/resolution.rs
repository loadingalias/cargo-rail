//! Lazy Cargo resolution views for exact package, feature, and target selections.
//!
//! The canonical native/workspace/default view is owned by one workspace
//! context and reuses its already-loaded metadata and graph. Every derived view
//! is single-flight cached by the complete request plus the selected Cargo,
//! rustc, rustdoc, compiler-wrapper, and non-secret Cargo configuration
//! identities used to resolve it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use cargo_metadata::{CargoOpt, Metadata, MetadataCommand, Package, PackageId};
use rustc_hash::FxHashMap;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::compiler::cfg_eval::{TargetCfgSet, cargo_target_constraint_matches};
use crate::error::{RailError, RailResult};
use crate::executable::resolve_program as resolve_executable_program;
use crate::graph::WorkspaceGraph;
use crate::source::ContentDigest;
use crate::utils::canonicalize_existing;

const MAX_CARGO_CONFIG_INCLUDE_DEPTH: usize = 64;
const CREDENTIAL_CAPABILITY_KEY: &str = "cargo-rail-credential-capability-v1";
const CREDENTIAL_ENV_MARKER_PREFIX: &str = "<cargo-rail-credential-capability-v1:";
const RUSTC_ENV_PRECEDENCE: &[&str] = &["RUSTC", "CARGO_BUILD_RUSTC"];
const RUSTDOC_ENV_PRECEDENCE: &[&str] = &["RUSTDOC", "CARGO_BUILD_RUSTDOC"];
const RUSTC_WRAPPER_ENV_PRECEDENCE: &[&str] = &["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"];
const RUSTC_WORKSPACE_WRAPPER_ENV_PRECEDENCE: &[&str] =
  &["RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"];

/// Workspace packages whose feature roots define one Cargo resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResolutionPackages {
  /// Use every workspace member, matching ordinary `cargo metadata` behavior.
  Workspace,
  /// Use these exact workspace package identities as feature roots.
  Selected(BTreeSet<PackageId>),
}

/// Cargo feature roots for one resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResolutionFeatures {
  /// Enable Cargo's default features for the selected packages.
  Default,
  /// Disable default features and add no explicit feature roots.
  NoDefaultFeatures,
  /// Enable every declared feature for the selected packages.
  AllFeatures,
  /// Disable defaults and enable exact package-qualified feature roots.
  Selected(BTreeMap<PackageId, BTreeSet<String>>),
}

/// Complete semantic request for one Cargo resolution view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolutionRequest {
  packages: ResolutionPackages,
  features: ResolutionFeatures,
  target_filter: Option<String>,
}

impl ResolutionRequest {
  /// Construct an exact resolution request.
  ///
  /// Package and feature identities are validated against the canonical
  /// workspace metadata when the request is loaded. An empty target filter is
  /// rejected instead of being interpreted as the native target.
  pub fn new(
    packages: ResolutionPackages,
    features: ResolutionFeatures,
    target_filter: Option<String>,
  ) -> RailResult<Self> {
    if target_filter.as_ref().is_some_and(|target| target.is_empty()) {
      return Err(RailError::message("Cargo resolution target filter cannot be empty"));
    }
    if matches!(&packages, ResolutionPackages::Selected(selected) if selected.is_empty()) {
      return Err(RailError::message("Cargo resolution package selection cannot be empty"));
    }
    let features = match features {
      ResolutionFeatures::Selected(mut selected) => {
        selected.retain(|_, features| !features.is_empty());
        if selected.is_empty() {
          ResolutionFeatures::NoDefaultFeatures
        } else {
          ResolutionFeatures::Selected(selected)
        }
      }
      features => features,
    };
    Ok(Self {
      packages,
      features,
      target_filter,
    })
  }

  /// Borrow the exact package selection.
  pub fn packages(&self) -> &ResolutionPackages {
    &self.packages
  }

  /// Borrow the exact feature selection.
  pub fn features(&self) -> &ResolutionFeatures {
    &self.features
  }

  /// Borrow the Cargo `--filter-platform` value, if present.
  pub fn target_filter(&self) -> Option<&str> {
    self.target_filter.as_deref()
  }

  fn is_native_workspace_default(&self) -> bool {
    self.packages == ResolutionPackages::Workspace
      && self.features == ResolutionFeatures::Default
      && self.target_filter.is_none()
  }
}

impl Default for ResolutionRequest {
  fn default() -> Self {
    Self {
      packages: ResolutionPackages::Workspace,
      features: ResolutionFeatures::Default,
      target_filter: None,
    }
  }
}

/// Immutable Cargo metadata and exact dependency graph for one resolution.
pub struct ResolutionView {
  request: ResolutionRequest,
  metadata: Arc<Metadata>,
  graph: Arc<WorkspaceGraph>,
}

impl ResolutionView {
  /// Borrow the semantic request that produced this view.
  pub fn request(&self) -> &ResolutionRequest {
    &self.request
  }

  /// Borrow Cargo's metadata for this resolution.
  pub fn metadata(&self) -> &Metadata {
    &self.metadata
  }

  /// Borrow the exact `PackageId` dependency graph for this resolution.
  pub fn graph(&self) -> &WorkspaceGraph {
    &self.graph
  }

  pub(crate) fn shared_metadata(&self) -> Arc<Metadata> {
    Arc::clone(&self.metadata)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResolutionViewKey {
  request: ResolutionRequest,
  toolchain: ToolchainIdentity,
  cargo_config: ContentDigest,
  credential_sensitive: bool,
}

/// Actual Cargo, rustc, and rustdoc programs plus compiler wrapper selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolchainIdentity {
  cargo_program: OsString,
  cargo_verbose_version: String,
  rustc_program: OsString,
  rustc_verbose_version: String,
  rustdoc_program: OsString,
  rustdoc_verbose_version: String,
  rustc_wrapper_program: Option<OsString>,
  rustc_workspace_wrapper_program: Option<OsString>,
  host_target: String,
  rustc_sysroot: PathBuf,
}

impl ToolchainIdentity {
  /// Return the selected Cargo program.
  pub fn cargo_program(&self) -> &OsStr {
    &self.cargo_program
  }

  /// Return the complete normalized output of `cargo -Vv`.
  pub fn cargo_verbose_version(&self) -> &str {
    &self.cargo_verbose_version
  }

  /// Return the selected rustc program.
  pub fn rustc_program(&self) -> &OsStr {
    &self.rustc_program
  }

  /// Return the complete normalized output of `rustc -vV`.
  pub fn rustc_verbose_version(&self) -> &str {
    &self.rustc_verbose_version
  }

  /// Return the selected rustdoc program.
  pub fn rustdoc_program(&self) -> &OsStr {
    &self.rustdoc_program
  }

  /// Return the complete normalized output of `rustdoc -vV`.
  pub fn rustdoc_verbose_version(&self) -> &str {
    &self.rustdoc_verbose_version
  }

  /// Return Cargo's outer rustc wrapper, when configured.
  pub fn rustc_wrapper_program(&self) -> Option<&OsStr> {
    self.rustc_wrapper_program.as_deref()
  }

  /// Return Cargo's workspace-member rustc wrapper, when configured.
  pub fn rustc_workspace_wrapper_program(&self) -> Option<&OsStr> {
    self.rustc_workspace_wrapper_program.as_deref()
  }

  /// Return the host tuple reported by the selected rustc.
  pub fn host_target(&self) -> &str {
    &self.host_target
  }

  /// Return the stable sysroot selected by the effective rustc wrapper chain.
  pub(crate) fn rustc_sysroot(&self) -> &Path {
    &self.rustc_sysroot
  }

  /// Discover a preinstalled toolchain without invoking Cargo compiler wrappers.
  pub(crate) fn capture_hermetic(cargo_current_dir: &Path) -> RailResult<Self> {
    let selected_rustc = OsStr::new("rustc");
    let rustc_sysroot = PathBuf::from(hermetic_command_identity(
      selected_rustc,
      "--print=sysroot",
      cargo_current_dir,
      "preinstalled 'rustc --print=sysroot'",
    )?);
    let cargo_program = toolchain_program(&rustc_sysroot, "cargo").into_os_string();
    let rustc_program = toolchain_program(&rustc_sysroot, "rustc").into_os_string();
    let rustdoc_program = toolchain_program(&rustc_sysroot, "rustdoc").into_os_string();
    let rustc_verbose_version =
      hermetic_command_identity(&rustc_program, "-vV", cargo_current_dir, "exact sysroot 'rustc -vV'")?;
    let host_target = parse_rustc_host(&rustc_verbose_version)?;
    Ok(Self {
      cargo_verbose_version: hermetic_command_identity(
        &cargo_program,
        "-Vv",
        cargo_current_dir,
        "exact sysroot 'cargo -Vv'",
      )?,
      rustdoc_verbose_version: hermetic_command_identity(
        &rustdoc_program,
        "-vV",
        cargo_current_dir,
        "exact sysroot 'rustdoc -vV'",
      )?,
      cargo_program,
      rustc_program,
      rustc_verbose_version,
      rustdoc_program,
      rustc_wrapper_program: None,
      rustc_workspace_wrapper_program: None,
      host_target,
      rustc_sysroot,
    })
  }
}

/// Exact identity of a custom rustc target specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomTargetSpecification {
  name: String,
  path: PathBuf,
  bytes: Arc<[u8]>,
  digest: ContentDigest,
}

impl CustomTargetSpecification {
  /// Return the filename stem Cargo uses for `[target.<name>]` lookup.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Return the canonical host path retained as capture provenance.
  pub fn path(&self) -> &Path {
    &self.path
  }

  /// Return the exact target JSON bytes consumed by rustc.
  pub fn bytes(&self) -> &[u8] {
    &self.bytes
  }

  /// Return the SHA-256 identity of the exact target JSON bytes.
  pub fn digest(&self) -> ContentDigest {
    self.digest
  }
}

/// Built-in or exact custom target specification selected for a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSpecificationIdentity {
  /// A rustc built-in target, bound to [`ToolchainIdentity`].
  BuiltIn(String),
  /// A custom JSON target, including its exact content.
  Custom(CustomTargetSpecification),
}

/// Effective Cargo configuration for one host or selected compilation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetIdentity {
  specification: TargetSpecificationIdentity,
  host: bool,
  build_target: bool,
  analysis_target: bool,
  cfg: Vec<String>,
  runner: Option<Vec<OsString>>,
  linker: Option<OsString>,
  rustflags: Vec<String>,
  rustdocflags: Vec<String>,
  host_artifact_rustflags: Option<Vec<String>>,
  host_artifact_rustdocflags: Option<Vec<String>>,
}

impl TargetIdentity {
  /// Return the exact built-in or custom target specification identity.
  pub fn specification(&self) -> &TargetSpecificationIdentity {
    &self.specification
  }

  /// Return whether this is the selected compiler host target.
  pub fn is_host(&self) -> bool {
    self.host
  }

  /// Return whether Cargo selected this as a default build target.
  pub fn is_build_target(&self) -> bool {
    self.build_target
  }

  /// Return whether rail configuration selected this for target analysis.
  pub fn is_analysis_target(&self) -> bool {
    self.analysis_target
  }

  /// Return rustc's effective target cfg set after Cargo's flag fixed point.
  pub fn cfg(&self) -> &[String] {
    &self.cfg
  }

  /// Return the selected runner program and fixed arguments.
  pub fn runner(&self) -> Option<&[OsString]> {
    self.runner.as_deref()
  }

  /// Return the selected linker program.
  pub fn linker(&self) -> Option<&OsStr> {
    self.linker.as_deref()
  }

  /// Return Cargo's effective rustc flags for this target.
  pub fn rustflags(&self) -> &[String] {
    &self.rustflags
  }

  /// Return Cargo's effective rustdoc flags for this target.
  pub fn rustdocflags(&self) -> &[String] {
    &self.rustdocflags
  }

  /// Return flags Cargo applies to host-only artifacts for the current build selection.
  ///
  /// Non-host target identities return `None`. An explicitly selected target
  /// makes the host entry `Some([])` under stable Cargo semantics.
  pub fn host_artifact_rustflags(&self) -> Option<&[String]> {
    self.host_artifact_rustflags.as_deref()
  }

  /// Return rustdoc flags for host-only artifacts in the current build selection.
  pub fn host_artifact_rustdocflags(&self) -> Option<&[String]> {
    self.host_artifact_rustdocflags.as_deref()
  }

  pub(crate) fn validate_custom_specification_unchanged(&self) -> RailResult<()> {
    match &self.specification {
      TargetSpecificationIdentity::BuiltIn(_) => Ok(()),
      TargetSpecificationIdentity::Custom(specification) => validate_custom_target_unchanged(specification),
    }
  }
}

/// One sanitized Cargo configuration file in effective merge order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoConfigSource {
  path: PathBuf,
  settings: JsonValue,
}

impl CargoConfigSource {
  /// Return the host path Cargo loaded.
  pub fn path(&self) -> &Path {
    &self.path
  }

  /// Return the non-secret build-affecting settings supplied by this file.
  pub fn settings(&self) -> &JsonValue {
    &self.settings
  }
}

/// Sanitized Cargo configuration inputs and their effective file merge.
///
/// Known credential-bearing values are represented only by typed capability markers.
/// Credential-bearing URLs fail capture rather than entering this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoConfigSnapshot {
  digest: ContentDigest,
  effective_file_settings: JsonValue,
  environment: BTreeMap<String, String>,
  provenance: Vec<CargoConfigSource>,
  credential_capabilities: JsonValue,
  credential_provenance: Option<PathBuf>,
  unmodeled_settings: BTreeSet<String>,
}

impl CargoConfigSnapshot {
  /// Return the context-local identity of the complete sanitized capture.
  ///
  /// This digest includes host config paths for resolution-cache isolation and
  /// is not a path-independent workspace or action identity.
  pub fn digest(&self) -> ContentDigest {
    self.digest
  }

  /// Return configuration-file settings after Cargo's hierarchical merge order.
  ///
  /// Relevant environment entries are exposed separately because Cargo applies
  /// key-specific parsing and precedence rules to them.
  pub fn effective_file_settings(&self) -> &JsonValue {
    &self.effective_file_settings
  }

  /// Return relevant non-secret environment settings that override files.
  ///
  /// Secret-named variables contain only a typed redacted capability marker.
  pub fn environment(&self) -> &BTreeMap<String, String> {
    &self.environment
  }

  /// Return sanitized configuration files in lowest-to-highest precedence order.
  pub fn provenance(&self) -> &[CargoConfigSource] {
    &self.provenance
  }

  /// Return typed capabilities discovered in Cargo's credentials file.
  pub fn credential_capabilities(&self) -> &JsonValue {
    &self.credential_capabilities
  }

  /// Return Cargo settings outside the explicit hermetic build contract.
  pub(crate) fn unmodeled_settings(&self) -> &BTreeSet<String> {
    &self.unmodeled_settings
  }

  pub(crate) fn has_credential_capability(&self) -> bool {
    contains_credential_capability(&self.effective_file_settings)
      || !self.credential_capabilities.as_object().is_some_and(JsonMap::is_empty)
      || self
        .environment
        .values()
        .any(|value| is_credential_environment_marker(value))
  }

  pub(crate) fn repository_config_paths(&self, source_root: &Path) -> RailResult<Vec<crate::source::RepositoryPath>> {
    self
      .provenance
      .iter()
      .map(|source| source.path())
      .chain(self.credential_provenance.iter().map(PathBuf::as_path))
      .filter_map(|path| path.strip_prefix(source_root).ok())
      .map(crate::source::RepositoryPath::new)
      .collect()
  }

  pub(crate) fn materialized_environment(
    &self,
    source_root: &Path,
    materialized_root: &Path,
  ) -> RailResult<Vec<(String, OsString, String)>> {
    let Some(environment) = self.effective_file_settings.get("env") else {
      return Ok(Vec::new());
    };
    let environment = environment
      .as_object()
      .ok_or_else(|| RailError::message("Cargo configuration key 'env' must be a table"))?;
    let mut bindings = Vec::with_capacity(environment.len());
    for (name, setting) in environment {
      if is_credential_capability(setting) {
        return Err(RailError::with_help(
          format!("hermetic execution cannot apply redacted Cargo environment value 'env.{name}'"),
          "move build secrets out of Cargo configuration; acquisition credentials remain fetch-only capabilities",
        ));
      }
      let (configured, force, relative, value_path) = cargo_environment_setting(name, setting)?;
      let (value, identity) = if !force && let Some(captured) = self.environment.get(name) {
        if is_credential_environment_marker(captured) {
          return Err(RailError::with_help(
            format!("hermetic execution cannot apply secret environment variable '{name}'"),
            "remove the secret from the build environment or keep the action explicitly uncacheable",
          ));
        }
        materialize_environment_value(captured, source_root, materialized_root)?
      } else if relative {
        let (_, source) = self
          .config_string_with_source(&value_path)?
          .ok_or_else(|| RailError::message(format!("Cargo configuration key 'env.{name}' has no path provenance")))?;
        let resolved = config_relative_root(source.path())?.join(configured);
        let relative = resolved.strip_prefix(source_root).map_err(|_| {
          RailError::with_help(
            format!("relative Cargo environment value 'env.{name}' resolves outside the captured repository"),
            "move the referenced input into the repository or keep the action explicitly uncacheable",
          )
        })?;
        (
          materialized_root.join(relative).into_os_string(),
          format!("repository:{}", crate::utils::path_to_git_format(relative)),
        )
      } else {
        materialize_environment_value(configured, source_root, materialized_root)?
      };
      bindings.push((name.clone(), value, identity));
    }
    Ok(bindings)
  }

  fn config_string_with_source(&self, path: &[&str]) -> RailResult<Option<(&str, &CargoConfigSource)>> {
    let Some(value) = json_value_at(&self.effective_file_settings, path) else {
      return Ok(None);
    };
    let value = value
      .as_str()
      .ok_or_else(|| RailError::message(format!("Cargo configuration key '{}' must be a string", path.join("."))))?;
    let source = self
      .provenance
      .iter()
      .rev()
      .find(|source| json_value_at(&source.settings, path).is_some_and(JsonValue::is_string))
      .ok_or_else(|| {
        RailError::message(format!(
          "Cargo configuration key '{}' has no source provenance",
          path.join(".")
        ))
      })?;
    Ok(Some((value, source)))
  }

  fn target_value_with_source(
    &self,
    target: &str,
    field: &str,
  ) -> RailResult<Option<(&JsonValue, &CargoConfigSource)>> {
    let path = ["target", target, field];
    self.config_value_with_source(&path)
  }

  fn config_value_with_source(&self, path: &[&str]) -> RailResult<Option<(&JsonValue, &CargoConfigSource)>> {
    let Some(value) = json_value_at(&self.effective_file_settings, path) else {
      return Ok(None);
    };
    let source = if value.is_array() {
      self.provenance.iter().find(|source| {
        json_value_at(&source.settings, path)
          .and_then(JsonValue::as_array)
          .is_some_and(|values| !values.is_empty())
      })
    } else {
      self
        .provenance
        .iter()
        .rev()
        .find(|source| json_value_at(&source.settings, path).is_some())
    }
    .ok_or_else(|| {
      RailError::message(format!(
        "Cargo configuration key '{}' has no source provenance",
        path.join(".")
      ))
    })?;
    Ok(Some((value, source)))
  }

  fn selected_build_targets(&self, host: &str) -> RailResult<Vec<SelectedTarget>> {
    if let Some(value) = self.environment.get("CARGO_BUILD_TARGET") {
      if value.is_empty() {
        return Err(RailError::message("CARGO_BUILD_TARGET selects no targets"));
      }
      return Ok(vec![SelectedTarget {
        host: value == host || value == "host-tuple",
        value: value.clone(),
        origin: TargetOrigin::CurrentDirectory,
        build_target: true,
        analysis_target: false,
      }]);
    }
    if json_value_at(&self.effective_file_settings, &["build", "target"]).is_none() {
      return Ok(Vec::new());
    }

    let mut selected = Vec::new();
    for source in &self.provenance {
      let Some(value) = json_value_at(&source.settings, &["build", "target"]) else {
        continue;
      };
      let values = if let Some(value) = value.as_str() {
        selected.clear();
        vec![value.to_string()]
      } else {
        value
          .as_array()
          .ok_or_else(|| RailError::message("Cargo configuration key 'build.target' must be a string or string array"))?
          .iter()
          .map(|value| {
            value
              .as_str()
              .filter(|value| !value.is_empty())
              .map(str::to_string)
              .ok_or_else(|| {
                RailError::message("Cargo configuration key 'build.target' contains an empty or non-string target")
              })
          })
          .collect::<RailResult<Vec<_>>>()?
      };
      selected.extend(values.into_iter().map(|value| SelectedTarget {
        host: value == host || value == "host-tuple",
        value,
        origin: TargetOrigin::Config(source.path.clone()),
        build_target: true,
        analysis_target: false,
      }));
    }
    if selected.is_empty() {
      return Err(RailError::message(
        "Cargo configuration key 'build.target' selects no targets",
      ));
    }
    Ok(selected)
  }

  fn effective_environment_value(&self, name: &str) -> RailResult<Option<OsString>> {
    let configured = self
      .effective_file_settings
      .get("env")
      .and_then(JsonValue::as_object)
      .and_then(|environment| environment.get(name));
    let Some(setting) = configured else {
      return self
        .environment
        .get(name)
        .map(|value| {
          if is_credential_environment_marker(value) {
            Err(RailError::message(format!(
              "Cargo target identity cannot materialize redacted environment variable '{name}'"
            )))
          } else {
            Ok(OsString::from(value))
          }
        })
        .transpose();
    };
    if is_credential_capability(setting) {
      return Err(RailError::with_help(
        format!("Cargo target identity cannot apply redacted configuration value 'env.{name}'"),
        "move secret environment values out of Cargo configuration before snapshot capture",
      ));
    }
    let (value, force, relative, value_path) = cargo_environment_setting(name, setting)?;
    if !force && let Some(captured) = self.environment.get(name) {
      if is_credential_environment_marker(captured) {
        return Err(RailError::message(format!(
          "Cargo target identity cannot materialize redacted environment variable '{name}'"
        )));
      }
      return Ok(Some(OsString::from(captured)));
    }
    if is_credential_environment_marker(value) {
      return Err(RailError::with_help(
        format!("Cargo target identity cannot apply redacted configuration value 'env.{name}'"),
        "move secret environment values out of Cargo configuration before snapshot capture",
      ));
    }
    if relative {
      let (_, source) = self
        .config_string_with_source(&value_path)?
        .ok_or_else(|| RailError::message(format!("Cargo configuration key 'env.{name}' has no path provenance")))?;
      return Ok(Some(config_relative_root(source.path())?.join(value).into_os_string()));
    }
    Ok(Some(OsString::from(value)))
  }

  pub(crate) fn portable_snapshot_identity(&self, source_root: &Path) -> RailResult<Vec<u8>> {
    let mut identity = Vec::from(&b"cargo-config-snapshot-v1\0"[..]);
    append_frame(
      &mut identity,
      b"effective-settings",
      &serde_json::to_vec(&self.effective_file_settings)?,
    );
    append_frame(
      &mut identity,
      b"environment",
      &serde_json::to_vec(&portable_environment(&self.environment, source_root)?)?,
    );
    append_frame(
      &mut identity,
      b"credential-capabilities",
      &serde_json::to_vec(&self.credential_capabilities)?,
    );
    append_frame(
      &mut identity,
      b"unmodeled-settings",
      &serde_json::to_vec(&self.unmodeled_settings)?,
    );
    for source in &self.provenance {
      let mut provenance = Vec::new();
      append_frame(
        &mut provenance,
        b"path",
        portable_path(source_root, source.path(), "Cargo configuration provenance")?.as_bytes(),
      );
      append_frame(&mut provenance, b"settings", &serde_json::to_vec(source.settings())?);
      append_frame(&mut identity, b"provenance", &provenance);
    }
    Ok(identity)
  }

  pub(crate) fn portable_acquisition_identity(&self, source_root: &Path) -> RailResult<Vec<u8>> {
    let mut identity = Vec::from(&b"cargo-acquisition-snapshot-v1\0"[..]);
    append_frame(
      &mut identity,
      b"effective-settings",
      &serde_json::to_vec(&acquisition_settings(&self.effective_file_settings))?,
    );
    let environment = self
      .environment
      .iter()
      .filter(|(name, _)| is_acquisition_environment(name))
      .map(|(name, value)| (name.clone(), value.clone()))
      .collect::<BTreeMap<_, _>>();
    append_frame(
      &mut identity,
      b"environment",
      &serde_json::to_vec(&portable_environment(&environment, source_root)?)?,
    );
    append_frame(
      &mut identity,
      b"credential-capabilities",
      &serde_json::to_vec(&self.credential_capabilities)?,
    );
    for source in &self.provenance {
      let settings = acquisition_settings(source.settings());
      if settings.as_object().is_none_or(JsonMap::is_empty) {
        continue;
      }
      let mut provenance = Vec::new();
      append_frame(
        &mut provenance,
        b"path",
        portable_path(source_root, source.path(), "Cargo acquisition configuration provenance")?.as_bytes(),
      );
      append_frame(&mut provenance, b"settings", &serde_json::to_vec(&settings)?);
      append_frame(&mut identity, b"provenance", &provenance);
    }
    Ok(identity)
  }

  pub(crate) fn non_secret_acquisition_environment(&self) -> BTreeMap<String, String> {
    self
      .environment
      .iter()
      .filter(|(name, value)| is_acquisition_environment(name) && !is_credential_environment_marker(value))
      .map(|(name, value)| (name.clone(), value.clone()))
      .collect()
  }
}

fn acquisition_settings(settings: &JsonValue) -> JsonValue {
  const KEYS: &[&str] = &[
    "credential-alias",
    "http",
    "net",
    "patch",
    "paths",
    "registries",
    "registry",
    "source",
  ];
  let Some(settings) = settings.as_object() else {
    return JsonValue::Object(JsonMap::new());
  };
  let mut acquisition = settings
    .iter()
    .filter(|(key, _)| KEYS.contains(&key.as_str()))
    .map(|(key, value)| (key.clone(), value.clone()))
    .collect::<JsonMap<_, _>>();
  if let Some(environment) = settings.get("env").and_then(JsonValue::as_object) {
    let environment = environment
      .iter()
      .filter(|(name, _)| is_acquisition_environment(name))
      .map(|(name, value)| (name.clone(), value.clone()))
      .collect::<JsonMap<_, _>>();
    if !environment.is_empty() {
      acquisition.insert("env".to_string(), JsonValue::Object(environment));
    }
  }
  JsonValue::Object(acquisition)
}

fn is_acquisition_environment(name: &str) -> bool {
  name.starts_with("CARGO_NET_")
    || name.starts_with("CARGO_REGISTRIES_")
    || name.starts_with("CARGO_REGISTRY_")
    || name.starts_with("CARGO_SOURCE_")
    || matches!(
      name,
      "ALL_PROXY"
        | "GIT_SSH_COMMAND"
        | "HTTP_PROXY"
        | "HTTPS_PROXY"
        | "NO_PROXY"
        | "SSH_AUTH_SOCK"
        | "SSL_CERT_DIR"
        | "SSL_CERT_FILE"
        | "all_proxy"
        | "http_proxy"
        | "https_proxy"
        | "no_proxy"
    )
}

fn materialize_environment_value(
  value: &str,
  source_root: &Path,
  materialized_root: &Path,
) -> RailResult<(OsString, String)> {
  let path = Path::new(value);
  if path.is_absolute()
    && let Ok(relative) = path.strip_prefix(source_root)
  {
    return Ok((
      materialized_root.join(relative).into_os_string(),
      format!("repository:{}", crate::utils::path_to_git_format(relative)),
    ));
  }
  Ok((OsString::from(value), value.to_string()))
}

fn portable_environment(
  environment: &BTreeMap<String, String>,
  source_root: &Path,
) -> RailResult<BTreeMap<String, String>> {
  let source_root = source_root
    .to_str()
    .ok_or_else(|| RailError::message("snapshot source root is not valid UTF-8 for portable Cargo environment"))?;
  Ok(
    environment
      .iter()
      .map(|(name, value)| {
        let value = if is_credential_environment_marker(value) {
          value.clone()
        } else if let Some(suffix) = value.strip_prefix(source_root)
          && (suffix.is_empty() || suffix.starts_with('/') || suffix.starts_with('\\'))
        {
          format!(
            "repository:{}",
            suffix.trim_start_matches(['/', '\\']).replace('\\', "/")
          )
        } else {
          value.clone()
        };
        (name.clone(), value)
      })
      .collect(),
  )
}

impl ToolchainIdentity {
  pub(crate) fn portable_snapshot_identity(&self, source_root: &Path) -> RailResult<Vec<u8>> {
    let mut identity = Vec::from(&b"toolchain-snapshot-v1\0"[..]);
    append_os_frame(
      &mut identity,
      b"cargo-program",
      self.cargo_program(),
      source_root,
      "Cargo program",
    )?;
    append_frame(&mut identity, b"cargo-version", self.cargo_verbose_version().as_bytes());
    append_os_frame(
      &mut identity,
      b"rustc-program",
      self.rustc_program(),
      source_root,
      "rustc program",
    )?;
    append_frame(&mut identity, b"rustc-version", self.rustc_verbose_version().as_bytes());
    append_os_frame(
      &mut identity,
      b"rustdoc-program",
      self.rustdoc_program(),
      source_root,
      "rustdoc program",
    )?;
    append_frame(
      &mut identity,
      b"rustdoc-version",
      self.rustdoc_verbose_version().as_bytes(),
    );
    append_optional_os_frame(
      &mut identity,
      b"rustc-wrapper",
      self.rustc_wrapper_program(),
      source_root,
      "rustc wrapper",
    )?;
    append_optional_os_frame(
      &mut identity,
      b"rustc-workspace-wrapper",
      self.rustc_workspace_wrapper_program(),
      source_root,
      "workspace rustc wrapper",
    )?;
    append_frame(&mut identity, b"host-target", self.host_target().as_bytes());
    Ok(identity)
  }
}

impl TargetIdentity {
  pub(crate) fn portable_snapshot_identity(&self, source_root: &Path) -> RailResult<Vec<u8>> {
    let mut identity = Vec::from(&b"target-snapshot-v1\0"[..]);
    match &self.specification {
      TargetSpecificationIdentity::BuiltIn(target) => append_frame(&mut identity, b"built-in", target.as_bytes()),
      TargetSpecificationIdentity::Custom(specification) => {
        append_frame(&mut identity, b"custom-name", specification.name().as_bytes());
        append_frame(
          &mut identity,
          b"custom-path",
          portable_path(source_root, specification.path(), "custom target specification")?.as_bytes(),
        );
        append_frame(&mut identity, b"custom-content", specification.digest().as_bytes());
      }
    }
    append_frame(&mut identity, b"host", &[u8::from(self.host)]);
    append_frame(&mut identity, b"build-target", &[u8::from(self.build_target)]);
    append_frame(&mut identity, b"analysis-target", &[u8::from(self.analysis_target)]);
    append_string_list(&mut identity, b"cfg", &self.cfg);
    append_optional_os_list(
      &mut identity,
      b"runner",
      self.runner.as_deref(),
      source_root,
      "target runner",
    )?;
    append_optional_os_frame(
      &mut identity,
      b"linker",
      self.linker.as_deref(),
      source_root,
      "target linker",
    )?;
    append_string_list(&mut identity, b"rustflags", &self.rustflags);
    append_string_list(&mut identity, b"rustdocflags", &self.rustdocflags);
    append_optional_string_list(
      &mut identity,
      b"host-artifact-rustflags",
      self.host_artifact_rustflags.as_deref(),
    );
    append_optional_string_list(
      &mut identity,
      b"host-artifact-rustdocflags",
      self.host_artifact_rustdocflags.as_deref(),
    );
    Ok(identity)
  }
}

fn portable_path(source_root: &Path, path: &Path, description: &str) -> RailResult<String> {
  if path.is_absolute()
    && let Ok(relative) = path.strip_prefix(source_root)
  {
    let relative = crate::source::RepositoryPath::new(relative)?;
    return Ok(format!("repository:{}", relative.as_str()));
  }
  path.to_str().map(|path| format!("external:{path}")).ok_or_else(|| {
    RailError::message(format!(
      "{description} path is not valid UTF-8 for portable snapshot identity"
    ))
  })
}

fn append_os_frame(
  output: &mut Vec<u8>,
  tag: &[u8],
  value: &OsStr,
  source_root: &Path,
  description: &str,
) -> RailResult<()> {
  let portable = portable_path(source_root, Path::new(value), description)?;
  append_frame(output, tag, portable.as_bytes());
  Ok(())
}

fn append_optional_os_frame(
  output: &mut Vec<u8>,
  tag: &[u8],
  value: Option<&OsStr>,
  source_root: &Path,
  description: &str,
) -> RailResult<()> {
  match value {
    Some(value) => append_os_frame(output, tag, value, source_root, description),
    None => {
      append_frame(output, tag, b"absent");
      Ok(())
    }
  }
}

fn append_optional_os_list(
  output: &mut Vec<u8>,
  tag: &[u8],
  values: Option<&[OsString]>,
  source_root: &Path,
  description: &str,
) -> RailResult<()> {
  let mut framed = Vec::new();
  match values {
    Some(values) => {
      append_frame(&mut framed, b"state", b"present");
      for value in values {
        append_os_frame(&mut framed, b"value", value, source_root, description)?;
      }
    }
    None => append_frame(&mut framed, b"state", b"absent"),
  }
  append_frame(output, tag, &framed);
  Ok(())
}

fn append_string_list(output: &mut Vec<u8>, tag: &[u8], values: &[String]) {
  let mut framed = Vec::new();
  for value in values {
    append_frame(&mut framed, b"value", value.as_bytes());
  }
  append_frame(output, tag, &framed);
}

fn append_optional_string_list(output: &mut Vec<u8>, tag: &[u8], values: Option<&[String]>) {
  let mut framed = Vec::new();
  match values {
    Some(values) => {
      append_frame(&mut framed, b"state", b"present");
      append_string_list(&mut framed, b"values", values);
    }
    None => append_frame(&mut framed, b"state", b"absent"),
  }
  append_frame(output, tag, &framed);
}

fn json_value_at<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
  path.iter().try_fold(value, |value, key| value.get(*key))
}

#[derive(Clone)]
pub(crate) struct ResolutionInputs {
  pub(crate) cargo_config: Arc<CargoConfigSnapshot>,
  pub(crate) toolchain: ToolchainIdentity,
  pub(crate) hermetic: bool,
}

impl ResolutionInputs {
  pub(crate) fn capture_hermetic(cargo_current_dir: &Path) -> RailResult<Self> {
    Self::from_hermetic_config(
      cargo_current_dir,
      Arc::new(CargoConfigSnapshot::capture(cargo_current_dir)?),
    )
  }

  pub(crate) fn from_hermetic_config(
    cargo_current_dir: &Path,
    cargo_config: Arc<CargoConfigSnapshot>,
  ) -> RailResult<Self> {
    Ok(Self {
      cargo_config,
      toolchain: ToolchainIdentity::capture_hermetic(cargo_current_dir)?,
      hermetic: true,
    })
  }
}

#[derive(Debug, Clone)]
struct CachedResolutionError {
  message: String,
  help: Option<String>,
}

impl CachedResolutionError {
  fn from_error(error: RailError) -> Self {
    Self {
      message: error.to_string(),
      help: error.help_message(),
    }
  }

  fn to_error(&self) -> RailError {
    self.help.as_ref().map_or_else(
      || RailError::message(self.message.clone()),
      |help| RailError::with_help(self.message.clone(), help.clone()),
    )
  }
}

type CachedView = Result<Arc<ResolutionView>, CachedResolutionError>;
type ViewCell = OnceLock<CachedView>;

/// Context-local lazy cache of exact Cargo resolution views.
pub(crate) struct ResolutionViews {
  workspace_root: PathBuf,
  cargo_current_dir: PathBuf,
  hermetic_cargo_home: Option<PathBuf>,
  base: Arc<ResolutionView>,
  cache: Mutex<FxHashMap<ResolutionViewKey, Arc<ViewCell>>>,
  cargo_config: OnceLock<Result<Arc<CargoConfigSnapshot>, CachedResolutionError>>,
  toolchain: OnceLock<Result<ToolchainIdentity, CachedResolutionError>>,
}

impl ResolutionViews {
  pub(crate) fn new(
    workspace_root: PathBuf,
    cargo_current_dir: PathBuf,
    metadata: Arc<Metadata>,
    graph: Arc<WorkspaceGraph>,
  ) -> Self {
    Self::new_inner(workspace_root, cargo_current_dir, metadata, graph, None, None)
  }

  pub(crate) fn new_with_inputs(
    workspace_root: PathBuf,
    cargo_current_dir: PathBuf,
    metadata: Arc<Metadata>,
    graph: Arc<WorkspaceGraph>,
    inputs: ResolutionInputs,
  ) -> Self {
    Self::new_inner(workspace_root, cargo_current_dir, metadata, graph, Some(inputs), None)
  }

  pub(crate) fn new_hermetic_with_inputs(
    workspace_root: PathBuf,
    cargo_current_dir: PathBuf,
    metadata: Arc<Metadata>,
    graph: Arc<WorkspaceGraph>,
    inputs: ResolutionInputs,
    cargo_home: PathBuf,
  ) -> Self {
    Self::new_inner(
      workspace_root,
      cargo_current_dir,
      metadata,
      graph,
      Some(inputs),
      Some(cargo_home),
    )
  }

  fn new_inner(
    workspace_root: PathBuf,
    cargo_current_dir: PathBuf,
    metadata: Arc<Metadata>,
    graph: Arc<WorkspaceGraph>,
    inputs: Option<ResolutionInputs>,
    hermetic_cargo_home: Option<PathBuf>,
  ) -> Self {
    let cargo_config = OnceLock::new();
    let toolchain = OnceLock::new();
    if let Some(inputs) = inputs {
      let _ = cargo_config.set(Ok(inputs.cargo_config));
      let _ = toolchain.set(Ok(inputs.toolchain));
    }
    Self {
      workspace_root,
      cargo_current_dir,
      hermetic_cargo_home,
      base: Arc::new(ResolutionView {
        request: ResolutionRequest::default(),
        metadata,
        graph,
      }),
      cache: Mutex::new(FxHashMap::default()),
      cargo_config,
      toolchain,
    }
  }

  pub(crate) fn capture_inputs(cargo_current_dir: &Path) -> RailResult<ResolutionInputs> {
    let cargo_config = Arc::new(CargoConfigSnapshot::capture(cargo_current_dir)?);
    let toolchain = ToolchainIdentity::capture(cargo_current_dir, &cargo_config)?;
    Ok(ResolutionInputs {
      cargo_config,
      toolchain,
      hermetic: false,
    })
  }

  pub(crate) fn inputs(&self) -> RailResult<ResolutionInputs> {
    let cargo_config = cached_value(self.cargo_config.get_or_init(|| {
      CargoConfigSnapshot::capture(&self.cargo_current_dir)
        .map(Arc::new)
        .map_err(CachedResolutionError::from_error)
    }))?;
    let toolchain = cached_value(self.toolchain.get_or_init(|| {
      ToolchainIdentity::capture(&self.cargo_current_dir, &cargo_config).map_err(CachedResolutionError::from_error)
    }))?;
    Ok(ResolutionInputs {
      cargo_config,
      toolchain,
      hermetic: self.hermetic_cargo_home.is_some(),
    })
  }

  pub(crate) fn cargo_current_dir(&self) -> &Path {
    &self.cargo_current_dir
  }

  /// Return one resolution view, loading a derived Cargo graph at most once.
  ///
  /// The native/workspace/default request returns the context's canonical view
  /// without reading Cargo configuration, querying tool versions, or invoking
  /// `cargo metadata` again. Derived requests fail before Cargo runs when any
  /// key input cannot be represented without secret material.
  pub(crate) fn view(&self, request: ResolutionRequest) -> RailResult<Arc<ResolutionView>> {
    if request.is_native_workspace_default() {
      return Ok(Arc::clone(&self.base));
    }

    let options = command_options(&request, self.base.metadata())?;
    let inputs = self.inputs()?;
    let key = ResolutionViewKey {
      request,
      toolchain: inputs.toolchain,
      cargo_config: inputs.cargo_config.digest,
      credential_sensitive: inputs.cargo_config.has_credential_capability(),
    };

    let cell = {
      let mut cache = self
        .cache
        .lock()
        .map_err(|_| RailError::message("Cargo resolution view cache lock poisoned"))?;
      Arc::clone(cache.entry(key.clone()).or_insert_with(|| Arc::new(ViewCell::new())))
    };
    let result = cell.get_or_init(|| self.load(&key, options).map_err(CachedResolutionError::from_error));
    cached_value(result)
  }

  fn load(&self, key: &ResolutionViewKey, options: ResolutionCommandOptions) -> RailResult<Arc<ResolutionView>> {
    self.validate_cargo_config_unchanged(key.cargo_config)?;
    let mut command = MetadataCommand::new();
    let cargo_program = self.hermetic_cargo_home.as_ref().map_or_else(
      || PathBuf::from(&key.toolchain.cargo_program),
      |_| toolchain_program(key.toolchain.rustc_sysroot(), "cargo"),
    );
    command
      .cargo_path(cargo_program)
      .current_dir(&self.cargo_current_dir)
      .manifest_path(self.workspace_root.join("Cargo.toml"));
    if options.no_default_features {
      command.features(CargoOpt::NoDefaultFeatures);
    }
    if options.all_features {
      command.features(CargoOpt::AllFeatures);
    }
    if !options.features.is_empty() {
      command.features(CargoOpt::SomeFeatures(options.features));
    }
    let mut other_options = vec!["--locked".to_string()];
    if self.hermetic_cargo_home.is_some() {
      other_options.push("--offline".to_string());
    }
    if let Some(target) = key.request.target_filter() {
      other_options.extend(["--filter-platform".to_string(), target.to_string()]);
    }
    command.other_options(other_options);
    if let Some(cargo_home) = &self.hermetic_cargo_home {
      for (name, value) in self.inputs()?.cargo_config.non_secret_acquisition_environment() {
        command.env(name, value);
      }
      command
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_CACHE_RUSTC_INFO", "0")
        .env("RUSTC", toolchain_program(key.toolchain.rustc_sysroot(), "rustc"))
        .env("RUSTDOC", toolchain_program(key.toolchain.rustc_sysroot(), "rustdoc"));
    }

    crate::instrumentation::record_cargo_metadata_load(key.request.target_filter().is_some());
    let metadata = command.exec();
    self.validate_cargo_config_unchanged(key.cargo_config)?;
    let metadata =
      Arc::new(metadata.map_err(|error| resolution_load_error(&key.request, error, key.credential_sensitive))?);
    if metadata.resolve.is_none() {
      return Err(RailError::message(
        "Cargo returned no resolve graph for a full resolution view",
      ));
    }
    let graph = Arc::new(WorkspaceGraph::from_metadata(&metadata)?);
    Ok(Arc::new(ResolutionView {
      request: key.request.clone(),
      metadata,
      graph,
    }))
  }

  fn validate_cargo_config_unchanged(&self, expected: ContentDigest) -> RailResult<()> {
    let current = CargoConfigSnapshot::capture(&self.cargo_current_dir)?;
    if current.digest == expected {
      return Ok(());
    }
    Err(RailError::with_help(
      "Cargo configuration changed while loading an exact resolution view",
      "retry after Cargo configuration and environment changes have stopped",
    ))
  }
}

fn toolchain_program(sysroot: &Path, name: &str) -> PathBuf {
  #[cfg(windows)]
  let name = format!("{name}.exe");
  sysroot.join("bin").join(name)
}

fn cached_value<T: Clone>(value: &Result<T, CachedResolutionError>) -> RailResult<T> {
  value.clone().map_err(|error| error.to_error())
}

#[derive(Default)]
struct ResolutionCommandOptions {
  no_default_features: bool,
  all_features: bool,
  features: Vec<String>,
}

fn command_options(request: &ResolutionRequest, metadata: &Metadata) -> RailResult<ResolutionCommandOptions> {
  let packages = selected_packages(request, metadata)?;
  let package_by_id: FxHashMap<&PackageId, &Package> = packages.iter().map(|package| (&package.id, *package)).collect();
  let mut package_names = FxHashMap::default();
  for package in metadata.workspace_packages() {
    *package_names.entry(package.name.as_str()).or_insert(0usize) += 1;
  }
  let mut options = ResolutionCommandOptions::default();
  let mut features = BTreeSet::new();

  if let ResolutionFeatures::Selected(selected) = request.features() {
    for (package_id, selected_features) in selected {
      if matches!(request.packages(), ResolutionPackages::Selected(packages) if !packages.contains(package_id)) {
        return Err(RailError::message(format!(
          "Feature selection names package '{}' outside the selected package set",
          package_id
        )));
      }
      let package = package_by_id
        .get(package_id)
        .copied()
        .ok_or_else(|| selected_package_error(package_id))?;
      for feature in selected_features {
        if feature.is_empty() {
          return Err(RailError::message(format!(
            "Feature selection for package '{}' contains an empty feature name",
            package_id
          )));
        }
        if !package.features.contains_key(feature) {
          return Err(RailError::message(format!(
            "Package '{}' does not declare feature '{}'",
            package_id, feature
          )));
        }
      }
    }
  }

  match (request.packages(), request.features()) {
    (ResolutionPackages::Workspace, ResolutionFeatures::Default) => {}
    (ResolutionPackages::Workspace, ResolutionFeatures::NoDefaultFeatures) => {
      options.no_default_features = true;
    }
    (ResolutionPackages::Workspace, ResolutionFeatures::AllFeatures) => {
      options.all_features = true;
    }
    (ResolutionPackages::Selected(_), ResolutionFeatures::Default) => {
      options.no_default_features = true;
      for package in &packages {
        if package.features.contains_key("default") {
          features.insert(package_feature(package, "default", &package_names)?);
        }
      }
    }
    (ResolutionPackages::Selected(_), ResolutionFeatures::NoDefaultFeatures) => {
      options.no_default_features = true;
    }
    (ResolutionPackages::Selected(_), ResolutionFeatures::AllFeatures) => {
      options.no_default_features = true;
      for package in &packages {
        for feature in package.features.keys() {
          features.insert(package_feature(package, feature, &package_names)?);
        }
      }
    }
    (_, ResolutionFeatures::Selected(selected)) => {
      options.no_default_features = true;
      for (package_id, package_features) in selected {
        let package = package_by_id
          .get(package_id)
          .copied()
          .ok_or_else(|| selected_package_error(package_id))?;
        for feature in package_features {
          features.insert(package_feature(package, feature, &package_names)?);
        }
      }
    }
  }

  options.features = features.into_iter().collect();
  Ok(options)
}

fn selected_packages<'a>(request: &ResolutionRequest, metadata: &'a Metadata) -> RailResult<Vec<&'a Package>> {
  let workspace_ids: HashSet<&PackageId> = metadata.workspace_members.iter().collect();
  let workspace_packages: FxHashMap<&PackageId, &Package> = metadata
    .packages
    .iter()
    .filter(|package| workspace_ids.contains(&package.id))
    .map(|package| (&package.id, package))
    .collect();
  match request.packages() {
    ResolutionPackages::Workspace => Ok(workspace_packages.into_values().collect()),
    ResolutionPackages::Selected(selected) => selected
      .iter()
      .map(|package_id| {
        workspace_packages
          .get(package_id)
          .copied()
          .ok_or_else(|| selected_package_error(package_id))
      })
      .collect(),
  }
}

fn selected_package_error(package_id: &PackageId) -> RailError {
  RailError::message(format!(
    "Package '{}' is not an exact workspace member in the canonical resolution",
    package_id
  ))
}

fn package_feature(package: &Package, feature: &str, package_names: &FxHashMap<&str, usize>) -> RailResult<String> {
  if package_names.get(package.name.as_str()).copied().unwrap_or_default() != 1 {
    return Err(RailError::with_help(
      format!(
        "Cannot encode exact feature selection for ambiguous workspace package name '{}'",
        package.name
      ),
      format!(
        "Cargo's feature CLI cannot distinguish package ID '{}' by name",
        package.id
      ),
    ));
  }
  Ok(format!("{}/{}", package.name, feature))
}

fn resolution_load_error(
  request: &ResolutionRequest,
  error: cargo_metadata::Error,
  credential_sensitive: bool,
) -> RailError {
  let message = error.to_string();
  if let Some(target) = request.target_filter()
    && (message.contains("error[E0463]")
      || message.contains("can't find crate")
      || message.contains("target may not be installed"))
  {
    return RailError::with_help(
      format!("Target '{target}' is not installed on this machine"),
      format!("Install the target with: rustup target add {target}"),
    );
  }
  if credential_sensitive {
    return RailError::with_help(
      "Failed to load exact Cargo resolution while credential capabilities were active",
      "run cargo metadata directly for provider diagnostics; cargo-rail suppresses credential-provider output",
    );
  }
  request.target_filter().map_or_else(
    || {
      RailError::with_help(
        "Failed to load exact Cargo resolution",
        format!("Cargo metadata error: {message}"),
      )
    },
    |target| {
      RailError::with_help(
        format!("Failed to load Cargo resolution for target '{target}'"),
        format!("Cargo metadata error: {message}"),
      )
    },
  )
}

impl ToolchainIdentity {
  fn capture(cargo_current_dir: &Path, cargo_config: &CargoConfigSnapshot) -> RailResult<Self> {
    let cargo_program = selected_program(cargo_config, cargo_current_dir, &["CARGO"], &[], Some("cargo"), "Cargo")?
      .ok_or_else(|| RailError::message("Cargo program selection is empty"))?;
    let rustc_program = selected_program(
      cargo_config,
      cargo_current_dir,
      RUSTC_ENV_PRECEDENCE,
      &["build", "rustc"],
      Some("rustc"),
      "rustc",
    )?
    .ok_or_else(|| RailError::message("rustc program selection is empty"))?;
    let rustdoc_program = selected_program(
      cargo_config,
      cargo_current_dir,
      RUSTDOC_ENV_PRECEDENCE,
      &["build", "rustdoc"],
      Some("rustdoc"),
      "rustdoc",
    )?
    .ok_or_else(|| RailError::message("rustdoc program selection is empty"))?;
    let rustc_wrapper_program = selected_program(
      cargo_config,
      cargo_current_dir,
      RUSTC_WRAPPER_ENV_PRECEDENCE,
      &["build", "rustc-wrapper"],
      None,
      "rustc wrapper",
    )?;
    let rustc_workspace_wrapper_program = selected_program(
      cargo_config,
      cargo_current_dir,
      RUSTC_WORKSPACE_WRAPPER_ENV_PRECEDENCE,
      &["build", "rustc-workspace-wrapper"],
      None,
      "workspace rustc wrapper",
    )?;
    reject_recursive_cargo_rail_wrappers(
      cargo_current_dir,
      rustc_wrapper_program.as_deref(),
      rustc_workspace_wrapper_program.as_deref(),
    )?;
    let rustc_verbose_version = wrapped_rustc_identity(
      &cargo_program,
      &rustc_program,
      rustc_wrapper_program.as_deref(),
      rustc_workspace_wrapper_program.as_deref(),
      cargo_current_dir,
      cargo_config,
    )?;
    let rustc_sysroot = PathBuf::from(wrapped_rustc_query(
      &cargo_program,
      &rustc_program,
      rustc_wrapper_program.as_deref(),
      rustc_workspace_wrapper_program.as_deref(),
      cargo_current_dir,
      cargo_config,
      "--print=sysroot",
      "wrapped 'rustc --print=sysroot'",
    )?);
    let host_target = parse_rustc_host(&rustc_verbose_version)?;
    Ok(Self {
      cargo_verbose_version: command_identity(&cargo_program, "-Vv", cargo_current_dir, None)?,
      rustdoc_verbose_version: command_identity(&rustdoc_program, "-vV", cargo_current_dir, Some(cargo_config))?,
      cargo_program,
      rustc_program,
      rustc_verbose_version,
      rustdoc_program,
      rustc_wrapper_program,
      rustc_workspace_wrapper_program,
      host_target,
      rustc_sysroot,
    })
  }
}

fn reject_recursive_cargo_rail_wrappers(
  current_dir: &Path,
  rustc_wrapper: Option<&OsStr>,
  workspace_wrapper: Option<&OsStr>,
) -> RailResult<()> {
  let current_executable = std::env::current_exe()
    .map_err(|error| RailError::message(format!("failed to locate cargo-rail executable: {error}")))?;
  let current_executable = fs::canonicalize(&current_executable).map_err(|error| {
    RailError::message(format!(
      "failed to resolve cargo-rail executable '{}': {error}",
      current_executable.display()
    ))
  })?;
  let recursive = [rustc_wrapper, workspace_wrapper]
    .into_iter()
    .flatten()
    .filter_map(|wrapper| resolve_executable_program(wrapper, current_dir).ok())
    .filter_map(|wrapper| fs::canonicalize(wrapper).ok())
    .any(|wrapper| wrapper == current_executable);
  if recursive {
    return Err(RailError::with_help(
      "recursive cargo-rail rustc wrapper configuration",
      "remove cargo-rail from RUSTC_WRAPPER and RUSTC_WORKSPACE_WRAPPER; wrapper injection is automatic",
    ));
  }
  Ok(())
}

fn parse_rustc_host(verbose_version: &str) -> RailResult<String> {
  let mut hosts = verbose_version.lines().filter_map(|line| line.strip_prefix("host: "));
  let host = hosts
    .next()
    .filter(|host| !host.is_empty())
    .ok_or_else(|| RailError::message("rustc -vV did not report a non-empty host target"))?;
  if hosts.next().is_some() {
    return Err(RailError::message("rustc -vV reported more than one host target"));
  }
  Ok(host.to_string())
}

#[derive(Clone, Copy)]
enum ProgramOrigin<'a> {
  Environment,
  Config(&'a Path),
  Default,
}

fn selected_program(
  cargo_config: &CargoConfigSnapshot,
  cargo_current_dir: &Path,
  environment_names: &[&str],
  config_path: &[&str],
  default: Option<&str>,
  description: &str,
) -> RailResult<Option<OsString>> {
  for name in environment_names {
    if let Some(value) = cargo_config.environment.get(*name) {
      if value.is_empty() && default.is_none() {
        return Ok(None);
      }
      return resolve_program(value, ProgramOrigin::Environment, cargo_current_dir, description).map(Some);
    }
  }
  if !config_path.is_empty()
    && let Some((value, source)) = cargo_config.config_string_with_source(config_path)?
  {
    return resolve_program(
      value,
      ProgramOrigin::Config(source.path()),
      cargo_current_dir,
      description,
    )
    .map(Some);
  }
  default
    .map(|value| resolve_program(value, ProgramOrigin::Default, cargo_current_dir, description))
    .transpose()
}

fn resolve_program(
  value: &str,
  origin: ProgramOrigin<'_>,
  cargo_current_dir: &Path,
  description: &str,
) -> RailResult<OsString> {
  if value.is_empty() {
    return Err(RailError::message(format!(
      "Cargo selected an empty {description} program"
    )));
  }
  let path = Path::new(value);
  if !path.is_absolute() && !value.contains(['/', '\\']) {
    return Ok(OsString::from(value));
  }
  let resolved = if path.is_absolute() {
    path.to_path_buf()
  } else {
    match origin {
      ProgramOrigin::Environment | ProgramOrigin::Default => cargo_current_dir.join(path),
      ProgramOrigin::Config(config_path) => config_relative_root(config_path)?.join(path),
    }
  };
  Ok(resolved.into_os_string())
}

fn config_relative_root(config_path: &Path) -> RailResult<&Path> {
  config_path.parent().and_then(Path::parent).ok_or_else(|| {
    RailError::message(format!(
      "Cargo configuration '{}' has no relative-path base",
      config_path.display()
    ))
  })
}

#[derive(Clone)]
struct SelectedTarget {
  value: String,
  origin: TargetOrigin,
  host: bool,
  build_target: bool,
  analysis_target: bool,
}

#[derive(Clone)]
enum TargetOrigin {
  CurrentDirectory,
  Config(PathBuf),
}

struct ResolvedTarget {
  argument: OsString,
  config_name: String,
  specification: TargetSpecificationIdentity,
  host: bool,
  build_target: bool,
  analysis_target: bool,
}

pub(crate) fn capture_target_identities(
  cargo_current_dir: &Path,
  analysis_targets: &[String],
  inputs: &ResolutionInputs,
) -> RailResult<Vec<TargetIdentity>> {
  if inputs.cargo_config.effective_file_settings.get("host").is_some()
    || json_value_at(
      &inputs.cargo_config.effective_file_settings,
      &["unstable", "target-applies-to-host"],
    )
    .is_some()
  {
    return Err(RailError::with_help(
      "workspace snapshot target identity does not support Cargo's unstable host configuration",
      "remove [host] and unstable.target-applies-to-host, or use stable target configuration before snapshot capture",
    ));
  }
  let built_in_targets = rustc_output_lines(
    &inputs.toolchain,
    &inputs.cargo_config,
    cargo_current_dir,
    &[OsString::from("--print"), OsString::from("target-list")],
    &[],
    "target list",
  )?
  .into_iter()
  .collect::<BTreeSet<_>>();
  if !built_in_targets.contains(inputs.toolchain.host_target()) {
    return Err(RailError::message(format!(
      "rustc host target '{}' is absent from rustc --print target-list",
      inputs.toolchain.host_target()
    )));
  }

  let mut selected = inputs
    .cargo_config
    .selected_build_targets(inputs.toolchain.host_target())?;
  let has_explicit_build_targets = !selected.is_empty();
  if !has_explicit_build_targets {
    selected.push(SelectedTarget {
      value: inputs.toolchain.host_target().to_string(),
      origin: TargetOrigin::CurrentDirectory,
      host: true,
      build_target: true,
      analysis_target: false,
    });
  }
  for target in analysis_targets {
    selected.push(SelectedTarget {
      value: target.clone(),
      origin: TargetOrigin::CurrentDirectory,
      host: target == inputs.toolchain.host_target(),
      build_target: false,
      analysis_target: true,
    });
  }
  if has_explicit_build_targets
    && !selected
      .iter()
      .any(|target| target.value == inputs.toolchain.host_target())
  {
    selected.push(SelectedTarget {
      value: inputs.toolchain.host_target().to_string(),
      origin: TargetOrigin::CurrentDirectory,
      host: true,
      build_target: false,
      analysis_target: false,
    });
  }

  let mut resolved = BTreeMap::<(u8, OsString), ResolvedTarget>::new();
  for target in selected {
    let target = resolve_target(
      target,
      cargo_current_dir,
      &built_in_targets,
      &inputs.toolchain,
      &inputs.cargo_config,
    )?;
    let key = match &target.specification {
      TargetSpecificationIdentity::BuiltIn(triple) => (0, OsString::from(triple)),
      TargetSpecificationIdentity::Custom(specification) => (1, specification.path().as_os_str().to_owned()),
    };
    if let Some(existing) = resolved.get_mut(&key) {
      existing.host |= target.host;
      existing.build_target |= target.build_target;
      existing.analysis_target |= target.analysis_target;
    } else {
      resolved.insert(key, target);
    }
  }

  resolved
    .into_values()
    .map(|target| capture_target_identity(cargo_current_dir, target, inputs, has_explicit_build_targets))
    .collect()
}

fn resolve_target(
  mut selected: SelectedTarget,
  cargo_current_dir: &Path,
  built_in_targets: &BTreeSet<String>,
  toolchain: &ToolchainIdentity,
  cargo_config: &CargoConfigSnapshot,
) -> RailResult<ResolvedTarget> {
  if selected.value == "host-tuple" {
    selected.value = toolchain.host_target().to_string();
    selected.host = true;
  }
  if built_in_targets.contains(&selected.value) {
    return Ok(ResolvedTarget {
      argument: OsString::from(&selected.value),
      config_name: selected.value.clone(),
      specification: TargetSpecificationIdentity::BuiltIn(selected.value),
      host: selected.host,
      build_target: selected.build_target,
      analysis_target: selected.analysis_target,
    });
  }

  let selected_path = Path::new(&selected.value);
  let selected_path = if selected_path.is_absolute() {
    selected_path.to_path_buf()
  } else {
    match &selected.origin {
      TargetOrigin::CurrentDirectory => cargo_current_dir.join(selected_path),
      TargetOrigin::Config(config_path) => config_relative_root(config_path)?.join(selected_path),
    }
  };
  let path = if selected_path.is_file() {
    selected_path
  } else if let Some(path) = custom_target_lookup_path(&selected.value, cargo_current_dir, cargo_config)? {
    path
  } else {
    return Err(RailError::with_help(
      format!(
        "rustc cannot resolve selected target '{}' to a built-in or custom target specification",
        selected.value
      ),
      "use a target reported by rustc --print target-list, an existing target file, or a resolvable RUST_TARGET_PATH entry",
    ));
  };
  let specification = capture_custom_target(&path)?;
  let argument = specification.path().as_os_str().to_owned();
  Ok(ResolvedTarget {
    argument,
    config_name: specification.name().to_string(),
    specification: TargetSpecificationIdentity::Custom(specification),
    host: false,
    build_target: selected.build_target,
    analysis_target: selected.analysis_target,
  })
}

fn custom_target_lookup_path(
  name: &str,
  cargo_current_dir: &Path,
  cargo_config: &CargoConfigSnapshot,
) -> RailResult<Option<PathBuf>> {
  if let Some(search_path) = cargo_config.effective_environment_value("RUST_TARGET_PATH")? {
    for directory in std::env::split_paths(&search_path) {
      let directory = if directory.is_absolute() {
        directory
      } else {
        cargo_current_dir.join(directory)
      };
      let candidate = directory.join(format!("{name}.json"));
      if candidate.is_file() {
        return Ok(Some(candidate));
      }
    }
  }
  Ok(None)
}

fn capture_custom_target(path: &Path) -> RailResult<CustomTargetSpecification> {
  if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
    return Err(RailError::with_help(
      format!("custom target specification '{}' is a symbolic link", path.display()),
      "replace it with a regular JSON file before snapshot capture",
    ));
  }
  let canonical = canonicalize_existing(path).map_err(|error| {
    RailError::message(format!(
      "failed to resolve custom target specification '{}': {error}",
      path.display()
    ))
  })?;
  let name = canonical
    .file_stem()
    .and_then(OsStr::to_str)
    .filter(|name| !name.is_empty())
    .ok_or_else(|| {
      RailError::message(format!(
        "custom target specification '{}' has no UTF-8 filename stem",
        canonical.display()
      ))
    })?
    .to_string();
  let before = fs::metadata(&canonical).map_err(|error| {
    RailError::message(format!(
      "failed to inspect custom target specification '{}': {error}",
      canonical.display()
    ))
  })?;
  if !before.is_file() {
    return Err(RailError::message(format!(
      "custom target specification '{}' is not a regular file",
      canonical.display()
    )));
  }
  let bytes = fs::read(&canonical).map_err(|error| {
    RailError::message(format!(
      "failed to read custom target specification '{}': {error}",
      canonical.display()
    ))
  })?;
  let repeated = fs::read(&canonical).map_err(|error| {
    RailError::message(format!(
      "failed to reread custom target specification '{}': {error}",
      canonical.display()
    ))
  })?;
  let after = fs::metadata(&canonical).map_err(|error| {
    RailError::message(format!(
      "failed to reinspect custom target specification '{}': {error}",
      canonical.display()
    ))
  })?;
  if bytes != repeated || file_metadata_changed(&before, &after) {
    return Err(RailError::with_help(
      format!(
        "custom target specification '{}' changed during capture",
        canonical.display()
      ),
      "retry after target configuration changes have stopped",
    ));
  }
  let digest = ContentDigest::sha256(&bytes);
  Ok(CustomTargetSpecification {
    name,
    path: canonical,
    bytes: bytes.into(),
    digest,
  })
}

fn file_metadata_changed(left: &fs::Metadata, right: &fs::Metadata) -> bool {
  left.file_type() != right.file_type()
    || left.len() != right.len()
    || left.modified().ok() != right.modified().ok()
    || left.permissions().readonly() != right.permissions().readonly()
}

fn capture_target_identity(
  workspace_root: &Path,
  target: ResolvedTarget,
  inputs: &ResolutionInputs,
  has_explicit_build_targets: bool,
) -> RailResult<TargetIdentity> {
  let mut rustflags = effective_flags(&inputs.cargo_config, &target.config_name, None, CompilerFlags::Rust)?;
  for turn in 0..2 {
    let cfg = rustc_target_cfg(
      &inputs.toolchain,
      &inputs.cargo_config,
      workspace_root,
      &target.argument,
      &rustflags,
    )?;
    let cfg_set = TargetCfgSet::from_rustc_output(&cfg.join("\n"));
    let new_rustflags = effective_flags(
      &inputs.cargo_config,
      &target.config_name,
      Some(&cfg_set),
      CompilerFlags::Rust,
    )?;
    if new_rustflags == rustflags {
      let runner = selected_target_runner(&inputs.cargo_config, &target.config_name, &cfg_set, workspace_root)?;
      let linker = selected_target_linker(&inputs.cargo_config, &target.config_name, &cfg_set, workspace_root)?;
      let rustdocflags = effective_flags(
        &inputs.cargo_config,
        &target.config_name,
        Some(&cfg_set),
        CompilerFlags::Rustdoc,
      )?;
      if let TargetSpecificationIdentity::Custom(specification) = &target.specification {
        validate_custom_target_unchanged(specification)?;
      }
      let host_artifact_rustflags = target.host.then(|| {
        if has_explicit_build_targets {
          Vec::new()
        } else {
          rustflags.clone()
        }
      });
      let host_artifact_rustdocflags = target.host.then(|| {
        if has_explicit_build_targets {
          Vec::new()
        } else {
          rustdocflags.clone()
        }
      });
      return Ok(TargetIdentity {
        specification: target.specification,
        host: target.host,
        build_target: target.build_target,
        analysis_target: target.analysis_target,
        cfg,
        runner,
        linker,
        rustflags,
        rustdocflags,
        host_artifact_rustflags,
        host_artifact_rustdocflags,
      });
    }
    if turn == 0 {
      rustflags = new_rustflags;
    }
  }
  Err(RailError::with_help(
    format!(
      "target '{}' has a non-convergent dependency between Cargo target cfg and rustflags",
      target.config_name
    ),
    "remove cfg-dependent flags that add or remove the cfg predicates selecting those same flags",
  ))
}

fn validate_custom_target_unchanged(specification: &CustomTargetSpecification) -> RailResult<()> {
  let current = fs::read(specification.path()).map_err(|error| {
    RailError::message(format!(
      "failed to revalidate custom target specification '{}': {error}",
      specification.path().display()
    ))
  })?;
  if current.as_slice() != specification.bytes() {
    return Err(RailError::with_help(
      format!(
        "custom target specification '{}' changed while resolving target identity",
        specification.path().display()
      ),
      "retry after target configuration changes have stopped",
    ));
  }
  Ok(())
}

fn rustc_target_cfg(
  toolchain: &ToolchainIdentity,
  cargo_config: &CargoConfigSnapshot,
  workspace_root: &Path,
  target: &OsStr,
  rustflags: &[String],
) -> RailResult<Vec<String>> {
  let arguments = [
    OsString::from("--print"),
    OsString::from("cfg"),
    OsString::from("--target"),
    target.to_owned(),
  ];
  let mut cfg = rustc_output_lines(
    toolchain,
    cargo_config,
    workspace_root,
    &arguments,
    rustflags,
    "target cfg",
  )?;
  cfg.retain(|line| line != "proc_macro");
  cfg.sort_unstable();
  cfg.dedup();
  if cfg.is_empty() {
    return Err(RailError::message(format!(
      "rustc returned an empty cfg set for target '{}'",
      target.to_string_lossy()
    )));
  }
  Ok(cfg)
}

fn rustc_output_lines(
  toolchain: &ToolchainIdentity,
  cargo_config: &CargoConfigSnapshot,
  current_dir: &Path,
  arguments: &[OsString],
  rustflags: &[String],
  description: &str,
) -> RailResult<Vec<String>> {
  let mut command = rustc_command(toolchain);
  apply_cargo_environment(&mut command, cargo_config)?;
  disable_implicit_toolchain_install(&mut command);
  let output = command
    .current_dir(current_dir)
    .env("CARGO", toolchain.cargo_program())
    .env_remove("RUSTC_LOG")
    .args(arguments)
    .args(rustflags)
    .output()
    .map_err(|error| {
      RailError::with_help(
        format!("failed to query rustc {description}: {error}"),
        "ensure the selected rustc and compiler wrappers are available and executable",
      )
    })?;
  if !output.status.success() {
    return Err(RailError::message(format!(
      "rustc {description} query failed with status {}",
      output.status
    )));
  }
  let stdout = String::from_utf8(output.stdout)
    .map_err(|_| RailError::message(format!("rustc {description} query returned non-UTF-8 output")))?;
  Ok(stdout.replace("\r\n", "\n").lines().map(str::to_string).collect())
}

fn rustc_command(toolchain: &ToolchainIdentity) -> Command {
  configured_rustc_command(
    toolchain.rustc_program(),
    toolchain.rustc_wrapper_program(),
    toolchain.rustc_workspace_wrapper_program(),
  )
}

fn configured_rustc_command(
  rustc_program: &OsStr,
  rustc_wrapper_program: Option<&OsStr>,
  rustc_workspace_wrapper_program: Option<&OsStr>,
) -> Command {
  crate::compiler::wrapper::rustc_command(rustc_program, rustc_wrapper_program, rustc_workspace_wrapper_program)
}

fn apply_cargo_environment(command: &mut Command, cargo_config: &CargoConfigSnapshot) -> RailResult<()> {
  let Some(environment) = cargo_config.effective_file_settings.get("env") else {
    return Ok(());
  };
  let environment = environment
    .as_object()
    .ok_or_else(|| RailError::message("Cargo configuration key 'env' must be a table"))?;
  for (name, setting) in environment {
    if is_credential_capability(setting) {
      return Err(RailError::with_help(
        format!("Cargo target identity cannot apply redacted configuration value 'env.{name}'"),
        "move secret environment values out of Cargo configuration before snapshot capture",
      ));
    }
    let (value, force, relative, value_path) = cargo_environment_setting(name, setting)?;

    if !force && let Some(captured) = cargo_config.environment.get(name) {
      if is_credential_environment_marker(captured) {
        if std::env::var_os(name).is_none() {
          return Err(RailError::message(format!(
            "redacted Cargo environment variable '{name}' disappeared during target identity capture"
          )));
        }
      } else {
        command.env(name, captured);
      }
      continue;
    }
    if is_credential_environment_marker(value) {
      return Err(RailError::with_help(
        format!("Cargo target identity cannot apply redacted configuration value 'env.{name}'"),
        "move secret environment values out of Cargo configuration before snapshot capture",
      ));
    }
    if relative {
      let (_, source) = cargo_config
        .config_string_with_source(&value_path)?
        .ok_or_else(|| RailError::message(format!("Cargo configuration key 'env.{name}' has no path provenance")))?;
      command.env(name, config_relative_root(source.path())?.join(value));
    } else {
      command.env(name, value);
    }
  }
  Ok(())
}

fn cargo_environment_setting<'a>(
  name: &'a str,
  setting: &'a JsonValue,
) -> RailResult<(&'a str, bool, bool, Vec<&'a str>)> {
  if let Some(value) = setting.as_str() {
    return Ok((value, false, false, vec!["env", name]));
  }
  let table = setting.as_object().ok_or_else(|| {
    RailError::message(format!(
      "Cargo configuration key 'env.{name}' must be a string or table"
    ))
  })?;
  let value = table
    .get("value")
    .and_then(JsonValue::as_str)
    .ok_or_else(|| RailError::message(format!("Cargo configuration key 'env.{name}.value' must be a string")))?;
  let force = optional_config_bool(table, "force", &format!("env.{name}.force"))?;
  let relative = optional_config_bool(table, "relative", &format!("env.{name}.relative"))?;
  Ok((value, force, relative, vec!["env", name, "value"]))
}

fn optional_config_bool(table: &JsonMap<String, JsonValue>, key: &str, path: &str) -> RailResult<bool> {
  table.get(key).map_or(Ok(false), |value| {
    value
      .as_bool()
      .ok_or_else(|| RailError::message(format!("Cargo configuration key '{path}' must be a boolean")))
  })
}

#[derive(Clone, Copy)]
enum CompilerFlags {
  Rust,
  Rustdoc,
}

impl CompilerFlags {
  fn key(self) -> &'static str {
    match self {
      Self::Rust => "rustflags",
      Self::Rustdoc => "rustdocflags",
    }
  }

  fn environment(self) -> (&'static str, &'static str, &'static str) {
    match self {
      Self::Rust => ("CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS"),
      Self::Rustdoc => ("CARGO_ENCODED_RUSTDOCFLAGS", "RUSTDOCFLAGS", "CARGO_BUILD_RUSTDOCFLAGS"),
    }
  }
}

fn effective_flags(
  cargo_config: &CargoConfigSnapshot,
  target: &str,
  cfg_set: Option<&TargetCfgSet>,
  kind: CompilerFlags,
) -> RailResult<Vec<String>> {
  let (encoded_name, plain_name, build_name) = kind.environment();
  if let Some(encoded) = cargo_config.environment.get(encoded_name) {
    return Ok(if encoded.is_empty() {
      Vec::new()
    } else {
      encoded.split('\u{1f}').map(str::to_string).collect()
    });
  }
  if let Some(plain) = cargo_config.environment.get(plain_name) {
    return Ok(split_space_arguments(plain));
  }

  let mut target_flags = target_field_string_list(cargo_config, target, kind.key())?.unwrap_or_default();
  if let Some(cfg_set) = cfg_set {
    for (constraint, table) in target_cfg_tables(cargo_config)? {
      if cargo_target_constraint_matches(constraint, target, cfg_set)?
        && let Some(value) = table.get(kind.key())
      {
        target_flags.extend(string_list(value, &format!("target.{constraint}.{}", kind.key()))?);
      }
    }
  }
  if !target_flags.is_empty() {
    return Ok(target_flags);
  }
  if let Some(flags) = cargo_config.environment.get(build_name) {
    return Ok(split_space_arguments(flags));
  }
  json_value_at(&cargo_config.effective_file_settings, &["build", kind.key()])
    .map(|value| string_list(value, &format!("build.{}", kind.key())))
    .transpose()
    .map(Option::unwrap_or_default)
}

fn target_field_string_list(
  cargo_config: &CargoConfigSnapshot,
  target: &str,
  field: &str,
) -> RailResult<Option<Vec<String>>> {
  let environment_name = target_environment_name(target, field);
  if let Some(value) = cargo_config.environment.get(&environment_name) {
    return Ok(Some(split_space_arguments(value)));
  }
  json_value_at(&cargo_config.effective_file_settings, &["target", target, field])
    .map(|value| string_list(value, &format!("target.{target}.{field}")))
    .transpose()
}

fn target_cfg_tables(cargo_config: &CargoConfigSnapshot) -> RailResult<Vec<(&str, &JsonMap<String, JsonValue>)>> {
  let Some(targets) = cargo_config.effective_file_settings.get("target") else {
    return Ok(Vec::new());
  };
  let targets = targets
    .as_object()
    .ok_or_else(|| RailError::message("Cargo configuration key 'target' must be a table"))?;
  targets
    .iter()
    .filter(|(key, _)| key.starts_with("cfg("))
    .map(|(key, value)| {
      value
        .as_object()
        .map(|table| (key.as_str(), table))
        .ok_or_else(|| RailError::message(format!("Cargo configuration key 'target.{key}' must be a table")))
    })
    .collect()
}

fn selected_target_runner(
  cargo_config: &CargoConfigSnapshot,
  target: &str,
  cfg_set: &TargetCfgSet,
  cargo_current_dir: &Path,
) -> RailResult<Option<Vec<OsString>>> {
  let environment_name = target_environment_name(target, "runner");
  if let Some(value) = cargo_config.environment.get(&environment_name) {
    let arguments = split_space_arguments(value);
    return resolve_runner(arguments, ProgramOrigin::Environment, cargo_current_dir, target);
  }
  if let Some((value, source)) = cargo_config.target_value_with_source(target, "runner")? {
    let arguments = string_list(value, &format!("target.{target}.runner"))?;
    return resolve_runner(
      arguments,
      ProgramOrigin::Config(source.path()),
      cargo_current_dir,
      target,
    );
  }

  let mut matches = Vec::new();
  for (constraint, table) in target_cfg_tables(cargo_config)? {
    if cargo_target_constraint_matches(constraint, target, cfg_set)? && table.get("runner").is_some() {
      matches.push(constraint);
    }
  }
  if matches.len() > 1 {
    return Err(RailError::message(format!(
      "multiple Cargo target cfg runner entries match target '{target}': {}",
      matches.join(", ")
    )));
  }
  let Some(constraint) = matches.first() else {
    return Ok(None);
  };
  let (value, source) = cargo_config
    .target_value_with_source(constraint, "runner")?
    .ok_or_else(|| RailError::message(format!("Cargo target cfg runner '{constraint}' lost source provenance")))?;
  resolve_runner(
    string_list(value, &format!("target.{constraint}.runner"))?,
    ProgramOrigin::Config(source.path()),
    cargo_current_dir,
    target,
  )
}

fn resolve_runner(
  mut arguments: Vec<String>,
  origin: ProgramOrigin<'_>,
  cargo_current_dir: &Path,
  target: &str,
) -> RailResult<Option<Vec<OsString>>> {
  if arguments.is_empty() {
    return Ok(None);
  }
  let program = resolve_program(&arguments.remove(0), origin, cargo_current_dir, "target runner")?;
  let mut resolved = Vec::with_capacity(arguments.len() + 1);
  resolved.push(program);
  resolved.extend(arguments.into_iter().map(OsString::from));
  if resolved[0].is_empty() {
    return Err(RailError::message(format!(
      "Cargo selected an empty runner for target '{target}'"
    )));
  }
  Ok(Some(resolved))
}

fn selected_target_linker(
  cargo_config: &CargoConfigSnapshot,
  target: &str,
  cfg_set: &TargetCfgSet,
  cargo_current_dir: &Path,
) -> RailResult<Option<OsString>> {
  let environment_name = target_environment_name(target, "linker");
  if let Some(value) = cargo_config.environment.get(&environment_name) {
    return resolve_program(value, ProgramOrigin::Environment, cargo_current_dir, "target linker").map(Some);
  }
  if let Some((value, source)) = cargo_config.target_value_with_source(target, "linker")? {
    let value = value.as_str().ok_or_else(|| {
      RailError::message(format!(
        "Cargo configuration key 'target.{target}.linker' must be a string"
      ))
    })?;
    return resolve_program(
      value,
      ProgramOrigin::Config(source.path()),
      cargo_current_dir,
      "target linker",
    )
    .map(Some);
  }

  let mut matches = Vec::new();
  for (constraint, table) in target_cfg_tables(cargo_config)? {
    if cargo_target_constraint_matches(constraint, target, cfg_set)? && table.get("linker").is_some() {
      matches.push(constraint);
    }
  }
  if matches.len() > 1 {
    return Err(RailError::message(format!(
      "multiple Cargo target cfg linker entries match target '{target}': {}",
      matches.join(", ")
    )));
  }
  let Some(constraint) = matches.first() else {
    return Ok(None);
  };
  let (value, source) = cargo_config
    .target_value_with_source(constraint, "linker")?
    .ok_or_else(|| RailError::message(format!("Cargo target cfg linker '{constraint}' lost source provenance")))?;
  let value = value.as_str().ok_or_else(|| {
    RailError::message(format!(
      "Cargo configuration key 'target.{constraint}.linker' must be a string"
    ))
  })?;
  resolve_program(
    value,
    ProgramOrigin::Config(source.path()),
    cargo_current_dir,
    "target linker",
  )
  .map(Some)
}

fn target_environment_name(target: &str, field: &str) -> String {
  let normalize = |value: &str| {
    value
      .chars()
      .map(|character| {
        if character.is_ascii_alphanumeric() {
          character.to_ascii_uppercase()
        } else {
          '_'
        }
      })
      .collect::<String>()
  };
  format!("CARGO_TARGET_{}_{}", normalize(target), normalize(field))
}

fn string_list(value: &JsonValue, key: &str) -> RailResult<Vec<String>> {
  if let Some(value) = value.as_str() {
    return Ok(split_space_arguments(value));
  }
  let values = value.as_array().ok_or_else(|| {
    RailError::message(format!(
      "Cargo configuration key '{key}' must be a string or string array"
    ))
  })?;
  values
    .iter()
    .map(|value| {
      value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| RailError::message(format!("Cargo configuration key '{key}' contains a non-string value")))
    })
    .collect()
}

fn split_space_arguments(value: &str) -> Vec<String> {
  value
    .split(' ')
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
    .collect()
}

fn command_identity(
  program: &OsStr,
  argument: &str,
  workspace_root: &Path,
  cargo_config: Option<&CargoConfigSnapshot>,
) -> RailResult<String> {
  let mut command = Command::new(program);
  if let Some(cargo_config) = cargo_config {
    apply_cargo_environment(&mut command, cargo_config)?;
  }
  run_identity_command(
    command,
    argument,
    workspace_root,
    &format!("'{} {argument}'", program.to_string_lossy()),
    "ensure the selected Cargo, rustc, or rustdoc executable is available and executable",
  )
}

fn run_identity_command(
  mut command: Command,
  argument: &str,
  workspace_root: &Path,
  description: &str,
  help: &str,
) -> RailResult<String> {
  disable_implicit_toolchain_install(&mut command);
  let output = command
    .current_dir(workspace_root)
    .arg(argument)
    .output()
    .map_err(|error| {
      RailError::with_help(
        format!("Failed to run {description} for Cargo resolution identity: {error}"),
        help,
      )
    })?;
  if !output.status.success() {
    return Err(RailError::message(format!(
      "{description} failed with status {}",
      output.status,
    )));
  }
  let identity = String::from_utf8(output.stdout)
    .map_err(|_| RailError::message(format!("{description} returned a non-UTF-8 identity")))?;
  let identity = identity.replace("\r\n", "\n").trim_end().to_string();
  if identity.is_empty() {
    return Err(RailError::message(format!("{description} returned an empty identity")));
  }
  Ok(identity)
}

fn hermetic_command_identity(
  program: &OsStr,
  argument: &str,
  workspace_root: &Path,
  description: &str,
) -> RailResult<String> {
  crate::instrumentation::record_hermetic_toolchain_probe(program);
  let mut command = Command::new(program);
  for name in [
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "LIBPATH",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTDOC",
    "SHLIB_PATH",
  ] {
    command.env_remove(name);
  }
  run_identity_command(
    command,
    argument,
    workspace_root,
    description,
    "install the selected Rust toolchain before hermetic execution; cargo-rail will not auto-install it",
  )
}

fn disable_implicit_toolchain_install(command: &mut Command) {
  command
    .env("RUSTUP_AUTO_INSTALL", "0")
    .env("RUSTUP_NO_UPDATE_CHECK", "1");
}

fn wrapped_rustc_identity(
  cargo_program: &OsStr,
  rustc_program: &OsStr,
  rustc_wrapper_program: Option<&OsStr>,
  rustc_workspace_wrapper_program: Option<&OsStr>,
  workspace_root: &Path,
  cargo_config: &CargoConfigSnapshot,
) -> RailResult<String> {
  wrapped_rustc_query(
    cargo_program,
    rustc_program,
    rustc_wrapper_program,
    rustc_workspace_wrapper_program,
    workspace_root,
    cargo_config,
    "-vV",
    "wrapped 'rustc -vV'",
  )
}

#[allow(clippy::too_many_arguments)]
fn wrapped_rustc_query(
  cargo_program: &OsStr,
  rustc_program: &OsStr,
  rustc_wrapper_program: Option<&OsStr>,
  rustc_workspace_wrapper_program: Option<&OsStr>,
  workspace_root: &Path,
  cargo_config: &CargoConfigSnapshot,
  argument: &str,
  description: &str,
) -> RailResult<String> {
  let mut command = configured_rustc_command(rustc_program, rustc_wrapper_program, rustc_workspace_wrapper_program);
  apply_cargo_environment(&mut command, cargo_config)?;
  command.env("CARGO", cargo_program);
  run_identity_command(
    command,
    argument,
    workspace_root,
    description,
    "ensure the selected rustc and compiler wrappers are available and executable",
  )
}

impl CargoConfigSnapshot {
  pub(crate) fn capture(cargo_current_dir: &Path) -> RailResult<Self> {
    let cargo_current_dir = canonicalize_existing(cargo_current_dir).map_err(|error| {
      RailError::message(format!(
        "Failed to resolve Cargo current directory '{}' for configuration identity: {error}",
        cargo_current_dir.display()
      ))
    })?;
    let mut framed = Vec::from(&b"cargo-rail-resolution-config-v1\0"[..]);
    let mut stack = HashSet::new();
    let mut provenance = Vec::new();
    let mut unmodeled_settings = BTreeSet::new();
    let cargo_home = cargo_home(&cargo_current_dir)?;

    for config_path in discovered_cargo_configs(&cargo_current_dir, &cargo_home)?
      .into_iter()
      .rev()
    {
      capture_config_file(
        &config_path,
        false,
        0,
        &mut stack,
        &mut framed,
        &mut provenance,
        &mut unmodeled_settings,
      )?;
    }
    let mut effective_file_settings = JsonValue::Object(JsonMap::new());
    for source in &provenance {
      merge_config_value(&mut effective_file_settings, &source.settings, "")?;
    }
    let environment = capture_relevant_environment(&effective_file_settings, &mut framed)?;
    let (credential_capabilities, credential_provenance) = capture_credential_capabilities(&cargo_home, &mut framed)?;

    Ok(Self {
      digest: ContentDigest::sha256(&framed),
      effective_file_settings,
      environment,
      provenance,
      credential_capabilities,
      credential_provenance,
      unmodeled_settings,
    })
  }
}

fn discovered_cargo_configs(cargo_current_dir: &Path, cargo_home: &Path) -> RailResult<Vec<PathBuf>> {
  let mut configs = Vec::new();
  for directory in cargo_current_dir.ancestors() {
    if let Some(path) = cargo_config_in(&directory.join(".cargo")) {
      configs.push(path);
    }
  }

  if let Some(path) = cargo_config_in(cargo_home)
    && !configs.contains(&path)
  {
    configs.push(path);
  }
  Ok(configs)
}

fn cargo_home(cargo_current_dir: &Path) -> RailResult<PathBuf> {
  if let Some(path) = std::env::var_os("CARGO_HOME") {
    let path = PathBuf::from(path);
    if path.is_absolute() {
      Ok(path)
    } else {
      Ok(cargo_current_dir.join(path))
    }
  } else {
    default_cargo_home()?.ok_or_else(|| {
      RailError::with_help(
        "Cargo resolution identity cannot determine the default CARGO_HOME",
        "set CARGO_HOME explicitly before requesting a derived resolution view",
      )
    })
  }
}

fn capture_credential_capabilities(
  cargo_home: &Path,
  framed: &mut Vec<u8>,
) -> RailResult<(JsonValue, Option<PathBuf>)> {
  let legacy = cargo_home.join("credentials");
  let toml = cargo_home.join("credentials.toml");
  let path = if legacy.is_file() {
    Some(legacy)
  } else if toml.is_file() {
    Some(toml)
  } else {
    None
  };
  let Some(path) = path else {
    let capabilities = JsonValue::Object(JsonMap::new());
    append_frame(framed, b"credential-capabilities", &serde_json::to_vec(&capabilities)?);
    return Ok((capabilities, None));
  };
  let canonical = canonicalize_existing(&path).map_err(|error| {
    RailError::message(format!(
      "Failed to resolve Cargo credentials file for capability capture: {error}"
    ))
  })?;
  let bytes = fs::read(&canonical).map_err(|error| {
    RailError::message(format!(
      "Failed to read Cargo credentials for capability capture: {error}"
    ))
  })?;
  let text =
    std::str::from_utf8(&bytes).map_err(|_| RailError::message("Cargo credentials file is not valid UTF-8"))?;
  let document: JsonValue = toml_edit::de::from_str(text).map_err(|_| {
    RailError::with_help(
      "Failed to parse Cargo credentials for capability capture",
      "fix the credentials file syntax; raw parser context is suppressed because it may contain secrets",
    )
  })?;
  let capabilities = credential_file_capabilities(&document)?;
  append_frame(framed, b"credential-capabilities", &serde_json::to_vec(&capabilities)?);
  Ok((capabilities, Some(canonical)))
}

fn credential_file_capabilities(document: &JsonValue) -> RailResult<JsonValue> {
  let root = document
    .as_object()
    .ok_or_else(|| RailError::message("Cargo credentials file is not a TOML table"))?;
  let mut capabilities = JsonMap::new();
  if let Some(registry) = root.get("registry").and_then(JsonValue::as_object)
    && let Some(token) = registry.get("token")
  {
    validate_credential_token(token, "registry.token")?;
    capabilities.insert(
      "registry".to_string(),
      serde_json::json!({"token": credential_capability_marker("token-present", &[])}),
    );
  }
  if let Some(registries) = root.get("registries") {
    let registries = registries
      .as_object()
      .ok_or_else(|| RailError::message("Cargo credentials key 'registries' must be a table"))?;
    let mut captured = JsonMap::new();
    for (name, registry) in registries {
      let registry = registry
        .as_object()
        .ok_or_else(|| RailError::message(format!("Cargo credentials registry '{name}' must be a table")))?;
      if let Some(token) = registry.get("token") {
        validate_credential_token(token, &format!("registries.{name}.token"))?;
        captured.insert(
          name.clone(),
          serde_json::json!({"token": credential_capability_marker("token-present", &[])}),
        );
      }
    }
    if !captured.is_empty() {
      capabilities.insert("registries".to_string(), JsonValue::Object(captured));
    }
  }
  Ok(JsonValue::Object(capabilities))
}

fn validate_credential_token(value: &JsonValue, path: &str) -> RailResult<()> {
  if value.is_string() {
    Ok(())
  } else {
    Err(RailError::message(format!(
      "Cargo credentials key '{path}' must be a string"
    )))
  }
}

fn cargo_config_in(directory: &Path) -> Option<PathBuf> {
  let legacy = directory.join("config");
  if legacy.is_file() {
    return Some(legacy);
  }
  let toml = directory.join("config.toml");
  toml.is_file().then_some(toml)
}

#[cfg(unix)]
fn default_cargo_home() -> RailResult<Option<PathBuf>> {
  Ok(std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
}

#[cfg(windows)]
fn default_cargo_home() -> RailResult<Option<PathBuf>> {
  if let Some(profile) = std::env::var_os("USERPROFILE") {
    return Ok(Some(PathBuf::from(profile).join(".cargo")));
  }
  let drive = std::env::var_os("HOMEDRIVE");
  let path = std::env::var_os("HOMEPATH");
  Ok(drive.zip(path).map(|(drive, path)| {
    let mut home = PathBuf::from(drive);
    home.push(path);
    home.push(".cargo");
    home
  }))
}

fn capture_config_file(
  path: &Path,
  optional: bool,
  depth: usize,
  stack: &mut HashSet<PathBuf>,
  framed: &mut Vec<u8>,
  provenance: &mut Vec<CargoConfigSource>,
  unmodeled_settings: &mut BTreeSet<String>,
) -> RailResult<()> {
  if depth > MAX_CARGO_CONFIG_INCLUDE_DEPTH {
    return Err(RailError::message(format!(
      "Cargo configuration include depth exceeds {MAX_CARGO_CONFIG_INCLUDE_DEPTH}"
    )));
  }
  if !path.exists() {
    if optional {
      append_frame(framed, b"optional-missing", path_bytes(path)?);
      return Ok(());
    }
    return Err(RailError::message(format!(
      "Required Cargo configuration include '{}' does not exist",
      path.display()
    )));
  }
  let canonical = canonicalize_existing(path).map_err(|error| {
    RailError::message(format!(
      "Failed to resolve Cargo configuration '{}': {error}",
      path.display()
    ))
  })?;
  if !stack.insert(canonical.clone()) {
    return Err(RailError::message(format!(
      "Cargo configuration include cycle reaches '{}'",
      path.display()
    )));
  }

  let result = (|| {
    let bytes = fs::read(path).map_err(|error| {
      RailError::message(format!(
        "Failed to read Cargo configuration '{}': {error}",
        path.display()
      ))
    })?;
    let text = std::str::from_utf8(&bytes)
      .map_err(|_| RailError::message(format!("Cargo configuration '{}' is not valid UTF-8", path.display())))?;
    let mut document: JsonValue = toml_edit::de::from_str(text).map_err(|_| {
      RailError::with_help(
        format!("Failed to parse Cargo configuration '{}'", path.display()),
        "fix the TOML syntax; raw parser context is suppressed because Cargo configuration may contain credentials",
      )
    })?;
    let includes = take_includes(&mut document, path)?;
    for include in includes {
      capture_config_file(
        &include.path,
        include.optional,
        depth + 1,
        stack,
        framed,
        provenance,
        unmodeled_settings,
      )?;
    }

    let relevant = relevant_config(document, path, unmodeled_settings)?;
    append_frame(framed, b"config-path", path_bytes(&canonical)?);
    append_frame(framed, b"config-value", &serde_json::to_vec(&relevant)?);
    provenance.push(CargoConfigSource {
      path: canonical.clone(),
      settings: relevant,
    });
    Ok(())
  })();
  stack.remove(&canonical);
  result
}

struct ConfigInclude {
  path: PathBuf,
  optional: bool,
}

fn take_includes(document: &mut JsonValue, config_path: &Path) -> RailResult<Vec<ConfigInclude>> {
  let Some(object) = document.as_object_mut() else {
    return Err(RailError::message(format!(
      "Cargo configuration '{}' is not a TOML table",
      config_path.display()
    )));
  };
  let Some(include) = object.remove("include") else {
    return Ok(Vec::new());
  };
  let entries = include.as_array().ok_or_else(|| {
    RailError::message(format!(
      "Cargo configuration '{}' has a non-array include value",
      config_path.display()
    ))
  })?;
  let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
  entries
    .iter()
    .map(|entry| {
      let (path, optional) = if let Some(path) = entry.as_str() {
        (path, false)
      } else {
        let table = entry.as_object().ok_or_else(|| {
          RailError::message(format!(
            "Cargo configuration '{}' has an invalid include entry",
            config_path.display()
          ))
        })?;
        let path = table.get("path").and_then(JsonValue::as_str).ok_or_else(|| {
          RailError::message(format!(
            "Cargo configuration '{}' has an include without a string path",
            config_path.display()
          ))
        })?;
        let optional = match table.get("optional") {
          Some(value) => value.as_bool().ok_or_else(|| {
            RailError::message(format!(
              "Cargo configuration '{}' has a non-boolean include optional flag",
              config_path.display()
            ))
          })?,
          None => false,
        };
        (path, optional)
      };
      if !path.ends_with(".toml") {
        return Err(RailError::message(format!(
          "Cargo configuration include '{}' must end in .toml",
          path
        )));
      }
      Ok(ConfigInclude {
        path: parent.join(path),
        optional,
      })
    })
    .collect()
}

fn relevant_config(
  document: JsonValue,
  config_path: &Path,
  unmodeled_settings: &mut BTreeSet<String>,
) -> RailResult<JsonValue> {
  const RELEVANT_TOP_LEVEL: &[&str] = &[
    "build",
    "credential-alias",
    "env",
    "host",
    "http",
    "net",
    "patch",
    "paths",
    "profile",
    "registries",
    "registry",
    "resolver",
    "source",
    "target",
    "unstable",
  ];
  let object = document.as_object().ok_or_else(|| {
    RailError::message(format!(
      "Cargo configuration '{}' is not a TOML table",
      config_path.display()
    ))
  })?;
  let mut relevant = JsonMap::new();
  for key in RELEVANT_TOP_LEVEL {
    if let Some(value) = object.get(*key) {
      relevant.insert((*key).to_string(), sanitize_config_value(value, key)?);
    }
  }
  const KNOWN_NON_BUILD_TOP_LEVEL: &[&str] = &["alias", "doc", "future-incompat-report", "term"];
  for key in object.keys() {
    if !RELEVANT_TOP_LEVEL.contains(&key.as_str()) && !KNOWN_NON_BUILD_TOP_LEVEL.contains(&key.as_str()) {
      unmodeled_settings.insert(key.clone());
    }
  }
  collect_unmodeled_build_settings(&relevant, unmodeled_settings);
  Ok(JsonValue::Object(relevant))
}

fn collect_unmodeled_build_settings(settings: &JsonMap<String, JsonValue>, unmodeled: &mut BTreeSet<String>) {
  const BUILD_FIELDS: &[&str] = &[
    "build-dir",
    "dep-info-basedir",
    "incremental",
    "jobs",
    "pipelining",
    "rustc",
    "rustc-wrapper",
    "rustc-workspace-wrapper",
    "rustdoc",
    "rustdocflags",
    "rustflags",
    "target",
    "target-dir",
  ];
  if let Some(build) = settings.get("build").and_then(JsonValue::as_object) {
    for field in build.keys().filter(|field| !BUILD_FIELDS.contains(&field.as_str())) {
      unmodeled.insert(format!("build.{field}"));
    }
  }
  const TARGET_FIELDS: &[&str] = &["linker", "runner", "rustdocflags", "rustflags"];
  if let Some(targets) = settings.get("target").and_then(JsonValue::as_object) {
    for (target, table) in targets {
      if let Some(table) = table.as_object() {
        for field in table.keys().filter(|field| !TARGET_FIELDS.contains(&field.as_str())) {
          unmodeled.insert(format!("target.{target}.{field}"));
        }
      }
    }
  }
  if settings.contains_key("host") {
    unmodeled.insert("host".to_string());
  }
  if settings.contains_key("unstable") {
    unmodeled.insert("unstable".to_string());
  }
}

fn sanitize_config_value(value: &JsonValue, path: &str) -> RailResult<JsonValue> {
  if let Some(capability) = known_credential_capability(value, path)? {
    return Ok(capability);
  }
  match value {
    JsonValue::Object(object) => {
      let mut sanitized = JsonMap::new();
      for (key, value) in object {
        let nested_path = format!("{path}.{key}");
        sanitized.insert(key.clone(), sanitize_config_value(value, &nested_path)?);
      }
      Ok(JsonValue::Object(sanitized))
    }
    JsonValue::Array(values) => values
      .iter()
      .map(|value| sanitize_config_value(value, path))
      .collect::<RailResult<Vec<_>>>()
      .map(JsonValue::Array),
    JsonValue::String(value) if credential_bearing_url(value) => Err(RailError::with_help(
      format!("Cargo resolution identity found credentials in URL-valued setting '{path}'"),
      "move credentials to Cargo's credential provider or credential environment and keep the configured URL secret-free",
    )),
    value => Ok(value.clone()),
  }
}

fn capture_relevant_environment(
  effective_file_settings: &JsonValue,
  framed: &mut Vec<u8>,
) -> RailResult<BTreeMap<String, String>> {
  let configured_environment = effective_file_settings
    .get("env")
    .and_then(JsonValue::as_object)
    .map(|environment| environment.keys().map(String::as_str).collect::<HashSet<_>>())
    .unwrap_or_default();
  let mut environment = BTreeMap::new();
  for (name, value) in std::env::vars_os() {
    let Some(name) = name.to_str() else {
      let lossy = name.to_string_lossy();
      if lossy.starts_with("CARGO_") || lossy.starts_with("RUST") {
        return Err(RailError::message(
          "Cargo resolution identity found a non-UTF-8 Cargo or Rust environment name",
        ));
      }
      continue;
    };
    if !is_relevant_environment(name) && !configured_environment.contains(name) {
      continue;
    }
    if is_secret_name(name) {
      environment.insert(
        name.to_string(),
        credential_environment_marker(secret_capability_kind(name)),
      );
      continue;
    }
    let value = value.to_str().ok_or_else(|| {
      RailError::message(format!(
        "Cargo resolution identity found a non-UTF-8 value for environment variable '{name}'"
      ))
    })?;
    if credential_bearing_url(value) {
      return Err(RailError::with_help(
        format!("Cargo resolution identity found credentials in URL-valued environment variable '{name}'"),
        "move credentials to Cargo's credential provider or credential environment and keep configured URLs secret-free",
      ));
    }
    environment.insert(name.to_string(), value.to_string());
  }
  append_frame(framed, b"environment", &serde_json::to_vec(&environment)?);
  Ok(environment)
}

fn known_credential_capability(value: &JsonValue, path: &str) -> RailResult<Option<JsonValue>> {
  let field = path.rsplit('.').next().unwrap_or(path);
  if field == "token" {
    return Ok(Some(credential_capability_marker("token-present", &[])));
  }
  if field == "credential-provider" {
    return provider_capability(value, false, path).map(Some);
  }
  if field == "global-credential-providers" {
    return provider_capability(value, true, path).map(Some);
  }
  if path.starts_with("credential-alias.") {
    return provider_capability(value, false, path).map(Some);
  }
  if is_secret_name(field) {
    return Ok(Some(credential_capability_marker(secret_capability_kind(field), &[])));
  }
  Ok(None)
}

fn provider_capability(value: &JsonValue, provider_list: bool, path: &str) -> RailResult<JsonValue> {
  let mechanisms = if provider_list {
    value
      .as_array()
      .ok_or_else(|| RailError::message(format!("Cargo configuration key '{path}' must be an array")))?
      .iter()
      .map(|provider| {
        provider.as_str().map(provider_mechanism).ok_or_else(|| {
          RailError::message(format!(
            "Cargo configuration key '{path}' contains a non-string provider"
          ))
        })
      })
      .collect::<RailResult<Vec<_>>>()?
  } else if let Some(provider) = value.as_str() {
    vec![provider_mechanism(provider)]
  } else if let Some(command) = value.as_array() {
    if command.is_empty() || command.iter().any(|argument| !argument.is_string()) {
      return Err(RailError::message(format!(
        "Cargo configuration key '{path}' must be a provider string or non-empty string array"
      )));
    }
    vec!["external-command"]
  } else {
    return Err(RailError::message(format!(
      "Cargo configuration key '{path}' must be a provider string or string array"
    )));
  };
  Ok(credential_capability_marker("credential-provider", &mechanisms))
}

fn provider_mechanism(provider: &str) -> &'static str {
  match provider {
    "cargo:token" => "cargo-token",
    "cargo:wincred" => "windows-credential-manager",
    "cargo:macos-keychain" => "macos-keychain",
    "cargo:libsecret" => "linux-secret-service",
    _ if provider.starts_with("cargo:token-from-stdout ") => "stdout-token-command",
    _ => "configured-provider",
  }
}

fn secret_capability_kind(name: &str) -> &'static str {
  let normalized = name.to_ascii_lowercase().replace('_', "-");
  if normalized == "token" || normalized.ends_with("-token") {
    "token-present"
  } else if normalized.contains("password") {
    "password-present"
  } else if normalized.contains("private-key") {
    "private-key-present"
  } else if normalized.contains("credential") {
    "credential-present"
  } else {
    "secret-present"
  }
}

fn credential_capability_marker(kind: &str, mechanisms: &[&str]) -> JsonValue {
  let mut capability = JsonMap::new();
  capability.insert("kind".to_string(), JsonValue::String(kind.to_string()));
  if !mechanisms.is_empty() {
    capability.insert(
      "mechanisms".to_string(),
      JsonValue::Array(
        mechanisms
          .iter()
          .map(|mechanism| JsonValue::String((*mechanism).to_string()))
          .collect(),
      ),
    );
  }
  JsonValue::Object(JsonMap::from_iter([(
    CREDENTIAL_CAPABILITY_KEY.to_string(),
    JsonValue::Object(capability),
  )]))
}

fn is_credential_capability(value: &JsonValue) -> bool {
  value
    .as_object()
    .is_some_and(|object| object.len() == 1 && object.contains_key(CREDENTIAL_CAPABILITY_KEY))
}

fn contains_credential_capability(value: &JsonValue) -> bool {
  is_credential_capability(value)
    || match value {
      JsonValue::Array(values) => values.iter().any(contains_credential_capability),
      JsonValue::Object(values) => values.values().any(contains_credential_capability),
      _ => false,
    }
}

fn credential_environment_marker(kind: &str) -> String {
  format!("{CREDENTIAL_ENV_MARKER_PREFIX}{kind}>")
}

fn is_credential_environment_marker(value: &str) -> bool {
  value.starts_with(CREDENTIAL_ENV_MARKER_PREFIX) && value.ends_with('>')
}

fn merge_config_value(current: &mut JsonValue, incoming: &JsonValue, path: &str) -> RailResult<()> {
  match (current, incoming) {
    (JsonValue::Object(current), JsonValue::Object(incoming)) => {
      for (key, incoming) in incoming {
        let nested_path = if path.is_empty() {
          key.clone()
        } else {
          format!("{path}.{key}")
        };
        if let Some(current) = current.get_mut(key) {
          merge_config_value(current, incoming, &nested_path)?;
        } else {
          current.insert(key.clone(), incoming.clone());
        }
      }
      Ok(())
    }
    (JsonValue::Array(current), JsonValue::Array(incoming)) => {
      current.extend(incoming.iter().cloned());
      Ok(())
    }
    (current, incoming) if json_value_kind(current) == json_value_kind(incoming) => {
      *current = incoming.clone();
      Ok(())
    }
    (current, incoming) => Err(RailError::message(format!(
      "Cargo configuration key '{path}' changes type from {} to {} across merged inputs",
      json_value_kind(current),
      json_value_kind(incoming)
    ))),
  }
}

fn json_value_kind(value: &JsonValue) -> &'static str {
  match value {
    JsonValue::Null => "null",
    JsonValue::Bool(_) => "boolean",
    JsonValue::Number(_) => "number",
    JsonValue::String(_) => "string",
    JsonValue::Array(_) => "array",
    JsonValue::Object(_) => "table",
  }
}

fn is_relevant_environment(name: &str) -> bool {
  name == "CARGO"
    || name == "CARGO_HOME"
    || name == "CARGO_INCREMENTAL"
    || name == "CARGO_ENCODED_RUSTFLAGS"
    || name == "CARGO_ENCODED_RUSTDOCFLAGS"
    || name.starts_with("CARGO_BUILD_")
    || name.starts_with("CARGO_NET_")
    || name.starts_with("CARGO_PROFILE_")
    || name.starts_with("CARGO_REGISTRIES_")
    || name.starts_with("CARGO_REGISTRY_")
    || name.starts_with("CARGO_RESOLVER_")
    || name.starts_with("CARGO_SOURCE_")
    || name.starts_with("CARGO_TARGET_")
    || name.starts_with("CARGO_UNSTABLE_")
    || matches!(
      name,
      "RUSTC"
        | "RUSTC_BOOTSTRAP"
        | "RUSTC_WRAPPER"
        | "RUSTC_WORKSPACE_WRAPPER"
        | "RUSTDOC"
        | "RUSTDOCFLAGS"
        | "RUSTFLAGS"
        | "RUSTUP_TOOLCHAIN"
        | "RUST_TARGET_PATH"
    )
}

fn is_secret_name(name: &str) -> bool {
  let normalized = name.to_ascii_lowercase().replace('_', "-");
  normalized == "token"
    || normalized.ends_with("-token")
    || normalized.contains("password")
    || normalized.contains("secret")
    || normalized.contains("credential")
    || normalized.contains("private-key")
}

pub(crate) fn credential_bearing_url(value: &str) -> bool {
  let Some((scheme, after_scheme)) = value.split_once("://") else {
    return false;
  };
  let authority_end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
  let authority = &after_scheme[..authority_end];
  if let Some((userinfo, _)) = authority.rsplit_once('@') {
    let ssh_username_only =
      matches!(scheme.to_ascii_lowercase().as_str(), "ssh" | "git+ssh") && !userinfo.contains(':');
    if !ssh_username_only {
      return true;
    }
  }
  let Some((_, query_and_fragment)) = after_scheme.split_once('?') else {
    return false;
  };
  let query = query_and_fragment.split('#').next().unwrap_or(query_and_fragment);
  query.split('&').any(|pair| {
    let key = pair.split('=').next().unwrap_or(pair).to_ascii_lowercase();
    is_secret_name(&key) || key.contains("signature") || key == "key" || key.ends_with("_key") || key.ends_with("-key")
  })
}

fn path_bytes(path: &Path) -> RailResult<&[u8]> {
  path.to_str().map(str::as_bytes).ok_or_else(|| {
    RailError::message(format!(
      "Cargo configuration path '{}' is not valid UTF-8",
      path.display()
    ))
  })
}

fn append_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
  output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
  output.extend_from_slice(tag);
  output.extend_from_slice(&(value.len() as u64).to_le_bytes());
  output.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
  use super::*;

  const CARGO_TOOL_ENVIRONMENT: &[&str] = &[
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTDOC",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTDOC",
  ];

  fn rustc_host() -> &'static str {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST
      .get_or_init(|| {
        let output = Command::new("rustc").arg("-vV").output().expect("rustc -vV should run");
        assert!(output.status.success(), "rustc -vV should succeed");
        String::from_utf8(output.stdout)
          .expect("rustc identity should be UTF-8")
          .lines()
          .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
          .expect("rustc identity should contain a host")
      })
      .as_str()
  }

  fn target_test_inputs(root: &Path) -> ResolutionInputs {
    let mut cargo_config = CargoConfigSnapshot::capture(root).expect("Cargo config should capture");
    cargo_config.environment.retain(|name, _| {
      !name.ends_with("RUSTFLAGS")
        && !name.ends_with("RUSTDOCFLAGS")
        && name != "CARGO_BUILD_TARGET"
        && !CARGO_TOOL_ENVIRONMENT.contains(&name.as_str())
    });
    let toolchain = ToolchainIdentity::capture(root, &cargo_config).expect("tool identity should resolve");
    ResolutionInputs {
      cargo_config: Arc::new(cargo_config),
      toolchain,
      hermetic: false,
    }
  }

  fn cargo_probe_command(root: &Path) -> Command {
    let mut command = Command::new("cargo");
    command.current_dir(root);
    for name in CARGO_TOOL_ENVIRONMENT {
      command.env_remove(name);
    }
    command
  }

  fn write_cargo_config_probe(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("probe source directory should be created");
    fs::write(
      root.join("Cargo.toml"),
      "[package]\nname = \"cargo-rail-config-probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("probe manifest should be written");
    fs::write(root.join("src/lib.rs"), "pub fn probe() {}\n").expect("probe library should be written");
    fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("probe binary should be written");
  }

  fn run_cargo_rustc_cfg(root: &Path, leading_arguments: &[&str], environment: &[(&str, &str)]) -> BTreeSet<String> {
    let mut command = cargo_probe_command(root);
    command
      .args(leading_arguments)
      .args(["rustc", "--offline", "--quiet", "--lib", "--", "--print=cfg"]);
    for name in ["CARGO_BUILD_RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"] {
      command.env_remove(name);
    }
    command.env_remove(target_environment_name(rustc_host(), "rustflags"));
    for (name, value) in environment {
      command.env(name, value);
    }
    let output = command.output().expect("Cargo rustc cfg probe should run");
    assert!(
      output.status.success(),
      "Cargo rustc cfg probe failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
      .expect("Cargo rustc cfg output should be UTF-8")
      .lines()
      .map(str::to_string)
      .collect()
  }

  fn probe_flags(flags: &[String]) -> Vec<&str> {
    flags
      .iter()
      .filter_map(|flag| flag.strip_prefix("cargo_rail_config_"))
      .collect()
  }

  #[cfg(unix)]
  fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::write(path, contents).expect("probe executable should be written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("probe executable should be executable");
  }

  #[test]
  fn credential_config_values_become_typed_capabilities_before_identity_framing() {
    let value = serde_json::json!({
      "private": {
        "index": "https://example.invalid/index",
        "token": "do-not-hash-me",
        "credential-provider": ["cargo:token", "do-not-hash-me-either"]
      },
      "global-credential-providers": ["cargo:token", "private-provider-alias"]
    });
    let sanitized = sanitize_config_value(&value, "registries").expect("configuration should sanitize");
    let encoded = serde_json::to_string(&sanitized).expect("sanitized config should serialize");
    assert!(!encoded.contains("do-not-hash-me"));
    assert!(!encoded.contains("private-provider-alias"));
    assert!(encoded.contains(CREDENTIAL_CAPABILITY_KEY));
    assert!(encoded.contains("token-present"));
    assert!(encoded.contains("external-command"));
    assert!(encoded.contains("cargo-token"));
    assert!(encoded.contains("configured-provider"));
  }

  #[test]
  fn credential_alias_commands_never_enter_sanitized_config() {
    let value = serde_json::json!(["credential-helper", "--password", "do-not-capture"]);
    let sanitized =
      sanitize_config_value(&value, "credential-alias.private").expect("credential alias should become a capability");
    let encoded = serde_json::to_string(&sanitized).expect("capability should serialize");
    assert_eq!(
      sanitized[CREDENTIAL_CAPABILITY_KEY]["mechanisms"],
      serde_json::json!(["external-command"])
    );
    assert!(!encoded.contains("credential-helper"));
    assert!(!encoded.contains("do-not-capture"));
  }

  #[test]
  fn cargo_credentials_capture_binds_capability_without_hashing_token_material() {
    let cargo_home = tempfile::tempdir().expect("temporary Cargo home should be created");
    let credentials = cargo_home.path().join("credentials.toml");
    fs::write(&credentials, "[registry]\ntoken = \"first-raw-token\"\n")
      .expect("first credentials file should be written");
    let mut first_framed = Vec::new();
    let (first, first_path) = capture_credential_capabilities(cargo_home.path(), &mut first_framed)
      .expect("first credential capability should capture");

    fs::write(&credentials, "[registry]\ntoken = \"different-raw-token\"\n")
      .expect("second credentials file should be written");
    let mut second_framed = Vec::new();
    let (second, second_path) = capture_credential_capabilities(cargo_home.path(), &mut second_framed)
      .expect("second credential capability should capture");

    assert_eq!(first, second, "token values must collapse to the same capability");
    assert_eq!(first_framed, second_framed, "raw token bytes must not affect identity");
    assert_eq!(first_path, second_path);
    let encoded = String::from_utf8(first_framed).expect("capability framing should be UTF-8 apart from lengths");
    assert!(!encoded.contains("raw-token"));
    assert!(encoded.contains("token-present"));
  }

  #[test]
  fn legacy_cargo_credentials_file_has_cargos_documented_precedence() {
    let cargo_home = tempfile::tempdir().expect("temporary Cargo home should be created");
    let legacy = cargo_home.path().join("credentials");
    fs::write(&legacy, "[registries.legacy]\ntoken = \"legacy-secret\"\n")
      .expect("legacy credentials file should be written");
    fs::write(
      cargo_home.path().join("credentials.toml"),
      "[registries.toml]\ntoken = \"toml-secret\"\n",
    )
    .expect("TOML credentials file should be written");

    let mut framed = Vec::new();
    let (capabilities, provenance) =
      capture_credential_capabilities(cargo_home.path(), &mut framed).expect("credential capability should capture");
    assert_eq!(
      provenance.as_deref(),
      Some(
        canonicalize_existing(&legacy)
          .expect("legacy credentials should canonicalize")
          .as_path()
      )
    );
    assert!(capabilities.pointer("/registries/legacy/token").is_some());
    assert!(capabilities.pointer("/registries/toml/token").is_none());
    let encoded = String::from_utf8_lossy(&framed);
    assert!(!encoded.contains("legacy-secret"));
    assert!(!encoded.contains("toml-secret"));
  }

  #[test]
  fn redacted_cargo_env_semantics_fail_closed_for_tool_queries() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[env]\nBUILD_TOKEN = { value = \"not-captured\", force = true }\n",
    )
    .expect("Cargo config should be written");
    let captured = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    let mut command = Command::new("rustc");
    let error = apply_cargo_environment(&mut command, &captured)
      .expect_err("redacted Cargo environment semantics must fail closed");
    assert!(error.to_string().contains("cannot apply redacted"), "{error}");
  }

  #[test]
  fn credential_bearing_urls_fail_closed() {
    assert!(credential_bearing_url("https://user:password@example.invalid/index"));
    assert!(credential_bearing_url("https://example.invalid/index?token=secret"));
    assert!(!credential_bearing_url("ssh://git@example.invalid/repository"));
    assert!(!credential_bearing_url("https://example.invalid/index"));
  }

  #[test]
  fn cargo_config_merge_matches_hierarchical_scalar_and_array_precedence() {
    let mut effective = serde_json::json!({
      "build": {
        "rustc": "ancestor-rustc",
        "rustflags": ["--cfg", "ancestor"]
      }
    });
    let deeper = serde_json::json!({
      "build": {
        "rustc": "workspace-rustc",
        "rustflags": ["--cfg", "workspace"]
      }
    });

    merge_config_value(&mut effective, &deeper, "").expect("compatible Cargo settings should merge");
    assert_eq!(effective["build"]["rustc"], "workspace-rustc");
    assert_eq!(
      effective["build"]["rustflags"],
      serde_json::json!(["--cfg", "ancestor", "--cfg", "workspace"])
    );
  }

  #[test]
  fn cargo_config_merge_rejects_incompatible_types() {
    let mut effective = serde_json::json!({"build": {"rustflags": ["--cfg", "ancestor"]}});
    let invalid = serde_json::json!({"build": {"rustflags": "--cfg workspace"}});
    let error =
      merge_config_value(&mut effective, &invalid, "").expect_err("incompatible Cargo setting types must fail closed");
    assert!(error.to_string().contains("build.rustflags"), "{error}");
  }

  #[test]
  fn relevant_cargo_config_keeps_build_target_profile_and_environment_settings() {
    let document = serde_json::json!({
      "build": {"rustc-wrapper": "wrapper"},
      "target": {"x86_64-unknown-linux-gnu": {"linker": "clang"}},
      "profile": {"release": {"lto": "thin"}},
      "env": {"CC": "clang"},
      "term": {"color": "never"}
    });
    let mut unmodeled = BTreeSet::new();
    let relevant = relevant_config(document, Path::new(".cargo/config.toml"), &mut unmodeled)
      .expect("build-affecting Cargo settings should be retained");

    assert_eq!(relevant["build"]["rustc-wrapper"], "wrapper");
    assert_eq!(relevant["target"]["x86_64-unknown-linux-gnu"]["linker"], "clang");
    assert_eq!(relevant["profile"]["release"]["lto"], "thin");
    assert_eq!(relevant["env"]["CC"], "clang");
    assert!(relevant.get("term").is_none());
    assert!(unmodeled.is_empty());
  }

  #[test]
  fn cargo_config_contract_marks_unknown_build_influence_uncacheable() {
    let document = serde_json::json!({
      "build": {"rustflags": ["--cfg", "known"], "future-compiler-mode": true},
      "target": {"x86_64-unknown-linux-gnu": {"future-link-mode": "new"}},
      "future-build-system": {"enabled": true},
      "term": {"color": "never"}
    });
    let mut unmodeled = BTreeSet::new();
    let relevant = relevant_config(document, Path::new(".cargo/config.toml"), &mut unmodeled)
      .expect("unknown settings should be captured fail-closed");

    assert_eq!(relevant["build"]["rustflags"], serde_json::json!(["--cfg", "known"]));
    assert_eq!(
      unmodeled,
      BTreeSet::from([
        "build.future-compiler-mode".to_string(),
        "future-build-system".to_string(),
        "target.x86_64-unknown-linux-gnu.future-link-mode".to_string(),
      ])
    );
  }

  #[test]
  fn cargo_config_capture_retains_include_provenance_and_effective_order() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("base.toml"),
      "[build]\nrustflags = [\"--cfg\", \"base\"]\n",
    )
    .expect("included config should be written");
    fs::write(
      cargo_dir.join("config.toml"),
      "include = [\"base.toml\"]\n[build]\nrustflags = [\"--cfg\", \"workspace\"]\n",
    )
    .expect("workspace config should be written");

    let captured = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config capture should succeed");
    let local_sources = captured
      .provenance()
      .iter()
      .filter_map(|source| source.path().file_name().and_then(OsStr::to_str))
      .filter(|name| matches!(*name, "base.toml" | "config.toml"))
      .collect::<Vec<_>>();
    assert_eq!(local_sources, ["base.toml", "config.toml"]);
    assert_eq!(
      captured.effective_file_settings()["build"]["rustflags"],
      serde_json::json!(["--cfg", "base", "--cfg", "workspace"])
    );
  }

  #[test]
  fn cargo_config_hierarchy_and_includes_match_cargo_rustc() {
    let root = tempfile::tempdir().expect("temporary Cargo hierarchy should be created");
    let ancestor = root.path().join("ancestor");
    let workspace = ancestor.join("workspace");
    let ancestor_cargo = ancestor.join(".cargo");
    let workspace_cargo = workspace.join(".cargo");
    fs::create_dir_all(&ancestor_cargo).expect("ancestor Cargo directory should be created");
    fs::create_dir_all(&workspace_cargo).expect("workspace Cargo directory should be created");
    write_cargo_config_probe(&workspace);
    fs::write(
      ancestor_cargo.join("base.toml"),
      "[build]\nrustflags = [\"--cfg\", \"cargo_rail_config_ancestor_include\"]\n",
    )
    .expect("ancestor include should be written");
    fs::write(
      ancestor_cargo.join("config.toml"),
      "include = [\"base.toml\"]\n[build]\nrustflags = [\"--cfg\", \"cargo_rail_config_ancestor\"]\n",
    )
    .expect("ancestor config should be written");
    fs::write(
      workspace_cargo.join("base.toml"),
      "[build]\nrustflags = [\"--cfg\", \"cargo_rail_config_workspace_include\"]\n",
    )
    .expect("workspace include should be written");
    fs::write(
      workspace_cargo.join("config.toml"),
      "include = [\"base.toml\"]\n[build]\nrustflags = [\"--cfg\", \"cargo_rail_config_workspace\"]\n",
    )
    .expect("workspace config should be written");

    let inputs = target_test_inputs(&workspace);
    let targets = capture_target_identities(&workspace, &[], &inputs).expect("target identity should resolve");
    let target = targets.first().expect("host target identity");
    assert_eq!(
      probe_flags(target.rustflags()),
      ["ancestor_include", "ancestor", "workspace_include", "workspace"],
      "cargo-rail must preserve Cargo's low-to-high array merge order"
    );

    let cargo_cfg = run_cargo_rustc_cfg(&workspace, &[], &[]);
    for marker in [
      "cargo_rail_config_ancestor_include",
      "cargo_rail_config_ancestor",
      "cargo_rail_config_workspace_include",
      "cargo_rail_config_workspace",
    ] {
      assert!(cargo_cfg.contains(marker), "Cargo did not apply modeled cfg '{marker}'");
    }
  }

  #[test]
  fn cargo_flag_environment_and_cli_precedence_match_cargo_rustc() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    write_cargo_config_probe(workspace.path());
    fs::create_dir_all(workspace.path().join(".cargo")).expect("Cargo config directory should be created");
    fs::write(
      workspace.path().join(".cargo/config.toml"),
      "[build]\nrustflags = [\"--cfg\", \"cargo_rail_config_file\"]\n",
    )
    .expect("Cargo config should be written");

    let cargo_cfg = run_cargo_rustc_cfg(
      workspace.path(),
      &["--config", "build.rustflags=['--cfg','cargo_rail_config_cli']"],
      &[],
    );
    assert!(cargo_cfg.contains("cargo_rail_config_cli"));
    assert!(cargo_cfg.contains("cargo_rail_config_file"));

    let mut config = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    config.environment.retain(|name, _| !name.ends_with("RUSTFLAGS"));
    config.environment.insert(
      "RUSTFLAGS".to_string(),
      "--cfg cargo_rail_config_environment".to_string(),
    );
    let modeled =
      effective_flags(&config, rustc_host(), None, CompilerFlags::Rust).expect("RUSTFLAGS precedence should resolve");
    assert_eq!(probe_flags(&modeled), ["environment"]);
    let cargo_cfg = run_cargo_rustc_cfg(
      workspace.path(),
      &["--config", "build.rustflags=['--cfg','cargo_rail_config_cli']"],
      &[("RUSTFLAGS", "--cfg cargo_rail_config_environment")],
    );
    assert!(cargo_cfg.contains("cargo_rail_config_environment"));
    assert!(!cargo_cfg.contains("cargo_rail_config_cli"));
    assert!(!cargo_cfg.contains("cargo_rail_config_file"));

    config.environment.insert(
      "CARGO_ENCODED_RUSTFLAGS".to_string(),
      "--cfg\u{1f}cargo_rail_config_encoded".to_string(),
    );
    let modeled = effective_flags(&config, rustc_host(), None, CompilerFlags::Rust)
      .expect("encoded rustflags precedence should resolve");
    assert_eq!(probe_flags(&modeled), ["encoded"]);
    let cargo_cfg = run_cargo_rustc_cfg(
      workspace.path(),
      &[],
      &[
        ("RUSTFLAGS", "--cfg cargo_rail_config_environment"),
        ("CARGO_ENCODED_RUSTFLAGS", "--cfg\u{1f}cargo_rail_config_encoded"),
      ],
    );
    assert!(cargo_cfg.contains("cargo_rail_config_encoded"));
    assert!(!cargo_cfg.contains("cargo_rail_config_environment"));
    assert!(!cargo_cfg.contains("cargo_rail_config_file"));
  }

  #[test]
  fn cargo_profile_and_incremental_environment_are_authoritative_inputs() {
    assert!(is_relevant_environment("CARGO_PROFILE_DEV_OPT_LEVEL"));
    assert!(is_relevant_environment("CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_DEBUG"));
    assert!(is_relevant_environment("CARGO_INCREMENTAL"));
    assert!(!is_relevant_environment("CARGO_RAIL_UNRELATED"));
  }

  #[cfg(unix)]
  #[test]
  fn cargo_target_flags_linker_and_wrapper_chain_match_cargo_rustc() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    write_cargo_config_probe(workspace.path());
    let cargo_dir = workspace.path().join(".cargo");
    let tools_dir = workspace.path().join("tools");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::create_dir_all(&tools_dir).expect("probe tool directory should be created");
    let host = rustc_host();
    fs::write(
      cargo_dir.join("config.toml"),
      format!(
        r#"[build]
target = "host-tuple"
rustc-wrapper = "tools/global-wrapper"
rustc-workspace-wrapper = "tools/workspace-wrapper"

[target.{host}]
linker = "tools/linker"
rustflags = ["--cfg", "cargo_rail_config_triple"]

[target.'cfg(unix)']
rustflags = ["--cfg", "cargo_rail_config_cfg"]

[env]
CARGO_RAIL_CONFIG_WRAPPER_LOG = {{ value = "wrapper.log", relative = true, force = true }}
CARGO_RAIL_CONFIG_ARGV_LOG = {{ value = "argv.log", relative = true, force = true }}
"#
      ),
    )
    .expect("Cargo config should be written");
    write_executable(
      &tools_dir.join("global-wrapper"),
      r#"#!/bin/sh
case " $* " in
  *" cargo_rail_config_probe "*) printf 'global\n' >> "$CARGO_RAIL_CONFIG_WRAPPER_LOG" ;;
esac
exec "$@"
"#,
    );
    write_executable(
      &tools_dir.join("workspace-wrapper"),
      r#"#!/bin/sh
case " $* " in
  *" cargo_rail_config_probe "*)
    printf 'workspace\n' >> "$CARGO_RAIL_CONFIG_WRAPPER_LOG"
    printf '%s\n' "$@" > "$CARGO_RAIL_CONFIG_ARGV_LOG"
    ;;
esac
exec "$@"
"#,
    );
    write_executable(&tools_dir.join("linker"), "#!/bin/sh\nexec cc \"$@\"\n");

    let inputs = target_test_inputs(workspace.path());
    let targets = capture_target_identities(workspace.path(), &[], &inputs).expect("target identity should resolve");
    let target = targets.first().expect("configured target identity");
    assert_eq!(probe_flags(target.rustflags()), ["triple", "cfg"]);
    let canonical_workspace = canonicalize_existing(workspace.path()).expect("workspace should canonicalize");
    let linker = canonical_workspace.join("tools/linker");
    assert_eq!(target.linker(), Some(linker.as_os_str()));
    assert_eq!(
      inputs.toolchain.rustc_wrapper_program(),
      Some(canonical_workspace.join("tools/global-wrapper").as_os_str())
    );
    assert_eq!(
      inputs.toolchain.rustc_workspace_wrapper_program(),
      Some(canonical_workspace.join("tools/workspace-wrapper").as_os_str())
    );

    let cargo_cfg = run_cargo_rustc_cfg(workspace.path(), &[], &[]);
    assert!(cargo_cfg.contains("cargo_rail_config_triple"));
    assert!(cargo_cfg.contains("cargo_rail_config_cfg"));
    let wrappers = fs::read_to_string(workspace.path().join("wrapper.log")).expect("wrapper log should be readable");
    assert_eq!(wrappers.lines().collect::<Vec<_>>(), ["global", "workspace"]);
    let rustc_argv = fs::read_to_string(workspace.path().join("argv.log")).expect("rustc argv should be readable");
    assert!(
      rustc_argv
        .lines()
        .any(|argument| argument == format!("linker={}", linker.display())),
      "Cargo rustc argv did not contain the modeled linker: {rustc_argv}"
    );
  }

  #[cfg(unix)]
  #[test]
  fn cargo_target_runner_matches_cargo_execution() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    write_cargo_config_probe(workspace.path());
    let cargo_dir = workspace.path().join(".cargo");
    let tools_dir = workspace.path().join("tools");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::create_dir_all(&tools_dir).expect("probe tool directory should be created");
    let host = rustc_host();
    fs::write(
      cargo_dir.join("config.toml"),
      format!(
        r#"[build]
target = "host-tuple"

[target.{host}]
runner = ["tools/runner", "--fixed"]

[env]
CARGO_RAIL_CONFIG_RUNNER_LOG = {{ value = "runner.log", relative = true, force = true }}
"#
      ),
    )
    .expect("Cargo config should be written");
    write_executable(
      &tools_dir.join("runner"),
      "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CARGO_RAIL_CONFIG_RUNNER_LOG\"\n",
    );

    let inputs = target_test_inputs(workspace.path());
    let targets = capture_target_identities(workspace.path(), &[], &inputs).expect("target identity should resolve");
    let target = targets.first().expect("configured target identity");
    let canonical_workspace = canonicalize_existing(workspace.path()).expect("workspace should canonicalize");
    assert_eq!(
      target.runner(),
      Some(
        [
          canonical_workspace.join("tools/runner").into_os_string(),
          "--fixed".into(),
        ]
        .as_slice()
      )
    );

    let output = cargo_probe_command(workspace.path())
      .args(["run", "--offline", "--quiet"])
      .output()
      .expect("Cargo runner probe should run");
    assert!(
      output.status.success(),
      "Cargo runner probe failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let runner_argv = fs::read_to_string(workspace.path().join("runner.log")).expect("runner log should be readable");
    let runner_argv = runner_argv.lines().collect::<Vec<_>>();
    assert_eq!(runner_argv.first().copied(), Some("--fixed"));
    assert!(
      runner_argv
        .get(1)
        .is_some_and(|executable| executable.contains("cargo-rail-config-probe")),
      "Cargo did not append the built executable after fixed runner arguments: {runner_argv:?}"
    );
  }

  #[cfg(unix)]
  #[test]
  fn cargo_rustdoc_flags_match_the_selected_rustdoc_boundary() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    write_cargo_config_probe(workspace.path());
    let cargo_dir = workspace.path().join(".cargo");
    let tools_dir = workspace.path().join("tools");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::create_dir_all(&tools_dir).expect("probe tool directory should be created");
    let host = rustc_host();
    fs::write(
      cargo_dir.join("config.toml"),
      format!(
        r#"[build]
target = "host-tuple"
rustdoc = "tools/rustdoc-proxy"

[target.{host}]
rustdocflags = ["--cfg", "cargo_rail_config_rustdoc_triple"]

[target.'cfg(unix)']
rustdocflags = ["--cfg", "cargo_rail_config_rustdoc_cfg"]

[env]
CARGO_RAIL_CONFIG_RUSTDOC_LOG = {{ value = "rustdoc.log", relative = true, force = true }}
"#
      ),
    )
    .expect("Cargo config should be written");
    write_executable(
      &tools_dir.join("rustdoc-proxy"),
      r#"#!/bin/sh
case " $* " in
  *" cargo_rail_config_probe "*) printf '%s\n' "$@" > "$CARGO_RAIL_CONFIG_RUSTDOC_LOG" ;;
esac
exec rustdoc "$@"
"#,
    );

    let inputs = target_test_inputs(workspace.path());
    let targets = capture_target_identities(workspace.path(), &[], &inputs).expect("target identity should resolve");
    let target = targets.first().expect("configured target identity");
    assert_eq!(probe_flags(target.rustdocflags()), ["rustdoc_triple", "rustdoc_cfg"]);

    let output = cargo_probe_command(workspace.path())
      .args(["doc", "--offline", "--quiet", "--no-deps"])
      .output()
      .expect("Cargo rustdoc probe should run");
    assert!(
      output.status.success(),
      "Cargo rustdoc probe failed: {}",
      String::from_utf8_lossy(&output.stderr)
    );
    let rustdoc_argv =
      fs::read_to_string(workspace.path().join("rustdoc.log")).expect("rustdoc log should be readable");
    assert!(
      rustdoc_argv
        .lines()
        .any(|argument| argument == "cargo_rail_config_rustdoc_triple")
    );
    assert!(
      rustdoc_argv
        .lines()
        .any(|argument| argument == "cargo_rail_config_rustdoc_cfg")
    );
  }

  #[test]
  fn cargo_config_provenance_uses_snapshot_canonical_path_representation() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[build]\nrustflags = [\"--cfg\", \"snapshot\"]\n",
    )
    .expect("Cargo config should be written");

    let captured = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config capture should succeed");
    let source_root = canonicalize_existing(workspace.path()).expect("workspace should canonicalize");
    let repository_paths = captured
      .repository_config_paths(&source_root)
      .expect("repository config paths should normalize");

    assert!(
      repository_paths
        .iter()
        .any(|path| path.as_str() == ".cargo/config.toml"),
      "repository Cargo config was not recognized under snapshot root: {repository_paths:?}"
    );
  }

  #[cfg(unix)]
  #[test]
  fn target_identity_resolves_effective_tools_and_cargo_flag_fixed_point() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    let host = rustc_host();
    fs::write(
      cargo_dir.join("config.toml"),
      format!(
        r#"[build]
target = "host-tuple"
rustflags = ["--cfg", "ignored_build_flag"]
rustdocflags = ["--cfg", "ignored_build_doc_flag"]

[target.{host}]
linker = "tools/exact-linker"
rustflags = ["--cfg", "snapshot_exact"]
rustdocflags = ["--cfg", "snapshot_docs_exact"]

[target.'cfg(snapshot_exact)']
runner = "tools/cfg-runner --fixed"
linker = "tools/cfg-linker"
rustflags = ["--cfg", "snapshot_cfg"]
rustdocflags = ["--cfg", "snapshot_docs_cfg"]
"#
      ),
    )
    .expect("Cargo config should be written");

    let inputs = target_test_inputs(workspace.path());
    let targets = capture_target_identities(workspace.path(), &[], &inputs).expect("target identity should resolve");
    assert_eq!(targets.len(), 1);
    let target = &targets[0];
    assert!(matches!(
      target.specification(),
      TargetSpecificationIdentity::BuiltIn(triple) if triple == host
    ));
    assert!(target.is_host());
    assert!(target.is_build_target());
    assert!(target.cfg().iter().any(|cfg| cfg == "snapshot_exact"));
    assert!(target.cfg().iter().any(|cfg| cfg == "snapshot_cfg"));
    assert_eq!(target.rustflags(), ["--cfg", "snapshot_exact", "--cfg", "snapshot_cfg"]);
    assert_eq!(
      target.rustdocflags(),
      ["--cfg", "snapshot_docs_exact", "--cfg", "snapshot_docs_cfg"]
    );
    assert_eq!(target.host_artifact_rustflags(), Some([].as_slice()));
    assert_eq!(target.host_artifact_rustdocflags(), Some([].as_slice()));
    assert_eq!(
      target.runner(),
      Some(
        [
          canonicalize_existing(workspace.path())
            .expect("workspace should canonicalize")
            .join("tools/cfg-runner")
            .into_os_string(),
          "--fixed".into(),
        ]
        .as_slice()
      )
    );
    assert_eq!(
      target.linker(),
      Some(
        canonicalize_existing(workspace.path())
          .expect("workspace should canonicalize")
          .join("tools/exact-linker")
          .as_os_str()
      )
    );
  }

  #[test]
  fn target_identity_fails_closed_when_cfg_and_flags_do_not_converge() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[build]\ntarget = \"host-tuple\"\n\n[target.'cfg(not(snapshot_paradox))']\nrustflags = [\"--cfg\", \"snapshot_paradox\"]\n",
    )
    .expect("Cargo config should be written");

    let inputs = target_test_inputs(workspace.path());
    let error = capture_target_identities(workspace.path(), &[], &inputs)
      .expect_err("non-convergent target flags must fail closed");
    assert!(error.to_string().contains("non-convergent"), "{error}");
  }

  #[test]
  fn unstable_host_configuration_fails_closed() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[unstable]\ntarget-applies-to-host = false\n",
    )
    .expect("Cargo config should be written");

    let inputs = target_test_inputs(workspace.path());
    let error =
      capture_target_identities(workspace.path(), &[], &inputs).expect_err("unstable host semantics must fail closed");
    assert!(error.to_string().contains("unstable host configuration"), "{error}");
  }

  #[test]
  fn custom_target_capture_preserves_exact_json_bytes() {
    let workspace = tempfile::tempdir().expect("temporary target directory should be created");
    let path = workspace.path().join("custom-target.json");
    let bytes = b"{\n  \"llvm-target\": \"example\"\n}\n";
    fs::write(&path, bytes).expect("custom target should be written");

    let target = capture_custom_target(&path).expect("custom target should capture");
    assert_eq!(target.name(), "custom-target");
    assert_eq!(
      target.path(),
      canonicalize_existing(&path).expect("target path should canonicalize")
    );
    assert_eq!(target.bytes(), bytes);
    assert_eq!(target.digest(), ContentDigest::sha256(bytes));
  }

  #[test]
  fn selected_custom_target_must_be_accepted_by_the_selected_rustc() {
    let workspace = tempfile::tempdir().expect("temporary target directory should be created");
    write_cargo_config_probe(workspace.path());
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(cargo_dir.join("config.toml"), "[build]\ntarget = \"custom-target\"\n")
      .expect("Cargo config should be written");
    fs::write(
      workspace.path().join("custom-target"),
      "{\"llvm-target\":\"incomplete\"}\n",
    )
    .expect("custom target should be written");

    let inputs = target_test_inputs(workspace.path());
    let error = capture_target_identities(workspace.path(), &[], &inputs)
      .expect_err("incomplete custom target must fail rustc validation");
    assert!(error.to_string().contains("target cfg query failed"), "{error}");

    let cargo = cargo_probe_command(workspace.path())
      .args(["check", "--offline", "--quiet"])
      .output()
      .expect("Cargo custom-target probe should run");
    assert!(!cargo.status.success(), "Cargo must also reject the incomplete target");
    assert!(
      String::from_utf8_lossy(&cargo.stderr).contains("target specification"),
      "Cargo failed for an unrelated reason: {}",
      String::from_utf8_lossy(&cargo.stderr)
    );
  }

  #[test]
  fn multiple_matching_cfg_tools_fail_closed() {
    let workspace = tempfile::tempdir().expect("temporary target directory should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      r#"[target.'cfg(unix)']
runner = "runner-a"
linker = "linker-a"

[target.'cfg(target_os = "linux")']
runner = "runner-b"
linker = "linker-b"
"#,
    )
    .expect("Cargo config should be written");
    let cargo_config = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    let cfg = TargetCfgSet::from_rustc_output("unix\ntarget_os=\"linux\"\n");

    let runner = selected_target_runner(&cargo_config, "x86_64-unknown-linux-gnu", &cfg, workspace.path())
      .expect_err("ambiguous runner must fail closed");
    assert!(
      runner.to_string().contains("multiple Cargo target cfg runner"),
      "{runner}"
    );
    let linker = selected_target_linker(&cargo_config, "x86_64-unknown-linux-gnu", &cfg, workspace.path())
      .expect_err("ambiguous linker must fail closed");
    assert!(
      linker.to_string().contains("multiple Cargo target cfg linker"),
      "{linker}"
    );
  }

  #[test]
  fn encoded_flags_override_target_and_build_flags_even_when_empty() {
    let cargo_config = CargoConfigSnapshot {
      digest: ContentDigest::sha256(b"test-config"),
      effective_file_settings: serde_json::json!({
        "build": {"rustflags": ["--cfg", "build"]},
        "target": {"x86_64-unknown-linux-gnu": {"rustflags": ["--cfg", "target"]}}
      }),
      environment: BTreeMap::from([("CARGO_ENCODED_RUSTFLAGS".to_string(), String::new())]),
      provenance: Vec::new(),
      credential_capabilities: JsonValue::Object(JsonMap::new()),
      credential_provenance: None,
      unmodeled_settings: BTreeSet::new(),
    };
    let cfg = TargetCfgSet::from_rustc_output("unix\n");
    assert_eq!(
      effective_flags(
        &cargo_config,
        "x86_64-unknown-linux-gnu",
        Some(&cfg),
        CompilerFlags::Rust
      )
      .expect("encoded empty flags should resolve"),
      Vec::<String>::new()
    );
  }

  #[test]
  fn config_relative_wrapper_selection_uses_the_parent_of_dot_cargo() {
    let workspace = tempfile::tempdir().expect("temporary target directory should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[build]\nrustc-wrapper = \"tools/wrapper\"\n",
    )
    .expect("Cargo config should be written");
    let mut cargo_config = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    cargo_config.environment.remove("CARGO_BUILD_RUSTC_WRAPPER");
    cargo_config.environment.remove("RUSTC_WRAPPER");

    let wrapper = selected_program(
      &cargo_config,
      workspace.path(),
      RUSTC_WRAPPER_ENV_PRECEDENCE,
      &["build", "rustc-wrapper"],
      None,
      "rustc wrapper",
    )
    .expect("wrapper selection should resolve")
    .expect("wrapper should be selected");
    assert_eq!(
      wrapper,
      canonicalize_existing(workspace.path())
        .expect("workspace should canonicalize")
        .join("tools/wrapper")
        .into_os_string()
    );
  }

  #[cfg(unix)]
  #[test]
  fn rustc_identity_query_runs_through_cargos_wrapper_chain() {
    use std::os::unix::fs::PermissionsExt as _;

    let workspace = tempfile::tempdir().expect("temporary target directory should be created");
    let cargo_dir = workspace.path().join(".cargo");
    let tools_dir = workspace.path().join("tools");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::create_dir_all(&tools_dir).expect("wrapper directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      r#"[build]
rustc-wrapper = "tools/global-wrapper"
rustc-workspace-wrapper = "tools/workspace-wrapper"

[env]
CARGO_RAIL_WRAPPER_LOG = { value = "wrapper.log", relative = true, force = true }
"#,
    )
    .expect("Cargo config should be written");
    for (name, marker) in [("global-wrapper", "global"), ("workspace-wrapper", "workspace")] {
      let path = tools_dir.join(name);
      fs::write(
        &path,
        format!("#!/bin/sh\nprintf '{marker}\\n' >> \"$CARGO_RAIL_WRAPPER_LOG\"\nexec \"$@\"\n"),
      )
      .expect("wrapper should be written");
      fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("wrapper should be executable");
    }

    let mut cargo_config = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    for name in [
      "RUSTC_WRAPPER",
      "CARGO_BUILD_RUSTC_WRAPPER",
      "RUSTC_WORKSPACE_WRAPPER",
      "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    ] {
      cargo_config.environment.remove(name);
    }
    let toolchain =
      ToolchainIdentity::capture(workspace.path(), &cargo_config).expect("wrapped rustc identity should resolve");

    assert!(toolchain.rustc_verbose_version().starts_with("rustc "));
    let log = fs::read_to_string(workspace.path().join("wrapper.log")).expect("wrapper log should be readable");
    assert_eq!(
      log.lines().collect::<Vec<_>>(),
      ["global", "workspace", "global", "workspace"]
    );
  }

  #[test]
  fn direct_tool_environment_outranks_build_environment() {
    let selections = [
      (
        "CARGO_BUILD_RUSTC",
        "RUSTC",
        RUSTC_ENV_PRECEDENCE,
        &["build", "rustc"][..],
        Some("rustc"),
        "rustc",
      ),
      (
        "CARGO_BUILD_RUSTDOC",
        "RUSTDOC",
        RUSTDOC_ENV_PRECEDENCE,
        &["build", "rustdoc"][..],
        Some("rustdoc"),
        "rustdoc",
      ),
      (
        "CARGO_BUILD_RUSTC_WRAPPER",
        "RUSTC_WRAPPER",
        RUSTC_WRAPPER_ENV_PRECEDENCE,
        &["build", "rustc-wrapper"][..],
        None,
        "rustc wrapper",
      ),
      (
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        RUSTC_WORKSPACE_WRAPPER_ENV_PRECEDENCE,
        &["build", "rustc-workspace-wrapper"][..],
        None,
        "workspace rustc wrapper",
      ),
    ];
    let environment = selections
      .iter()
      .flat_map(|(build, direct, ..)| {
        [
          ((*build).to_string(), format!("build-{direct}")),
          ((*direct).to_string(), format!("direct-{direct}")),
        ]
      })
      .collect();
    let cargo_config = CargoConfigSnapshot {
      digest: ContentDigest::sha256(b"test-config"),
      effective_file_settings: JsonValue::Object(JsonMap::new()),
      environment,
      provenance: Vec::new(),
      credential_capabilities: JsonValue::Object(JsonMap::new()),
      credential_provenance: None,
      unmodeled_settings: BTreeSet::new(),
    };

    for (_, direct, precedence, config_path, default, description) in selections {
      let selected = selected_program(
        &cargo_config,
        Path::new("/workspace"),
        precedence,
        config_path,
        default,
        description,
      )
      .expect("direct program selection should resolve");
      assert_eq!(
        selected.as_deref(),
        Some(OsStr::new(&format!("direct-{direct}"))),
        "{direct} must outrank its CARGO_BUILD_* compatibility variable"
      );
    }
  }

  #[test]
  fn derived_view_rejects_cargo_config_changes_after_key_capture() {
    let workspace = tempfile::tempdir().expect("temporary workspace should be created");
    fs::create_dir_all(workspace.path().join("src")).expect("source directory should be created");
    fs::create_dir_all(workspace.path().join(".cargo")).expect("Cargo config directory should be created");
    fs::write(
      workspace.path().join("Cargo.toml"),
      "[package]\nname = \"resolution-config-change\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("manifest should be written");
    fs::write(workspace.path().join("src/lib.rs"), "pub fn value() {}\n").expect("source should be written");
    let config_path = workspace.path().join(".cargo/config.toml");
    fs::write(&config_path, "[build]\nrustflags = []\n").expect("initial Cargo config should be written");

    let metadata = Arc::new(
      MetadataCommand::new()
        .current_dir(workspace.path())
        .exec()
        .expect("fixture metadata should load"),
    );
    let graph = Arc::new(WorkspaceGraph::from_metadata(&metadata).expect("fixture graph should build"));
    let views = ResolutionViews::new(
      workspace.path().to_path_buf(),
      workspace.path().to_path_buf(),
      metadata,
      graph,
    );
    views
      .view(
        ResolutionRequest::new(
          ResolutionPackages::Workspace,
          ResolutionFeatures::NoDefaultFeatures,
          None,
        )
        .expect("first derived request should be valid"),
      )
      .expect("first derived view should load and freeze its config identity");

    fs::write(&config_path, "[build]\nrustflags = [\"--cfg\", \"changed\"]\n")
      .expect("changed Cargo config should be written");
    let error = match views.view(
      ResolutionRequest::new(ResolutionPackages::Workspace, ResolutionFeatures::AllFeatures, None)
        .expect("second derived request should be valid"),
    ) {
      Ok(_) => panic!("a changed Cargo config must not execute under the old cache key"),
      Err(error) => error,
    };
    assert!(error.to_string().contains("Cargo configuration changed"), "{error}");
  }

  #[test]
  fn cargo_env_capture_preserves_existing_overrides_and_resolves_relative_values() {
    let workspace = tempfile::tempdir().expect("temporary target directory should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[env]\nPATH = \"configured-but-not-forced\"\nCARGO_RAIL_RELATIVE_TEST = { value = \"tools\", relative = true, force = true }\n",
    )
    .expect("Cargo config should be written");
    let cargo_config = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    assert_eq!(
      cargo_config.environment().get("PATH").map(String::as_str),
      std::env::var("PATH").ok().as_deref()
    );

    let mut command = Command::new("rustc");
    apply_cargo_environment(&mut command, &cargo_config).expect("Cargo environment should apply");
    let environment = command
      .get_envs()
      .map(|(name, value)| (name.to_os_string(), value.map(OsStr::to_os_string)))
      .collect::<BTreeMap<_, _>>();
    assert_eq!(
      environment.get(OsStr::new("PATH")).and_then(Option::as_deref),
      std::env::var_os("PATH").as_deref()
    );
    assert_eq!(
      environment
        .get(OsStr::new("CARGO_RAIL_RELATIVE_TEST"))
        .and_then(Option::as_deref),
      Some(
        canonicalize_existing(workspace.path())
          .expect("workspace should canonicalize")
          .join("tools")
          .as_os_str()
      )
    );
  }

  #[test]
  fn materialized_cargo_environment_remaps_repository_relative_values() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[env]\nCARGO_RAIL_LITERAL = { value = \"stable\", force = true }\nCARGO_RAIL_RELATIVE = { value = \"tools\", relative = true, force = true }\n",
    )
    .expect("Cargo config should be written");
    let captured = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    let source_root = canonicalize_existing(workspace.path()).expect("workspace should canonicalize");
    let destination = Path::new("/isolated/workspace");
    let bindings = captured
      .materialized_environment(&source_root, destination)
      .expect("Cargo environment should materialize");
    assert!(bindings.contains(&(
      "CARGO_RAIL_LITERAL".to_string(),
      OsString::from("stable"),
      "stable".to_string()
    )));
    assert!(bindings.contains(&(
      "CARGO_RAIL_RELATIVE".to_string(),
      destination.join("tools").into_os_string(),
      "repository:tools".to_string()
    )));
  }

  #[test]
  fn acquisition_identity_excludes_build_only_configuration() {
    let workspace = tempfile::tempdir().expect("temporary Cargo workspace should be created");
    let cargo_dir = workspace.path().join(".cargo");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    let config = cargo_dir.join("config.toml");
    let write = |rustflag: &str, registry: &str| {
      fs::write(
        &config,
        format!(
          "[build]\nrustflags = [\"--cfg\", \"{rustflag}\"]\n[env]\nCARGO_RAIL_BUILD_VALUE = {{ value = \"{rustflag}\", force = true }}\n[source.crates-io]\nreplace-with = \"mirror\"\n[source.mirror]\nregistry = \"{registry}\"\n"
        ),
      )
      .expect("Cargo config should be written");
    };
    write("first", "https://example.invalid/first-index");
    let first = CargoConfigSnapshot::capture(workspace.path())
      .expect("first Cargo config should capture")
      .portable_acquisition_identity(workspace.path())
      .expect("first acquisition identity should encode");
    write("second", "https://example.invalid/first-index");
    let build_changed = CargoConfigSnapshot::capture(workspace.path())
      .expect("changed build config should capture")
      .portable_acquisition_identity(workspace.path())
      .expect("changed build acquisition identity should encode");
    assert_eq!(first, build_changed);

    write("second", "https://example.invalid/second-index");
    let acquisition_changed = CargoConfigSnapshot::capture(workspace.path())
      .expect("changed acquisition config should capture")
      .portable_acquisition_identity(workspace.path())
      .expect("changed acquisition identity should encode");
    assert_ne!(first, acquisition_changed);
  }

  #[test]
  fn cargo_env_rust_target_path_participates_in_custom_target_lookup() {
    let workspace = tempfile::tempdir().expect("temporary target directory should be created");
    let cargo_dir = workspace.path().join(".cargo");
    let target_dir = workspace.path().join("targets");
    fs::create_dir_all(&cargo_dir).expect("Cargo config directory should be created");
    fs::create_dir_all(&target_dir).expect("target directory should be created");
    fs::write(
      cargo_dir.join("config.toml"),
      "[env]\nRUST_TARGET_PATH = { value = \"targets\", relative = true, force = true }\n",
    )
    .expect("Cargo config should be written");
    let target_path = target_dir.join("custom.json");
    fs::write(&target_path, "{}\n").expect("custom target should be written");

    let cargo_config = CargoConfigSnapshot::capture(workspace.path()).expect("Cargo config should capture");
    let selected = custom_target_lookup_path("custom", workspace.path(), &cargo_config)
      .expect("custom target lookup should resolve");
    assert_eq!(
      selected.as_deref(),
      Some(
        canonicalize_existing(&target_path)
          .expect("custom target should canonicalize")
          .as_path()
      )
    );
  }

  #[test]
  fn rustc_wrapper_chain_nests_global_then_workspace_then_compiler() {
    let toolchain = ToolchainIdentity {
      cargo_program: "cargo".into(),
      cargo_verbose_version: "cargo 1.95.0".to_string(),
      rustc_program: "selected-rustc".into(),
      rustc_verbose_version: "rustc 1.95.0\nhost: x86_64-unknown-linux-gnu".to_string(),
      rustdoc_program: "rustdoc".into(),
      rustdoc_verbose_version: "rustdoc 1.95.0".to_string(),
      rustc_wrapper_program: Some("sccache".into()),
      rustc_workspace_wrapper_program: Some("workspace-wrapper".into()),
      host_target: "x86_64-unknown-linux-gnu".to_string(),
      rustc_sysroot: PathBuf::from("/toolchain"),
    };
    let command = rustc_command(&toolchain);
    assert_eq!(command.get_program(), OsStr::new("sccache"));
    assert_eq!(
      command.get_args().collect::<Vec<_>>(),
      [OsStr::new("workspace-wrapper"), OsStr::new("selected-rustc")]
    );

    let diagnostics = crate::compiler::wrapper::rustc_command(
      OsStr::new("selected-rustc"),
      Some(OsStr::new("sccache")),
      Some(OsStr::new("cargo-rail")),
    );
    assert_eq!(diagnostics.get_program(), OsStr::new("sccache"));
    assert_eq!(
      diagnostics.get_args().collect::<Vec<_>>(),
      [OsStr::new("cargo-rail"), OsStr::new("selected-rustc")]
    );
    let inner = crate::compiler::wrapper::rustc_command(
      OsStr::new("selected-rustc"),
      None,
      Some(OsStr::new("workspace-wrapper")),
    );
    assert_eq!(inner.get_program(), OsStr::new("workspace-wrapper"));
    assert_eq!(inner.get_args().collect::<Vec<_>>(), [OsStr::new("selected-rustc")]);
  }

  #[test]
  fn resolution_key_distinguishes_every_authoritative_input() {
    let package = PackageId {
      repr: "path+file:///workspace#member@0.1.0".to_string(),
    };
    let base = ResolutionViewKey {
      request: ResolutionRequest::new(
        ResolutionPackages::Workspace,
        ResolutionFeatures::Default,
        Some("x86_64-unknown-linux-gnu".to_string()),
      )
      .expect("base request should be valid"),
      toolchain: ToolchainIdentity {
        cargo_program: "cargo".into(),
        cargo_verbose_version: "cargo 1.95.0".to_string(),
        rustc_program: "rustc".into(),
        rustc_verbose_version: "rustc 1.95.0".to_string(),
        rustdoc_program: "rustdoc".into(),
        rustdoc_verbose_version: "rustdoc 1.95.0".to_string(),
        rustc_wrapper_program: None,
        rustc_workspace_wrapper_program: None,
        host_target: "x86_64-unknown-linux-gnu".to_string(),
        rustc_sysroot: PathBuf::from("/toolchain"),
      },
      cargo_config: ContentDigest::sha256(b"config-a"),
      credential_sensitive: false,
    };
    let mut keys = HashSet::from([base.clone()]);

    let mut changed = base.clone();
    changed.request.packages = ResolutionPackages::Selected(BTreeSet::from([package]));
    keys.insert(changed);
    let mut changed = base.clone();
    changed.request.features = ResolutionFeatures::NoDefaultFeatures;
    keys.insert(changed);
    let mut changed = base.clone();
    changed.request.target_filter = Some("x86_64-pc-windows-msvc".to_string());
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.cargo_verbose_version = "cargo 1.96.0".to_string();
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.cargo_program = "/opt/cargo".into();
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.rustc_verbose_version = "rustc 1.96.0".to_string();
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.rustc_program = "/opt/rustc".into();
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.rustdoc_verbose_version = "rustdoc 1.96.0".to_string();
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.rustdoc_program = "/opt/rustdoc".into();
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.rustc_wrapper_program = Some("sccache".into());
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.rustc_workspace_wrapper_program = Some("workspace-wrapper".into());
    keys.insert(changed);
    let mut changed = base.clone();
    changed.toolchain.host_target = "aarch64-unknown-linux-gnu".to_string();
    keys.insert(changed);
    let mut changed = base.clone();
    changed.cargo_config = ContentDigest::sha256(b"config-b");
    keys.insert(changed);
    let mut changed = base;
    changed.credential_sensitive = true;
    keys.insert(changed);

    assert_eq!(keys.len(), 15);
  }
}
