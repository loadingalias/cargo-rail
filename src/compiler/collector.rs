//! Target-aware compiler diagnostics collection with persistent caching.

use crate::build_script::{
  BuildScriptActionInputs, BuildScriptCargoOutputSummary, BuildScriptResultInputs,
  analyze_action_key as analyze_build_script_action_key, analyze_result as analyze_build_script_result,
};
use crate::cargo::DepKind;
use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::compiler::diagnostics_store::CompilerDiagnosticsStore;
use crate::compiler::model::{
  AnalysisConfiguration, COLLECTOR_VERSION, CargoTargetKind, CompilationUnitEvidence, CompilationUnitId,
  CompilerDiagEntry, CompilerDiagKey, DependencyEvidenceState, DiagnosticsCompleteness, EvidenceCacheSummary,
  FeatureSelection, MemberEvidence, PlatformTarget, TargetEvidence,
};
use crate::compiler::native_cache::{NativeCompilerSession, SESSION_ENV};
use crate::compiler::observation::{
  BuildScriptResultBinding, CargoArtifactObservation, CompilationObservationContext, CompilationObservationManifest,
  CompilationProfile, CompilerCacheWrapperMetadata, CompilerCacheWrapperStatus, CompilerMode, CompilerWrapperIdentity,
  CompilerWrapperRole, FileObservation, ObservationPath, attach_build_script_result_dependencies,
  attach_execution_identities, build_manifests, load_raw,
};
use crate::compiler::wrapper::{
  CACHE_WRAPPER_MARKER, CacheWrapperPlan, INNER_WRAPPER_ENV, OBSERVATION_DIRECTORY_ENV, OBSERVATION_SOURCE_ROOT_ENV,
  WRAPPER_MARKER,
};
use crate::error::{RailError, RailResult, ResultExt};
use crate::executable::{ExecutableIdentity, ToolchainExecutableIdentities, ToolchainExecutableScope};
use crate::progress;
use crate::source::{ContentDigest, SourceEntryKind};
use crate::workspace::{WorkspaceContext, WorkspaceSnapshot};
use cargo_metadata::{Message, PackageId, TargetKind};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// One exact rustc crate candidate and the platforms where its declaration applies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompilerCandidate {
  /// Workspace member owning the declaration.
  pub member: String,
  /// rustc crate name passed through `--extern`.
  pub crate_name: String,
  /// Dependency domain whose compilation units provide evidence.
  pub kind: crate::cargo::manifest_analyzer::DepKind,
  /// Configured platform targets where this declaration applies.
  pub applicable_targets: BTreeSet<String>,
  /// Restrict evidence to one feature mode (used for optional activation proofs).
  pub required_features: Option<FeatureSelection>,
}

/// Compiler diagnostics collector and cache coordinator.
pub struct CompilerDiagnosticsCollector<'a> {
  workspace_root: &'a Path,
  manifests: &'a ManifestAnalyzer,
  targets: Vec<&'a str>,
  identity: CompilerCacheIdentity,
}

/// Exact snapshot-derived inputs shared by every compiler-evidence key.
#[derive(Clone)]
pub(crate) struct CompilerCacheIdentity {
  rustc_version: String,
  cargo_version: String,
  host_triple: String,
  toolchain_fingerprint: String,
  target_fingerprints: HashMap<String, String>,
  lock_fingerprint: String,
  compiler_env_fingerprint: String,
  cargo_config_fingerprint: String,
  cargo_program: OsString,
  rustc_workspace_wrapper: Option<OsString>,
  manifest_fingerprints: HashMap<PackageId, String>,
  source_fingerprints: HashMap<PackageId, String>,
  observation_context: CompilationObservationContext,
  package_observation_identities: HashMap<PackageId, String>,
  package_dependencies: HashMap<String, BTreeSet<String>>,
  build_script_packages: HashMap<String, BuildScriptPackageContext>,
  rustc_executable: ExecutableIdentity,
  wrapper_chain: Vec<CompilerWrapperIdentity>,
  cache_wrapper: CompilerCacheWrapperMetadata,
  cache_wrapper_plan: CacheWrapperPlan,
  executable_bypasses: BTreeSet<String>,
  cache_bypass_reason: Option<&'static str>,
}

#[derive(Clone)]
struct BuildScriptPackageContext {
  package_id: PackageId,
  working_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CargoBuildScriptOutput {
  One(BuildScriptCargoOutputSummary),
  Ambiguous,
}

impl CompilerCacheIdentity {
  /// Capture exact compiler-cache identity from one immutable workspace snapshot.
  pub fn capture(snapshot: &WorkspaceSnapshot) -> RailResult<Self> {
    let rustc_version = snapshot.toolchain().rustc_verbose_version().to_string();
    let cargo_version = snapshot.toolchain().cargo_verbose_version().to_string();
    let host_triple = snapshot.toolchain().host_target().to_string();
    let current_executable =
      std::env::current_exe().with_context(|| "locating cargo-rail compiler-observation wrapper".to_string())?;
    let cargo_rail_executable = ExecutableIdentity::capture(
      current_executable.as_os_str(),
      snapshot.source_root(),
      snapshot.source_root(),
    )?;
    let executables = snapshot.executable_identities(ToolchainExecutableScope::Compilation)?;
    let cache_wrapper_plan = CacheWrapperPlan::for_chain(
      snapshot.toolchain().rustc_wrapper_program(),
      snapshot.toolchain().rustc_workspace_wrapper_program(),
    );
    let toolchain_fingerprint =
      executable_toolchain_fingerprint(snapshot, executables, &cargo_rail_executable, cache_wrapper_plan)?;
    let target_fingerprints = target_fingerprints(snapshot)?;
    let lock_fingerprint = snapshot.lockfile_fingerprint();
    let compiler_env_fingerprint = compiler_env_fingerprint(snapshot)?;
    let cargo_config_fingerprint = cargo_config_fingerprint(snapshot)?;
    let cargo_program = snapshot.toolchain().cargo_program().to_owned();
    let rustc_workspace_wrapper = snapshot
      .toolchain()
      .rustc_workspace_wrapper_program()
      .map(OsString::from);
    let local_dependencies = declared_local_dependency_graph(snapshot)?;
    let manifest_fingerprints = manifest_closure_fingerprints(snapshot, &local_dependencies)?;
    let source_fingerprints = source_closure_fingerprints(snapshot, &local_dependencies)?;
    let observation_context = CompilationObservationContext::capture(snapshot)?;
    let package_observation_identities = package_observation_identities(snapshot)?;
    let package_dependencies = package_dependency_graph(snapshot, &package_observation_identities)?;
    let build_script_packages = build_script_package_contexts(snapshot, &package_observation_identities)?;
    let rustc_executable = executables.rustc().clone();
    let configured_wrappers = [executables.rustc_wrapper(), executables.rustc_workspace_wrapper()];
    if configured_wrappers
      .iter()
      .flatten()
      .any(|wrapper| wrapper.same_resolved_file(&cargo_rail_executable))
    {
      return Err(RailError::with_help(
        "recursive cargo-rail rustc wrapper configuration",
        "remove cargo-rail from RUSTC_WRAPPER and RUSTC_WORKSPACE_WRAPPER; diagnostics injection is automatic",
      ));
    }
    let mut executable_bypasses = executables.limitations().map(str::to_string).collect::<BTreeSet<_>>();
    executable_bypasses.extend(
      cargo_rail_executable
        .limitations()
        .map(|limitation| format!("compiler_wrapper_{limitation}")),
    );
    let mut wrapper_chain = Vec::with_capacity(4);
    if cache_wrapper_plan.installs_cargo_rail() {
      wrapper_chain.push(CompilerWrapperIdentity::new(
        CompilerWrapperRole::Cache,
        cargo_rail_executable.clone(),
      ));
    }
    wrapper_chain.extend(
      executables
        .rustc_wrapper()
        .cloned()
        .map(|executable| CompilerWrapperIdentity::new(CompilerWrapperRole::Global, executable)),
    );
    wrapper_chain.push(CompilerWrapperIdentity::new(
      CompilerWrapperRole::Diagnostic,
      cargo_rail_executable,
    ));
    wrapper_chain.extend(
      executables
        .rustc_workspace_wrapper()
        .cloned()
        .map(|executable| CompilerWrapperIdentity::new(CompilerWrapperRole::Workspace, executable)),
    );
    let cache_wrapper =
      CompilerCacheWrapperMetadata::new(CompilerCacheWrapperStatus::Bypassed, cache_wrapper_plan.reason());
    let cache_bypass_reason = compiler_cache_bypass_reason(snapshot);

    Ok(Self {
      rustc_version,
      cargo_version,
      host_triple,
      toolchain_fingerprint,
      target_fingerprints,
      lock_fingerprint,
      compiler_env_fingerprint,
      cargo_config_fingerprint,
      cargo_program,
      rustc_workspace_wrapper,
      manifest_fingerprints,
      source_fingerprints,
      observation_context,
      package_observation_identities,
      package_dependencies,
      build_script_packages,
      rustc_executable,
      wrapper_chain,
      cache_wrapper,
      cache_wrapper_plan,
      executable_bypasses,
      cache_bypass_reason,
    })
  }
}

impl<'a> CompilerDiagnosticsCollector<'a> {
  /// Create a new collector for a workspace-level analysis pass.
  pub fn new(workspace_root: &'a Path, manifests: &'a ManifestAnalyzer, targets: Vec<&'a str>) -> RailResult<Self> {
    let context = WorkspaceContext::build_with_snapshot(workspace_root)?;
    let identity = CompilerCacheIdentity::capture(context.snapshot()?)?;
    Ok(Self {
      workspace_root,
      manifests,
      targets,
      identity,
    })
  }

  pub(crate) fn with_identity(
    workspace_root: &'a Path,
    manifests: &'a ManifestAnalyzer,
    targets: Vec<&'a str>,
    identity: &CompilerCacheIdentity,
  ) -> Self {
    Self {
      workspace_root,
      manifests,
      targets,
      identity: identity.clone(),
    }
  }

  /// Collect diagnostics for selected workspace members.
  pub fn collect_for_candidates(
    &self,
    candidates: &[CompilerCandidate],
  ) -> RailResult<HashMap<PackageId, MemberEvidence>> {
    let members: HashSet<&str> = candidates.iter().map(|candidate| candidate.member.as_str()).collect();
    if members.is_empty() {
      return Ok(HashMap::new());
    }

    let mut store = CompilerDiagnosticsStore::load(self.workspace_root);
    let key_inputs = self.build_key_inputs(&members)?;
    let manifest_to_member = build_manifest_member_index(&self.manifests.members);
    let member_ids: HashMap<&str, &PackageId> = self
      .manifests
      .members
      .iter()
      .map(|member| (member.package_name.as_str(), &member.package_id))
      .collect();
    let candidate_targets = build_candidate_target_index(candidates);

    let mut result: HashMap<PackageId, MemberEvidence> = HashMap::with_capacity(members.len());
    let mut cache_by_member: HashMap<String, EvidenceCacheSummary> = HashMap::with_capacity(members.len());
    let mut stale_by_configuration: BTreeMap<AnalysisConfiguration, Vec<&str>> = BTreeMap::new();
    let mut retained_observations = HashMap::<String, CompilationObservationManifest>::new();
    let mut surviving_unused: HashMap<String, BTreeSet<CandidateId>> = candidate_targets
      .iter()
      .map(|(member, candidates)| (member.clone(), candidates.keys().cloned().collect()))
      .collect();

    for (member, target, features, key) in key_inputs {
      let cached = self
        .identity
        .cache_bypass_reason
        .is_none()
        .then(|| store.get(&key))
        .flatten();
      let observation_miss =
        cached.and_then(|entry| compiler_observation_miss_reason(&entry.observations, self.workspace_root));
      if self.identity.cache_bypass_reason.is_none()
        && observation_miss.is_none()
        && let Some(entry) = cached
      {
        cache_by_member.entry(member.to_string()).or_default().hits += 1;
        update_candidate_survivors(
          &mut surviving_unused,
          &candidate_targets,
          member,
          target,
          &entry.evidence,
        );
        let package_id = member_ids
          .get(member)
          .ok_or_else(|| RailError::message(format!("missing package identity for member '{member}'")))?;
        record_target_evidence(&mut result, package_id, &entry.evidence);
        continue;
      }

      let reason = self
        .identity
        .cache_bypass_reason
        .or(observation_miss)
        .unwrap_or_else(|| store.miss_reason(&key));
      let summary = cache_by_member.entry(member.to_string()).or_default();
      summary.misses += 1;
      *summary.miss_reasons.entry(reason.to_string()).or_default() += 1;

      stale_by_configuration
        .entry(AnalysisConfiguration {
          platform: PlatformTarget::from(target),
          features,
        })
        .or_default()
        .push(member);
    }

    let mut skipped_member_targets = 0usize;
    for (configuration, stale_members) in stale_by_configuration {
      let target = configuration.platform.as_str();
      let features = configuration.features;
      let active_members: Vec<&str> = stale_members
        .iter()
        .copied()
        .filter(|member| has_applicable_survivor(&surviving_unused, &candidate_targets, member, target, &features))
        .collect();
      skipped_member_targets += stale_members.len() - active_members.len();
      if active_members.is_empty() {
        continue;
      }

      let mut stale_set = HashSet::with_capacity(active_members.len());
      for member in &active_members {
        stale_set.insert(*member);
      }

      progress!(
        "  Checking unused dependencies for target {} ({} package{})...",
        format_args!("{} / {}", target, features.label()),
        active_members.len(),
        if active_members.len() == 1 { "" } else { "s" }
      );
      let started = Instant::now();
      let mut run = run_workspace_check(self.workspace_root, &self.identity, target, &features, &active_members)?;
      progress!(
        "    Finished target {} in {:.1}s",
        format_args!("{} / {}", target, features.label()),
        started.elapsed().as_secs_f64()
      );
      let parsed = parse_target_run(
        &run.stdout,
        self.workspace_root,
        &manifest_to_member,
        &stale_set,
        candidates,
      );
      let invocations = std::mem::take(&mut run.invocations);
      let mut compilation_observations =
        parse_compilation_observations(&run.stdout, invocations, &self.identity, target)?;
      reconcile_exact_artifact_observations(&mut compilation_observations, &mut retained_observations);
      let completeness = if run.success {
        DiagnosticsCompleteness::Complete
      } else {
        DiagnosticsCompleteness::Incomplete
      };

      for member in active_members {
        let manifests_member = self
          .manifests
          .members
          .iter()
          .find(|m| m.package_name == member)
          .ok_or_else(|| RailError::message(format!("missing manifest entry for member '{member}'")))?;

        let key = self.key_for(manifests_member, target, features.clone())?;

        let mut unused = BTreeSet::new();
        let mut compiled = BTreeSet::new();

        if completeness == DiagnosticsCompleteness::Complete
          && let Some(parsed_member) = parsed.get(member)
        {
          compiled = parsed_member.compiled_targets.clone();
        }

        let unit_evidence = parsed
          .get(member)
          .map(ParsedMemberTarget::unit_evidence)
          .unwrap_or_default();
        let normal_units: Vec<_> = compiled
          .iter()
          .filter(|unit| !unit.test_mode && unit.kind != CargoTargetKind::CustomBuild)
          .collect();
        if !normal_units.is_empty() {
          for candidate in candidates
            .iter()
            .filter(|candidate| candidate.member == member && candidate.kind == DepKind::Normal)
          {
            if normal_units.iter().all(|unit| {
              unit_evidence
                .iter()
                .find(|evidence| &evidence.unit == *unit)
                .is_some_and(|evidence| evidence.unused_crates.contains(&candidate.crate_name))
            }) {
              unused.insert(candidate.crate_name.clone());
            }
          }
        }
        let evidence = TargetEvidence {
          platform: PlatformTarget::from(target),
          features: features.clone(),
          compiled_units: compiled,
          unused_crates: unused,
          unit_evidence,
          completeness,
        };
        let entry = CompilerDiagEntry {
          key,
          evidence: evidence.clone(),
          generated_at_unix_ms: now_unix_ms(),
          collector_version: COLLECTOR_VERSION,
          observations: self
            .identity
            .package_observation_identities
            .get(&manifests_member.package_id)
            .map(|package| {
              compilation_observations
                .iter()
                .filter(|manifest| manifest.unit.package == *package)
                .cloned()
                .collect()
            })
            .unwrap_or_default(),
        };

        update_candidate_survivors(
          &mut surviving_unused,
          &candidate_targets,
          member,
          target,
          &entry.evidence,
        );

        record_target_evidence(&mut result, &manifests_member.package_id, &entry.evidence);
        store.put(entry);
      }
    }

    store.flush()?;
    if skipped_member_targets > 0 {
      progress!(
        "  Skipped {} target-package check{} after dependencies were proven used",
        skipped_member_targets,
        if skipped_member_targets == 1 { "" } else { "s" }
      );
    }
    for member in &self.manifests.members {
      if let Some(evidence) = result.get_mut(&member.package_id) {
        evidence.cache = cache_by_member.remove(&member.package_name).unwrap_or_default();
      }
    }

    Ok(result)
  }

  fn build_key_inputs(
    &self,
    members: &HashSet<&str>,
  ) -> RailResult<Vec<(&str, &str, FeatureSelection, CompilerDiagKey)>> {
    let mut keys = Vec::with_capacity(members.len() * self.targets.len() * FeatureSelection::BASELINES.len());

    for member in &self.manifests.members {
      if !members.contains(member.package_name.as_str()) {
        continue;
      }

      let selections = planned_feature_selections(member);
      for target in &self.targets {
        for features in &selections {
          keys.push((
            member.package_name.as_str(),
            *target,
            features.clone(),
            self.key_for(member, target, features.clone())?,
          ));
        }
      }
    }

    Ok(keys)
  }

  fn key_for(
    &self,
    member: &crate::cargo::manifest_analyzer::ParsedManifest,
    target: &str,
    features: FeatureSelection,
  ) -> RailResult<CompilerDiagKey> {
    let identity = &self.identity;
    Ok(CompilerDiagKey {
      package_id: member.package_id.clone(),
      target: PlatformTarget::from(target),
      features,
      rustc_version: identity.rustc_version.clone(),
      cargo_version: identity.cargo_version.clone(),
      host_triple: identity.host_triple.clone(),
      toolchain_fingerprint: identity.toolchain_fingerprint.clone(),
      target_fingerprint: identity
        .target_fingerprints
        .get(target)
        .cloned()
        .ok_or_else(|| RailError::message(format!("missing compiler target identity for '{target}'")))?,
      lock_fingerprint: identity.lock_fingerprint.clone(),
      manifest_fingerprint: identity
        .manifest_fingerprints
        .get(&member.package_id)
        .cloned()
        .ok_or_else(|| RailError::message(format!("missing manifest identity for member '{}'", member.package_id)))?,
      source_fingerprint: identity
        .source_fingerprints
        .get(&member.package_id)
        .cloned()
        .ok_or_else(|| RailError::message(format!("missing source identity for member '{}'", member.package_id)))?,
      compiler_env_fingerprint: identity.compiler_env_fingerprint.clone(),
      cargo_config_fingerprint: identity.cargo_config_fingerprint.clone(),
    })
  }
}

/// Confirm which borrowed dependency features are named by a member's
/// standalone compiler failure.
pub fn standalone_missing_features(
  workspace_root: &Path,
  member: &str,
  candidates: &[(String, Vec<String>)],
) -> RailResult<BTreeMap<(String, String), BTreeSet<String>>> {
  let output = Command::new("cargo")
    .current_dir(workspace_root)
    .args(["check", "--package", member, "--all-targets", "--message-format=json"])
    .output()
    .with_context(|| format!("checking standalone feature requirements for member '{member}'"))?;
  if output.status.success() {
    return Ok(BTreeMap::new());
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  let mut missing: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
  for line in stdout.lines() {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
      continue;
    };
    if event["reason"] != "compiler-message" || event["message"]["level"] != "error" {
      continue;
    }
    let diagnostic = event["message"].to_string();
    let source_paths: BTreeSet<String> = event["message"]["spans"]
      .as_array()
      .into_iter()
      .flatten()
      .filter(|span| span["is_primary"] == true)
      .filter_map(|span| span["file_name"].as_str())
      .map(|path| {
        let path = Path::new(path);
        let relative = path.strip_prefix(workspace_root).unwrap_or(path);
        crate::utils::path_to_git_format(relative)
      })
      .collect();
    for (dependency, features) in candidates {
      let crate_name = dependency.replace('-', "_");
      if !diagnostic.contains(dependency) && !diagnostic.contains(&crate_name) {
        continue;
      }
      for feature in features {
        if diagnostic.contains(feature) {
          missing
            .entry((dependency.clone(), feature.clone()))
            .or_default()
            .extend(source_paths.iter().cloned());
        }
      }
    }
  }
  Ok(missing)
}

/// Verify that one member compiles without relying on other workspace members
/// after a causal feature repair is applied.
pub fn verify_standalone_member(workspace_root: &Path, member: &str) -> RailResult<()> {
  let output = Command::new("cargo")
    .current_dir(workspace_root)
    .args(["check", "--package", member, "--all-targets"])
    .output()
    .with_context(|| format!("verifying standalone member '{member}'"))?;
  if output.status.success() {
    return Ok(());
  }
  Err(RailError::message(format!(
    "standalone check failed for member '{member}': {}",
    String::from_utf8_lossy(&output.stderr).trim()
  )))
}

fn planned_feature_selections(member: &crate::cargo::manifest_analyzer::ParsedManifest) -> Vec<FeatureSelection> {
  let mut selections = FeatureSelection::BASELINES.to_vec();
  let crate_root = member.path.parent().unwrap_or(member.path.as_path());
  selections.extend(
    crate::cargo::feature_scanner::scan_source_for_feature_selections(crate_root)
      .into_iter()
      .filter(|selected| {
        selected
          .iter()
          .all(|feature| member.declared_features.contains(feature))
      })
      .map(FeatureSelection::Selected),
  );
  selections.extend(
    member
      .required_feature_selections
      .iter()
      .cloned()
      .map(FeatureSelection::Selected),
  );
  selections.sort();
  selections.dedup();
  selections
}

fn record_target_evidence(
  result: &mut HashMap<PackageId, MemberEvidence>,
  package_id: &PackageId,
  evidence: &TargetEvidence,
) {
  let member = result
    .entry(package_id.clone())
    .or_insert_with(|| MemberEvidence::new(package_id.clone()));
  member
    .configurations
    .entry(evidence.platform.clone())
    .or_default()
    .insert(evidence.features.clone(), evidence.clone());
}

type CandidateId = (
  crate::cargo::manifest_analyzer::DepKind,
  String,
  Option<FeatureSelection>,
);

fn build_candidate_target_index(
  candidates: &[CompilerCandidate],
) -> HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>> {
  let mut index: HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>> = HashMap::new();
  for candidate in candidates {
    index
      .entry(candidate.member.clone())
      .or_default()
      .entry((
        candidate.kind,
        candidate.crate_name.clone(),
        candidate.required_features.clone(),
      ))
      .or_default()
      .extend(candidate.applicable_targets.iter().cloned());
  }
  index
}

fn has_applicable_survivor(
  surviving_unused: &HashMap<String, BTreeSet<CandidateId>>,
  candidate_targets: &HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>>,
  member: &str,
  target: &str,
  features: &FeatureSelection,
) -> bool {
  surviving_unused.get(member).is_some_and(|survivors| {
    survivors.iter().any(|candidate| {
      candidate_targets
        .get(member)
        .and_then(|targets| targets.get(candidate))
        .is_some_and(|targets| targets.contains(target))
        && candidate.2.as_ref().is_none_or(|required| required == features)
    })
  })
}

fn update_candidate_survivors(
  surviving_unused: &mut HashMap<String, BTreeSet<CandidateId>>,
  candidate_targets: &HashMap<String, BTreeMap<CandidateId, BTreeSet<String>>>,
  member: &str,
  target: &str,
  evidence: &TargetEvidence,
) {
  if evidence.completeness != DiagnosticsCompleteness::Complete || evidence.compiled_units.is_empty() {
    return;
  }
  let Some(targets_by_candidate) = candidate_targets.get(member) else {
    return;
  };
  let Some(survivors) = surviving_unused.get_mut(member) else {
    return;
  };
  survivors.retain(|candidate| {
    let applicable = targets_by_candidate
      .get(candidate)
      .is_some_and(|targets| targets.contains(target))
      && candidate
        .2
        .as_ref()
        .is_none_or(|required| required == &evidence.features);
    !applicable || evidence.dependency_state_for_kind(&candidate.1, candidate.0) != DependencyEvidenceState::Used
  });
}

#[derive(Debug)]
struct WorkspaceCheckOutput {
  stdout: String,
  success: bool,
  invocations: Vec<crate::compiler::observation::RawCompilerInvocation>,
}

fn run_workspace_check(
  workspace_root: &Path,
  identity: &CompilerCacheIdentity,
  target: &str,
  features: &FeatureSelection,
  members: &[&str],
) -> RailResult<WorkspaceCheckOutput> {
  let wrapper =
    std::env::current_exe().with_context(|| "locating cargo-rail executable for rustc wrapper".to_string())?;
  let existing_workspace_wrapper = identity.rustc_workspace_wrapper.as_deref();
  let observation_directory = tempfile::Builder::new()
    .prefix("cargo-rail-compiler-observations-")
    .tempdir()
    .with_context(|| "creating compiler observation directory".to_string())?;
  let native_cache_session = identity.cache_wrapper_plan.installs_cargo_rail().then(|| {
    NativeCompilerSession::write(
      observation_directory.path(),
      workspace_root,
      &identity.rustc_version,
      &identity.cargo_version,
      &identity.toolchain_fingerprint,
      &identity.compiler_env_fingerprint,
      &identity.cargo_config_fingerprint,
    )
    .unwrap_or_else(|_| observation_directory.path().join("native-cache-session-unavailable"))
  });

  let mut args: Vec<OsString> = vec![
    "check".into(),
    "--locked".into(),
    "--all-targets".into(),
    "--message-format=json".into(),
  ];
  match features {
    FeatureSelection::Default => {}
    FeatureSelection::NoDefaultFeatures => args.push("--no-default-features".into()),
    FeatureSelection::AllFeatures => args.push("--all-features".into()),
    FeatureSelection::Selected(selected) => {
      args.push("--no-default-features".into());
      for member in members {
        for feature in selected {
          args.push("--features".into());
          args.push(format!("{member}/{feature}").into());
        }
      }
    }
  }
  for member in members {
    args.push("--package".into());
    args.push((*member).into());
  }
  if target != "default" {
    args.push("--target".into());
    args.push(target.into());
  }

  let mut command = Command::new(&identity.cargo_program);
  command
    .current_dir(workspace_root)
    .env("RUSTC_WORKSPACE_WRAPPER", &wrapper)
    .env(WRAPPER_MARKER, "1")
    .env(OBSERVATION_DIRECTORY_ENV, observation_directory.path())
    .env(OBSERVATION_SOURCE_ROOT_ENV, workspace_root)
    .env_remove(CACHE_WRAPPER_MARKER)
    .args(&args);
  if identity.cache_wrapper_plan.installs_cargo_rail() {
    command.env("RUSTC_WRAPPER", &wrapper).env(CACHE_WRAPPER_MARKER, "1");
    if let Some(session) = &native_cache_session {
      command.env(SESSION_ENV, session);
    }
  }
  if let Some(inner_wrapper) = existing_workspace_wrapper
    && inner_wrapper != wrapper.as_os_str()
  {
    command.env(INNER_WRAPPER_ENV, inner_wrapper);
  }

  let output = command.output().with_context(|| {
    format!(
      "running cargo check for target '{target}' in {}",
      workspace_root.display()
    )
  })?;

  Ok(WorkspaceCheckOutput {
    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    success: output.status.success(),
    invocations: load_raw(observation_directory.path())?,
  })
}

#[derive(Debug, Default)]
struct ParsedMemberTarget {
  compiled_targets: BTreeSet<CompilationUnitId>,
  warned_targets_by_dep: HashMap<String, BTreeSet<CompilationUnitId>>,
}

impl ParsedMemberTarget {
  fn unit_evidence(&self) -> Vec<CompilationUnitEvidence> {
    let mut by_unit: BTreeMap<CompilationUnitId, BTreeSet<String>> = self
      .compiled_targets
      .iter()
      .cloned()
      .map(|unit| (unit, BTreeSet::new()))
      .collect();
    for (dependency, units) in &self.warned_targets_by_dep {
      for unit in units {
        by_unit.entry(unit.clone()).or_default().insert(dependency.clone());
      }
    }
    by_unit
      .into_iter()
      .map(|(unit, unused_crates)| CompilationUnitEvidence { unit, unused_crates })
      .collect()
  }
}

fn parse_target_run(
  stdout: &str,
  workspace_root: &Path,
  manifest_to_member: &HashMap<String, String>,
  stale_members: &HashSet<&str>,
  candidates: &[CompilerCandidate],
) -> HashMap<String, ParsedMemberTarget> {
  let mut parsed: HashMap<String, ParsedMemberTarget> = HashMap::new();
  let mut warnings_by_target: HashMap<(String, CompilationUnitId), BTreeSet<String>> = HashMap::new();

  for line in stdout.lines() {
    let Ok(message) = serde_json::from_str::<CargoEvent>(line) else {
      continue;
    };
    if message.reason != "compiler-message" && message.reason != "compiler-artifact" {
      continue;
    }

    let Some(manifest_path) = message.manifest_path.as_deref() else {
      continue;
    };
    let Some(member_name) = manifest_to_member.get(manifest_path) else {
      continue;
    };
    if !stale_members.contains(member_name.as_str()) {
      continue;
    }

    let Some(target) = message.target.as_ref() else {
      continue;
    };
    if !is_relevant_target(target) {
      continue;
    }

    let base_target = target.identifier(workspace_root, false);
    if message.reason == "compiler-message" {
      let Some(diagnostic) = message.message.as_ref() else {
        continue;
      };
      if diagnostic.code.as_ref().and_then(|c| c.code.as_deref()) != Some("unused_crate_dependencies") {
        continue;
      }
      let Some(crate_name) = parse_unused_crate_name(&diagnostic.message) else {
        continue;
      };
      warnings_by_target
        .entry((member_name.clone(), base_target))
        .or_default()
        .insert(crate_name.replace('-', "_"));
      continue;
    }

    let target_id = target.identifier(
      workspace_root,
      message.profile.as_ref().is_some_and(|profile| profile.test),
    );
    let parsed_member = parsed.entry(member_name.clone()).or_default();
    parsed_member.compiled_targets.insert(target_id.clone());
  }

  // Cargo does not guarantee whether a diagnostic is emitted before or after
  // its artifact message. Correlate after consuming the stream by stable Cargo
  // target identity, then project it into the dependency's manifest domain.
  for ((member, base_target), warnings) in warnings_by_target {
    let Some(parsed_member) = parsed.get_mut(&member) else {
      continue;
    };
    let matching: Vec<_> = parsed_member
      .compiled_targets
      .iter()
      .filter(|unit| {
        unit.kind == base_target.kind && unit.name == base_target.name && unit.source == base_target.source
      })
      .cloned()
      .collect();
    for crate_name in &warnings {
      let kinds: BTreeSet<_> = candidates
        .iter()
        .filter(|candidate| candidate.member == member && candidate.crate_name == *crate_name)
        .map(|candidate| candidate.kind)
        .collect();
      for target_id in matching.iter().filter(|unit| {
        kinds.iter().any(|kind| match kind {
          DepKind::Normal => !unit.test_mode && unit.kind != CargoTargetKind::CustomBuild,
          DepKind::Dev => {
            unit.test_mode
              || matches!(
                unit.kind,
                CargoTargetKind::Test | CargoTargetKind::Example | CargoTargetKind::Benchmark
              )
          }
          DepKind::Build => unit.kind == CargoTargetKind::CustomBuild,
        })
      }) {
        parsed_member
          .warned_targets_by_dep
          .entry(crate_name.clone())
          .or_default()
          .insert(target_id.clone());
      }
    }
  }

  parsed
}

fn parse_compilation_observations(
  stdout: &str,
  invocations: Vec<crate::compiler::observation::RawCompilerInvocation>,
  identity: &CompilerCacheIdentity,
  requested_target: &str,
) -> RailResult<Vec<CompilationObservationManifest>> {
  let source_root = &identity.observation_context.source_root;
  let mut artifacts = Vec::new();
  let mut build_script_outputs = HashMap::<String, CargoBuildScriptOutput>::new();
  for message in Message::parse_stream(BufReader::new(stdout.as_bytes())) {
    let message =
      message.map_err(|error| RailError::message(format!("failed to parse stable Cargo JSON message: {error}")))?;
    if let Message::BuildScriptExecuted(script) = message {
      if let Some(package) = identity.package_observation_identities.get(&script.package_id)
        && package.starts_with("local:")
      {
        let summary = build_script_output_summary(&script);
        build_script_outputs
          .entry(package.clone())
          .and_modify(|output| *output = CargoBuildScriptOutput::Ambiguous)
          .or_insert_with(|| CargoBuildScriptOutput::One(summary));
      }
      continue;
    }
    let Message::CompilerArtifact(artifact) = message else {
      continue;
    };
    let mut bypasses = BTreeSet::new();
    let package = identity
      .package_observation_identities
      .get(&artifact.package_id)
      .cloned()
      .unwrap_or_else(|| {
        bypasses.insert("cargo_package_identity_unavailable".to_string());
        format!("unknown:{}", artifact.package_id)
      });
    if !package.starts_with("local:") {
      continue;
    }
    let is_custom_build = artifact.target.kind.contains(&TargetKind::CustomBuild);
    let explicit_executable_path = artifact
      .executable
      .as_ref()
      .map(|path| ObservationPath::capture(path.as_std_path(), source_root, source_root));
    let mut outputs = Vec::new();
    for filename in &artifact.filenames {
      match FileObservation::capture(filename.as_std_path(), source_root, source_root) {
        Ok(file) => outputs.push(file),
        Err(_) => {
          bypasses.insert("cargo_artifact_output_bytes_unavailable".to_string());
        }
      }
    }
    if let Some(executable) = &artifact.executable
      && explicit_executable_path
        .as_ref()
        .is_some_and(|path| !outputs.iter().any(|output| &output.path == path))
    {
      match FileObservation::capture(executable.as_std_path(), source_root, source_root) {
        Ok(file) => outputs.push(file),
        Err(_) => {
          bypasses.insert("cargo_executable_output_bytes_unavailable".to_string());
        }
      }
    }
    outputs.sort();
    outputs.dedup();
    let executable = explicit_executable_path
      .as_ref()
      .and_then(|path| outputs.iter().find(|output| &output.path == path).cloned())
      .or_else(|| {
        if is_custom_build {
          build_script_executable_output(&outputs, &artifact.target.name, std::env::consts::EXE_SUFFIX)
        } else {
          None
        }
      });
    if is_custom_build && executable.is_none() {
      bypasses.insert("cargo_build_script_executable_output_unavailable".to_string());
    }
    artifacts.push(CargoArtifactObservation {
      package,
      target_kinds: artifact.target.kind.iter().map(ToString::to_string).collect(),
      target_name: artifact.target.name,
      crate_types: artifact.target.crate_types.iter().map(ToString::to_string).collect(),
      source: ObservationPath::capture(artifact.target.src_path.as_std_path(), source_root, source_root),
      profile: CompilationProfile {
        opt_level: artifact.profile.opt_level,
        debuginfo: artifact.profile.debuginfo.to_string(),
        debug_assertions: artifact.profile.debug_assertions,
        overflow_checks: artifact.profile.overflow_checks,
        test: artifact.profile.test,
      },
      features: artifact.features.into_iter().collect(),
      outputs,
      executable,
      fresh: artifact.fresh,
      bypasses,
    });
  }
  let mut manifests = build_manifests(
    invocations,
    artifacts,
    &identity.observation_context,
    requested_target,
    CompilerMode::Rustc,
  )?;
  attach_execution_identities(
    &mut manifests,
    &identity.rustc_executable,
    &identity.wrapper_chain,
    &identity.cache_wrapper,
    &identity.executable_bypasses,
  );
  attach_build_script_action_keys(&mut manifests, identity, requested_target)?;
  attach_build_script_results(&mut manifests, identity, &build_script_outputs);
  let result_bindings = manifests
    .iter()
    .filter(|manifest| manifest.unit.target_kind == crate::compiler::observation::CompilationTargetKind::BuildScript)
    .map(|manifest| BuildScriptResultBinding {
      package: manifest.unit.package.clone(),
      action_key: manifest
        .build_script_action_key
        .as_ref()
        .and_then(crate::build_script::BuildScriptActionKeyAnalysis::key)
        .map(str::to_string),
      result_digest: manifest
        .build_script_result
        .as_ref()
        .and_then(crate::build_script::BuildScriptResultAnalysis::digest)
        .map(str::to_string),
    })
    .collect::<Vec<_>>();
  attach_build_script_result_dependencies(&mut manifests, &identity.package_dependencies, &result_bindings)?;
  Ok(manifests)
}

fn build_script_executable_output(
  outputs: &[FileObservation],
  target_name: &str,
  executable_suffix: &str,
) -> Option<FileObservation> {
  let expected_name = format!("{target_name}{executable_suffix}");
  let mut matches = outputs.iter().filter(|output| {
    let path = match &output.path {
      ObservationPath::Repository(path) | ObservationPath::Host(path) => path,
    };
    path.rsplit('/').next() == Some(expected_name.as_str())
  });
  let executable = matches.next()?.clone();
  matches.next().is_none().then_some(executable)
}

fn build_script_output_summary(script: &cargo_metadata::BuildScript) -> BuildScriptCargoOutputSummary {
  BuildScriptCargoOutputSummary {
    linked_libraries: script.linked_libs.len(),
    linked_paths: script.linked_paths.len(),
    cfgs: script.cfgs.len(),
    rustc_environment: script.env.len(),
    output_directory_reported: !script.out_dir.as_str().is_empty(),
  }
}

fn select_build_script_output(
  output: Option<&CargoBuildScriptOutput>,
) -> (Option<BuildScriptCargoOutputSummary>, &'static str) {
  match output {
    Some(CargoBuildScriptOutput::One(output)) => (
      Some(output.clone()),
      "cargo_build_script_execution_freshness_unavailable",
    ),
    Some(CargoBuildScriptOutput::Ambiguous) => (None, "cargo_build_script_output_ambiguous"),
    None => (None, "cargo_build_script_output_unavailable"),
  }
}

fn attach_build_script_action_keys(
  manifests: &mut [CompilationObservationManifest],
  identity: &CompilerCacheIdentity,
  requested_target: &str,
) -> RailResult<()> {
  for manifest in manifests {
    if manifest.unit.target_kind != crate::compiler::observation::CompilationTargetKind::BuildScript {
      continue;
    }
    let package = identity.build_script_packages.get(&manifest.unit.package);
    let source_inputs = manifest
      .declared_inputs
      .iter()
      .chain(&manifest.observed_reads)
      .cloned()
      .collect();
    let target = if requested_target == "default" {
      identity.host_triple.clone()
    } else {
      requested_target.to_string()
    };
    let inputs = BuildScriptActionInputs {
      compiled_artifact: manifest.executable_output.clone(),
      source_inputs,
      manifest_closure: package.and_then(|package| identity.manifest_fingerprints.get(&package.package_id).cloned()),
      lock_closure: Some(identity.lock_fingerprint.clone()),
      toolchain: Some(identity.toolchain_fingerprint.clone()),
      action_id: format!("build-script:{}", manifest.unit_identity),
      package: manifest.unit.package.clone(),
      arguments: Vec::new(),
      working_directory: package.map(|package| package.working_directory.clone()),
      host_target: identity.host_triple.clone(),
      target,
      target_identity: identity.target_fingerprints.get(requested_target).cloned(),
      role: manifest.unit.role,
      profile: manifest.unit.profile.clone(),
      features: manifest.unit.features.clone(),
      cfg: manifest.unit.cfg.clone(),
      configuration: Some(identity.cargo_config_fingerprint.clone()),
      environment: None,
      secret_environment: BTreeSet::new(),
      dependency_actions: BTreeSet::new(),
      dependency_results: None,
      executable_path: None,
      output_root: None,
      platform_identity: None,
    };
    manifest.build_script_action_key = Some(analyze_build_script_action_key(
      &identity.observation_context.source_root,
      inputs,
    )?);
  }
  Ok(())
}

fn attach_build_script_results(
  manifests: &mut [CompilationObservationManifest],
  identity: &CompilerCacheIdentity,
  cargo_outputs: &HashMap<String, CargoBuildScriptOutput>,
) {
  for manifest in manifests {
    if manifest.unit.target_kind != crate::compiler::observation::CompilationTargetKind::BuildScript {
      continue;
    }
    let (cargo_output, limitation) = select_build_script_output(cargo_outputs.get(&manifest.unit.package));
    manifest.build_script_result = Some(analyze_build_script_result(
      &identity.observation_context.source_root,
      BuildScriptResultInputs {
        instruction_stream: None,
        environment_reads: None,
        generated_outputs: None,
        execution: None,
        secret_capabilities: BTreeSet::new(),
        limitations: BTreeSet::from([limitation.to_string()]),
      },
      cargo_output,
    ));
  }
}

fn compiler_observation_miss_reason<'a>(
  observations: &'a [CompilationObservationManifest],
  workspace_root: &Path,
) -> Option<&'a str> {
  if observations.is_empty() {
    return Some("compilation_observations_absent");
  }
  if let Some(reason) = observations
    .iter()
    .flat_map(|manifest| manifest.bypasses.iter().map(String::as_str))
    .next()
  {
    return Some(reason);
  }
  for manifest in observations {
    if let Some(reason) = manifest.revalidation_reason(workspace_root) {
      return Some(reason);
    }
  }
  None
}

fn reconcile_exact_artifact_observations(
  observations: &mut [CompilationObservationManifest],
  retained: &mut HashMap<String, CompilationObservationManifest>,
) {
  for observation in observations {
    let Some(identity) = observation.cargo_artifact_identity.clone() else {
      continue;
    };
    if observation.has_bypass("rustc_invocation_unavailable") {
      if let Some(exact) = retained.get(&identity) {
        observation.clone_from(exact);
      }
    } else if !observation.has_bypass("cargo_artifact_unavailable")
      && !observation.has_bypass("dep_info_unavailable")
      && !observation.has_bypass("dep_info_path_unavailable")
    {
      retained.insert(identity, observation.clone());
    }
  }
}

fn executable_toolchain_fingerprint(
  snapshot: &WorkspaceSnapshot,
  executables: &ToolchainExecutableIdentities,
  cargo_rail_executable: &ExecutableIdentity,
  cache_wrapper_plan: CacheWrapperPlan,
) -> RailResult<String> {
  let toolchain = snapshot.toolchain();
  let mut framed = Vec::from(&b"cargo-rail-executable-toolchain-v2\0"[..]);
  append_identity_frame(&mut framed, b"executables", &executables.identity_bytes()?);
  append_identity_frame(&mut framed, b"host-target", toolchain.host_target().as_bytes());
  append_identity_frame(&mut framed, b"platform-family", std::env::consts::FAMILY.as_bytes());
  append_identity_frame(&mut framed, b"platform-os", std::env::consts::OS.as_bytes());
  append_identity_frame(&mut framed, b"platform-arch", std::env::consts::ARCH.as_bytes());
  append_identity_frame(
    &mut framed,
    b"cargo-rail-diagnostic-wrapper",
    &cargo_rail_executable.identity_bytes()?,
  );
  append_identity_frame(
    &mut framed,
    b"compiler-cache-disposition",
    cache_wrapper_plan.reason().as_bytes(),
  );
  if cache_wrapper_plan.installs_cargo_rail() {
    append_identity_frame(
      &mut framed,
      b"cargo-rail-cache-wrapper",
      &cargo_rail_executable.identity_bytes()?,
    );
  }
  Ok(format!("sha256:{}", ContentDigest::sha256(&framed)))
}

fn package_observation_identities(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<PackageId, String>> {
  snapshot
    .base_resolution()
    .metadata()
    .packages
    .iter()
    .map(|package| {
      let identity = if let Some(source) = &package.source {
        let checksum = snapshot.lockfile().and_then(|lockfile| {
          lockfile.packages().iter().find_map(|locked| {
            (locked.name() == package.name.as_str()
              && locked.version() == package.version.to_string()
              && locked.source() == Some(source.repr.as_str()))
            .then(|| locked.checksum())
            .flatten()
          })
        });
        format!(
          "external:{}#{}@{}#{}",
          source.repr,
          package.name,
          package.version,
          checksum.unwrap_or("unverified")
        )
      } else {
        let snapshot_package = snapshot
          .packages()
          .iter()
          .find(|candidate| candidate.id() == &package.id)
          .ok_or_else(|| RailError::message(format!("snapshot is missing local package '{}'", package.id)))?;
        let manifest = snapshot_package
          .manifest_path()
          .ok_or_else(|| RailError::message(format!("local package '{}' has no manifest identity", package.id)))?;
        format!("local:{}#{}@{}", manifest.as_str(), package.name, package.version)
      };
      Ok((package.id.clone(), identity))
    })
    .collect()
}

fn package_dependency_graph(
  snapshot: &WorkspaceSnapshot,
  identities: &HashMap<PackageId, String>,
) -> RailResult<HashMap<String, BTreeSet<String>>> {
  let mut graph = identities
    .values()
    .cloned()
    .map(|identity| (identity, BTreeSet::new()))
    .collect::<HashMap<_, _>>();
  let resolve = snapshot
    .base_resolution()
    .metadata()
    .resolve
    .as_ref()
    .ok_or_else(|| RailError::message("Cargo metadata omitted the resolved package graph"))?;
  for node in &resolve.nodes {
    let consumer = identities.get(&node.id).ok_or_else(|| {
      RailError::message(format!(
        "resolved package '{}' has no portable compiler-observation identity",
        node.id
      ))
    })?;
    let dependencies = graph
      .get_mut(consumer)
      .ok_or_else(|| RailError::message("portable package dependency graph lost its consumer"))?;
    for dependency in &node.deps {
      let identity = identities.get(&dependency.pkg).ok_or_else(|| {
        RailError::message(format!(
          "resolved dependency '{}' has no portable compiler-observation identity",
          dependency.pkg
        ))
      })?;
      dependencies.insert(identity.clone());
    }
  }
  Ok(graph)
}

fn build_script_package_contexts(
  snapshot: &WorkspaceSnapshot,
  observation_identities: &HashMap<PackageId, String>,
) -> RailResult<HashMap<String, BuildScriptPackageContext>> {
  let package_ids = snapshot
    .base_resolution()
    .metadata()
    .packages
    .iter()
    .filter(|package| package.source.is_none())
    .filter(|package| {
      package
        .targets
        .iter()
        .any(|target| target.kind.contains(&TargetKind::CustomBuild))
    })
    .map(|package| package.id.clone())
    .collect::<HashSet<_>>();
  snapshot
    .packages()
    .iter()
    .filter(|package| package_ids.contains(package.id()))
    .map(|package| {
      let observation_identity = observation_identities.get(package.id()).ok_or_else(|| {
        RailError::message(format!(
          "local build-script package '{}' has no portable observation identity",
          package.id()
        ))
      })?;
      let manifest = package
        .manifest_path()
        .ok_or_else(|| RailError::message(format!("local package '{}' has no manifest identity", package.id())))?;
      let working_directory = manifest.as_path().parent().unwrap_or_else(|| Path::new(""));
      Ok((
        observation_identity.clone(),
        BuildScriptPackageContext {
          package_id: package.id().clone(),
          working_directory: format!("repository:{}", crate::utils::path_to_git_format(working_directory)),
        },
      ))
    })
    .collect()
}

fn target_fingerprints(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<String, String>> {
  let mut fingerprints = HashMap::new();
  for target in snapshot.targets() {
    let identity = format!(
      "sha256:{}",
      ContentDigest::sha256(&target.portable_snapshot_identity(snapshot.source_root())?)
    );
    fingerprints.insert(target_name(target).to_string(), identity.clone());
    if target.is_build_target() || (target.is_host() && !fingerprints.contains_key("default")) {
      fingerprints.insert("default".to_string(), identity);
    }
  }
  if !fingerprints.contains_key("default") {
    return Err(RailError::message(
      "compiler evidence snapshot contains no default build or host target identity",
    ));
  }
  Ok(fingerprints)
}

fn compiler_cache_bypass_reason(snapshot: &WorkspaceSnapshot) -> Option<&'static str> {
  if !snapshot.cargo_config().unmodeled_settings().is_empty() {
    return Some("cargo_configuration_unmodeled");
  }
  for package in &snapshot.base_resolution().metadata().packages {
    if package
      .targets
      .iter()
      .flat_map(|target| target.kind.iter())
      .any(|kind| *kind == TargetKind::CustomBuild)
    {
      return Some("build_script_observations_unavailable");
    }
    if package
      .targets
      .iter()
      .flat_map(|target| target.kind.iter())
      .any(|kind| *kind == TargetKind::ProcMacro)
    {
      return Some("proc_macro_observations_unavailable");
    }
  }
  snapshot
    .packages()
    .iter()
    .any(|package| package.source().is_some() && package.checksum().is_none())
    .then_some("external_source_digest_unavailable")
}

fn target_name(target: &crate::cargo::resolution::TargetIdentity) -> &str {
  match target.specification() {
    crate::cargo::resolution::TargetSpecificationIdentity::BuiltIn(name) => name,
    crate::cargo::resolution::TargetSpecificationIdentity::Custom(specification) => specification.name(),
  }
}

fn package_source_fingerprints(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<PackageId, String>> {
  let manifest_paths = snapshot
    .manifests()
    .iter()
    .map(|manifest| manifest.path())
    .collect::<BTreeSet<_>>();
  let mut package_roots = HashMap::new();
  let mut roots_by_package = HashMap::new();
  let mut identities = HashMap::new();
  for package in snapshot
    .packages()
    .iter()
    .filter(|package| package.package_root().is_some())
  {
    let root = package.package_root().ok_or_else(|| {
      RailError::message(format!(
        "compiler evidence package '{}' is not backed by local snapshot source",
        package.id()
      ))
    })?;
    package_roots.insert(root, package.id());
    roots_by_package.insert(package.id(), root);
    identities.insert(package.id(), Vec::from(&b"cargo-rail-compiler-source-v1\0"[..]));
  }

  for entry in snapshot.source().tree().entries() {
    if manifest_paths.contains(&entry.path) {
      continue;
    }
    let Some(package_id) = entry
      .path
      .as_path()
      .ancestors()
      .find_map(|ancestor| package_roots.get(ancestor).copied())
    else {
      continue;
    };
    let package_root = roots_by_package[package_id];
    let relative = entry.path.as_path().strip_prefix(package_root).map_err(|_| {
      RailError::message(format!(
        "source entry '{}' is outside package '{}' root",
        entry.path, package_id
      ))
    })?;
    let identity = identities.get_mut(package_id).ok_or_else(|| {
      RailError::message(format!(
        "compiler source identity is missing local package '{package_id}'"
      ))
    })?;
    append_identity_frame(identity, b"path", crate::utils::path_to_git_format(relative).as_bytes());
    match &entry.kind {
      SourceEntryKind::RegularFile { digest, executable } => {
        append_identity_frame(identity, b"kind", b"regular-file");
        append_identity_frame(identity, b"content", digest.as_bytes());
        append_identity_frame(identity, b"executable", &[u8::from(*executable)]);
      }
      SourceEntryKind::Symlink { target } => {
        append_identity_frame(identity, b"kind", b"symlink");
        append_identity_frame(identity, b"target", target.as_bytes());
      }
      SourceEntryKind::Deleted => {
        return Err(RailError::message(format!(
          "compiler source identity contains deleted entry '{}'",
          entry.path
        )));
      }
    }
  }
  Ok(
    identities
      .into_iter()
      .map(|(package_id, identity)| {
        (
          package_id.clone(),
          format!("sha256:{}", ContentDigest::sha256(&identity)),
        )
      })
      .collect(),
  )
}

fn declared_local_dependency_graph(snapshot: &WorkspaceSnapshot) -> RailResult<HashMap<PackageId, Vec<PackageId>>> {
  let local_packages = snapshot
    .packages()
    .iter()
    .filter(|package| package.source().is_none())
    .map(|package| package.id())
    .collect::<HashSet<_>>();
  let mut roots = HashMap::new();
  for package in &snapshot.base_resolution().metadata().packages {
    if !local_packages.contains(&package.id) {
      continue;
    }
    let root = package
      .manifest_path
      .as_std_path()
      .parent()
      .ok_or_else(|| RailError::message(format!("local package '{}' manifest has no parent", package.id)))?;
    let root = std::fs::canonicalize(root)
      .with_context(|| format!("resolving local package '{}' root for compiler evidence", package.id))?;
    if let Some(previous) = roots.insert(root.clone(), package.id.clone())
      && previous != package.id
    {
      return Err(RailError::message(format!(
        "local packages '{previous}' and '{}' share compiler input root '{}'",
        package.id,
        root.display()
      )));
    }
  }

  let mut graph = HashMap::new();
  for package in &snapshot.base_resolution().metadata().packages {
    if !local_packages.contains(&package.id) {
      continue;
    }
    let mut dependencies = BTreeSet::new();
    for dependency in &package.dependencies {
      let Some(path) = dependency.path.as_ref() else {
        for candidate in &snapshot.base_resolution().metadata().packages {
          if local_packages.contains(&candidate.id)
            && candidate.name == dependency.name
            && dependency.req.matches(&candidate.version)
          {
            dependencies.insert(candidate.id.clone());
          }
        }
        continue;
      };
      let root = std::fs::canonicalize(path.as_std_path()).with_context(|| {
        format!(
          "resolving local dependency '{}' declared by '{}' for compiler evidence",
          dependency.name, package.id
        )
      })?;
      let dependency_id = roots.get(&root).ok_or_else(|| {
        RailError::message(format!(
          "local dependency '{}' declared by '{}' is absent from the captured package graph",
          dependency.name, package.id
        ))
      })?;
      dependencies.insert(dependency_id.clone());
    }
    graph.insert(package.id.clone(), dependencies.into_iter().collect());
  }
  Ok(graph)
}

fn manifest_closure_fingerprints(
  snapshot: &WorkspaceSnapshot,
  dependencies: &HashMap<PackageId, Vec<PackageId>>,
) -> RailResult<HashMap<PackageId, String>> {
  let manifests = snapshot
    .manifests()
    .iter()
    .map(|manifest| (manifest.path(), manifest))
    .collect::<HashMap<_, _>>();
  let packages = snapshot
    .packages()
    .iter()
    .map(|package| (package.id(), package))
    .collect::<HashMap<_, _>>();
  let root_manifest = snapshot
    .manifests()
    .iter()
    .find(|manifest| manifest.path().as_path() == Path::new("Cargo.toml"));
  let mut fingerprints = HashMap::new();

  for member in snapshot
    .packages()
    .iter()
    .filter(|package| package.is_workspace_member())
  {
    let mut closure = BTreeMap::new();
    if let Some(manifest) = root_manifest {
      closure.insert(manifest.path(), manifest.digest());
    }
    for package_id in local_dependency_closure(member.id(), dependencies) {
      let package = packages.get(&package_id).ok_or_else(|| {
        RailError::message(format!(
          "compiler manifest identity is missing local package '{package_id}'"
        ))
      })?;
      let manifest_path = package.manifest_path().ok_or_else(|| {
        RailError::message(format!(
          "local dependency '{package_id}' has no logical manifest identity"
        ))
      })?;
      let manifest = manifests.get(manifest_path).ok_or_else(|| {
        RailError::message(format!(
          "local dependency '{package_id}' manifest '{manifest_path}' is absent from the snapshot"
        ))
      })?;
      closure.insert(manifest.path(), manifest.digest());
    }

    let mut identity = Vec::from(&b"cargo-rail-compiler-manifest-closure-v1\0"[..]);
    for (path, digest) in closure {
      append_identity_frame(&mut identity, b"path", path.as_str().as_bytes());
      append_identity_frame(&mut identity, b"content", digest.as_bytes());
    }
    fingerprints.insert(
      member.id().clone(),
      format!("sha256:{}", ContentDigest::sha256(&identity)),
    );
  }
  Ok(fingerprints)
}

fn source_closure_fingerprints(
  snapshot: &WorkspaceSnapshot,
  dependencies: &HashMap<PackageId, Vec<PackageId>>,
) -> RailResult<HashMap<PackageId, String>> {
  let package_sources = package_source_fingerprints(snapshot)?;
  let packages = snapshot
    .packages()
    .iter()
    .map(|package| (package.id(), package))
    .collect::<HashMap<_, _>>();
  let mut fingerprints = HashMap::new();

  for member in snapshot
    .packages()
    .iter()
    .filter(|package| package.is_workspace_member())
  {
    let mut closure = BTreeMap::new();
    for package_id in local_dependency_closure(member.id(), dependencies) {
      let package = packages.get(&package_id).ok_or_else(|| {
        RailError::message(format!(
          "compiler source identity is missing local package '{package_id}'"
        ))
      })?;
      let source_fingerprint = package_sources.get(&package_id).ok_or_else(|| {
        RailError::message(format!(
          "local dependency '{package_id}' source is absent from the authoritative snapshot"
        ))
      })?;
      let manifest = package.manifest_path().ok_or_else(|| {
        RailError::message(format!(
          "local dependency '{package_id}' has no logical manifest identity"
        ))
      })?;
      closure.insert(manifest, source_fingerprint);
    }

    let mut identity = Vec::from(&b"cargo-rail-compiler-source-closure-v1\0"[..]);
    for (manifest, source_fingerprint) in closure {
      append_identity_frame(&mut identity, b"manifest", manifest.as_str().as_bytes());
      append_identity_frame(&mut identity, b"source", source_fingerprint.as_bytes());
    }
    fingerprints.insert(
      member.id().clone(),
      format!("sha256:{}", ContentDigest::sha256(&identity)),
    );
  }
  Ok(fingerprints)
}

fn local_dependency_closure(
  root: &PackageId,
  dependencies: &HashMap<PackageId, Vec<PackageId>>,
) -> BTreeSet<PackageId> {
  let mut pending = vec![root.clone()];
  let mut visited = BTreeSet::new();
  while let Some(package_id) = pending.pop() {
    if !visited.insert(package_id.clone()) {
      continue;
    }
    if let Some(package_dependencies) = dependencies.get(&package_id) {
      pending.extend(package_dependencies.iter().cloned());
    }
  }
  visited
}

fn now_unix_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as u64)
    .unwrap_or(0)
}

fn compiler_env_fingerprint(snapshot: &WorkspaceSnapshot) -> RailResult<String> {
  Ok(format!(
    "sha256:{}",
    ContentDigest::sha256(&serde_json::to_vec(snapshot.cargo_config().environment())?)
  ))
}

fn cargo_config_fingerprint(snapshot: &WorkspaceSnapshot) -> RailResult<String> {
  Ok(format!(
    "sha256:{}",
    ContentDigest::sha256(
      &snapshot
        .cargo_config()
        .portable_snapshot_identity(snapshot.source_root())?
    )
  ))
}

fn append_identity_frame(output: &mut Vec<u8>, tag: &[u8], value: &[u8]) {
  output.extend_from_slice(&(tag.len() as u64).to_le_bytes());
  output.extend_from_slice(tag);
  output.extend_from_slice(&(value.len() as u64).to_le_bytes());
  output.extend_from_slice(value);
}

fn build_manifest_member_index(members: &[crate::cargo::manifest_analyzer::ParsedManifest]) -> HashMap<String, String> {
  let mut index = HashMap::with_capacity(members.len() * 2);
  for member in members {
    index.insert(member.path.to_string_lossy().into_owned(), member.package_name.clone());
    if let Ok(canonical) = member.path.canonicalize() {
      index.insert(canonical.to_string_lossy().into_owned(), member.package_name.clone());
    }
  }
  index
}

fn parse_unused_crate_name(message: &str) -> Option<&str> {
  let prefix = "extern crate `";
  let start = message.find(prefix)? + prefix.len();
  let rest = &message[start..];
  let end = rest.find('`')?;
  Some(&rest[..end])
}

#[derive(Debug, Deserialize)]
struct CargoEvent {
  reason: String,
  manifest_path: Option<String>,
  target: Option<CargoTarget>,
  message: Option<CargoDiagnostic>,
  profile: Option<CargoProfile>,
}

#[derive(Debug, Deserialize)]
struct CargoProfile {
  test: bool,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
  kind: Vec<String>,
  name: String,
  src_path: Option<String>,
}

impl CargoTarget {
  fn identifier(&self, workspace_root: &Path, test_mode: bool) -> CompilationUnitId {
    let kind = if self
      .kind
      .iter()
      .any(|kind| matches!(kind.as_str(), "lib" | "rlib" | "dylib" | "cdylib" | "staticlib"))
    {
      CargoTargetKind::Library
    } else if self.kind.iter().any(|kind| kind == "proc-macro") {
      CargoTargetKind::ProcMacro
    } else if self.kind.iter().any(|kind| kind == "bin") {
      CargoTargetKind::Binary
    } else if self.kind.iter().any(|kind| kind == "test") {
      CargoTargetKind::Test
    } else if self.kind.iter().any(|kind| kind == "example") {
      CargoTargetKind::Example
    } else if self.kind.iter().any(|kind| kind == "bench") {
      CargoTargetKind::Benchmark
    } else if self.kind.iter().any(|kind| kind == "custom-build") {
      CargoTargetKind::CustomBuild
    } else {
      CargoTargetKind::Other(self.kind.join(","))
    };
    let source = self.src_path.as_deref().map(|source| {
      Path::new(source)
        .strip_prefix(workspace_root)
        .unwrap_or_else(|_| Path::new(source))
        .to_string_lossy()
        .into_owned()
    });
    CompilationUnitId {
      kind,
      name: self.name.clone(),
      source,
      test_mode,
    }
  }
}

#[derive(Debug, Deserialize)]
struct CargoDiagnostic {
  message: String,
  code: Option<CargoDiagnosticCode>,
}

#[derive(Debug, Deserialize)]
struct CargoDiagnosticCode {
  code: Option<String>,
}

fn is_relevant_target(target: &CargoTarget) -> bool {
  !target.kind.is_empty()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn build_script_executable_comes_from_cargo_filenames() {
    let unix = FileObservation {
      path: ObservationPath::Repository("target/debug/build/unit/build-script-build".to_string()),
      content_digest: "sha256:unix".to_string(),
      executable: true,
      symlink_target: None,
    };
    assert_eq!(
      build_script_executable_output(std::slice::from_ref(&unix), "build-script-build", ""),
      Some(unix)
    );

    let windows = FileObservation {
      path: ObservationPath::Repository("target/debug/build/unit/build-script-build.exe".to_string()),
      content_digest: "sha256:windows".to_string(),
      executable: false,
      symlink_target: None,
    };
    let debug_symbols = FileObservation {
      path: ObservationPath::Repository("target/debug/build/unit/build-script-build.pdb".to_string()),
      content_digest: "sha256:pdb".to_string(),
      executable: false,
      symlink_target: None,
    };
    assert_eq!(
      build_script_executable_output(&[debug_symbols, windows.clone()], "build-script-build", ".exe"),
      Some(windows.clone())
    );
    let ambiguous = FileObservation {
      path: ObservationPath::Repository("other/build-script-build.exe".to_string()),
      ..windows.clone()
    };
    assert_eq!(
      build_script_executable_output(&[windows, ambiguous], "build-script-build", ".exe"),
      None
    );
  }

  #[test]
  fn cargo_build_script_summary_discards_values_and_physical_paths() {
    let script: cargo_metadata::BuildScript = serde_json::from_value(serde_json::json!({
      "package_id": "path+file:///workspace#unit@0.1.0",
      "linked_libs": ["static=never-persist-this-library"],
      "linked_paths": ["native=/physical/never-persist-this-path"],
      "cfgs": ["never_persist_this_cfg"],
      "env": [["REGISTRY_TOKEN", "never-persist-this-value"]],
      "out_dir": "/physical/never-persist-this-output",
    }))
    .expect("Cargo build-script message");
    let summary = build_script_output_summary(&script);
    assert_eq!(
      summary,
      BuildScriptCargoOutputSummary {
        linked_libraries: 1,
        linked_paths: 1,
        cfgs: 1,
        rustc_environment: 1,
        output_directory_reported: true,
      }
    );
    let encoded = serde_json::to_string(&summary).expect("serialize redacted summary");
    for raw in [
      "never-persist-this-library",
      "never-persist-this-path",
      "never_persist_this_cfg",
      "REGISTRY_TOKEN",
      "never-persist-this-value",
      "never-persist-this-output",
    ] {
      assert!(!encoded.contains(raw), "persisted raw Cargo output {raw:?}");
    }
  }

  #[test]
  fn cargo_build_script_output_selection_fails_closed() {
    let output = BuildScriptCargoOutputSummary {
      linked_libraries: 1,
      linked_paths: 2,
      cfgs: 3,
      rustc_environment: 4,
      output_directory_reported: true,
    };
    assert_eq!(
      select_build_script_output(Some(&CargoBuildScriptOutput::One(output.clone()))),
      (
        Some(output.clone()),
        "cargo_build_script_execution_freshness_unavailable"
      )
    );
    assert_eq!(
      select_build_script_output(None),
      (None, "cargo_build_script_output_unavailable")
    );
    assert_eq!(
      select_build_script_output(Some(&CargoBuildScriptOutput::Ambiguous)),
      (None, "cargo_build_script_output_ambiguous")
    );
  }

  #[test]
  fn test_candidate_scheduler_keeps_inapplicable_declaration_for_later_target() {
    let candidates = vec![
      CompilerCandidate {
        member: "member".to_string(),
        crate_name: "alpha".to_string(),
        kind: crate::cargo::manifest_analyzer::DepKind::Normal,
        applicable_targets: BTreeSet::from(["linux".to_string()]),
        required_features: None,
      },
      CompilerCandidate {
        member: "member".to_string(),
        crate_name: "beta".to_string(),
        kind: crate::cargo::manifest_analyzer::DepKind::Normal,
        applicable_targets: BTreeSet::from(["linux".to_string(), "macos".to_string()]),
        required_features: None,
      },
    ];
    let targets = build_candidate_target_index(&candidates);
    let mut survivors = HashMap::from([(
      "member".to_string(),
      BTreeSet::from([
        (
          crate::cargo::manifest_analyzer::DepKind::Normal,
          "alpha".to_string(),
          None,
        ),
        (
          crate::cargo::manifest_analyzer::DepKind::Normal,
          "beta".to_string(),
          None,
        ),
      ]),
    )]);
    let evidence = test_evidence(&["alpha"]);

    update_candidate_survivors(&mut survivors, &targets, "member", "macos", &evidence);

    assert_eq!(
      survivors["member"],
      BTreeSet::from([(
        crate::cargo::manifest_analyzer::DepKind::Normal,
        "alpha".to_string(),
        None
      )])
    );
    assert!(!has_applicable_survivor(
      &survivors,
      &targets,
      "member",
      "macos",
      &FeatureSelection::Default
    ));
    assert!(has_applicable_survivor(
      &survivors,
      &targets,
      "member",
      "linux",
      &FeatureSelection::Default
    ));
  }

  #[test]
  fn test_candidate_scheduler_stops_after_positive_usage() {
    let candidates = vec![CompilerCandidate {
      member: "member".to_string(),
      crate_name: "alpha".to_string(),
      kind: crate::cargo::manifest_analyzer::DepKind::Normal,
      applicable_targets: BTreeSet::from(["linux".to_string()]),
      required_features: None,
    }];
    let targets = build_candidate_target_index(&candidates);
    let mut survivors = HashMap::from([(
      "member".to_string(),
      BTreeSet::from([(
        crate::cargo::manifest_analyzer::DepKind::Normal,
        "alpha".to_string(),
        None,
      )]),
    )]);

    update_candidate_survivors(&mut survivors, &targets, "member", "linux", &test_evidence(&[]));

    assert!(survivors["member"].is_empty());
  }

  #[test]
  fn test_candidate_scheduler_defers_optional_candidate_until_required_feature_mode() {
    let candidates = vec![CompilerCandidate {
      member: "member".to_string(),
      crate_name: "optional_dep".to_string(),
      kind: crate::cargo::manifest_analyzer::DepKind::Normal,
      applicable_targets: BTreeSet::from(["linux".to_string()]),
      required_features: Some(FeatureSelection::AllFeatures),
    }];
    let targets = build_candidate_target_index(&candidates);
    let candidate = (
      crate::cargo::manifest_analyzer::DepKind::Normal,
      "optional_dep".to_string(),
      Some(FeatureSelection::AllFeatures),
    );
    let mut survivors = HashMap::from([("member".to_string(), BTreeSet::from([candidate]))]);

    update_candidate_survivors(&mut survivors, &targets, "member", "linux", &test_evidence(&[]));
    assert_eq!(survivors["member"].len(), 1, "default-mode evidence is inapplicable");
    assert!(!has_applicable_survivor(
      &survivors,
      &targets,
      "member",
      "linux",
      &FeatureSelection::Default
    ));
    assert!(has_applicable_survivor(
      &survivors,
      &targets,
      "member",
      "linux",
      &FeatureSelection::AllFeatures
    ));

    let mut all_features = test_evidence(&[]);
    all_features.features = FeatureSelection::AllFeatures;
    update_candidate_survivors(&mut survivors, &targets, "member", "linux", &all_features);
    assert!(survivors["member"].is_empty());
  }

  fn test_evidence(unused: &[&str]) -> TargetEvidence {
    TargetEvidence {
      platform: PlatformTarget::from("test"),
      features: FeatureSelection::Default,
      compiled_units: BTreeSet::from([CompilationUnitId {
        kind: CargoTargetKind::Library,
        name: "member".to_string(),
        source: Some("src/lib.rs".to_string()),
        test_mode: false,
      }]),
      unused_crates: unused.iter().map(|value| (*value).to_string()).collect(),
      unit_evidence: Vec::new(),
      completeness: DiagnosticsCompleteness::Complete,
    }
  }
}
