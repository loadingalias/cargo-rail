//! Workspace dependency graph built from cargo_metadata + petgraph
//!
//! # Design Philosophy
//!
//! We use `cargo_metadata` + `petgraph` directly instead of guppy because:
//! - **No simulation needed**: We need workspace membership, dep edges, reverse deps,
//!   and "what's affected" - all available in cargo_metadata already
//! - **Your domain, your types**: Rail's concepts (WorkspaceGraph, affected analysis)
//!   should be first-class, not wrappers around guppy
//! - **Minimal cognitive tax**: Single engineer, opinionated tool - every abstraction
//!   layer must earn its keep
//!
//! ## Graph Structure
//!
//! - **Directed Graph**: `A → B` means "A depends on B"
//! - **Nodes**: Packages (workspace members + dependencies)
//! - **Edges**: Dependency relationships (normal/dev/build)
//! - **Index**: Fast lookups by crate name / package ID
//! - **Algorithms**: Toposort, SCC, shortest paths, reachability
//! - **Path cache**: File → owning crate mapping (lazy, interior mutability)

use crate::cargo::metadata::WorkspaceMetadata;
use crate::core::config::{RailConfig, Visibility};
use crate::core::error::{RailError, RailResult};
use cargo_metadata::{DependencyKind, PackageId};
use petgraph::Direction;
use petgraph::algo;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// A package node in the dependency graph.
#[derive(Debug, Clone)]
pub struct PackageNode {
  pub name: String,
  // TODO: Used by future version analysis and MSRV checks
  #[allow(dead_code)]
  pub version: String,
  pub manifest_path: PathBuf,
  pub is_workspace_member: bool,
  /// Visibility tiers this crate belongs to (computed from ReleaseConfig)
  /// A crate can appear in multiple releases with different visibility levels
  /// TODO: Used by visibility-filtered graph commands and tier violation checks
  #[allow(dead_code)]
  pub visibilities: HashSet<Visibility>,
}

/// Workspace dependency graph.
///
/// Built from cargo_metadata, using petgraph for efficient traversals.
pub struct WorkspaceGraph {
  /// The dependency graph (petgraph DiGraph)
  /// Nodes: PackageNode
  /// Edges: DependencyKind (Normal, Dev, Build)
  graph: DiGraph<PackageNode, DependencyKind>,

  /// Index: package name → node index
  name_to_node: HashMap<String, NodeIndex>,

  /// Index: PackageId → node index
  // TODO: Used for advanced graph queries and cross-referencing with cargo metadata
  #[allow(dead_code)]
  id_to_node: HashMap<PackageId, NodeIndex>,

  /// Workspace members only (subset of graph nodes)
  workspace_members: HashSet<String>,

  /// Path cache: directory → owning crate name
  /// Built lazily on first file lookup (thread-safe interior mutability)
  /// Uses RwLock instead of RefCell for Send/Sync compatibility
  path_cache: RwLock<Option<HashMap<PathBuf, String>>>,

  /// Original metadata (for fallback queries)
  // TODO: Used for feature analysis and detailed package info queries
  #[allow(dead_code)]
  metadata: WorkspaceMetadata,
}

impl WorkspaceGraph {
  /// Load workspace graph from root directory.
  ///
  /// Optionally accepts RailConfig to compute visibility annotations for each crate.
  ///
  /// # Performance
  /// - cargo_metadata: 50-200ms for large workspaces
  /// - Graph construction: 10-50ms
  /// - Path cache: deferred until first file lookup
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    Self::load_with_config(workspace_root, None)
  }

  /// Load workspace graph with optional RailConfig for visibility annotations.
  ///
  /// If config is provided, crates will be annotated with their visibility tiers
  /// based on ReleaseConfig entries that include them.
  pub fn load_with_config(workspace_root: &Path, config: Option<&RailConfig>) -> RailResult<Self> {
    let metadata = WorkspaceMetadata::load(workspace_root)?;

    // Build visibility mapping from release configs
    // Maps crate_name → Set<Visibility>
    let mut visibility_map: HashMap<String, HashSet<Visibility>> = HashMap::new();
    if let Some(config) = config {
      for release in &config.releases {
        // Get the crate name from the crate_path
        if let Ok(manifest_path) = release.crate_path.join("Cargo.toml").canonicalize() {
          // Find the package in metadata that matches this manifest path
          for package in &metadata.metadata_json().packages {
            if let Ok(pkg_manifest) = package.manifest_path.clone().into_std_path_buf().canonicalize()
              && pkg_manifest == manifest_path
            {
              // This crate is part of this release
              visibility_map
                .entry(package.name.as_ref().to_string())
                .or_default()
                .insert(release.visibility);

              // Also add all crates listed in `includes`
              for included_crate in &release.includes {
                visibility_map
                  .entry(included_crate.clone())
                  .or_default()
                  .insert(release.visibility);
              }
              break;
            }
          }
        }
      }
    }

    // Build petgraph
    let mut graph = DiGraph::new();
    let mut name_to_node = HashMap::new();
    let mut id_to_node = HashMap::new();
    let mut workspace_members = HashSet::new();

    // Get workspace member IDs
    let workspace_pkg_ids: HashSet<_> = metadata.list_crates().iter().map(|pkg| pkg.id.clone()).collect();

    // Add all packages as nodes (workspace + dependencies)
    for package in &metadata.metadata_json().packages {
      let crate_name = package.name.as_ref().to_string();
      let visibilities = visibility_map.get(&crate_name).cloned().unwrap_or_default();

      let node = PackageNode {
        name: crate_name.clone(),
        version: package.version.to_string(),
        manifest_path: package.manifest_path.clone().into_std_path_buf(),
        is_workspace_member: workspace_pkg_ids.contains(&package.id),
        visibilities,
      };

      let node_idx = graph.add_node(node.clone());
      name_to_node.insert(package.name.as_ref().to_string(), node_idx);
      id_to_node.insert(package.id.clone(), node_idx);

      if workspace_pkg_ids.contains(&package.id) {
        workspace_members.insert(package.name.as_ref().to_string());
      }
    }

    // Add dependency edges
    for package in &metadata.metadata_json().packages {
      let from_idx = id_to_node[&package.id];

      for dep in &package.dependencies {
        // Find the resolved dependency
        if let Some(to_idx) = name_to_node.get(dep.name.as_str()) {
          graph.add_edge(from_idx, *to_idx, dep.kind);
        }
      }
    }

    Ok(Self {
      graph,
      name_to_node,
      id_to_node,
      workspace_members,
      path_cache: RwLock::new(None),
      metadata,
    })
  }

  /// Get all workspace member crate names.
  pub fn workspace_members(&self) -> Vec<String> {
    let mut members: Vec<_> = self.workspace_members.iter().cloned().collect();
    members.sort();
    members
  }

  /// Get direct dependencies of a crate (what it uses).
  ///
  /// TODO: Used by `cargo rail graph --crate X` command
  #[allow(dead_code)]
  pub fn direct_dependencies(&self, crate_name: &str) -> RailResult<Vec<String>> {
    let node_idx = self.find_node(crate_name)?;

    let mut deps: Vec<String> = self
      .graph
      .neighbors_directed(node_idx, Direction::Outgoing)
      .map(|idx| self.graph[idx].name.clone())
      .collect();

    deps.sort();
    deps.dedup();
    Ok(deps)
  }

  /// Get direct dependents of a crate (what uses it).
  ///
  /// TODO: Used by `cargo rail graph --crate X` command
  #[allow(dead_code)]
  pub fn direct_dependents(&self, crate_name: &str) -> RailResult<Vec<String>> {
    let node_idx = self.find_node(crate_name)?;

    let mut dependents: Vec<String> = self
      .graph
      .neighbors_directed(node_idx, Direction::Incoming)
      .filter_map(|idx| {
        let node = &self.graph[idx];
        if node.is_workspace_member {
          Some(node.name.clone())
        } else {
          None
        }
      })
      .collect();

    dependents.sort();
    dependents.dedup();
    Ok(dependents)
  }

  /// Get transitive reverse dependencies (all workspace crates that depend on this one).
  ///
  /// Uses petgraph DFS for efficient traversal.
  ///
  /// # Performance
  /// O(V + E) where V = vertices, E = edges. Typically <10ms for <100 crates.
  pub fn transitive_dependents(&self, crate_name: &str) -> RailResult<Vec<String>> {
    let start_node = self.find_node(crate_name)?;

    // DFS in reverse direction (incoming edges)
    let mut visited = HashSet::new();
    let mut stack = vec![start_node];
    let mut dependents = HashSet::new();

    while let Some(node_idx) = stack.pop() {
      if visited.contains(&node_idx) {
        continue;
      }
      visited.insert(node_idx);

      // Add all incoming neighbors (things that depend on this)
      for neighbor_idx in self.graph.neighbors_directed(node_idx, Direction::Incoming) {
        let neighbor = &self.graph[neighbor_idx];

        // Only include workspace members
        if neighbor.is_workspace_member && neighbor_idx != start_node {
          dependents.insert(neighbor.name.clone());
        }

        stack.push(neighbor_idx);
      }
    }

    let mut result: Vec<_> = dependents.into_iter().collect();
    result.sort();
    Ok(result)
  }

  /// Map a file path to its owning crate.
  ///
  /// Builds path cache on first call, then O(1) lookups.
  pub fn file_to_crate(&self, file_path: &Path) -> Option<String> {
    // Build cache if needed (interior mutability with RwLock)
    {
      let cache = self.path_cache.read().unwrap();
      if cache.is_none() {
        drop(cache); // Release read lock before acquiring write lock
        self.build_path_cache();
      }
    }

    // Normalize path
    let normalized = file_path.canonicalize().ok()?;

    let cache = self.path_cache.read().unwrap();
    let cache_ref = cache.as_ref()?;

    // Direct lookup
    if let Some(crate_name) = cache_ref.get(&normalized) {
      return Some(crate_name.clone());
    }

    // Walk up directory tree
    let mut current = normalized.as_path();

    while let Some(parent) = current.parent() {
      if let Some(crate_name) = cache_ref.get(parent) {
        return Some(crate_name.clone());
      }
      current = parent;
    }

    None
  }

  /// Map multiple files to owning crates.
  pub fn files_to_crates(&self, file_paths: &[impl AsRef<Path>]) -> HashSet<String> {
    file_paths
      .iter()
      .filter_map(|p| self.file_to_crate(p.as_ref()))
      .collect()
  }

  /// Find node index by crate name.
  fn find_node(&self, crate_name: &str) -> RailResult<NodeIndex> {
    self.name_to_node.get(crate_name).copied().ok_or_else(|| {
      RailError::message(format!(
        "Crate '{}' not found. Available workspace crates: {}",
        crate_name,
        self.workspace_members().join(", ")
      ))
    })
  }

  /// Build path-to-crate mapping cache.
  ///
  /// Maps each workspace crate's root directory to its name.
  fn build_path_cache(&self) {
    let mut cache = HashMap::new();

    for crate_name in &self.workspace_members {
      if let Some(node_idx) = self.name_to_node.get(crate_name) {
        let node = &self.graph[*node_idx];

        // Get crate root (parent of Cargo.toml)
        if let Some(crate_root) = node.manifest_path.parent()
          && let Ok(canonical) = crate_root.canonicalize()
        {
          cache.insert(canonical, crate_name.clone());
        }
      }
    }

    *self.path_cache.write().unwrap() = Some(cache);
  }

  /// Access raw metadata for advanced queries.
  ///
  /// TODO: Used for feature analysis and package metadata queries
  #[allow(dead_code)]
  pub fn metadata(&self) -> &WorkspaceMetadata {
    &self.metadata
  }

  /// Get topological order of workspace crates (build order).
  ///
  /// Returns crates in dependency order: if A depends on B, B appears before A.
  ///
  /// # Errors
  /// Returns error if the dependency graph contains cycles.
  ///
  /// TODO: Used by `cargo rail doctor` and release publish ordering
  #[allow(dead_code)]
  pub fn topological_order(&self) -> RailResult<Vec<String>> {
    // toposort returns NodeIndex in topo order
    let topo = algo::toposort(&self.graph, None).map_err(|cycle| {
      let node = &self.graph[cycle.node_id()];
      RailError::message(format!("Dependency cycle detected involving crate '{}'", node.name))
    })?;

    // Filter to workspace members only and extract names
    let order: Vec<String> = topo
      .into_iter()
      .filter_map(|idx| {
        let node = &self.graph[idx];
        if node.is_workspace_member {
          Some(node.name.clone())
        } else {
          None
        }
      })
      .collect();

    Ok(order)
  }

  /// Detect dependency cycles using Tarjan's SCC algorithm.
  ///
  /// Returns strongly connected components with size > 1 (cycles).
  ///
  /// TODO: Used by `cargo rail doctor` for workspace health checks
  #[allow(dead_code)]
  pub fn find_cycles(&self) -> Vec<Vec<String>> {
    let sccs = algo::tarjan_scc(&self.graph);

    sccs
      .into_iter()
      .filter(|component| component.len() > 1)
      .map(|component| {
        component
          .into_iter()
          .filter_map(|idx| {
            let node = &self.graph[idx];
            if node.is_workspace_member {
              Some(node.name.clone())
            } else {
              None
            }
          })
          .collect()
      })
      .filter(|cycle: &Vec<String>| !cycle.is_empty())
      .collect()
  }

  /// Find dependency path: why does `from` depend on `to`?
  ///
  /// Returns the shortest dependency chain, or None if no dependency exists.
  ///
  /// # Example
  /// If bin-cli → lib-core → lib-util, and you ask why_depends_on("bin-cli", "lib-util"),
  /// returns: vec!["bin-cli", "lib-core", "lib-util"]
  ///
  /// TODO: Used by `cargo rail graph --why <from> <to>` command
  #[allow(dead_code)]
  pub fn why_depends_on(&self, from: &str, to: &str) -> RailResult<Option<Vec<String>>> {
    let from_idx = self.find_node(from)?;
    let to_idx = self.find_node(to)?;

    // BFS to find shortest path
    let mut queue = VecDeque::new();
    let mut visited = HashMap::new();

    queue.push_back(from_idx);
    visited.insert(from_idx, None);

    while let Some(current) = queue.pop_front() {
      if current == to_idx {
        // Reconstruct path
        let mut path = vec![];
        let mut node = Some(current);

        while let Some(idx) = node {
          path.push(self.graph[idx].name.clone());
          node = visited[&idx];
        }

        path.reverse();
        return Ok(Some(path));
      }

      // Explore dependencies (outgoing edges)
      for neighbor in self.graph.neighbors_directed(current, Direction::Outgoing) {
        if let std::collections::hash_map::Entry::Vacant(e) = visited.entry(neighbor) {
          e.insert(Some(current));
          queue.push_back(neighbor);
        }
      }
    }

    Ok(None)
  }

  /// Export graph to DOT format (Graphviz).
  ///
  /// # Example
  /// ```bash
  /// cargo rail graph --dot > graph.dot
  /// dot -Tpng graph.dot -o graph.png
  /// ```
  ///
  /// TODO: Used by `cargo rail graph --dot` command
  #[allow(dead_code)]
  pub fn to_dot(&self) -> String {
    use petgraph::dot::{Config, Dot};

    let dot = Dot::with_attr_getters(
      &self.graph,
      &[Config::EdgeNoLabel],
      &|_, edge_ref| match edge_ref.weight() {
        DependencyKind::Normal => String::new(),
        DependencyKind::Development => "color=blue".to_string(),
        DependencyKind::Build => "color=orange".to_string(),
        _ => String::new(),
      },
      &|_, (_idx, node)| {
        if node.is_workspace_member {
          format!("label=\"{}\" shape=box style=filled fillcolor=lightblue", node.name)
        } else {
          format!("label=\"{}\" shape=ellipse", node.name)
        }
      },
    );

    format!("{:?}", dot)
  }

  /// Get all workspace crates with the specified visibility tier.
  ///
  /// Returns crates that are included in releases with this visibility level.
  ///
  /// TODO: Used by `cargo rail graph affected --visibility=X` command
  #[allow(dead_code)]
  pub fn crates_with_visibility(&self, visibility: Visibility) -> Vec<String> {
    let mut crates: Vec<String> = self
      .workspace_members
      .iter()
      .filter(|name| {
        if let Some(node_idx) = self.name_to_node.get(*name) {
          let node = &self.graph[*node_idx];
          node.visibilities.contains(&visibility)
        } else {
          false
        }
      })
      .cloned()
      .collect();

    crates.sort();
    crates
  }

  /// Get the visibilities for a specific crate.
  ///
  /// Returns the set of visibility tiers this crate belongs to,
  /// or an empty set if not found or not in any releases.
  ///
  /// TODO: Used by tier violation checks
  #[allow(dead_code)]
  pub fn crate_visibilities(&self, crate_name: &str) -> HashSet<Visibility> {
    self
      .name_to_node
      .get(crate_name)
      .map(|idx| self.graph[*idx].visibilities.clone())
      .unwrap_or_default()
  }

  /// Check if a crate has the specified visibility.
  ///
  /// TODO: Used by tier violation checks
  #[allow(dead_code)]
  pub fn has_visibility(&self, crate_name: &str, visibility: Visibility) -> bool {
    self.crate_visibilities(crate_name).contains(&visibility)
  }

  /// Get transitive dependents filtered by visibility.
  ///
  /// Returns all workspace crates that:
  /// 1. Depend on `crate_name` (directly or transitively)
  /// 2. Have the specified visibility tier
  ///
  /// TODO: Used by `cargo rail graph test --visibility=X` command
  #[allow(dead_code)]
  pub fn transitive_dependents_with_visibility(
    &self,
    crate_name: &str,
    visibility: Visibility,
  ) -> RailResult<Vec<String>> {
    let all_dependents = self.transitive_dependents(crate_name)?;

    let filtered: Vec<String> = all_dependents
      .into_iter()
      .filter(|dep| self.has_visibility(dep, visibility))
      .collect();

    Ok(filtered)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::env;

  #[test]
  fn test_load_workspace() {
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
      let workspace_root = PathBuf::from(manifest_dir);
      let graph = WorkspaceGraph::load(&workspace_root).unwrap();

      let members = graph.workspace_members();
      assert!(!members.is_empty());
      assert!(members.contains(&"cargo-rail".to_string()));
    }
  }

  #[test]
  fn test_dependencies() {
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
      let workspace_root = PathBuf::from(manifest_dir);
      let graph = WorkspaceGraph::load(&workspace_root).unwrap();

      // cargo-rail depends on clap, serde, etc.
      let deps = graph.direct_dependencies("cargo-rail").unwrap();
      assert!(!deps.is_empty());
    }
  }
}
