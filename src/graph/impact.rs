//! Cargo-domain-aware reverse impact propagation.

use std::collections::{BTreeSet, HashSet};

use cargo_metadata::{DependencyKind, PackageId};
use petgraph::Direction;
use petgraph::visit::EdgeRef as _;

use super::core::WorkspaceGraph;
use crate::error::{RailError, RailResult};

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
  /// Cargo did not filter the resolved graph to one target, so a target predicate may be inactive.
  TargetUnfiltered,
  /// Cargo reported a dependency kind unknown to this version of cargo-rail.
  UnknownDependencyKind,
  /// The resolved edge could not be matched unambiguously to its manifest declaration.
  DeclarationUnknown,
}

/// One semantic edge that propagates impact into a workspace package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImpactStep {
  pub(crate) dependency: PackageId,
  pub(crate) dependent: PackageId,
  pub(crate) alias: String,
  pub(crate) kind: DependencyKind,
  pub(crate) target: Option<String>,
  pub(crate) optional: Option<bool>,
  pub(crate) uses_default_features: Option<bool>,
  pub(crate) features: Vec<String>,
  pub(crate) proc_macro: bool,
  pub(crate) host: bool,
  pub(crate) domain: ImpactDomain,
  pub(crate) fallbacks: Vec<ImpactFallback>,
}

/// Complete package impact for one resolved Cargo graph.
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
  /// Merge impact observed in another exact resolution domain.
  pub(crate) fn merge(&mut self, mut other: Self) {
    self.build.append(&mut other.build);
    self.development.append(&mut other.development);
    self.development.retain(|package| !self.build.contains(package));
    self.steps.append(&mut other.steps);
    sort_steps(&mut self.steps);
  }
}

pub(super) fn propagate(
  graph: &WorkspaceGraph,
  seeds: &HashSet<PackageId>,
  target_filtered: bool,
) -> RailResult<ImpactPropagation> {
  if seeds.is_empty() {
    return Ok(ImpactPropagation::default());
  }

  let seed_nodes = seeds
    .iter()
    .map(|seed| {
      graph
        .id_to_node
        .get(seed)
        .copied()
        .ok_or_else(|| RailError::message(format!("Package ID '{}' not found in the resolved graph", seed)))
    })
    .collect::<RailResult<Vec<_>>>()?;
  let seed_node_set = seed_nodes.iter().copied().collect::<HashSet<_>>();
  let mut visited = seed_node_set.clone();
  let mut stack = seed_nodes;

  let mut impact = ImpactPropagation::default();
  let mut node_visits = 0;
  let mut edge_visits = 0;
  while let Some(dependency) = stack.pop() {
    node_visits += 1;
    for edge in graph.graph.edges_directed(dependency, Direction::Incoming) {
      edge_visits += 1;
      let dependent = edge.source();
      let semantics = edge.weight();
      let domain = match semantics.kind {
        DependencyKind::Development => ImpactDomain::Development,
        DependencyKind::Normal | DependencyKind::Build | DependencyKind::Unknown => ImpactDomain::Build,
      };

      if graph.graph[dependent].is_workspace_member && !seed_node_set.contains(&dependent) {
        let dependent_id = graph.graph[dependent].id.clone();
        match domain {
          ImpactDomain::Build => {
            impact.build.insert(dependent_id.clone());
          }
          ImpactDomain::Development => {
            impact.development.insert(dependent_id.clone());
          }
        }
        impact.steps.push(ImpactStep {
          dependency: graph.graph[dependency].id.clone(),
          dependent: dependent_id,
          alias: semantics.name.clone(),
          kind: semantics.kind,
          target: semantics.target.as_ref().map(ToString::to_string),
          optional: semantics.optional,
          uses_default_features: semantics.uses_default_features,
          features: semantics.features.clone(),
          proc_macro: graph.graph[dependency].is_proc_macro,
          host: semantics.kind == DependencyKind::Build || graph.graph[dependency].is_proc_macro,
          domain,
          fallbacks: fallbacks(
            semantics.kind,
            semantics.target.is_some(),
            semantics.optional,
            target_filtered,
          ),
        });
      }

      if domain == ImpactDomain::Build && visited.insert(dependent) {
        stack.push(dependent);
      }
    }
  }
  crate::instrumentation::record_graph_traversal(node_visits, edge_visits);

  impact.development.retain(|package| !impact.build.contains(package));
  sort_steps(&mut impact.steps);
  Ok(impact)
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

fn fallbacks(
  kind: DependencyKind,
  target_conditioned: bool,
  optional: Option<bool>,
  target_filtered: bool,
) -> Vec<ImpactFallback> {
  let mut fallbacks = Vec::with_capacity(3);
  if target_conditioned && !target_filtered {
    fallbacks.push(ImpactFallback::TargetUnfiltered);
  }
  if kind == DependencyKind::Unknown {
    fallbacks.push(ImpactFallback::UnknownDependencyKind);
  }
  if optional.is_none() {
    fallbacks.push(ImpactFallback::DeclarationUnknown);
  }
  fallbacks
}
