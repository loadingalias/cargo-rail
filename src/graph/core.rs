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

use crate::cargo::WorkspaceMetadata;
use crate::error::{RailError, RailResult};
use cargo_metadata::{DependencyKind, PackageId};
use petgraph::Direction;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// A package node in the dependency graph.
#[derive(Debug, Clone)]
pub struct PackageNode {
  pub name: String,

  /// Package version from Cargo.toml.
  ///
  /// # Future Use
  /// - Version conflict detection: Find crates with mismatched versions
  /// - MSRV analysis: Detect minimum Rust version requirements across workspace
  #[allow(dead_code)]
  pub version: String,

  pub manifest_path: PathBuf,
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

  /// Index: PackageId → node index (for resolve graph traversal).
  ///
  /// # Future Use
  /// - Resolve graph traversal: Map resolved dependencies to graph nodes
  /// - Feature analysis: Cross-reference cargo metadata with dependency graph
  /// - External dependency queries: Look up non-workspace packages efficiently
  ///
  /// TODO: Expose this via a public method when implementing `cargo rail resolve` or similar advanced analysis.
  #[allow(dead_code)]
  id_to_node: HashMap<PackageId, NodeIndex>,

  /// Workspace members only (subset of graph nodes)
  workspace_members: HashSet<String>,

  /// Path cache: directory → owning crate name
  /// Built lazily on first file lookup (thread-safe interior mutability)
  /// Uses RwLock instead of RefCell for Send/Sync compatibility
  path_cache: RwLock<Option<HashMap<PathBuf, String>>>,

  /// Original cargo metadata for advanced queries.
  ///
  /// # Future Use
  /// - Feature analysis: Query feature flags and their dependencies
  /// - Target analysis: Find packages with specific build targets (bins, libs, tests)
  /// - Edition/MSRV queries: Validate compatibility across workspace
  #[allow(dead_code)]
  metadata: WorkspaceMetadata,
}

impl WorkspaceGraph {
  /// Load workspace graph from root directory.
  ///
  /// # Performance
  /// - cargo_metadata: 50-200ms for large workspaces
  /// - Graph construction: 10-50ms
  /// - Path cache: deferred until first file lookup
  pub fn load(workspace_root: &Path) -> RailResult<Self> {
    let metadata = WorkspaceMetadata::load(workspace_root)?;

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

      let node = PackageNode {
        name: crate_name.clone(),
        version: package.version.to_string(),
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
  /// # Future Use
  /// Will power `cargo rail graph --crate X` to display:
  /// - Direct dependencies: "lib-core depends on: serde, tokio, anyhow"
  /// - Dependency analysis: Find immediate dependencies for auditing
  /// - Visualization: Generate dependency tree for a specific crate
  ///
  /// TODO: Use this for `cargo rail inspect` CLI command.
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
  /// # Future Use
  /// Will power `cargo rail graph --crate X` to display:
  /// - Direct dependents: "lib-core is used by: bin-cli, lib-api, lib-web"
  /// - Impact analysis: See what breaks when modifying this crate
  /// - Reverse dependency tree: Understand crate usage across workspace
  ///
  /// TODO: Use this for `cargo rail inspect` or `cargo rail affected` CLI commands.
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
    // Topological sort gives us the order (roots first)
    let sorted = toposort(&self.graph, None).map_err(|cycle| {
      let node = &self.graph[cycle.node_id()];
      RailError::message(format!(
        "Circular dependency detected involving crate: '{}'. This should not happen in a valid Cargo workspace.",
        node.name
      ))
    })?;

    // Filter to workspace members only and collect names
    // toposort returns in "dependency order" - roots (no deps) first, leaves (most deps) last
    // For publishing, we want dependencies before dependents, so this order is correct
    let result: Vec<String> = sorted
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

    Ok(result)
  }

  /// Map a file path to its owning crate.
  ///
  /// Builds path cache on first call, then O(1) lookups.
  /// Filters out VCS directories (.git, .jj) and other non-source paths.
  pub fn file_to_crate(&self, file_path: &Path) -> Option<String> {
    // Filter out VCS directories (git, jj, etc.)
    if Self::should_ignore_path(file_path) {
      return None;
    }

    // Build cache if needed (interior mutability with RwLock)
    {
      let cache = self
        .path_cache
        .read()
        .expect("RwLock poisoned: another thread panicked while holding the lock");
      if cache.is_none() {
        drop(cache); // Release read lock before acquiring write lock
        self.build_path_cache();
      }
    }

    // Normalize path
    let normalized = file_path.canonicalize().ok()?;

    let cache = self
      .path_cache
      .read()
      .expect("RwLock poisoned: another thread panicked while holding the lock");
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

  /// Access raw cargo metadata for advanced queries.
  ///
  /// # Future Use
  /// - Feature analysis: Query and validate feature flags
  /// - Package metadata: Access detailed package information
  /// - Custom graph algorithms: Build domain-specific graph queries
  ///
  /// # Example
  /// ```ignore
  /// let graph = WorkspaceGraph::load(root)?;
  /// let metadata = graph.metadata();
  /// let features = metadata.package_features("lib-core");
  /// ```
  #[allow(dead_code)]
  pub fn metadata(&self) -> &WorkspaceMetadata {
    &self.metadata
  }

  /// Find dependency path: why does `from` depend on `to`?
  ///
  /// Returns the shortest dependency chain, or None if no dependency exists.
  ///
  /// Uses BFS to find the shortest path in the dependency graph.
  ///
  /// # Future Use
  /// Will power `cargo rail graph --why <from> <to>` for dependency debugging:
  /// - Understand transitive dependencies: "Why does bin-cli pull in tokio?"
  /// - Audit dependency chains: Trace the path from app to vulnerable dependency
  /// - Refactoring decisions: Identify dependency paths to break or preserve
  ///
  /// TODO: Implement `cargo rail why` command using this method.
  ///
  /// # Example
  /// ```ignore
  /// // If bin-cli → lib-core → lib-util:
  /// let path = graph.why_depends_on("bin-cli", "lib-util")?;
  /// assert_eq!(path, Some(vec!["bin-cli", "lib-core", "lib-util"]));
  /// ```
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

  /// Export graph to DOT format (Graphviz) for visualization.
  ///
  /// Outputs workspace dependency graph in DOT format with:
  /// - Workspace members: Blue boxes
  /// - External dependencies: Ellipses
  /// - Dev dependencies: Blue edges
  /// - Build dependencies: Orange edges
  /// - Normal dependencies: Black edges
  ///
  /// # Future Use
  /// Will power `cargo rail graph --dot` for visualization:
  /// - Generate dependency diagrams for documentation
  /// - Visual debugging: See complex dependency relationships at a glance
  /// - Architecture analysis: Understand workspace structure visually
  ///
  /// TODO: Implement `cargo rail graph` command (as a top-level visualization command) using this method.
  ///
  /// # Example
  /// ```bash
  /// cargo rail graph --dot > graph.dot
  /// dot -Tpng graph.dot -o graph.png
  /// open graph.png
  /// ```
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
