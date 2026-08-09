//! Command-independent dependency universe for conservative planning.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use cargo_metadata::{Dependency, DependencyKind, Metadata, Package, PackageId, TargetKind};
use rustc_hash::FxHashMap;
use serde::Serialize;

use super::{ImpactDomain, ImpactFallback, ImpactPropagation, ImpactStep};
use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

pub(crate) const DEPENDENCY_UNIVERSE_MODE: &str = "declared_dependencies";
const DEPENDENCY_UNIVERSE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeclaredEdge {
  dependent: PackageId,
  alias: String,
  kind: DependencyKind,
  target: Option<String>,
  optional: bool,
  uses_default_features: bool,
  features: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct PackageFacts {
  workspace_member: bool,
  proc_macro: bool,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct PortableEdge {
  dependency: String,
  dependent: String,
  alias: String,
  kind: u8,
  target: Option<String>,
  optional: bool,
  uses_default_features: bool,
  features: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct PortableDeclaration {
  dependent: String,
  dependency_name: String,
  dependency_source: String,
  requirement: String,
  alias: String,
  kind: u8,
  target: Option<String>,
  optional: bool,
  uses_default_features: bool,
  features: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct PortablePackageFacts {
  package: String,
  workspace_member: bool,
  proc_macro: bool,
}

/// Every declared dependency edge that can resolve between captured packages.
///
/// This is deliberately narrower than [`super::WorkspaceGraph`]. It owns one
/// reverse adjacency index for conservative planner propagation and cannot be
/// queried as an exact Cargo resolution.
pub(crate) struct DependencyUniverse {
  packages: FxHashMap<PackageId, PackageFacts>,
  reverse_edges: FxHashMap<PackageId, Vec<DeclaredEdge>>,
  identity: String,
}

impl DependencyUniverse {
  /// Build the declared universe from the metadata already owned by the workspace capture.
  pub(crate) fn from_metadata(metadata: &Metadata, source_root: &Path) -> RailResult<Self> {
    let workspace_ids = metadata.workspace_members.iter().cloned().collect::<HashSet<_>>();
    let packages: FxHashMap<PackageId, PackageFacts> = metadata
      .packages
      .iter()
      .map(|package| {
        (
          package.id.clone(),
          PackageFacts {
            workspace_member: workspace_ids.contains(&package.id),
            proc_macro: package
              .targets
              .iter()
              .any(|target| target.is_kind(TargetKind::ProcMacro)),
          },
        )
      })
      .collect();
    let packages_by_name = package_indices_by_name(metadata);
    let portable_package_ids = metadata
      .packages
      .iter()
      .map(|package| (package.id.clone(), portable_package_id(package, source_root)))
      .collect::<FxHashMap<_, _>>();
    let mut portable_packages = metadata
      .packages
      .iter()
      .map(|package| {
        let facts = packages[&package.id];
        PortablePackageFacts {
          package: portable_package_ids[&package.id].clone(),
          workspace_member: facts.workspace_member,
          proc_macro: facts.proc_macro,
        }
      })
      .collect::<Vec<_>>();
    let mut reverse_edges = FxHashMap::<PackageId, Vec<DeclaredEdge>>::default();
    let mut portable_declarations = Vec::new();
    let mut portable_edges = Vec::new();

    for dependent in &metadata.packages {
      for declaration in &dependent.dependencies {
        let mut declaration_features = declaration.features.clone();
        declaration_features.sort();
        declaration_features.dedup();
        portable_declarations.push(PortableDeclaration {
          dependent: portable_package_ids[&dependent.id].clone(),
          dependency_name: declaration.name.clone(),
          dependency_source: portable_dependency_source(declaration, source_root),
          requirement: declaration.req.to_string(),
          alias: cargo_alias(declaration.rename.as_deref().unwrap_or(&declaration.name)),
          kind: dependency_kind_rank(declaration.kind),
          target: declaration.target.as_ref().map(ToString::to_string),
          optional: declaration.optional,
          uses_default_features: declaration.uses_default_features,
          features: declaration_features.clone(),
        });
        let Some(candidates) = packages_by_name.get(declaration.name.as_str()) else {
          continue;
        };
        for &candidate_index in candidates {
          let dependency = &metadata.packages[candidate_index];
          if !declaration_matches_package(declaration, dependency) {
            continue;
          }

          let edge = DeclaredEdge {
            dependent: dependent.id.clone(),
            alias: cargo_alias(declaration.rename.as_deref().unwrap_or(&declaration.name)),
            kind: declaration.kind,
            target: declaration.target.as_ref().map(ToString::to_string),
            optional: declaration.optional,
            uses_default_features: declaration.uses_default_features,
            features: declaration_features.clone(),
          };
          portable_edges.push(PortableEdge {
            dependency: portable_package_ids[&dependency.id].clone(),
            dependent: portable_package_ids[&edge.dependent].clone(),
            alias: edge.alias.clone(),
            kind: dependency_kind_rank(edge.kind),
            target: edge.target.clone(),
            optional: edge.optional,
            uses_default_features: edge.uses_default_features,
            features: edge.features.clone(),
          });
          reverse_edges.entry(dependency.id.clone()).or_default().push(edge);
        }
      }
    }

    for edges in reverse_edges.values_mut() {
      edges.sort_by(declared_edge_order);
      edges.dedup();
    }
    portable_packages.sort();
    portable_packages.dedup();
    portable_declarations.sort();
    portable_declarations.dedup();
    portable_edges.sort();
    portable_edges.dedup();
    let encoded = serde_json::to_vec(&(
      DEPENDENCY_UNIVERSE_VERSION,
      &portable_packages,
      &portable_declarations,
      &portable_edges,
    ))
    .map_err(|error| RailError::message(format!("failed to encode dependency universe identity: {error}")))?;
    let identity = format!(
      "resolution-universe-v{DEPENDENCY_UNIVERSE_VERSION}:sha256:{}",
      ContentDigest::sha256(&encoded)
    );

    Ok(Self {
      packages,
      reverse_edges,
      identity,
    })
  }

  /// Return the root-independent identity of all package facts, declarations, and matched destinations.
  pub(crate) fn identity(&self) -> &str {
    &self.identity
  }

  /// Propagate package changes through every potentially active declared edge.
  pub(crate) fn propagate(&self, seeds: &HashSet<PackageId>) -> RailResult<ImpactPropagation> {
    if seeds.is_empty() {
      return Ok(ImpactPropagation::default());
    }
    if let Some(missing) = seeds.iter().find(|seed| !self.packages.contains_key(*seed)) {
      return Err(RailError::message(format!(
        "Package ID '{}' is missing from the declared dependency universe",
        missing
      )));
    }

    let mut stack = seeds.iter().cloned().collect::<Vec<_>>();
    stack.sort();
    let mut visited = seeds.clone();
    let mut impact = ImpactPropagation::default();
    let mut node_visits = 0;
    let mut edge_visits = 0;

    while let Some(dependency) = stack.pop() {
      node_visits += 1;
      let Some(edges) = self.reverse_edges.get(&dependency) else {
        continue;
      };
      for edge in edges {
        edge_visits += 1;
        let domain = match edge.kind {
          DependencyKind::Development => ImpactDomain::Development,
          DependencyKind::Normal | DependencyKind::Build | DependencyKind::Unknown => ImpactDomain::Build,
        };

        if self.packages[&edge.dependent].workspace_member && !seeds.contains(&edge.dependent) {
          match domain {
            ImpactDomain::Build => {
              impact.build.insert(edge.dependent.clone());
            }
            ImpactDomain::Development => {
              impact.development.insert(edge.dependent.clone());
            }
          }
          impact.steps.push(ImpactStep {
            dependency: dependency.clone(),
            dependent: edge.dependent.clone(),
            alias: edge.alias.clone(),
            kind: edge.kind,
            target: edge.target.clone(),
            optional: edge.optional,
            uses_default_features: edge.uses_default_features,
            features: edge.features.clone(),
            proc_macro: self.packages[&dependency].proc_macro,
            host: edge.kind == DependencyKind::Build || self.packages[&dependency].proc_macro,
            domain,
            fallbacks: (edge.kind == DependencyKind::Unknown)
              .then_some(ImpactFallback::UnknownDependencyKind)
              .into_iter()
              .collect(),
          });
        }

        if domain == ImpactDomain::Build && visited.insert(edge.dependent.clone()) {
          stack.push(edge.dependent.clone());
        }
      }
    }

    crate::instrumentation::record_graph_traversal(node_visits, edge_visits);
    impact.normalize();
    Ok(impact)
  }
}

fn package_indices_by_name(metadata: &Metadata) -> BTreeMap<&str, Vec<usize>> {
  let mut packages = BTreeMap::<&str, Vec<usize>>::new();
  for (index, package) in metadata.packages.iter().enumerate() {
    packages.entry(package.name.as_str()).or_default().push(index);
  }
  for candidates in packages.values_mut() {
    candidates.sort_by(|left, right| metadata.packages[*left].id.cmp(&metadata.packages[*right].id));
  }
  packages
}

fn declaration_matches_package(declaration: &Dependency, package: &Package) -> bool {
  if declaration.name.as_str() != package.name.as_str() || !declaration.req.matches(&package.version) {
    return false;
  }
  if let Some(path) = &declaration.path {
    return package.manifest_path.parent() == Some(path.as_path());
  }
  // A Cargo patch may replace the declared source. Keeping every captured
  // name/version candidate is conservative; comparing sources can omit one.
  true
}

fn cargo_alias(alias: &str) -> String {
  alias.replace('-', "_")
}

fn portable_package_id(package: &Package, source_root: &Path) -> String {
  if let Some(source) = &package.source {
    return format!("{}@{}#{}", package.name, package.version, source.repr);
  }
  let package_root = package.manifest_path.parent().map(|path| path.as_std_path());
  let relative = package_root
    .and_then(|path| path.strip_prefix(source_root).ok())
    .map(normalized_path)
    .unwrap_or_else(|| "<outside-source-root>".to_string());
  format!("{}@{}#path:{relative}", package.name, package.version)
}

fn portable_dependency_source(dependency: &Dependency, source_root: &Path) -> String {
  if let Some(path) = &dependency.path {
    let relative = path
      .as_std_path()
      .strip_prefix(source_root)
      .ok()
      .map(normalized_path)
      .unwrap_or_else(|| "<outside-source-root>".to_string());
    return format!("path:{relative}");
  }
  dependency
    .source
    .as_ref()
    .map_or_else(|| "registry:default".to_string(), |source| source.repr.clone())
}

fn normalized_path(path: &Path) -> String {
  path
    .components()
    .map(|component| component.as_os_str().to_string_lossy())
    .collect::<Vec<_>>()
    .join("/")
}

fn declared_edge_order(left: &DeclaredEdge, right: &DeclaredEdge) -> std::cmp::Ordering {
  (
    &left.dependent,
    dependency_kind_rank(left.kind),
    &left.target,
    &left.alias,
    left.optional,
    left.uses_default_features,
    &left.features,
  )
    .cmp(&(
      &right.dependent,
      dependency_kind_rank(right.kind),
      &right.target,
      &right.alias,
      right.optional,
      right.uses_default_features,
      &right.features,
    ))
}

const fn dependency_kind_rank(kind: DependencyKind) -> u8 {
  match kind {
    DependencyKind::Normal => 0,
    DependencyKind::Development => 1,
    DependencyKind::Build => 2,
    DependencyKind::Unknown => 3,
  }
}
