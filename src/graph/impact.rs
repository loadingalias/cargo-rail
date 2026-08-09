//! Shared Cargo-domain impact evidence for conservative reverse propagation.

use std::collections::BTreeSet;

use cargo_metadata::{DependencyKind, PackageId};

/// Compilation domain affected in a dependent workspace package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ImpactDomain {
  /// The package output may change, so impact continues through its consumers.
  Build,
  /// Only tests, examples, and benches consume the changed dependency.
  Development,
}

/// Evidence that forced conservative propagation rather than a proven active edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ImpactFallback {
  /// The compared Git objects do not have an exact historical Cargo resolution.
  HistoricalResolutionUnavailable,
  /// Cargo reported a dependency kind unknown to this version of cargo-rail.
  UnknownDependencyKind,
}

/// One semantic edge that propagates impact into a workspace package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImpactStep {
  pub(crate) dependency: PackageId,
  pub(crate) dependent: PackageId,
  pub(crate) alias: String,
  pub(crate) kind: DependencyKind,
  pub(crate) target: Option<String>,
  pub(crate) optional: bool,
  pub(crate) uses_default_features: bool,
  pub(crate) features: Vec<String>,
  pub(crate) proc_macro: bool,
  pub(crate) host: bool,
  pub(crate) domain: ImpactDomain,
  pub(crate) fallbacks: Vec<ImpactFallback>,
}

/// Complete package impact for one dependency universe.
#[derive(Debug, Default)]
pub(crate) struct ImpactPropagation {
  /// Packages whose build output may change.
  pub(crate) build: BTreeSet<PackageId>,
  /// Packages affected only in tests, examples, and benches.
  pub(crate) development: BTreeSet<PackageId>,
  /// Deterministically ordered semantic propagation evidence.
  pub(crate) steps: Vec<ImpactStep>,
}

impl ImpactPropagation {
  pub(super) fn normalize(&mut self) {
    self.development.retain(|package| !self.build.contains(package));
    sort_steps(&mut self.steps);
  }
}

fn sort_steps(steps: &mut Vec<ImpactStep>) {
  steps.sort_by(|left, right| {
    (
      &left.dependent,
      left.domain,
      &left.dependency,
      &left.alias,
      dependency_kind_rank(left.kind),
      &left.target,
      &left.features,
    )
      .cmp(&(
        &right.dependent,
        right.domain,
        &right.dependency,
        &right.alias,
        dependency_kind_rank(right.kind),
        &right.target,
        &right.features,
      ))
  });
  steps.dedup();
}

const fn dependency_kind_rank(kind: DependencyKind) -> u8 {
  match kind {
    DependencyKind::Normal => 0,
    DependencyKind::Development => 1,
    DependencyKind::Build => 2,
    DependencyKind::Unknown => 3,
  }
}
