//! Compiler diagnostics cache model for target-aware source-unused detection.

use crate::cargo::manifest_analyzer::DepKind;
use cargo_metadata::PackageId;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Cache format version for compiler diagnostics.
pub const COMPILER_DIAG_CACHE_VERSION: u32 = 10;

/// Maximum number of entries accepted from one legacy diagnostics cache file.
pub(crate) const MAX_COMPILER_DIAG_CACHE_ENTRIES: usize = 4096;

/// Collector version used to invalidate stale semantic behavior.
pub const COLLECTOR_VERSION: u32 = 15;

/// A rustc platform target or `default` for the workspace's native target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlatformTarget(pub String);

impl PlatformTarget {
  /// Borrow the target triple or `default` marker.
  #[must_use]
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl From<&str> for PlatformTarget {
  fn from(target: &str) -> Self {
    Self(target.to_string())
  }
}

/// Cargo feature selection used for one compiler analysis pass.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FeatureSelection {
  /// Cargo's ordinary default-feature behavior.
  Default,
  /// A condition-derived set enabled without default features.
  Selected(Vec<String>),
  /// Disable default features for selected workspace packages.
  NoDefaultFeatures,
  /// Enable every declared feature for selected workspace packages.
  AllFeatures,
}

impl FeatureSelection {
  /// Configurations required before an unused result is exhaustive.
  pub const BASELINES: [Self; 3] = [Self::Default, Self::NoDefaultFeatures, Self::AllFeatures];

  /// Stable cache and display label.
  #[must_use]
  pub fn label(&self) -> String {
    match self {
      Self::Default => "default-features".to_string(),
      Self::NoDefaultFeatures => "no-default-features".to_string(),
      Self::AllFeatures => "all-features".to_string(),
      Self::Selected(features) => format!("selected:{}", features.join(",")),
    }
  }
}

/// One root-independent feature/target coverage view for workspace packages.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CoverageView {
  /// Configured platform target covered by this view.
  pub target: PlatformTarget,
  /// Exact Cargo feature selection shared by every package in the view.
  pub features: FeatureSelection,
  /// Deterministically ordered workspace package names.
  pub packages: Vec<String>,
}

impl CoverageView {
  pub(crate) fn new(target: PlatformTarget, features: FeatureSelection, packages: BTreeSet<String>) -> Self {
    Self {
      target,
      features,
      packages: packages.into_iter().collect(),
    }
  }

  /// Return a root-independent identity for this exact coverage view.
  pub fn identity(&self) -> crate::error::RailResult<String> {
    let bytes = serde_json::to_vec(self)?;
    Ok(format!(
      "coverage:v1-sha256-{}",
      crate::source::ContentDigest::sha256(&bytes)
    ))
  }

  /// Return argv after `cargo` for a compile-only all-targets check.
  #[must_use]
  pub fn cargo_arguments(&self) -> Vec<String> {
    let mut arguments = vec!["check".to_string(), "--locked".to_string(), "--all-targets".to_string()];
    self.extend_feature_arguments(&mut arguments);
    self.extend_package_and_target_arguments(&mut arguments);
    arguments
  }

  /// Return argv after `cargo` for a nextest execution of this view.
  #[must_use]
  pub fn nextest_arguments(&self) -> Vec<String> {
    let mut arguments = vec!["nextest".to_string(), "run".to_string(), "--locked".to_string()];
    self.extend_feature_arguments(&mut arguments);
    self.extend_package_and_target_arguments(&mut arguments);
    arguments
  }

  fn extend_feature_arguments(&self, arguments: &mut Vec<String>) {
    match &self.features {
      FeatureSelection::Default => {}
      FeatureSelection::NoDefaultFeatures => arguments.push("--no-default-features".to_string()),
      FeatureSelection::AllFeatures => arguments.push("--all-features".to_string()),
      FeatureSelection::Selected(features) => {
        arguments.push("--no-default-features".to_string());
        for package in &self.packages {
          for feature in features {
            arguments.push("--features".to_string());
            arguments.push(format!("{package}/{feature}"));
          }
        }
      }
    }
  }

  fn extend_package_and_target_arguments(&self, arguments: &mut Vec<String>) {
    for package in &self.packages {
      arguments.push("--package".to_string());
      arguments.push(package.clone());
    }
    if self.target.as_str() != "default" {
      arguments.push("--target".to_string());
      arguments.push(self.target.as_str().to_string());
    }
  }
}

/// Cargo target domain reported by compiler messages.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CargoTargetKind {
  /// Library target.
  Library,
  /// Binary target.
  Binary,
  /// Unit or integration test target.
  Test,
  /// Example target.
  Example,
  /// Benchmark target.
  Benchmark,
  /// Procedural macro target.
  ProcMacro,
  /// Package build script.
  CustomBuild,
  /// A future Cargo target kind unknown to this collector version.
  Other(String),
}

/// Stable identity for one compiled Cargo target within a package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CompilationUnitId {
  /// Typed Cargo target domain.
  pub kind: CargoTargetKind,
  /// Target name from Cargo's compiler message.
  pub name: String,
  /// Source path relative to the workspace when available.
  pub source: Option<String>,
  /// Whether Cargo compiled this target in test mode.
  pub test_mode: bool,
}

/// Unused-dependency diagnostics emitted by one exact compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationUnitEvidence {
  /// Typed compilation-unit identity.
  pub unit: CompilationUnitId,
  /// rustc crate names reported unused for this unit.
  pub unused_crates: BTreeSet<String>,
}

/// Compiler evidence for one dependency in one platform configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyEvidenceState {
  /// At least one complete compiled unit used the dependency.
  Used,
  /// Every relevant compiled unit reported the dependency unused.
  Unused,
  /// The configuration did not apply to the dependency declaration.
  Inapplicable,
  /// Required compiler evidence was missing or incomplete.
  Incomplete,
}

/// Counts used in a portable dependency-removal proof certificate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSummary {
  /// Configurations in which the declaration applies.
  pub applicable: usize,
  /// Applicable configurations with complete compiler evidence.
  pub complete: usize,
  /// Complete configurations with at least one usage observation.
  pub used: usize,
  /// Complete configurations where every compiled unit reported unused.
  pub unused: usize,
  /// Applicable configurations lacking complete evidence.
  pub incomplete: usize,
}

/// Cache reuse observed while collecting one member's compiler evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceCacheSummary {
  /// Exact semantic cache hits.
  pub hits: usize,
  /// Configurations requiring a Cargo check.
  pub misses: usize,
  /// Stable miss reason and occurrence count.
  pub miss_reasons: BTreeMap<String, usize>,
}

/// Stable identity for one dependency declaration and its resolved packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyIdentity {
  /// Workspace package that owns the declaration.
  pub member: PackageId,
  /// Key written in the member's Cargo.toml, including any rename.
  pub declaration_key: String,
  /// Opaque package IDs resolved across the configured platform matrix.
  pub resolved_packages: BTreeSet<PackageId>,
  /// Crate names passed to rustc across the configured platform matrix.
  pub crate_names: BTreeSet<String>,
  /// Manifest dependency domain.
  pub kind: DepKind,
  /// Exact target table or cfg constraint on the declaration.
  pub target: Option<String>,
  /// Whether Cargo activates the declaration through a feature.
  pub optional: bool,
}

/// Compiler evidence for one package and platform target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetEvidence {
  /// Platform target checked by Cargo.
  pub platform: PlatformTarget,
  /// Cargo feature selection checked by this evidence.
  pub features: FeatureSelection,
  /// Cargo targets that produced compiler artifacts or diagnostics.
  pub compiled_units: BTreeSet<CompilationUnitId>,
  /// Crate names reported by rustc as unused in every compiled unit.
  pub unused_crates: BTreeSet<String>,
  /// Crate names reported unused by each exact compilation unit.
  pub unit_evidence: Vec<CompilationUnitEvidence>,
  /// Whether Cargo completed the configuration successfully.
  pub completeness: DiagnosticsCompleteness,
}

impl TargetEvidence {
  /// Return compiler evidence for a resolved rustc crate name.
  #[must_use]
  pub fn dependency_state(&self, crate_name: &str) -> DependencyEvidenceState {
    if self.completeness != DiagnosticsCompleteness::Complete || self.compiled_units.is_empty() {
      return DependencyEvidenceState::Incomplete;
    }
    if self.unused_crates.contains(crate_name) {
      DependencyEvidenceState::Unused
    } else {
      DependencyEvidenceState::Used
    }
  }

  /// Return evidence within the compilation-unit domain for a dependency kind.
  #[must_use]
  pub fn dependency_state_for_kind(&self, crate_name: &str, kind: DepKind) -> DependencyEvidenceState {
    if self.completeness != DiagnosticsCompleteness::Complete {
      return DependencyEvidenceState::Incomplete;
    }
    let relevant: Vec<_> = self
      .compiled_units
      .iter()
      .filter(|unit| match kind {
        // Cargo can reuse a normal artifact when constructing the test
        // harness and does not promise to replay its diagnostics.  The
        // non-test unit is therefore the authoritative normal-dependency
        // domain; test-mode units belong to the dev-dependency domain.
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
      .collect();
    if relevant.is_empty() {
      return DependencyEvidenceState::Incomplete;
    }
    if relevant.iter().all(|unit| {
      self
        .unit_evidence
        .iter()
        .find(|evidence| &evidence.unit == *unit)
        .is_some_and(|evidence| evidence.unused_crates.contains(crate_name))
    }) {
      DependencyEvidenceState::Unused
    } else {
      DependencyEvidenceState::Used
    }
  }
}

/// Target-aware compiler evidence for one workspace package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberEvidence {
  /// Opaque Cargo package identity.
  pub package_id: PackageId,
  /// Evidence keyed deterministically by platform target.
  pub configurations: BTreeMap<PlatformTarget, BTreeMap<FeatureSelection, TargetEvidence>>,
  /// Cache outcomes for the configurations considered by the scheduler.
  pub cache: EvidenceCacheSummary,
}

impl MemberEvidence {
  /// Create an empty evidence record for a package.
  #[must_use]
  pub fn new(package_id: PackageId) -> Self {
    Self {
      package_id,
      configurations: BTreeMap::new(),
      cache: EvidenceCacheSummary::default(),
    }
  }

  /// Combine evidence across every applicable required platform.
  ///
  /// One positive usage observation disproves removal. Otherwise every target
  /// must contain complete unused evidence.
  #[must_use]
  pub fn dependency_state(
    &self,
    dependency: &DependencyIdentity,
    required_targets: &[&str],
  ) -> DependencyEvidenceState {
    if dependency.member != self.package_id || dependency.crate_names.is_empty() {
      return DependencyEvidenceState::Incomplete;
    }

    let mut incomplete = false;
    for crate_name in &dependency.crate_names {
      match self.crate_state(crate_name, dependency.kind, required_targets) {
        DependencyEvidenceState::Used => return DependencyEvidenceState::Used,
        DependencyEvidenceState::Incomplete | DependencyEvidenceState::Inapplicable => incomplete = true,
        DependencyEvidenceState::Unused => {}
      }
    }

    if incomplete {
      DependencyEvidenceState::Incomplete
    } else {
      DependencyEvidenceState::Unused
    }
  }

  /// Summarize per-configuration evidence for a dependency proof.
  #[must_use]
  pub fn dependency_summary(&self, dependency: &DependencyIdentity, required_targets: &[&str]) -> EvidenceSummary {
    let mut summary = EvidenceSummary::default();
    for target in required_targets {
      let target = PlatformTarget::from(*target);
      let configurations = self.configurations.get(&target);
      for features in FeatureSelection::BASELINES {
        if configurations.is_none_or(|items| !items.contains_key(&features)) {
          summary.applicable += 1;
          summary.incomplete += 1;
        }
      }
      for evidence in configurations.into_iter().flat_map(BTreeMap::values) {
        summary.applicable += 1;
        match dependency_state_in_configuration(evidence, dependency) {
          DependencyEvidenceState::Used => {
            summary.complete += 1;
            summary.used += 1;
          }
          DependencyEvidenceState::Unused => {
            summary.complete += 1;
            summary.unused += 1;
          }
          DependencyEvidenceState::Incomplete | DependencyEvidenceState::Inapplicable => summary.incomplete += 1,
        }
      }
    }
    summary
  }

  /// Evaluate only explicitly required feature modes.
  ///
  /// Optional-dependency proofs use this to ignore configurations where Cargo
  /// intentionally omits the dependency from the rustc invocation.
  #[must_use]
  pub fn dependency_state_for_features(
    &self,
    dependency: &DependencyIdentity,
    required_targets: &[&str],
    required_features: &[FeatureSelection],
  ) -> DependencyEvidenceState {
    if dependency.member != self.package_id || dependency.crate_names.is_empty() || required_targets.is_empty() {
      return DependencyEvidenceState::Incomplete;
    }
    let mut incomplete = false;
    for target in required_targets {
      let configurations = self.configurations.get(&PlatformTarget::from(*target));
      for features in required_features {
        let Some(evidence) = configurations.and_then(|items| items.get(features)) else {
          incomplete = true;
          continue;
        };
        match dependency_state_in_configuration(evidence, dependency) {
          DependencyEvidenceState::Used => return DependencyEvidenceState::Used,
          DependencyEvidenceState::Incomplete | DependencyEvidenceState::Inapplicable => incomplete = true,
          DependencyEvidenceState::Unused => {}
        }
      }
    }
    if incomplete {
      DependencyEvidenceState::Incomplete
    } else {
      DependencyEvidenceState::Unused
    }
  }

  /// Summarize evidence for explicitly required feature modes.
  #[must_use]
  pub fn dependency_summary_for_features(
    &self,
    dependency: &DependencyIdentity,
    required_targets: &[&str],
    required_features: &[FeatureSelection],
  ) -> EvidenceSummary {
    let mut summary = EvidenceSummary::default();
    for target in required_targets {
      let configurations = self.configurations.get(&PlatformTarget::from(*target));
      for features in required_features {
        summary.applicable += 1;
        let Some(evidence) = configurations.and_then(|items| items.get(features)) else {
          summary.incomplete += 1;
          continue;
        };
        match dependency_state_in_configuration(evidence, dependency) {
          DependencyEvidenceState::Used => {
            summary.complete += 1;
            summary.used += 1;
          }
          DependencyEvidenceState::Unused => {
            summary.complete += 1;
            summary.unused += 1;
          }
          DependencyEvidenceState::Incomplete | DependencyEvidenceState::Inapplicable => summary.incomplete += 1,
        }
      }
    }
    summary
  }

  fn crate_state(&self, crate_name: &str, kind: DepKind, required_targets: &[&str]) -> DependencyEvidenceState {
    if required_targets.is_empty() {
      return DependencyEvidenceState::Inapplicable;
    }

    let mut incomplete = false;
    for target in required_targets {
      let configurations = self.configurations.get(&PlatformTarget::from(*target));
      for features in FeatureSelection::BASELINES {
        if configurations.is_none_or(|items| !items.contains_key(&features)) {
          incomplete = true;
        }
      }
      for evidence in configurations.into_iter().flat_map(BTreeMap::values) {
        match evidence.dependency_state_for_kind(crate_name, kind) {
          DependencyEvidenceState::Used => return DependencyEvidenceState::Used,
          DependencyEvidenceState::Incomplete => incomplete = true,
          DependencyEvidenceState::Unused => {}
          DependencyEvidenceState::Inapplicable => incomplete = true,
        }
      }
    }

    if incomplete {
      DependencyEvidenceState::Incomplete
    } else {
      DependencyEvidenceState::Unused
    }
  }
}

fn dependency_state_in_configuration(
  evidence: &TargetEvidence,
  dependency: &DependencyIdentity,
) -> DependencyEvidenceState {
  let mut state = DependencyEvidenceState::Unused;
  for crate_name in &dependency.crate_names {
    match evidence.dependency_state_for_kind(crate_name, dependency.kind) {
      DependencyEvidenceState::Used => return DependencyEvidenceState::Used,
      DependencyEvidenceState::Incomplete | DependencyEvidenceState::Inapplicable => {
        state = DependencyEvidenceState::Incomplete;
      }
      DependencyEvidenceState::Unused => {}
    }
  }
  state
}

/// Stable key for a member+target diagnostics entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompilerDiagKey {
  /// Opaque workspace package identity.
  pub package_id: PackageId,
  /// Workspace package name used as the root-independent discovery identity.
  #[serde(default)]
  pub package_name: String,
  /// Target triple or `default` when no explicit target was used.
  pub target: PlatformTarget,
  /// Cargo feature selection used by the check.
  pub features: FeatureSelection,
  /// rustc semantic version string.
  pub rustc_version: String,
  /// Cargo semantic version string.
  pub cargo_version: String,
  /// rustc host triple.
  pub host_triple: String,
  /// Complete Cargo, rustc, rustdoc, and compiler-wrapper identity.
  pub toolchain_fingerprint: String,
  /// Exact target specification, cfg, tool, and compiler-flag identity.
  pub target_fingerprint: String,
  /// Workspace lockfile fingerprint.
  pub lock_fingerprint: String,
  /// Member manifest fingerprint.
  pub manifest_fingerprint: String,
  /// Member and local dependency-closure source fingerprint.
  pub source_fingerprint: String,
  /// Build-affecting environment fingerprint (RUSTFLAGS, CARGO_ENCODED_RUSTFLAGS).
  pub compiler_env_fingerprint: String,
  /// Workspace cargo config fingerprint (`.cargo/config.toml` and `.cargo/config`).
  pub cargo_config_fingerprint: String,
}

impl CompilerDiagKey {
  /// Deterministic string identifier for map storage.
  #[must_use]
  pub fn stable_id(&self) -> String {
    format!(
      "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
      self.package_id,
      self.target.as_str(),
      self.features.label(),
      self.rustc_version,
      self.cargo_version,
      self.host_triple,
      self.toolchain_fingerprint,
      self.target_fingerprint,
      self.lock_fingerprint,
      self.manifest_fingerprint,
      self.source_fingerprint,
      self.compiler_env_fingerprint,
      self.cargo_config_fingerprint
    )
  }
}

/// Completeness of compiler diagnostics for a member+target run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticsCompleteness {
  /// Cargo check finished successfully and diagnostics are complete.
  Complete,
  /// Cargo check failed; diagnostics are partial and must not drive auto-removal.
  Incomplete,
}

/// Cached diagnostics payload for one member+target pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerDiagEntry {
  /// Key used for cache lookup.
  pub key: CompilerDiagKey,
  /// Complete typed evidence collected for the configuration.
  pub evidence: TargetEvidence,
  /// Timestamp when collected.
  pub generated_at_unix_ms: u64,
  /// Collector semantic version.
  pub collector_version: u32,
  /// Immutable exact-byte compilation observations supporting this evidence.
  pub(crate) observations: Vec<crate::compiler::observation::CompilationObservationManifest>,
}

/// On-disk cache envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerDiagCacheFile {
  /// Cache format version.
  pub version: u32,
  /// Entries keyed by [`CompilerDiagKey::stable_id`].
  #[serde(deserialize_with = "deserialize_compiler_diag_entries")]
  pub entries: BTreeMap<String, CompilerDiagEntry>,
}

fn deserialize_compiler_diag_entries<'de, D>(deserializer: D) -> Result<BTreeMap<String, CompilerDiagEntry>, D::Error>
where
  D: Deserializer<'de>,
{
  struct EntriesVisitor;

  impl<'de> serde::de::Visitor<'de> for EntriesVisitor {
    type Value = BTreeMap<String, CompilerDiagEntry>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str("a bounded map of compiler diagnostics entries")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
      A: serde::de::MapAccess<'de>,
    {
      let mut entries = BTreeMap::new();
      while let Some(key) = map.next_key::<String>()? {
        if entries.len() == MAX_COMPILER_DIAG_CACHE_ENTRIES {
          return Err(serde::de::Error::custom(format_args!(
            "compiler diagnostics cache exceeds its {MAX_COMPILER_DIAG_CACHE_ENTRIES}-entry bound"
          )));
        }
        let entry = map.next_value()?;
        if entries.insert(key, entry).is_some() {
          return Err(serde::de::Error::custom(
            "compiler diagnostics cache contains a duplicate entry ID",
          ));
        }
      }
      Ok(entries)
    }
  }

  deserializer.deserialize_map(EntriesVisitor)
}

impl Default for CompilerDiagCacheFile {
  fn default() -> Self {
    Self {
      version: COMPILER_DIAG_CACHE_VERSION,
      entries: BTreeMap::new(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn package_id(name: &str) -> PackageId {
    PackageId {
      repr: format!("path+file:///workspace#{name}@0.1.0"),
    }
  }

  fn target_evidence(
    target: &str,
    features: FeatureSelection,
    unused: &[&str],
    completeness: DiagnosticsCompleteness,
  ) -> TargetEvidence {
    let unit = CompilationUnitId {
      kind: CargoTargetKind::Library,
      name: "member".to_string(),
      source: Some("src/lib.rs".to_string()),
      test_mode: false,
    };
    TargetEvidence {
      platform: PlatformTarget::from(target),
      features,
      compiled_units: BTreeSet::from([unit.clone()]),
      unused_crates: unused.iter().map(|name| (*name).to_string()).collect(),
      unit_evidence: vec![CompilationUnitEvidence {
        unit,
        unused_crates: unused.iter().map(|name| (*name).to_string()).collect(),
      }],
      completeness,
    }
  }

  fn insert_target(
    evidence: &mut MemberEvidence,
    target: &str,
    features: FeatureSelection,
    unused: &[&str],
    completeness: DiagnosticsCompleteness,
  ) {
    let target_evidence = target_evidence(target, features.clone(), unused, completeness);
    evidence
      .configurations
      .entry(target_evidence.platform.clone())
      .or_default()
      .insert(features, target_evidence);
  }

  fn dependency(member: &PackageId, crate_name: &str) -> DependencyIdentity {
    DependencyIdentity {
      member: member.clone(),
      declaration_key: crate_name.to_string(),
      resolved_packages: BTreeSet::from([package_id(crate_name)]),
      crate_names: BTreeSet::from([crate_name.to_string()]),
      kind: DepKind::Normal,
      target: None,
      optional: false,
    }
  }

  #[test]
  fn test_member_evidence_positive_usage_disproves_removal_with_missing_targets() {
    let mut evidence = MemberEvidence::new(package_id("member"));
    let dependency = dependency(&evidence.package_id, "serde");
    insert_target(
      &mut evidence,
      "aarch64-apple-darwin",
      FeatureSelection::Default,
      &[],
      DiagnosticsCompleteness::Complete,
    );

    assert_eq!(
      evidence.dependency_state(&dependency, &["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]),
      DependencyEvidenceState::Used
    );
  }

  #[test]
  fn test_member_evidence_requires_complete_unused_observations() {
    let mut evidence = MemberEvidence::new(package_id("member"));
    let dependency = dependency(&evidence.package_id, "serde");
    for target in ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"] {
      for features in FeatureSelection::BASELINES {
        insert_target(
          &mut evidence,
          target,
          features,
          &["serde"],
          DiagnosticsCompleteness::Complete,
        );
      }
    }

    assert_eq!(
      evidence.dependency_state(&dependency, &["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]),
      DependencyEvidenceState::Unused
    );
    assert_eq!(
      evidence.dependency_state(&dependency, &["aarch64-apple-darwin", "x86_64-pc-windows-msvc"]),
      DependencyEvidenceState::Incomplete
    );
  }

  #[test]
  fn test_member_evidence_keeps_package_identity_and_target_order() {
    let package_id = package_id("member");
    let mut evidence = MemberEvidence::new(package_id.clone());
    for target in ["z-target", "a-target"] {
      insert_target(
        &mut evidence,
        target,
        FeatureSelection::Default,
        &[],
        DiagnosticsCompleteness::Complete,
      );
    }

    assert_eq!(evidence.package_id, package_id.clone());
    assert_eq!(
      evidence
        .configurations
        .keys()
        .map(PlatformTarget::as_str)
        .collect::<Vec<_>>(),
      vec!["a-target", "z-target"]
    );

    let serialized = serde_json::to_string(&evidence).expect("serialize evidence");
    assert!(
      serialized.find("a-target") < serialized.find("z-target"),
      "serialized evidence must retain deterministic platform order: {serialized}"
    );
    assert!(serialized.contains(&package_id.repr));
  }

  #[test]
  fn test_dependency_kinds_use_independent_compilation_domains() {
    let normal = CompilationUnitId {
      kind: CargoTargetKind::Library,
      name: "member".to_string(),
      source: Some("src/lib.rs".to_string()),
      test_mode: false,
    };
    let test = CompilationUnitId {
      test_mode: true,
      ..normal.clone()
    };
    let build = CompilationUnitId {
      kind: CargoTargetKind::CustomBuild,
      name: "build-script-build".to_string(),
      source: Some("build.rs".to_string()),
      test_mode: false,
    };
    let evidence = TargetEvidence {
      platform: PlatformTarget::from("default"),
      features: FeatureSelection::Default,
      compiled_units: BTreeSet::from([normal.clone(), test.clone(), build.clone()]),
      unused_crates: BTreeSet::new(),
      unit_evidence: vec![
        CompilationUnitEvidence {
          unit: normal,
          unused_crates: BTreeSet::from(["normal_unused".to_string()]),
        },
        CompilationUnitEvidence {
          unit: test,
          unused_crates: BTreeSet::from(["dev_unused".to_string()]),
        },
        CompilationUnitEvidence {
          unit: build,
          unused_crates: BTreeSet::from(["build_unused".to_string()]),
        },
      ],
      completeness: DiagnosticsCompleteness::Complete,
    };

    assert_eq!(
      evidence.dependency_state_for_kind("normal_unused", DepKind::Normal),
      DependencyEvidenceState::Unused
    );
    assert_eq!(
      evidence.dependency_state_for_kind("normal_unused", DepKind::Dev),
      DependencyEvidenceState::Used
    );
    assert_eq!(
      evidence.dependency_state_for_kind("dev_unused", DepKind::Dev),
      DependencyEvidenceState::Unused
    );
    assert_eq!(
      evidence.dependency_state_for_kind("build_unused", DepKind::Build),
      DependencyEvidenceState::Unused
    );
  }
}
