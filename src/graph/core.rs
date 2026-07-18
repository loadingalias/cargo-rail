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
//! - **Edges**: Resolved aliases with independent kind and target semantics
//! - **Index**: Exact package IDs for graph work; package names only for user selection
//! - **Algorithms**: Shortest paths, reachability, transitive closure
//! - **Path cache**: File → owning crate mapping (lazy, interior mutability)

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use cargo_metadata::cargo_platform::Platform;
use cargo_metadata::{DependencyKind, PackageId, Source};
use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef as _;
use rustc_hash::FxHashMap;
use semver::Version;

use crate::error::{RailError, RailResult};

/// A package node in the dependency graph.
#[derive(Debug, Clone)]
pub struct PackageNode {
  /// Cargo's exact package identity.
  pub id: PackageId,
  /// Package name.
  pub name: String,
  /// Package version.
  pub version: Version,
  /// Package source, or `None` for a local package.
  pub source: Option<Source>,
  /// Path to `Cargo.toml`.
  pub manifest_path: PathBuf,
  /// Whether this is a workspace member.
  pub is_workspace_member: bool,
}

/// One resolved dependency relationship between two exact packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyEdge {
  /// Dependency name visible to Rust code, including Cargo renames.
  pub name: String,
  /// Compilation domain for this relationship.
  pub kind: DependencyKind,
  /// Target condition for this relationship, if any.
  pub target: Option<Platform>,
}

/// Workspace dependency graph.
///
/// Built from cargo_metadata, using petgraph for efficient traversals.
pub struct WorkspaceGraph {
  /// The dependency graph (petgraph DiGraph)
  /// Nodes: PackageNode
  /// Edges: resolved dependency alias + kind + target
  graph: DiGraph<PackageNode, DependencyEdge>,

  /// Primary index: exact Cargo package ID → node index.
  id_to_node: FxHashMap<PackageId, NodeIndex>,

  /// User-facing workspace-name lookup. Multiple exact members may share a name.
  workspace_name_index: FxHashMap<String, Vec<NodeIndex>>,

  /// Pre-sorted workspace member names - avoids repeated sort on each call
  sorted_members: Vec<String>,

  /// Workspace root directory (for converting absolute paths to relative)
  workspace_root: PathBuf,

  /// Path cache: workspace-relative directory → exact graph node
  /// Built eagerly during graph construction (saves 10-50ms on first file lookup)
  /// Uses RwLock instead of RefCell for Send/Sync compatibility
  /// Stores workspace-relative paths to support deleted files (no canonicalize needed)
  path_cache: RwLock<Option<FxHashMap<PathBuf, NodeIndex>>>,
}

impl WorkspaceGraph {
  /// Load workspace graph from cargo metadata.
  ///
  /// # Performance
  /// - Graph construction: 10-50ms
  /// - Path cache: built eagerly (10-50ms)
  /// - **Does not reload metadata** (use existing from CargoState)
  pub fn from_metadata(metadata: &cargo_metadata::Metadata) -> RailResult<Self> {
    // Build petgraph
    let mut graph = DiGraph::new();
    let mut workspace_name_index: FxHashMap<String, Vec<NodeIndex>> = FxHashMap::default();
    let mut id_to_node = FxHashMap::default();

    // Get workspace member IDs
    let workspace_pkg_ids: HashSet<_> = metadata.workspace_packages().iter().map(|pkg| pkg.id.clone()).collect();

    // Add all packages as nodes (workspace + dependencies)
    for package in &metadata.packages {
      let crate_name = package.name.as_ref().to_string();
      let is_workspace_member = workspace_pkg_ids.contains(&package.id);

      let node = PackageNode {
        id: package.id.clone(),
        name: crate_name.clone(),
        version: package.version.clone(),
        source: package.source.clone(),
        manifest_path: package.manifest_path.clone().into_std_path_buf(),
        is_workspace_member,
      };

      let node_idx = graph.add_node(node);
      if id_to_node.insert(package.id.clone(), node_idx).is_some() {
        return Err(RailError::message(format!(
          "Cargo metadata contains duplicate package ID '{}'",
          package.id
        )));
      }

      if is_workspace_member {
        workspace_name_index.entry(crate_name).or_default().push(node_idx);
      }
    }

    for matches in workspace_name_index.values_mut() {
      matches.sort_by(|left, right| graph[*left].id.repr.cmp(&graph[*right].id.repr));
    }

    // Build only from Cargo's exact resolved graph. Package.dependencies is
    // declaration metadata and cannot identify exact resolved destinations or
    // combined kind/target variants.
    if let Some(resolve) = &metadata.resolve {
      for node in &resolve.nodes {
        let Some(&from_idx) = id_to_node.get(&node.id) else {
          return Err(RailError::message(format!(
            "Cargo resolve node '{}' is missing from the package graph",
            node.id
          )));
        };

        for dependency in &node.deps {
          let Some(&to_idx) = id_to_node.get(&dependency.pkg) else {
            return Err(RailError::message(format!(
              "Cargo resolve dependency '{}' from '{}' points to unknown package '{}'",
              dependency.name, node.id, dependency.pkg
            )));
          };
          if dependency.dep_kinds.is_empty() {
            return Err(RailError::message(format!(
              "Cargo resolve dependency '{}' from '{}' has no kind or target semantics",
              dependency.name, node.id
            )));
          }

          for dep_kind in &dependency.dep_kinds {
            graph.add_edge(
              from_idx,
              to_idx,
              DependencyEdge {
                name: dependency.name.clone(),
                kind: dep_kind.kind,
                target: dep_kind.target.clone(),
              },
            );
          }
        }
      }
    }

    // Pre-sort workspace members once at construction
    let mut sorted_members: Vec<String> = workspace_name_index.keys().cloned().collect();
    sorted_members.sort();

    // Store workspace root for path normalization
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();

    let graph = Self {
      graph,
      id_to_node,
      workspace_name_index,
      sorted_members,
      workspace_root,
      path_cache: RwLock::new(None),
    };

    // Build path cache eagerly instead of lazily (saves 10-50ms on first file lookup)
    graph.build_path_cache();

    Ok(graph)
  }

  /// Get all workspace member crate names (pre-sorted).
  pub fn workspace_members(&self) -> &[String] {
    &self.sorted_members
  }

  /// Get a package node by Cargo's exact package identity.
  pub fn package(&self, package_id: &PackageId) -> Option<&PackageNode> {
    self.id_to_node.get(package_id).map(|&node| &self.graph[node])
  }

  /// Resolve a user-supplied package name to one exact package.
  ///
  /// # Errors
  ///
  /// Returns an error when the name is missing or matches multiple package IDs.
  pub fn workspace_package_by_name(&self, package_name: &str) -> RailResult<&PackageNode> {
    self.find_node(package_name).map(|node| &self.graph[node])
  }

  /// Iterate over every resolved semantic edge from one exact package to another.
  ///
  /// The iterator is empty when either package is unknown or no relationship exists.
  /// Parallel alias, dependency-kind, and target variants are returned independently.
  pub fn dependency_edges<'a>(
    &'a self,
    from: &PackageId,
    to: &PackageId,
  ) -> impl Iterator<Item = &'a DependencyEdge> + 'a {
    let endpoints = self.id_to_node.get(from).copied().zip(self.id_to_node.get(to).copied());
    endpoints
      .into_iter()
      .flat_map(move |(from, to)| self.graph.edges_connecting(from, to).map(|edge| edge.weight()))
  }

  /// Get transitive reverse dependencies (all workspace crates that depend on this one).
  ///
  /// Uses petgraph DFS for efficient traversal.
  ///
  /// # Errors
  ///
  /// Returns [`RailError`] if `crate_name` is not found in the graph.
  ///
  /// # Performance
  ///
  /// O(V + E) where V = vertices, E = edges. Typically <10ms for <100 crates.
  pub fn transitive_dependents(&self, crate_name: &str) -> RailResult<Vec<String>> {
    let start_node = self.find_node(crate_name)?;
    Ok(self.transitive_dependents_from(start_node))
  }

  /// Get transitive workspace dependents of one exact package.
  pub fn transitive_dependents_by_id(&self, package_id: &PackageId) -> RailResult<Vec<String>> {
    let start_node = self.find_node_by_id(package_id)?;
    Ok(self.transitive_dependents_from(start_node))
  }

  fn transitive_dependents_from(&self, start_node: NodeIndex) -> Vec<String> {
    // DFS in reverse direction (incoming edges)
    let mut visited = HashSet::new();
    let mut stack = vec![start_node];
    let mut dependents = HashSet::new();
    let mut node_visits = 0;
    let mut edge_visits = 0;

    while let Some(node_idx) = stack.pop() {
      if !visited.insert(node_idx) {
        continue;
      }
      node_visits += 1;

      // Add all incoming neighbors (things that depend on this)
      for neighbor_idx in self.graph.neighbors_directed(node_idx, Direction::Incoming) {
        edge_visits += 1;
        let neighbor = &self.graph[neighbor_idx];

        // Only include workspace members; skip clone if already in set
        if neighbor.is_workspace_member && neighbor_idx != start_node && !dependents.contains(&neighbor.name) {
          dependents.insert(neighbor.name.clone());
        }

        stack.push(neighbor_idx);
      }
    }
    crate::instrumentation::record_graph_traversal(node_visits, edge_visits);

    let mut result: Vec<_> = dependents.into_iter().collect();
    result.sort();
    result
  }

  /// Get direct workspace dependents for a crate.
  ///
  /// Returns only workspace members with an immediate dependency edge to `crate_name`.
  pub fn direct_dependents(&self, crate_name: &str) -> RailResult<Vec<String>> {
    let node = self.find_node(crate_name)?;
    Ok(self.direct_dependents_from(node))
  }

  /// Get direct workspace dependents of one exact package.
  pub fn direct_dependents_by_id(&self, package_id: &PackageId) -> RailResult<Vec<String>> {
    let node = self.find_node_by_id(package_id)?;
    Ok(self.direct_dependents_from(node))
  }

  fn direct_dependents_from(&self, node: NodeIndex) -> Vec<String> {
    let mut dependent_nodes = HashSet::new();
    let mut edge_visits = 0;

    for neighbor_idx in self.graph.neighbors_directed(node, Direction::Incoming) {
      edge_visits += 1;
      let neighbor = &self.graph[neighbor_idx];
      if neighbor.is_workspace_member {
        dependent_nodes.insert(neighbor_idx);
      }
    }
    crate::instrumentation::record_graph_traversal(1, edge_visits);

    let mut result: Vec<_> = dependent_nodes
      .into_iter()
      .map(|node_idx| self.graph[node_idx].name.clone())
      .collect();
    result.sort();
    result
  }

  /// Get transitive reverse dependencies for multiple crates in a single traversal.
  ///
  /// This is more efficient than calling `transitive_dependents()` multiple times
  /// when you have many direct crates, as it does a single O(V+E) traversal instead
  /// of O(N × (V+E)) where N is the number of crates.
  ///
  /// # Performance
  /// O(V + E) regardless of input set size. Significantly faster than N separate
  /// traversals for large input sets.
  pub fn transitive_dependents_of_set(&self, crate_names: &HashSet<String>) -> RailResult<HashSet<String>> {
    let package_ids = self.resolve_package_names(crate_names)?;
    self.transitive_dependents_of_ids(&package_ids)
  }

  /// Get transitive workspace dependents of exact packages in one traversal.
  pub fn transitive_dependents_of_ids(&self, package_ids: &HashSet<PackageId>) -> RailResult<HashSet<String>> {
    if package_ids.is_empty() {
      return Ok(HashSet::new());
    }

    let start_nodes: Vec<NodeIndex> = package_ids
      .iter()
      .map(|package_id| self.find_node_by_id(package_id))
      .collect::<RailResult<_>>()?;

    // Single BFS/DFS from all start nodes
    let mut visited = HashSet::new();
    let mut stack = start_nodes;
    let mut dependents = HashSet::new();
    let mut node_visits = 0;
    let mut edge_visits = 0;

    // Pre-compute start node indices for fast lookup
    let start_node_set: HashSet<NodeIndex> = stack.iter().copied().collect();

    while let Some(node_idx) = stack.pop() {
      if !visited.insert(node_idx) {
        continue;
      }
      node_visits += 1;

      // Add all incoming neighbors (things that depend on this)
      for neighbor_idx in self.graph.neighbors_directed(node_idx, Direction::Incoming) {
        edge_visits += 1;
        let neighbor = &self.graph[neighbor_idx];

        // Only include workspace members not in original set; skip clone if already in set
        if neighbor.is_workspace_member
          && !start_node_set.contains(&neighbor_idx)
          && !dependents.contains(&neighbor.name)
        {
          dependents.insert(neighbor.name.clone());
        }

        stack.push(neighbor_idx);
      }
    }
    crate::instrumentation::record_graph_traversal(node_visits, edge_visits);

    Ok(dependents)
  }

  /// Get `(depends_on, dependent)` pairs for transitive reverse dependencies of multiple crates.
  ///
  /// Returns one pair for each workspace dependent reachable from each input crate.
  /// If a dependent is reachable from multiple input crates, one pair is returned per
  /// originating crate. Output is sorted lexicographically by `(depends_on, dependent)`.
  pub fn transitive_dependent_pairs_of_set(&self, crate_names: &HashSet<String>) -> RailResult<Vec<(String, String)>> {
    let package_ids = self.resolve_package_names(crate_names)?;
    self.transitive_dependent_pairs_of_ids(&package_ids)
  }

  /// Get exact-package reverse dependency pairs in one traversal.
  pub fn transitive_dependent_pairs_of_ids(
    &self,
    package_ids: &HashSet<PackageId>,
  ) -> RailResult<Vec<(String, String)>> {
    if package_ids.is_empty() {
      return Ok(Vec::new());
    }

    let start_nodes: Vec<NodeIndex> = package_ids
      .iter()
      .map(|package_id| self.find_node_by_id(package_id))
      .collect::<RailResult<_>>()?;

    let mut visited: HashSet<(NodeIndex, NodeIndex)> = HashSet::new();
    let mut stack = Vec::with_capacity(start_nodes.len());
    for start_node in start_nodes {
      visited.insert((start_node, start_node));
      stack.push((start_node, start_node));
    }

    let mut pairs = Vec::new();
    let mut node_visits = 0;
    let mut edge_visits = 0;

    while let Some((node_idx, start_node)) = stack.pop() {
      node_visits += 1;
      for neighbor_idx in self.graph.neighbors_directed(node_idx, Direction::Incoming) {
        edge_visits += 1;
        let state = (neighbor_idx, start_node);
        if !visited.insert(state) {
          continue;
        }

        let neighbor = &self.graph[neighbor_idx];
        if neighbor.is_workspace_member && neighbor_idx != start_node {
          pairs.push((self.graph[start_node].name.clone(), neighbor.name.clone()));
        }

        stack.push(state);
      }
    }
    crate::instrumentation::record_graph_traversal(node_visits, edge_visits);

    pairs.sort();
    Ok(pairs)
  }

  /// Get workspace members in dependency order (dependencies first, dependents last).
  ///
  /// Returns crates in the order they should be published: a crate's dependencies
  /// are always published before the crate itself.
  ///
  /// Uses topological sort on the dependency graph to ensure correct ordering.
  ///
  /// # Errors
  /// Returns error if circular dependencies are detected (should never happen with Cargo).
  ///
  /// # Performance
  /// O(V + E) where V = vertices, E = edges. Typically <10ms for <100 crates.
  pub fn publish_order(&self) -> RailResult<Vec<String>> {
    // Build a subgraph with only workspace members
    // This is critical: external dependencies can have cycles (e.g., serde/serde_derive in dev deps),
    // but workspace members should never have cycles (Cargo enforces this)
    let mut subgraph = DiGraph::<&PackageNode, ()>::new();
    let mut node_to_subgraph_idx = FxHashMap::default();

    // Add only workspace member nodes
    for graph_idx in self.graph.node_indices() {
      let node = &self.graph[graph_idx];
      if node.is_workspace_member {
        let subgraph_idx = subgraph.add_node(node);
        node_to_subgraph_idx.insert(graph_idx, subgraph_idx);
      }
    }

    // Add edges between workspace members only
    // IMPORTANT: Skip dev-dependencies as they don't affect publish order
    // (dev-deps can have cycles, including self-references for feature-gated test utils)
    for (&from_graph_idx, &from_subgraph_idx) in &node_to_subgraph_idx {
      // Check every semantic edge. Multiple kind/target variants may connect
      // the same packages, but publish topology needs only one non-dev edge.
      for edge in self.graph.edges_directed(from_graph_idx, Direction::Outgoing) {
        let neighbor_graph_idx = edge.target();
        let neighbor_node = &self.graph[neighbor_graph_idx];

        // Only add edge if the neighbor is also a workspace member
        if neighbor_node.is_workspace_member
          && let Some(&to_subgraph_idx) = node_to_subgraph_idx.get(&neighbor_graph_idx)
          && edge.weight().kind != DependencyKind::Development
          && from_graph_idx != neighbor_graph_idx
          && subgraph.find_edge(from_subgraph_idx, to_subgraph_idx).is_none()
        {
          subgraph.add_edge(from_subgraph_idx, to_subgraph_idx, ());
        }
      }
    }

    crate::instrumentation::record_graph_traversal(subgraph.node_count(), subgraph.edge_count());

    // Now run toposort on the workspace-only subgraph
    let sorted = toposort(&subgraph, None).map_err(|cycle| {
      let node = subgraph[cycle.node_id()];
      RailError::message(format!(
        "Circular dependency detected involving workspace crate: '{}'. This should not happen in a valid Cargo workspace.",
        node.name
      ))
    })?;

    // Collect names in dependency order
    let result: Vec<String> = sorted.into_iter().rev().map(|idx| subgraph[idx].name.clone()).collect();

    Ok(result)
  }

  /// Map a file path to its owning crate.
  ///
  /// O(1) lookups using pre-built path cache.
  /// Filters out VCS directories (.git, .jj) and other non-source paths.
  ///
  /// Accepts both:
  /// - Workspace-relative paths (e.g., "crates/foo/src/lib.rs")
  /// - Absolute paths (will be converted to workspace-relative)
  ///
  /// Works for deleted files - does not require the file to exist on disk.
  pub fn file_to_crate(&self, file_path: &Path) -> Option<String> {
    self.file_to_package(file_path).map(|package| package.name.clone())
  }

  /// Map a file path to its exact owning workspace package.
  pub(crate) fn file_to_package(&self, file_path: &Path) -> Option<&PackageNode> {
    self.file_owner_node(file_path).map(|node| &self.graph[node])
  }

  fn file_owner_node(&self, file_path: &Path) -> Option<NodeIndex> {
    // Filter out VCS directories (git, jj, etc.)
    if Self::should_ignore_path(file_path) {
      return None;
    }

    // Normalize to workspace-relative path
    let relative_path = self.to_workspace_relative(file_path)?;

    // If the lock is poisoned (another thread panicked), return None gracefully
    let cache = self.path_cache.read().ok()?;
    let cache_ref = cache.as_ref()?;

    // Walk up directory tree looking for a crate root
    let mut current = relative_path.as_path();

    // Check the file's directory first
    if let Some(parent) = current.parent() {
      if let Some(&node) = cache_ref.get(parent) {
        return Some(node);
      }
      current = parent;
    }

    // Walk up looking for parent directories
    while let Some(parent) = current.parent() {
      if let Some(&node) = cache_ref.get(parent) {
        return Some(node);
      }
      current = parent;
    }

    // Check root directory (for root-level crate)
    cache_ref.get(Path::new("")).copied()
  }

  /// Convert a path to workspace-relative.
  ///
  /// Handles:
  /// - Already relative paths: returned as-is
  /// - Absolute paths: strips workspace root prefix
  /// - Paths that don't exist: works without filesystem access
  fn to_workspace_relative(&self, path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
      // Try to strip workspace root prefix
      path.strip_prefix(&self.workspace_root).ok().map(PathBuf::from)
    } else {
      // Already relative - assume it's workspace-relative
      Some(path.to_path_buf())
    }
  }

  /// Map multiple files to owning crates.
  pub fn files_to_crates(&self, file_paths: &[impl AsRef<Path>]) -> HashSet<String> {
    file_paths
      .iter()
      .filter_map(|p| self.file_to_crate(p.as_ref()))
      .collect()
  }

  /// Map files to exact owning workspace packages.
  pub(crate) fn files_to_package_ids(&self, file_paths: &[impl AsRef<Path>]) -> HashSet<PackageId> {
    file_paths
      .iter()
      .filter_map(|path| self.file_to_package(path.as_ref()))
      .map(|package| package.id.clone())
      .collect()
  }

  /// Find node index by crate name.
  fn find_node(&self, crate_name: &str) -> RailResult<NodeIndex> {
    let Some(matches) = self.workspace_name_index.get(crate_name) else {
      return Err(RailError::message(format!(
        "Crate '{}' not found. Available workspace crates: {}",
        crate_name,
        self.workspace_members().join(", ")
      )));
    };

    if let [node] = matches.as_slice() {
      return Ok(*node);
    }

    let candidates = matches
      .iter()
      .map(|&node| {
        let package = &self.graph[node];
        let source = package
          .source
          .as_ref()
          .map(ToString::to_string)
          .unwrap_or_else(|| "path".to_string());
        format!(
          "  - {} {} | id: {} | source: {} | manifest: {}",
          package.name,
          package.version,
          package.id,
          source,
          package.manifest_path.display()
        )
      })
      .collect::<Vec<_>>()
      .join("\n");
    Err(RailError::with_help(
      format!("Package name '{}' is ambiguous", crate_name),
      format!("Use an exact Cargo package ID. Candidates:\n{candidates}"),
    ))
  }

  fn find_node_by_id(&self, package_id: &PackageId) -> RailResult<NodeIndex> {
    self
      .id_to_node
      .get(package_id)
      .copied()
      .ok_or_else(|| RailError::message(format!("Package ID '{}' not found in the resolved graph", package_id)))
  }

  fn resolve_package_names(&self, package_names: &HashSet<String>) -> RailResult<HashSet<PackageId>> {
    package_names
      .iter()
      .map(|name| self.find_node(name).map(|node| self.graph[node].id.clone()))
      .collect()
  }

  /// Check if a path should be ignored (VCS directories, build artifacts, etc.)
  fn should_ignore_path(path: &Path) -> bool {
    path.components().any(|component| {
      if let std::path::Component::Normal(name) = component {
        let name_str = name.to_string_lossy();
        // Ignore VCS directories
        matches!(
          name_str.as_ref(),
          ".git" | ".jj" | ".hg" | ".svn" |
          // Ignore build/target directories
          "target" | "node_modules" |
          // Ignore common temp/cache dirs
          ".cache" | "tmp" | ".tmp"
        )
      } else {
        false
      }
    })
  }

  /// Build path-to-crate mapping cache.
  ///
  /// Maps each workspace crate's root directory (workspace-relative) to its name.
  /// Uses workspace-relative paths to support deleted files (no canonicalize needed).
  fn build_path_cache(&self) {
    let mut cache = FxHashMap::default();

    for node_idx in self.graph.node_indices() {
      let node = &self.graph[node_idx];
      if node.is_workspace_member {
        // Get crate root (parent of Cargo.toml) as workspace-relative path
        if let Some(crate_root) = node.manifest_path.parent() {
          // Convert to workspace-relative path
          let relative_root = if crate_root.is_absolute() {
            crate_root
              .strip_prefix(&self.workspace_root)
              .map(PathBuf::from)
              .unwrap_or_else(|_| crate_root.to_path_buf())
          } else {
            crate_root.to_path_buf()
          };

          cache.insert(relative_root, node_idx);
        }
      }
    }

    // If the lock is poisoned (another thread panicked), skip cache update
    // The cache will be rebuilt on next access attempt
    if let Ok(mut guard) = self.path_cache.write() {
      *guard = Some(cache);
    }
  }
}

#[cfg(test)]
mod tests {
  use std::path::{Path, PathBuf};
  use std::sync::RwLock;

  use cargo_metadata::{MetadataCommand, PackageId, PackageName, Source};
  use semver::Version;

  use super::*;

  fn test_graph(edges: &[(&str, &str)]) -> WorkspaceGraph {
    let mut graph = DiGraph::new();
    let mut id_to_node = FxHashMap::default();
    let mut name_to_node = FxHashMap::default();
    let mut workspace_name_index = FxHashMap::default();

    for name in ["a", "b", "c", "d"] {
      let id = PackageId {
        repr: format!("path+file:///tmp/workspace/crates/{name}#{name}@0.1.0"),
      };
      let node = graph.add_node(PackageNode {
        id: id.clone(),
        name: name.to_string(),
        version: Version::new(0, 1, 0),
        source: None,
        manifest_path: PathBuf::from(format!("crates/{name}/Cargo.toml")),
        is_workspace_member: true,
      });
      id_to_node.insert(id, node);
      name_to_node.insert(name.to_string(), node);
      workspace_name_index.insert(name.to_string(), vec![node]);
    }

    for (from, to) in edges {
      graph.add_edge(
        name_to_node[*from],
        name_to_node[*to],
        DependencyEdge {
          name: (*to).to_string(),
          kind: DependencyKind::Normal,
          target: None,
        },
      );
    }

    WorkspaceGraph {
      graph,
      id_to_node,
      workspace_name_index,
      sorted_members: vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()],
      workspace_root: PathBuf::from("/tmp/workspace"),
      path_cache: RwLock::new(None),
    }
  }

  fn write_test_package(root: &Path, relative: &str, name: &str, version: &str, dependencies: &str) {
    let package_root = root.join(relative);
    std::fs::create_dir_all(package_root.join("src")).expect("test package directory should be created");
    std::fs::write(
      package_root.join("Cargo.toml"),
      format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2024\"\n\n{dependencies}"),
    )
    .expect("test package manifest should be written");
    std::fs::write(package_root.join("src/lib.rs"), "pub fn value() {}\n")
      .expect("test package source should be written");
  }

  #[test]
  fn test_should_ignore_path() {
    // VCS directories
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".git")),
      ".git should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".git/objects")),
      ".git/objects should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new("foo/.git/bar")),
      "nested .git should be ignored"
    );

    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".jj")),
      ".jj should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".jj/repo")),
      ".jj/repo should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new("src/.jj/file")),
      "nested .jj should be ignored"
    );

    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".hg")),
      ".hg should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".svn")),
      ".svn should be ignored"
    );

    // Build/dependency directories
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new("target")),
      "target should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new("target/debug")),
      "target/debug should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new("node_modules")),
      "node_modules should be ignored"
    );

    // Cache/temp directories
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".cache")),
      ".cache should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new("tmp")),
      "tmp should be ignored"
    );
    assert!(
      WorkspaceGraph::should_ignore_path(Path::new(".tmp")),
      ".tmp should be ignored"
    );

    // Normal paths should NOT be ignored
    assert!(
      !WorkspaceGraph::should_ignore_path(Path::new("src")),
      "src should not be ignored"
    );
    assert!(
      !WorkspaceGraph::should_ignore_path(Path::new("src/main.rs")),
      "src/main.rs should not be ignored"
    );
    assert!(
      !WorkspaceGraph::should_ignore_path(Path::new("Cargo.toml")),
      "Cargo.toml should not be ignored"
    );
    assert!(
      !WorkspaceGraph::should_ignore_path(Path::new("crates/foo/src/lib.rs")),
      "crate files should not be ignored"
    );
    assert!(
      !WorkspaceGraph::should_ignore_path(Path::new("README.md")),
      "README.md should not be ignored"
    );
  }

  #[test]
  fn test_transitive_dependent_pairs_of_ids_is_sorted_and_preserves_seed_provenance() {
    let graph = test_graph(&[("b", "a"), ("c", "a"), ("d", "b"), ("d", "c")]);
    let seeds = ["a", "c"]
      .into_iter()
      .map(|name| {
        graph
          .workspace_package_by_name(name)
          .expect("seed should be unique")
          .id
          .clone()
      })
      .collect();

    let pairs = graph
      .transitive_dependent_pairs_of_ids(&seeds)
      .expect("pair traversal should succeed");

    assert_eq!(
      pairs,
      vec![
        ("a".to_string(), "b".to_string()),
        ("a".to_string(), "c".to_string()),
        ("a".to_string(), "d".to_string()),
        ("c".to_string(), "d".to_string()),
      ]
    );
  }

  #[test]
  fn direct_dependents_collapse_parallel_semantic_edges() {
    let mut graph = test_graph(&[("b", "a")]);
    graph.graph.add_edge(
      graph.workspace_name_index["b"][0],
      graph.workspace_name_index["a"][0],
      DependencyEdge {
        name: "a_alias".into(),
        kind: DependencyKind::Development,
        target: None,
      },
    );

    assert_eq!(
      graph
        .direct_dependents_by_id(&graph.workspace_package_by_name("a").expect("a should be unique").id)
        .expect("parallel edge traversal should succeed"),
      vec!["b"]
    );
  }

  #[test]
  fn package_name_lookup_distinguishes_unique_and_missing_names() {
    let graph = test_graph(&[]);
    let package = graph.workspace_package_by_name("a").expect("a should resolve uniquely");
    assert_eq!(package.name, "a");
    assert_eq!(package.version, Version::new(0, 1, 0));

    let error = graph
      .workspace_package_by_name("missing")
      .expect_err("unknown names must fail");
    assert_eq!(
      error.to_string(),
      "Crate 'missing' not found. Available workspace crates: a, b, c, d"
    );
  }

  #[test]
  fn graph_preserves_same_name_packages_by_exact_package_id() {
    let mut metadata = MetadataCommand::new()
      .current_dir(env!("CARGO_MANIFEST_DIR"))
      .no_deps()
      .other_options(vec!["--offline".into(), "--locked".into()])
      .exec()
      .expect("workspace metadata should load");
    let template = metadata
      .packages
      .first()
      .expect("workspace metadata should contain cargo-rail")
      .clone();

    let first_id = PackageId {
      repr: "registry+https://github.com/rust-lang/crates.io-index#shared@1.0.0".into(),
    };
    let second_id = PackageId {
      repr: "registry+https://github.com/rust-lang/crates.io-index#shared@2.0.0".into(),
    };
    let source = Source {
      repr: "registry+https://github.com/rust-lang/crates.io-index".into(),
    };

    let mut first = template.clone();
    first.id = first_id.clone();
    first.name = PackageName::new("shared".into());
    first.version = Version::new(1, 0, 0);
    first.source = Some(source.clone());
    first.manifest_path = cargo_metadata::camino::Utf8PathBuf::from("/registry/shared-1.0.0/Cargo.toml");

    let mut second = template;
    second.id = second_id.clone();
    second.name = PackageName::new("shared".into());
    second.version = Version::new(2, 0, 0);
    second.source = Some(source.clone());
    second.manifest_path = cargo_metadata::camino::Utf8PathBuf::from("/registry/shared-2.0.0/Cargo.toml");

    metadata.packages = vec![first, second];
    metadata.workspace_members.clear();
    let graph = WorkspaceGraph::from_metadata(&metadata).expect("graph should preserve duplicate package names");

    assert_eq!(graph.graph.node_count(), 2);
    assert_eq!(graph.id_to_node.len(), 2);

    let first = graph
      .package(&first_id)
      .expect("first package ID should remain addressable");
    assert_eq!(first.id, first_id);
    assert_eq!(first.name, "shared");
    assert_eq!(first.version, Version::new(1, 0, 0));
    assert_eq!(first.source.as_ref(), Some(&source));
    assert_eq!(first.manifest_path, PathBuf::from("/registry/shared-1.0.0/Cargo.toml"));

    let second = graph
      .package(&second_id)
      .expect("second package ID should remain addressable");
    assert_eq!(second.id, second_id);
    assert_eq!(second.name, "shared");
    assert_eq!(second.version, Version::new(2, 0, 0));
    assert_eq!(second.source.as_ref(), Some(&source));
    assert_eq!(second.manifest_path, PathBuf::from("/registry/shared-2.0.0/Cargo.toml"));

    metadata.workspace_members = vec![first_id.clone(), second_id.clone()];
    let ambiguous_graph =
      WorkspaceGraph::from_metadata(&metadata).expect("synthetic duplicate-name workspace should remain representable");
    let error = ambiguous_graph
      .workspace_package_by_name("shared")
      .expect_err("duplicate package names must be rejected at the name boundary");
    assert_eq!(error.to_string(), "Package name 'shared' is ambiguous");
    let help = error.help_message().expect("ambiguity should list exact candidates");
    let first_position = help
      .find(first_id.repr.as_str())
      .expect("first candidate identity should be rendered");
    let second_position = help
      .find(second_id.repr.as_str())
      .expect("second candidate identity should be rendered");
    assert!(
      first_position < second_position,
      "candidate identities must be ordered deterministically: {help}"
    );
    for expected in [
      first_id.to_string(),
      second_id.to_string(),
      "shared 1.0.0".to_string(),
      "shared 2.0.0".to_string(),
      source.to_string(),
      "/registry/shared-1.0.0/Cargo.toml".to_string(),
      "/registry/shared-2.0.0/Cargo.toml".to_string(),
    ] {
      assert!(
        help.contains(&expected),
        "candidate help is missing {expected:?}: {help}"
      );
    }

    metadata.packages[1].id = first_id.clone();
    let error = match WorkspaceGraph::from_metadata(&metadata) {
      Ok(_) => panic!("duplicate package IDs must be rejected"),
      Err(error) => error,
    };
    assert!(
      error
        .to_string()
        .contains(&format!("duplicate package ID '{first_id}'"))
    );
  }

  #[test]
  fn resolved_edges_preserve_exact_destination_alias_and_parallel_semantics() {
    let workspace = tempfile::tempdir().expect("test workspace should be created");
    std::fs::write(
      workspace.path().join("Cargo.toml"),
      "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\nexclude = [\"vendor/shared-v1\"]\n",
    )
    .expect("workspace manifest should be written");

    write_test_package(workspace.path(), "vendor/shared-v1", "shared", "1.0.0", "");
    write_test_package(workspace.path(), "crates/shared-v2", "shared", "2.0.0", "");
    write_test_package(
      workspace.path(),
      "crates/consumer-one",
      "consumer-one",
      "0.1.0",
      "[dependencies]\nshared_alias = { package = \"shared\", path = \"../../vendor/shared-v1\" }\n",
    );
    write_test_package(
      workspace.path(),
      "crates/consumer-two",
      "consumer-two",
      "0.1.0",
      "[dependencies]\nshared_alias = { package = \"shared\", path = \"../shared-v2\" }\n\
       [dev-dependencies]\nshared_alias = { package = \"shared\", path = \"../shared-v2\" }\n\
       [build-dependencies]\nshared_alias = { package = \"shared\", path = \"../shared-v2\" }\n\
       [target.'cfg(windows)'.dependencies]\n\
       shared_alias = { package = \"shared\", path = \"../shared-v2\" }\n",
    );

    let mut metadata = MetadataCommand::new()
      .current_dir(workspace.path())
      .other_options(vec!["--offline".into()])
      .exec()
      .expect("adversarial workspace metadata should load");
    let package_id = |name: &str, version: Version| {
      metadata
        .packages
        .iter()
        .find(|package| package.name == name && package.version == version)
        .map(|package| package.id.clone())
        .unwrap_or_else(|| panic!("missing {name} package"))
    };
    let consumer_one = package_id("consumer-one", Version::new(0, 1, 0));
    let consumer_two = package_id("consumer-two", Version::new(0, 1, 0));
    let shared_v1 = package_id("shared", Version::new(1, 0, 0));
    let shared_v2 = package_id("shared", Version::new(2, 0, 0));

    let resolve = metadata
      .resolve
      .as_ref()
      .expect("full metadata should contain a resolve graph");
    let consumer_two_resolve = resolve
      .nodes
      .iter()
      .find(|node| node.id == consumer_two)
      .expect("consumer-two should have a resolve node");
    let resolved_dependency = consumer_two_resolve
      .deps
      .iter()
      .find(|dependency| dependency.pkg == shared_v2)
      .expect("consumer-two should resolve shared v2");
    assert_eq!(resolved_dependency.name, "shared_alias");
    assert_eq!(resolved_dependency.dep_kinds.len(), 4);

    let shared_v1_index = metadata
      .packages
      .iter()
      .position(|package| package.id == shared_v1)
      .expect("shared v1 package should exist");
    let shared_v1_package = metadata.packages.remove(shared_v1_index);
    metadata.packages.push(shared_v1_package);

    let graph = WorkspaceGraph::from_metadata(&metadata).expect("resolved graph should build");
    let semantic_edges = |from: &PackageId, to: &PackageId| {
      let mut edges: Vec<_> = graph
        .dependency_edges(from, to)
        .map(|edge| {
          (
            edge.name.clone(),
            edge.kind.to_string(),
            edge.target.as_ref().map(ToString::to_string),
          )
        })
        .collect();
      edges.sort();
      edges
    };

    assert_eq!(
      semantic_edges(&consumer_one, &shared_v1),
      vec![("shared_alias".into(), "normal".into(), None)]
    );
    assert!(semantic_edges(&consumer_one, &shared_v2).is_empty());
    assert!(semantic_edges(&consumer_two, &shared_v1).is_empty());
    assert_eq!(
      semantic_edges(&consumer_two, &shared_v2),
      vec![
        ("shared_alias".into(), "build".into(), None),
        ("shared_alias".into(), "dev".into(), None),
        ("shared_alias".into(), "normal".into(), None),
        ("shared_alias".into(), "normal".into(), Some("cfg(windows)".into())),
      ]
    );

    assert_eq!(
      graph
        .direct_dependents_by_id(&shared_v1)
        .expect("shared v1 should remain independently traversable"),
      vec!["consumer-one"]
    );
    assert_eq!(
      graph
        .direct_dependents_by_id(&shared_v2)
        .expect("shared v2 should remain independently traversable"),
      vec!["consumer-two"]
    );
    assert_eq!(
      graph
        .workspace_package_by_name("shared")
        .expect("the workspace member must win over an external package with the same name")
        .id,
      shared_v2
    );

    let publish_order = graph
      .publish_order()
      .expect("parallel semantic edges should preserve publish ordering");
    let shared_position = publish_order
      .iter()
      .position(|name| name == "shared")
      .expect("shared should be published");
    let consumer_position = publish_order
      .iter()
      .position(|name| name == "consumer-two")
      .expect("consumer-two should be published");
    assert!(
      shared_position < consumer_position,
      "dependencies must be published first"
    );
  }
}
