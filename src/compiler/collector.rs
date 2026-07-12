//! Target-aware compiler diagnostics collection with persistent caching.

use crate::cargo::DepKind;
use crate::cargo::manifest_analyzer::ManifestAnalyzer;
use crate::compiler::diagnostics_store::CompilerDiagnosticsStore;
use crate::compiler::model::{
  AnalysisConfiguration, COLLECTOR_VERSION, CargoTargetKind, CompilationUnitEvidence, CompilationUnitId,
  CompilerDiagEntry, CompilerDiagKey, DependencyEvidenceState, DiagnosticsCompleteness, EvidenceCacheSummary,
  FeatureSelection, MemberEvidence, PlatformTarget, TargetEvidence,
};
use crate::compiler::wrapper::{INNER_WRAPPER_ENV, WRAPPER_MARKER};
use crate::error::{RailError, RailResult, ResultExt};
use crate::progress;
use crate::utils::{file_fingerprint, fnv1a64};
use cargo_metadata::PackageId;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
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
  rustc_version: String,
  cargo_version: String,
  host_triple: String,
  lock_fingerprint: String,
  compiler_env_fingerprint: String,
  cargo_config_fingerprint: String,
  enable_cache: bool,
}

impl<'a> CompilerDiagnosticsCollector<'a> {
  /// Create a new collector for a workspace-level analysis pass.
  pub fn new(
    workspace_root: &'a Path,
    manifests: &'a ManifestAnalyzer,
    targets: Vec<&'a str>,
    enable_cache: bool,
  ) -> RailResult<Self> {
    let (rustc_version, host_triple) = rustc_identity(workspace_root)?;
    let cargo_version = cargo_identity(workspace_root)?;
    let lock_fingerprint = file_fingerprint(&workspace_root.join("Cargo.lock"));
    let compiler_env_fingerprint = compiler_env_fingerprint();
    let cargo_config_fingerprint = cargo_config_fingerprint(workspace_root);

    Ok(Self {
      workspace_root,
      manifests,
      targets,
      rustc_version,
      cargo_version,
      host_triple,
      lock_fingerprint,
      compiler_env_fingerprint,
      cargo_config_fingerprint,
      enable_cache,
    })
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

    let mut store = self
      .enable_cache
      .then(|| CompilerDiagnosticsStore::load(self.workspace_root));
    let key_inputs = self.build_key_inputs(&members);
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
    let mut prior_source_evidence: HashMap<(String, AnalysisConfiguration), TargetEvidence> = HashMap::new();
    let mut surviving_unused: HashMap<String, BTreeSet<CandidateId>> = candidate_targets
      .iter()
      .map(|(member, candidates)| (member.clone(), candidates.keys().cloned().collect()))
      .collect();

    for (member, target, features, key) in key_inputs {
      if let Some(store) = store.as_ref()
        && let Some(entry) = store.get(&key)
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

      let reason = store.as_ref().map_or("cache_disabled", |store| store.miss_reason(&key));
      if let Some(prior) = store.as_ref().and_then(|store| store.prior_for_source_change(&key)) {
        prior_source_evidence.insert(
          (
            member.to_string(),
            AnalysisConfiguration {
              platform: PlatformTarget::from(target),
              features: features.clone(),
            },
          ),
          prior.evidence.clone(),
        );
      }
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
      let run = run_workspace_check(self.workspace_root, target, &features, &active_members)?;
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

        let manifest_fp = file_fingerprint(&manifests_member.path);
        let source_fp = source_tree_fingerprint(
          manifests_member
            .path
            .parent()
            .unwrap_or(manifests_member.path.as_path()),
        );

        let key = CompilerDiagKey {
          package_id: manifests_member.package_id.clone(),
          target: PlatformTarget::from(target),
          features: features.clone(),
          rustc_version: self.rustc_version.clone(),
          cargo_version: self.cargo_version.clone(),
          host_triple: self.host_triple.clone(),
          lock_fingerprint: self.lock_fingerprint.clone(),
          manifest_fingerprint: manifest_fp,
          source_fingerprint: source_fp,
          compiler_env_fingerprint: self.compiler_env_fingerprint.clone(),
          cargo_config_fingerprint: self.cargo_config_fingerprint.clone(),
        };

        let mut unused = BTreeSet::new();
        let mut compiled = BTreeSet::new();

        if completeness == DiagnosticsCompleteness::Complete
          && let Some(parsed_member) = parsed.get(member)
        {
          compiled = parsed_member.compiled_targets.clone();
        }

        let mut unit_evidence = parsed
          .get(member)
          .map(ParsedMemberTarget::unit_evidence)
          .unwrap_or_default();
        let configuration = AnalysisConfiguration {
          platform: PlatformTarget::from(target),
          features: features.clone(),
        };
        if let (Some(parsed_member), Some(prior)) = (
          parsed.get(member),
          prior_source_evidence.get(&(member.to_string(), configuration)),
        ) {
          merge_fresh_unit_evidence(&mut unit_evidence, prior, &parsed_member.fresh_targets);
        }
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
          unused_crates: unused.clone(),
          unit_evidence,
          completeness,
        };
        let entry = CompilerDiagEntry {
          key,
          evidence: evidence.clone(),
          generated_at_unix_ms: now_unix_ms(),
          collector_version: COLLECTOR_VERSION,
        };

        update_candidate_survivors(
          &mut surviving_unused,
          &candidate_targets,
          member,
          target,
          &entry.evidence,
        );

        record_target_evidence(&mut result, &manifests_member.package_id, &entry.evidence);
        if let Some(store) = store.as_mut() {
          store.put(entry);
        }
      }
    }

    if let Some(store) = store.as_mut() {
      store.flush()?;
    }
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

  fn build_key_inputs(&self, members: &HashSet<&str>) -> Vec<(&str, &str, FeatureSelection, CompilerDiagKey)> {
    let mut keys = Vec::with_capacity(members.len() * self.targets.len() * FeatureSelection::BASELINES.len());

    for member in &self.manifests.members {
      if !members.contains(member.package_name.as_str()) {
        continue;
      }

      let manifest_fp = file_fingerprint(&member.path);
      let source_fp = source_tree_fingerprint(member.path.parent().unwrap_or(member.path.as_path()));

      let selections = planned_feature_selections(member);
      for target in &self.targets {
        for features in &selections {
          keys.push((
            member.package_name.as_str(),
            *target,
            features.clone(),
            CompilerDiagKey {
              package_id: member.package_id.clone(),
              target: PlatformTarget::from(*target),
              features: features.clone(),
              rustc_version: self.rustc_version.clone(),
              cargo_version: self.cargo_version.clone(),
              host_triple: self.host_triple.clone(),
              lock_fingerprint: self.lock_fingerprint.clone(),
              manifest_fingerprint: manifest_fp.clone(),
              source_fingerprint: source_fp.clone(),
              compiler_env_fingerprint: self.compiler_env_fingerprint.clone(),
              cargo_config_fingerprint: self.cargo_config_fingerprint.clone(),
            },
          ));
        }
      }
    }

    keys
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
}

fn run_workspace_check(
  workspace_root: &Path,
  target: &str,
  features: &FeatureSelection,
  members: &[&str],
) -> RailResult<WorkspaceCheckOutput> {
  let wrapper =
    std::env::current_exe().with_context(|| "locating cargo-rail executable for rustc wrapper".to_string())?;
  let existing_workspace_wrapper = std::env::var_os("RUSTC_WORKSPACE_WRAPPER");

  let mut args: Vec<OsString> = vec!["check".into(), "--all-targets".into(), "--message-format=json".into()];
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

  let mut command = Command::new("cargo");
  command
    .current_dir(workspace_root)
    .env("RUSTC_WORKSPACE_WRAPPER", &wrapper)
    .env(WRAPPER_MARKER, "1")
    .args(&args);
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
  })
}

#[derive(Debug, Default)]
struct ParsedMemberTarget {
  compiled_targets: BTreeSet<CompilationUnitId>,
  fresh_targets: BTreeSet<CompilationUnitId>,
  warned_targets_by_dep: HashMap<String, BTreeSet<CompilationUnitId>>,
}

fn merge_fresh_unit_evidence(
  current: &mut Vec<CompilationUnitEvidence>,
  prior: &TargetEvidence,
  fresh_units: &BTreeSet<CompilationUnitId>,
) {
  let prior_by_unit: BTreeMap<_, _> = prior
    .unit_evidence
    .iter()
    .map(|evidence| (&evidence.unit, evidence))
    .collect();
  for evidence in current {
    if fresh_units.contains(&evidence.unit)
      && let Some(prior) = prior_by_unit.get(&evidence.unit)
    {
      evidence.unused_crates.clone_from(&prior.unused_crates);
    }
  }
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
    if message.fresh == Some(true) {
      parsed_member.fresh_targets.insert(target_id);
    }
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

fn rustc_identity(workspace_root: &Path) -> RailResult<(String, String)> {
  let output = Command::new("rustc")
    .current_dir(workspace_root)
    .arg("-vV")
    .output()
    .with_context(|| "running rustc -vV".to_string())?;

  let stdout = String::from_utf8_lossy(&output.stdout);
  let mut version = String::new();
  let mut host = String::new();

  for line in stdout.lines() {
    if let Some(value) = line.strip_prefix("release: ") {
      version = value.trim().to_string();
      continue;
    }
    if let Some(value) = line.strip_prefix("host: ") {
      host = value.trim().to_string();
    }
  }

  if version.is_empty() {
    version = "unknown".to_string();
  }
  if host.is_empty() {
    host = "unknown".to_string();
  }

  Ok((version, host))
}

fn cargo_identity(workspace_root: &Path) -> RailResult<String> {
  let output = Command::new("cargo")
    .current_dir(workspace_root)
    .arg("-V")
    .output()
    .with_context(|| "running cargo -V".to_string())?;
  if !output.status.success() {
    return Err(RailError::message(format!(
      "cargo -V failed with status {}",
      output.status
    )));
  }
  Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn source_tree_fingerprint(member_dir: &Path) -> String {
  let mut hash: u64 = 0xcbf29ce484222325;
  let mut roots = vec![
    member_dir.join("src"),
    member_dir.join("tests"),
    member_dir.join("examples"),
    member_dir.join("benches"),
    member_dir.join("build.rs"),
  ];
  roots.sort_unstable();

  for path in roots {
    hash_path_metadata(member_dir, &path, &mut hash);
  }

  format!("fnv1a64:{hash:016x}")
}

fn hash_path_metadata(base: &Path, path: &Path, hash: &mut u64) {
  if !path.exists() {
    hash_bytes(hash, b"missing");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );
    return;
  }

  let Ok(metadata) = std::fs::metadata(path) else {
    hash_bytes(hash, b"metadata-error");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );
    return;
  };

  if metadata.is_file() {
    hash_bytes(hash, b"file");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );
    hash_bytes(hash, &metadata.len().to_le_bytes());
    let modified = metadata
      .modified()
      .ok()
      .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
      .map(|d| d.as_nanos() as u64)
      .unwrap_or(0);
    hash_bytes(hash, &modified.to_le_bytes());
    return;
  }

  if metadata.is_dir() {
    hash_bytes(hash, b"dir");
    hash_bytes(
      hash,
      path.strip_prefix(base).unwrap_or(path).to_string_lossy().as_bytes(),
    );

    let Ok(entries) = std::fs::read_dir(path) else {
      hash_bytes(hash, b"read-dir-error");
      return;
    };

    let mut child_paths = Vec::new();
    for entry in entries.flatten() {
      child_paths.push(entry.path());
    }
    child_paths.sort_unstable();

    for child in child_paths {
      hash_path_metadata(base, &child, hash);
    }
  }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
  const FNV_PRIME: u64 = 0x100000001b3;
  for byte in bytes {
    *hash ^= u64::from(*byte);
    *hash = hash.wrapping_mul(FNV_PRIME);
  }
}

fn now_unix_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as u64)
    .unwrap_or(0)
}

fn compiler_env_fingerprint() -> String {
  let rustflags = std::env::var("RUSTFLAGS").unwrap_or_default();
  let encoded_rustflags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
  let combined = format!("RUSTFLAGS={rustflags}\nCARGO_ENCODED_RUSTFLAGS={encoded_rustflags}");
  format!("fnv1a64:{:016x}", fnv1a64(combined.as_bytes()))
}

fn cargo_config_fingerprint(workspace_root: &Path) -> String {
  let cfg_toml = file_fingerprint(&workspace_root.join(".cargo").join("config.toml"));
  let cfg_legacy = file_fingerprint(&workspace_root.join(".cargo").join("config"));
  let combined = format!("{cfg_toml}\n{cfg_legacy}");
  format!("fnv1a64:{:016x}", fnv1a64(combined.as_bytes()))
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
  fresh: Option<bool>,
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

  #[test]
  fn test_source_tree_fingerprint_changes_with_mtime_or_size() {
    let temp = tempfile::tempdir().expect("tempdir");
    let member = temp.path();
    std::fs::create_dir_all(member.join("src")).expect("mkdir");
    std::fs::write(member.join("src/lib.rs"), "pub fn a() {}\n").expect("write1");

    let before = source_tree_fingerprint(member);
    std::fs::write(member.join("src/lib.rs"), "pub fn a() { let _ = 1; }\n").expect("write2");
    let after = source_tree_fingerprint(member);

    assert_ne!(before, after);
  }

  #[test]
  fn test_merge_fresh_unit_evidence_reuses_only_cargo_fresh_targets() {
    let fresh = CompilationUnitId {
      kind: CargoTargetKind::Example,
      name: "fresh-example".to_string(),
      source: Some("examples/fresh.rs".to_string()),
      test_mode: false,
    };
    let dirty = CompilationUnitId {
      kind: CargoTargetKind::Test,
      name: "dirty-test".to_string(),
      source: Some("tests/dirty.rs".to_string()),
      test_mode: true,
    };
    let prior = TargetEvidence {
      platform: PlatformTarget::from("default"),
      features: FeatureSelection::Default,
      compiled_units: BTreeSet::from([fresh.clone(), dirty.clone()]),
      unused_crates: BTreeSet::new(),
      unit_evidence: vec![
        CompilationUnitEvidence {
          unit: fresh.clone(),
          unused_crates: BTreeSet::from(["alpha".to_string()]),
        },
        CompilationUnitEvidence {
          unit: dirty.clone(),
          unused_crates: BTreeSet::from(["stale".to_string()]),
        },
      ],
      completeness: DiagnosticsCompleteness::Complete,
    };
    let mut current = vec![
      CompilationUnitEvidence {
        unit: fresh.clone(),
        unused_crates: BTreeSet::new(),
      },
      CompilationUnitEvidence {
        unit: dirty.clone(),
        unused_crates: BTreeSet::from(["beta".to_string()]),
      },
    ];

    merge_fresh_unit_evidence(&mut current, &prior, &BTreeSet::from([fresh]));

    assert_eq!(current[0].unused_crates, BTreeSet::from(["alpha".to_string()]));
    assert_eq!(current[1].unused_crates, BTreeSet::from(["beta".to_string()]));
  }
}
