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
//! - **Algorithms**: Shortest paths, reachability, transitive closure
//! - **Path cache**: File → owning crate mapping (lazy, interior mutability)

use crate::error::{RailError, RailResult};
use cargo_metadata::DependencyKind;
use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// A package node in the dependency graph.
#[derive(Debug, Clone)]
pub struct PackageNode {
  /// Package name
  pub name: String,
  /// Path to Cargo.toml
  pub manifest_path: PathBuf,
  /// Whether this is a workspace member
  pub is_workspace_member: bool,
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

  /// Workspace members only (subset of graph nodes) - for O(1) membership checks
  workspace_members: HashSet<String>,

  /// Pre-sorted workspace member names - avoids repeated sort on each call
  sorted_members: Vec<String>,

  /// Workspace root directory (for converting absolute paths to relative)
  workspace_root: PathBuf,

  /// Path cache: workspace-relative directory → owning crate name
  /// Built eagerly during graph construction (saves 10-50ms on first file lookup)
  /// Uses RwLock instead of RefCell for Send/Sync compatibility
  /// Stores workspace-relative paths to support deleted files (no canonicalize needed)
  path_cache: RwLock<Option<HashMap<PathBuf, String>>>,
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
    let mut name_to_node = HashMap::new();
    let mut id_to_node = HashMap::new();
    let mut workspace_members = HashSet::new();

    // Get workspace member IDs
    let workspace_pkg_ids: HashSet<_> = metadata.workspace_packages().iter().map(|pkg| pkg.id.clone()).collect();

    // Add all packages as nodes (workspace + dependencies)
    for package in &metadata.packages {
      let crate_name = package.name.as_ref().to_string();

      let node = PackageNode {
        name: crate_name.clone(),
        manifest_path: package.manifest_path.clone().into_std_path_buf(),
        is_workspace_member: workspace_pkg_ids.contains(&package.id),
      };

      let node_idx = graph.add_node(node.clone());
      name_to_node.insert(package.name.as_ref().to_string(), node_idx);
      id_to_node.insert(package.id.clone(), node_idx);

      if workspace_pkg_ids.contains(&package.id) {
        workspace_members.insert(package.name.as_ref().to_string());
      }
    }

    // Add dependency edges
    for package in &metadata.packages {
      let from_idx = id_to_node[&package.id];

      for dep in &package.dependencies {
        // Find the resolved dependency
        if let Some(to_idx) = name_to_node.get(dep.name.as_str()) {
          graph.add_edge(from_idx, *to_idx, dep.kind);
        }
      }
    }

    // Pre-sort workspace members once at construction
    let mut sorted_members: Vec<String> = workspace_members.iter().cloned().collect();
    sorted_members.sort();

    // Store workspace root for path normalization
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();

    let graph = Self {
      graph,
      name_to_node,
      workspace_members,
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
    if crate_names.is_empty() {
      return Ok(HashSet::new());
    }

    // Find all start nodes
    let start_nodes: Vec<NodeIndex> = crate_names
      .iter()
      .filter_map(|name| self.name_to_node.get(name).copied())
      .collect();

    if start_nodes.is_empty() {
      return Ok(HashSet::new());
    }

    // Single BFS/DFS from all start nodes
    let mut visited = HashSet::new();
    let mut stack = start_nodes;
    let mut dependents = HashSet::new();

    // Pre-compute start node indices for fast lookup
    let start_node_set: HashSet<NodeIndex> = stack.iter().copied().collect();

    while let Some(node_idx) = stack.pop() {
      if visited.contains(&node_idx) {
        continue;
      }
      visited.insert(node_idx);

      // Add all incoming neighbors (things that depend on this)
      for neighbor_idx in self.graph.neighbors_directed(node_idx, Direction::Incoming) {
        let neighbor = &self.graph[neighbor_idx];

        // Only include workspace members that are not in the original set
        if neighbor.is_workspace_member && !start_node_set.contains(&neighbor_idx) {
          dependents.insert(neighbor.name.clone());
        }

        stack.push(neighbor_idx);
      }
    }

    Ok(dependents)
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
    let mut subgraph = DiGraph::<&PackageNode, DependencyKind>::new();
    let mut name_to_subgraph_idx = HashMap::new();

    // Add only workspace member nodes
    for (name, &idx) in &self.name_to_node {
      let node = &self.graph[idx];
      if node.is_workspace_member {
        let subgraph_idx = subgraph.add_node(node);
        name_to_subgraph_idx.insert(name.clone(), subgraph_idx);
      }
    }

    // Add edges between workspace members only
    // IMPORTANT: Skip dev-dependencies as they don't affect publish order
    // (dev-deps can have cycles, including self-references for feature-gated test utils)
    for (from_name, &from_subgraph_idx) in &name_to_subgraph_idx {
      let from_graph_idx = self.name_to_node[from_name];

      // Check all outgoing edges from this workspace member
      for neighbor_graph_idx in self.graph.neighbors_directed(from_graph_idx, Direction::Outgoing) {
        let neighbor_node = &self.graph[neighbor_graph_idx];

        // Only add edge if the neighbor is also a workspace member
        if neighbor_node.is_workspace_member
          && let Some(&to_subgraph_idx) = name_to_subgraph_idx.get(&neighbor_node.name)
          && let Some(edge) = self.graph.find_edge(from_graph_idx, neighbor_graph_idx)
        {
          let kind = *self.graph.edge_weight(edge).unwrap();
          // Skip dev-dependencies - they don't affect publish order and can have cycles
          // Also skip self-references (crate depending on itself for test features)
          if kind != DependencyKind::Development && from_name != &neighbor_node.name {
            subgraph.add_edge(from_subgraph_idx, to_subgraph_idx, kind);
          }
        }
      }
    }

    // Now run toposort on the workspace-only subgraph
    let sorted = toposort(&subgraph, None).map_err(|cycle| {
      let node = subgraph[cycle.node_id()];
      RailError::message(format!(
        "Circular dependency detected involving workspace crate: '{}'. This should not happen in a valid Cargo workspace.",
        node.name
      ))
    })?;

    // Collect names in dependency order
    let result: Vec<String> = sorted.into_iter().map(|idx| subgraph[idx].name.clone()).collect();

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
    // Filter out VCS directories (git, jj, etc.)
    if Self::should_ignore_path(file_path) {
      return None;
    }

    // Normalize to workspace-relative path
    let relative_path = self.to_workspace_relative(file_path)?;

    let cache = self
      .path_cache
      .read()
      .expect("RwLock poisoned: another thread panicked while holding the lock");
    let cache_ref = cache.as_ref()?;

    // Walk up directory tree looking for a crate root
    let mut current = relative_path.as_path();

    // Check the file's directory first
    if let Some(parent) = current.parent() {
      if let Some(crate_name) = cache_ref.get(parent) {
        return Some(crate_name.clone());
      }
      current = parent;
    }

    // Walk up looking for parent directories
    while let Some(parent) = current.parent() {
      if let Some(crate_name) = cache_ref.get(parent) {
        return Some(crate_name.clone());
      }
      current = parent;
    }

    // Check root directory (for root-level crate)
    cache_ref.get(Path::new("")).cloned()
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
    let mut cache = HashMap::new();

    for crate_name in &self.workspace_members {
      if let Some(node_idx) = self.name_to_node.get(crate_name) {
        let node = &self.graph[*node_idx];

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

          cache.insert(relative_root, crate_name.clone());
        }
      }
    }

    *self.path_cache.write().unwrap() = Some(cache);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
}
