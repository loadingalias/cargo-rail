//! Non-circular pre-execution identities for Cargo build scripts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compiler::observation::{CompilationProfile, CompilationRole, FileObservation, ObservationPath};
use crate::error::RailResult;
use crate::source::ContentDigest;

pub(crate) mod result;

pub(crate) use result::{
  BuildScriptCargoOutputSummary, BuildScriptResultAnalysis, BuildScriptResultInputs, analyze_result,
};

const BUILD_SCRIPT_ACTION_KEY_VERSION: u32 = 1;
const BUILD_SCRIPT_ACTION_SEMANTICS_VERSION: u32 = 1;

/// Diagnostic result of deriving one build-script action key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BuildScriptActionKeyAnalysis {
  version: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  key: Option<String>,
  source_inputs: usize,
  environment_entries: usize,
  dependency_results: usize,
  #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
  secret_capabilities: BTreeSet<String>,
  reasons: BTreeSet<String>,
}

impl BuildScriptActionKeyAnalysis {
  /// Return the authorizing pre-execution key when every input was proven.
  pub(crate) fn key(&self) -> Option<&str> {
    self.key.as_deref()
  }
}

/// Exact inputs known immediately before one build-script process starts.
///
/// The script's emitted instructions, observed reads, and generated files are
/// intentionally absent: those are result fields and cannot identify their own
/// producing action.
#[derive(Debug, Clone)]
pub(crate) struct BuildScriptActionInputs {
  pub(crate) compiled_artifact: Option<FileObservation>,
  pub(crate) source_inputs: Vec<FileObservation>,
  pub(crate) manifest_closure: Option<String>,
  pub(crate) lock_closure: Option<String>,
  pub(crate) toolchain: Option<String>,
  pub(crate) action_id: String,
  pub(crate) package: String,
  pub(crate) arguments: Vec<String>,
  pub(crate) working_directory: Option<String>,
  pub(crate) host_target: String,
  pub(crate) target: String,
  pub(crate) target_identity: Option<String>,
  pub(crate) role: CompilationRole,
  pub(crate) profile: CompilationProfile,
  pub(crate) features: BTreeSet<String>,
  pub(crate) cfg: BTreeSet<String>,
  pub(crate) configuration: Option<String>,
  /// Complete portable non-secret environment, or `None` while ambient values
  /// can still reach the process.
  pub(crate) environment: Option<BTreeMap<String, String>>,
  pub(crate) secret_environment: BTreeSet<String>,
  /// Actions that must complete before this script runs and their verified
  /// result digests. The sets must match exactly.
  pub(crate) dependency_actions: BTreeSet<String>,
  pub(crate) dependency_results: Option<BTreeMap<String, String>>,
  /// Stable logical launch layout. These remain absent until execution is
  /// isolated from ambient filesystem, process, network, clock, randomness,
  /// and persistent output-directory state.
  pub(crate) executable_path: Option<String>,
  pub(crate) output_root: Option<String>,
  pub(crate) platform_identity: Option<String>,
}

#[derive(Debug, Serialize)]
struct BuildScriptArtifactIdentity<'a> {
  content_digest: &'a str,
  executable: bool,
  symlink_target: Option<&'a str>,
}

#[derive(Serialize)]
struct BuildScriptActionKeyMaterial<'a> {
  version: u32,
  semantics_version: u32,
  compiled_artifact: BuildScriptArtifactIdentity<'a>,
  source_inputs: &'a [FileObservation],
  manifest_closure: &'a str,
  lock_closure: &'a str,
  toolchain: &'a str,
  action_id: &'a str,
  package: &'a str,
  arguments: &'a [String],
  working_directory: &'a str,
  host_target: &'a str,
  target: &'a str,
  target_identity: &'a str,
  role: CompilationRole,
  profile: &'a CompilationProfile,
  features: &'a BTreeSet<String>,
  cfg: &'a BTreeSet<String>,
  configuration: &'a str,
  environment: &'a BTreeMap<String, String>,
  dependency_results: &'a BTreeMap<String, String>,
  executable_path: &'a str,
  output_root: &'a str,
  platform_identity: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BuildScriptActionKey {
  version: u32,
  digest: ContentDigest,
}

impl fmt::Display for BuildScriptActionKey {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "build-script-v{}-sha256-{}", self.version, self.digest)
  }
}

/// Derive an authorizing key only when every pre-execution input is complete.
pub(crate) fn analyze_action_key(
  source_root: &Path,
  mut inputs: BuildScriptActionInputs,
) -> RailResult<BuildScriptActionKeyAnalysis> {
  inputs.source_inputs.sort();
  inputs.source_inputs.dedup();

  let mut reasons = BTreeSet::new();
  if inputs.compiled_artifact.is_none() {
    reasons.insert("compiled_build_script_artifact_unavailable".to_string());
  }
  if inputs.source_inputs.is_empty() {
    reasons.insert("build_script_source_inputs_unavailable".to_string());
  }
  for input in &inputs.source_inputs {
    if matches!(input.path, ObservationPath::Host(_)) {
      reasons.insert("build_script_source_input_outside_repository".to_string());
    }
  }
  require(
    &inputs.manifest_closure,
    "build_script_manifest_closure_unavailable",
    &mut reasons,
  );
  require(
    &inputs.lock_closure,
    "build_script_lock_closure_unavailable",
    &mut reasons,
  );
  require(
    &inputs.toolchain,
    "build_script_toolchain_identity_unavailable",
    &mut reasons,
  );
  require(
    &inputs.working_directory,
    "build_script_working_directory_unavailable",
    &mut reasons,
  );
  require(
    &inputs.target_identity,
    "build_script_target_identity_unavailable",
    &mut reasons,
  );
  require(
    &inputs.configuration,
    "build_script_configuration_identity_unavailable",
    &mut reasons,
  );
  if inputs.role != CompilationRole::Host {
    reasons.insert("build_script_host_role_unproven".to_string());
  }
  if inputs.action_id.is_empty() {
    reasons.insert("build_script_action_identity_unavailable".to_string());
  }
  if inputs.package.is_empty() {
    reasons.insert("build_script_package_identity_unavailable".to_string());
  }
  if inputs.host_target.is_empty() || inputs.target.is_empty() {
    reasons.insert("build_script_platform_role_unavailable".to_string());
  }

  if !inputs.secret_environment.is_empty() {
    reasons.insert("secret_build_script_environment".to_string());
  }
  let environment_entries = inputs.environment.as_ref().map_or(0, BTreeMap::len);
  if inputs.environment.is_none() {
    reasons.insert("build_script_environment_uncontrolled".to_string());
  }
  let dependency_result_count = inputs.dependency_results.as_ref().map_or(0, BTreeMap::len);
  match &inputs.dependency_results {
    Some(results)
      if results
        .keys()
        .map(String::as_str)
        .eq(inputs.dependency_actions.iter().map(String::as_str)) => {}
    Some(_) => {
      reasons.insert("build_script_dependency_result_set_incomplete".to_string());
    }
    None => {
      reasons.insert("build_script_dependency_results_unavailable".to_string());
    }
  }
  if inputs.dependency_actions.contains(&inputs.action_id) {
    reasons.insert("build_script_result_cycle".to_string());
  }
  if [
    inputs.executable_path.as_deref(),
    inputs.output_root.as_deref(),
    inputs.platform_identity.as_deref(),
  ]
  .into_iter()
  .any(|value| value.is_none_or(str::is_empty))
  {
    reasons.insert("build_script_ambient_inputs_unobserved".to_string());
  }
  if inputs
    .working_directory
    .as_deref()
    .is_some_and(|path| !path.starts_with("repository:"))
    || inputs
      .executable_path
      .as_deref()
      .is_some_and(|path| !path.starts_with("execution:"))
    || inputs
      .output_root
      .as_deref()
      .is_some_and(|path| !path.starts_with("output:"))
  {
    reasons.insert("build_script_logical_layout_invalid".to_string());
  }
  if inputs
    .environment
    .as_ref()
    .is_some_and(|environment| environment.keys().any(String::is_empty))
  {
    reasons.insert("build_script_environment_name_invalid".to_string());
  }
  if inputs.dependency_actions.iter().any(String::is_empty)
    || inputs.dependency_results.as_ref().is_some_and(|results| {
      results
        .iter()
        .any(|(action, result)| action.is_empty() || result.is_empty())
    })
  {
    reasons.insert("build_script_dependency_result_invalid".to_string());
  }

  if reasons.is_empty() {
    if inputs
      .compiled_artifact
      .as_ref()
      .is_some_and(|artifact| !artifact.revalidate(source_root))
    {
      reasons.insert("compiled_build_script_artifact_changed".to_string());
    }
    if inputs.source_inputs.iter().any(|input| !input.revalidate(source_root)) {
      reasons.insert("build_script_source_input_changed".to_string());
    }
    if contains_physical_source_root(&inputs, source_root) {
      reasons.insert("physical_checkout_path_in_build_script_key".to_string());
    }
  }

  let key = if reasons.is_empty()
    && let Some(material) = key_material(&inputs)
  {
    let mut bytes = Vec::from(&b"cargo-rail-build-script-action-key\0"[..]);
    bytes.extend(serde_json::to_vec(&material)?);
    Some(BuildScriptActionKey {
      version: BUILD_SCRIPT_ACTION_KEY_VERSION,
      digest: ContentDigest::sha256(&bytes),
    })
  } else {
    None
  }
  .map(|key| key.to_string());
  if reasons.is_empty() && key.is_none() {
    reasons.insert("build_script_pre_execution_inputs_incomplete".to_string());
  }

  Ok(BuildScriptActionKeyAnalysis {
    version: BUILD_SCRIPT_ACTION_KEY_VERSION,
    key,
    source_inputs: inputs.source_inputs.len(),
    environment_entries,
    dependency_results: dependency_result_count,
    secret_capabilities: inputs.secret_environment,
    reasons,
  })
}

fn require(value: &Option<String>, reason: &'static str, reasons: &mut BTreeSet<String>) {
  if value.as_deref().is_none_or(str::is_empty) {
    reasons.insert(reason.to_string());
  }
}

fn key_material(inputs: &BuildScriptActionInputs) -> Option<BuildScriptActionKeyMaterial<'_>> {
  let compiled_artifact = inputs.compiled_artifact.as_ref()?;
  Some(BuildScriptActionKeyMaterial {
    version: BUILD_SCRIPT_ACTION_KEY_VERSION,
    semantics_version: BUILD_SCRIPT_ACTION_SEMANTICS_VERSION,
    compiled_artifact: BuildScriptArtifactIdentity {
      content_digest: &compiled_artifact.content_digest,
      executable: compiled_artifact.executable,
      symlink_target: compiled_artifact.symlink_target.as_deref(),
    },
    source_inputs: &inputs.source_inputs,
    manifest_closure: inputs.manifest_closure.as_deref()?,
    lock_closure: inputs.lock_closure.as_deref()?,
    toolchain: inputs.toolchain.as_deref()?,
    action_id: &inputs.action_id,
    package: &inputs.package,
    arguments: &inputs.arguments,
    working_directory: inputs.working_directory.as_deref()?,
    host_target: &inputs.host_target,
    target: &inputs.target,
    target_identity: inputs.target_identity.as_deref()?,
    role: inputs.role,
    profile: &inputs.profile,
    features: &inputs.features,
    cfg: &inputs.cfg,
    configuration: inputs.configuration.as_deref()?,
    environment: inputs.environment.as_ref()?,
    dependency_results: inputs.dependency_results.as_ref()?,
    executable_path: inputs.executable_path.as_deref()?,
    output_root: inputs.output_root.as_deref()?,
    platform_identity: inputs.platform_identity.as_deref()?,
  })
}

fn contains_physical_source_root(inputs: &BuildScriptActionInputs, source_root: &Path) -> bool {
  let roots = physical_source_roots(source_root);
  let contains_root = |value: &str| roots.iter().any(|root| value.contains(root));
  inputs.arguments.iter().any(|value| contains_root(value))
    || inputs.working_directory.as_deref().is_some_and(contains_root)
    || contains_root(&inputs.host_target)
    || contains_root(&inputs.target)
    || inputs.target_identity.as_deref().is_some_and(contains_root)
    || inputs.environment.as_ref().is_some_and(|environment| {
      environment
        .iter()
        .any(|(name, value)| contains_root(name) || contains_root(value))
    })
    || inputs.dependency_actions.iter().any(|action| contains_root(action))
    || inputs.dependency_results.as_ref().is_some_and(|results| {
      results
        .iter()
        .any(|(action, result)| contains_root(action) || contains_root(result))
    })
    || [
      inputs.manifest_closure.as_deref(),
      inputs.lock_closure.as_deref(),
      inputs.toolchain.as_deref(),
      Some(inputs.action_id.as_str()),
      Some(inputs.package.as_str()),
      inputs.configuration.as_deref(),
      inputs.executable_path.as_deref(),
      inputs.output_root.as_deref(),
      inputs.platform_identity.as_deref(),
      inputs
        .compiled_artifact
        .as_ref()
        .and_then(|artifact| artifact.symlink_target.as_deref()),
    ]
    .into_iter()
    .flatten()
    .any(contains_root)
    || inputs
      .source_inputs
      .iter()
      .filter_map(|input| input.symlink_target.as_deref())
      .any(contains_root)
}

fn physical_source_roots(source_root: &Path) -> Vec<String> {
  let mut roots = vec![
    source_root.to_string_lossy().into_owned(),
    crate::utils::path_to_git_format(source_root),
  ];
  if let Ok(canonical) = std::fs::canonicalize(source_root) {
    roots.push(canonical.to_string_lossy().into_owned());
    roots.push(crate::utils::path_to_git_format(&canonical));
  }
  roots.sort();
  roots.dedup();
  roots.retain(|root| !root.is_empty());
  roots
}

#[cfg(test)]
mod tests {
  use std::fs;

  use tempfile::TempDir;

  use super::*;

  fn observation(root: &Path, relative: &str, bytes: &[u8]) -> FileObservation {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).expect("create observation parent");
    }
    fs::write(&path, bytes).expect("write observation file");
    FileObservation::capture(&path, root, root).expect("capture observation")
  }

  fn complete_inputs(root: &Path) -> BuildScriptActionInputs {
    BuildScriptActionInputs {
      compiled_artifact: Some(observation(root, "target/build-script", b"program")),
      source_inputs: vec![observation(root, "crates/example/build.rs", b"fn main() {}")],
      manifest_closure: Some("sha256:manifest".to_string()),
      lock_closure: Some("sha256:lock".to_string()),
      toolchain: Some("sha256:toolchain".to_string()),
      action_id: "build-script:example".to_string(),
      package: "local:crates/example/Cargo.toml#example@0.1.0".to_string(),
      arguments: Vec::new(),
      working_directory: Some("repository:crates/example".to_string()),
      host_target: "aarch64-apple-darwin".to_string(),
      target: "x86_64-unknown-linux-gnu".to_string(),
      target_identity: Some("sha256:target".to_string()),
      role: CompilationRole::Host,
      profile: CompilationProfile {
        opt_level: "0".to_string(),
        debuginfo: "2".to_string(),
        debug_assertions: true,
        overflow_checks: true,
        test: false,
      },
      features: BTreeSet::from(["native".to_string()]),
      cfg: BTreeSet::from(["target_os=linux".to_string()]),
      configuration: Some("sha256:configuration".to_string()),
      environment: Some(BTreeMap::from([
        (
          "CARGO_MANIFEST_DIR".to_string(),
          "repository:crates/example".to_string(),
        ),
        ("OUT_DIR".to_string(), "output:out".to_string()),
      ])),
      secret_environment: BTreeSet::new(),
      dependency_actions: BTreeSet::from(["compile:build-dependency".to_string()]),
      dependency_results: Some(BTreeMap::from([(
        "compile:build-dependency".to_string(),
        "v1-sha256-dependency".to_string(),
      )])),
      executable_path: Some("execution:build-script".to_string()),
      output_root: Some("output:out".to_string()),
      platform_identity: Some("sha256:platform".to_string()),
    }
  }

  fn key(root: &Path, inputs: BuildScriptActionInputs) -> Option<String> {
    analyze_action_key(root, inputs).expect("analyze build-script key").key
  }

  #[test]
  fn build_script_action_key_changes_for_every_pre_execution_domain() {
    type Mutation = Box<dyn Fn(&mut BuildScriptActionInputs)>;

    let root = TempDir::new().expect("temporary source root");
    let inputs = complete_inputs(root.path());
    let baseline = key(root.path(), inputs.clone()).expect("complete inputs issue a key");
    assert!(baseline.starts_with("build-script-v1-sha256-"));
    let mutations: Vec<(&str, Mutation)> = vec![
      (
        "artifact",
        Box::new(|inputs| inputs.compiled_artifact.as_mut().unwrap().content_digest.push('0')),
      ),
      (
        "source",
        Box::new(|inputs| inputs.source_inputs[0].content_digest.push('0')),
      ),
      (
        "manifests",
        Box::new(|inputs| inputs.manifest_closure.as_mut().unwrap().push('0')),
      ),
      (
        "lock",
        Box::new(|inputs| inputs.lock_closure.as_mut().unwrap().push('0')),
      ),
      (
        "toolchain",
        Box::new(|inputs| inputs.toolchain.as_mut().unwrap().push('0')),
      ),
      ("action id", Box::new(|inputs| inputs.action_id.push('0'))),
      ("package", Box::new(|inputs| inputs.package.push('0'))),
      ("argv", Box::new(|inputs| inputs.arguments.push("--probe".to_string()))),
      (
        "working directory",
        Box::new(|inputs| inputs.working_directory.as_mut().unwrap().push('0')),
      ),
      ("host", Box::new(|inputs| inputs.host_target.push('0'))),
      ("target", Box::new(|inputs| inputs.target.push('0'))),
      (
        "target identity",
        Box::new(|inputs| inputs.target_identity.as_mut().unwrap().push('0')),
      ),
      ("role", Box::new(|inputs| inputs.role = CompilationRole::Target)),
      ("profile", Box::new(|inputs| inputs.profile.opt_level.push('1'))),
      (
        "features",
        Box::new(|inputs| {
          inputs.features.insert("simd".to_string());
        }),
      ),
      (
        "cfg",
        Box::new(|inputs| {
          inputs.cfg.insert("target_env=gnu".to_string());
        }),
      ),
      (
        "configuration",
        Box::new(|inputs| inputs.configuration.as_mut().unwrap().push('0')),
      ),
      (
        "environment",
        Box::new(|inputs| {
          let values = inputs.environment.as_mut().unwrap();
          values.insert("CC".to_string(), "literal:clang".to_string());
        }),
      ),
      (
        "dependency result",
        Box::new(|inputs| {
          let results = inputs.dependency_results.as_mut().unwrap();
          results.get_mut("compile:build-dependency").unwrap().push('0');
        }),
      ),
      (
        "dependency action",
        Box::new(|inputs| {
          let result = inputs
            .dependency_results
            .as_mut()
            .unwrap()
            .remove("compile:build-dependency")
            .unwrap();
          inputs.dependency_actions.remove("compile:build-dependency");
          inputs
            .dependency_actions
            .insert("compile:other-build-dependency".to_string());
          inputs
            .dependency_results
            .as_mut()
            .unwrap()
            .insert("compile:other-build-dependency".to_string(), result);
        }),
      ),
      (
        "executable path",
        Box::new(|inputs| inputs.executable_path.as_mut().unwrap().push('0')),
      ),
      (
        "output root",
        Box::new(|inputs| {
          inputs.output_root.as_mut().unwrap().push('0');
        }),
      ),
      (
        "platform identity",
        Box::new(|inputs| inputs.platform_identity.as_mut().unwrap().push('0')),
      ),
    ];

    for (name, mutate) in mutations {
      let mut changed = inputs.clone();
      mutate(&mut changed);
      assert_ne!(
        key(root.path(), changed).as_deref(),
        Some(baseline.as_str()),
        "{name} collided"
      );
    }
  }

  #[test]
  fn build_script_action_key_is_checkout_root_independent() {
    let left = TempDir::new().expect("left source root");
    let right = TempDir::new().expect("right source root");
    let left_key = key(left.path(), complete_inputs(left.path()));
    let right_key = key(right.path(), complete_inputs(right.path()));
    assert_eq!(left_key, right_key);
  }

  #[test]
  fn build_script_action_key_rejects_physical_checkout_paths() {
    let root = TempDir::new().expect("temporary source root");
    let mut inputs = complete_inputs(root.path());
    inputs
      .environment
      .as_mut()
      .unwrap()
      .insert("LEAKED_ROOT".to_string(), root.path().to_string_lossy().into_owned());

    let analysis = analyze_action_key(root.path(), inputs).expect("analyze physical checkout path");
    assert!(analysis.key.is_none());
    assert!(analysis.reasons.contains("physical_checkout_path_in_build_script_key"));
  }

  #[test]
  fn build_script_action_key_changes_after_recapturing_exact_bytes() {
    let artifact_root = TempDir::new().expect("artifact source root");
    let mut artifact_inputs = complete_inputs(artifact_root.path());
    let artifact_baseline = key(artifact_root.path(), artifact_inputs.clone()).expect("artifact baseline key");
    fs::write(artifact_root.path().join("target/build-script"), b"PROGRAM").expect("same-size artifact mutation");
    artifact_inputs.compiled_artifact = Some(
      FileObservation::capture(
        &artifact_root.path().join("target/build-script"),
        artifact_root.path(),
        artifact_root.path(),
      )
      .expect("recapture artifact"),
    );
    assert_ne!(
      key(artifact_root.path(), artifact_inputs).as_deref(),
      Some(artifact_baseline.as_str())
    );

    let source_root = TempDir::new().expect("source input root");
    let mut source_inputs = complete_inputs(source_root.path());
    let source_baseline = key(source_root.path(), source_inputs.clone()).expect("source baseline key");
    fs::write(source_root.path().join("crates/example/build.rs"), b"fn main(){ }").expect("same-size source mutation");
    source_inputs.source_inputs[0] = FileObservation::capture(
      &source_root.path().join("crates/example/build.rs"),
      source_root.path(),
      source_root.path(),
    )
    .expect("recapture source");
    assert_ne!(
      key(source_root.path(), source_inputs).as_deref(),
      Some(source_baseline.as_str())
    );
  }

  #[test]
  fn build_script_action_key_refuses_ambient_or_future_dependent_authority() {
    let root = TempDir::new().expect("temporary source root");
    let mut inputs = complete_inputs(root.path());
    inputs.environment = None;
    inputs.dependency_results = None;
    inputs.executable_path = None;
    inputs.output_root = None;
    inputs.platform_identity = None;

    let analysis = analyze_action_key(root.path(), inputs).expect("analyze incomplete key");
    assert!(analysis.key.is_none());
    assert_eq!(
      analysis.reasons.iter().map(String::as_str).collect::<Vec<_>>(),
      vec![
        "build_script_ambient_inputs_unobserved",
        "build_script_dependency_results_unavailable",
        "build_script_environment_uncontrolled",
      ]
    );
  }

  #[test]
  fn build_script_action_key_requires_every_static_input() {
    type MissingInput = Box<dyn Fn(&mut BuildScriptActionInputs)>;

    let root = TempDir::new().expect("temporary source root");
    let cases: Vec<(&str, &str, MissingInput)> = vec![
      (
        "artifact",
        "compiled_build_script_artifact_unavailable",
        Box::new(|inputs| inputs.compiled_artifact = None),
      ),
      (
        "source",
        "build_script_source_inputs_unavailable",
        Box::new(|inputs| inputs.source_inputs.clear()),
      ),
      (
        "manifests",
        "build_script_manifest_closure_unavailable",
        Box::new(|inputs| inputs.manifest_closure = None),
      ),
      (
        "lock",
        "build_script_lock_closure_unavailable",
        Box::new(|inputs| inputs.lock_closure = None),
      ),
      (
        "toolchain",
        "build_script_toolchain_identity_unavailable",
        Box::new(|inputs| inputs.toolchain = None),
      ),
      (
        "action identity",
        "build_script_action_identity_unavailable",
        Box::new(|inputs| inputs.action_id.clear()),
      ),
      (
        "working directory",
        "build_script_working_directory_unavailable",
        Box::new(|inputs| inputs.working_directory = None),
      ),
      (
        "target",
        "build_script_target_identity_unavailable",
        Box::new(|inputs| inputs.target_identity = None),
      ),
      (
        "configuration",
        "build_script_configuration_identity_unavailable",
        Box::new(|inputs| inputs.configuration = None),
      ),
    ];

    for (name, reason, remove) in cases {
      let mut inputs = complete_inputs(root.path());
      remove(&mut inputs);
      let analysis = analyze_action_key(root.path(), inputs).expect("analyze missing input");
      assert!(analysis.key.is_none(), "{name} absence issued a key");
      assert!(
        analysis.reasons.contains(reason),
        "{name} absence did not report {reason}"
      );
    }
  }

  #[test]
  fn build_script_key_analysis_never_persists_environment_values() {
    let root = TempDir::new().expect("temporary source root");
    let mut controlled = complete_inputs(root.path());
    controlled
      .environment
      .as_mut()
      .expect("controlled environment")
      .insert("VISIBLE_INPUT".to_string(), "never-persist-this-value".to_string());
    let encoded =
      serde_json::to_string(&analyze_action_key(root.path(), controlled).expect("analyze controlled environment"))
        .expect("serialize key analysis");
    assert!(!encoded.contains("never-persist-this-value"));

    let mut inputs = complete_inputs(root.path());
    inputs.secret_environment.insert("REGISTRY_TOKEN".to_string());

    let analysis = analyze_action_key(root.path(), inputs).expect("analyze secret capability");
    assert!(analysis.key.is_none());
    assert_eq!(
      analysis.secret_capabilities,
      BTreeSet::from(["REGISTRY_TOKEN".to_string()])
    );
    assert!(analysis.reasons.contains("secret_build_script_environment"));
  }

  #[test]
  fn build_script_action_key_rejects_missing_or_circular_dependency_results() {
    let root = TempDir::new().expect("temporary source root");
    let mut incomplete = complete_inputs(root.path());
    incomplete.dependency_results.as_mut().unwrap().clear();
    let analysis = analyze_action_key(root.path(), incomplete).expect("analyze incomplete dependency results");
    assert!(analysis.key.is_none());
    assert!(
      analysis
        .reasons
        .contains("build_script_dependency_result_set_incomplete")
    );

    let mut circular = complete_inputs(root.path());
    circular.dependency_actions.insert(circular.action_id.clone());
    circular
      .dependency_results
      .as_mut()
      .unwrap()
      .insert(circular.action_id.clone(), "v1-sha256-future-result".to_string());
    let analysis = analyze_action_key(root.path(), circular).expect("analyze circular dependency result");
    assert!(analysis.key.is_none());
    assert!(analysis.reasons.contains("build_script_result_cycle"));
  }

  #[test]
  fn build_script_action_key_revalidates_exact_bytes() {
    let root = TempDir::new().expect("temporary source root");
    let inputs = complete_inputs(root.path());
    fs::write(root.path().join("crates/example/build.rs"), b"fn main(){ }").expect("same-size source mutation");

    let analysis = analyze_action_key(root.path(), inputs).expect("analyze changed source");
    assert!(analysis.key.is_none());
    assert!(analysis.reasons.contains("build_script_source_input_changed"));
  }
}
