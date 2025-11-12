//! Dependency graph construction and topological sorting
//!
//! Builds a directed graph of workspace dependencies to determine
//! the correct publish order (dependencies must be published before
//! dependents).

use crate::core::error::RailResult;
use cargo_metadata::{DependencyKind, Package};
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::{HashMap, HashSet};

/// Dependency graph for workspace crates
pub struct CrateGraph {
    graph: DiGraph<String, ()>,
    node_map: HashMap<String, NodeIndex>,
}

impl CrateGraph {
    /// Build dependency graph from workspace packages
    ///
    /// Creates a directed graph where edges point from dependent → dependency.
    /// This ensures topological sort produces dependencies-first ordering.
    pub fn from_workspace(packages: &[Package]) -> RailResult<Self> {
        let mut graph = DiGraph::new();
        let mut node_map = HashMap::new();

        // Build set of workspace package names for filtering
        let workspace_names: HashSet<String> = packages.iter().map(|p| p.name.to_string()).collect();

        // Add all packages as nodes
        for package in packages {
            let name = package.name.to_string();
            let idx = graph.add_node(name.clone());
            node_map.insert(name, idx);
        }

        // Add edges for dependencies
        // Edge direction: dependent → dependency (so topo sort gives us deps first)
        for package in packages {
            let dependent_name = package.name.to_string();
            let dependent_idx = node_map[&dependent_name];

            // Process all dependency types (normal, dev, build)
            for dep in &package.dependencies {
                // Only create edges for workspace members
                // (external dependencies don't matter for our publish order)
                let dep_name = dep.name.to_string();
                if workspace_names.contains(&dep_name) {
                    // Skip dev and build dependencies for publish ordering
                    // (they're not required at publish time)
                    match dep.kind {
                        DependencyKind::Normal => {
                            let dependency_idx = node_map[&dep_name];
                            graph.add_edge(dependent_idx, dependency_idx, ());
                        }
                        DependencyKind::Development | DependencyKind::Build => {
                            // Dev/build deps don't affect publish order
                            continue;
                        }
                        _ => continue,
                    }
                }
            }
        }

        Ok(Self { graph, node_map })
    }

    /// Get topological sort (publish order: dependencies first)
    ///
    /// Returns crates in the order they should be published.
    /// Dependencies appear before their dependents.
    pub fn topological_order(&self) -> RailResult<Vec<String>> {
        let sorted = toposort(&self.graph, None).map_err(|cycle| {
            let node_idx = cycle.node_id();
            let crate_name = &self.graph[node_idx];
            anyhow::anyhow!(
                "Circular dependency detected involving crate '{}'. \
                 Cannot determine publish order with cyclic dependencies.",
                crate_name
            )
        })?;

        // Reverse because edges point dependent → dependency,
        // but we want dependencies first in output
        Ok(sorted
            .into_iter()
            .rev()
            .map(|idx| self.graph[idx].clone())
            .collect())
    }

    /// Check if graph has cycles (circular dependencies)
    pub fn has_cycles(&self) -> bool {
        toposort(&self.graph, None).is_err()
    }

    /// Get dependencies of a specific crate
    pub fn dependencies_of(&self, crate_name: &str) -> Option<Vec<String>> {
        let idx = self.node_map.get(crate_name)?;
        let deps: Vec<String> = self
            .graph
            .neighbors(*idx)
            .map(|dep_idx| self.graph[dep_idx].clone())
            .collect();
        Some(deps)
    }

    /// Get number of crates in graph
    pub fn len(&self) -> usize {
        self.graph.node_count()
    }

    /// Check if graph is empty
    pub fn is_empty(&self) -> bool {
        self.graph.node_count() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_graph() {
        let graph = CrateGraph::from_workspace(&[]).unwrap();
        assert!(graph.is_empty());
        assert_eq!(graph.topological_order().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn test_manual_graph_construction() {
        // Test the graph algorithms directly without cargo_metadata
        let mut graph = DiGraph::new();
        let mut node_map = HashMap::new();

        // Create: a -> b -> c (a depends on b, b depends on c)
        let idx_a = graph.add_node("a".to_string());
        let idx_b = graph.add_node("b".to_string());
        let idx_c = graph.add_node("c".to_string());

        node_map.insert("a".to_string(), idx_a);
        node_map.insert("b".to_string(), idx_b);
        node_map.insert("c".to_string(), idx_c);

        // Add edges (dependent -> dependency)
        graph.add_edge(idx_a, idx_b, ());
        graph.add_edge(idx_b, idx_c, ());

        let crate_graph = CrateGraph { graph, node_map };

        // Topological order should be: c, b, a (dependencies first)
        let order = crate_graph.topological_order().unwrap();
        assert_eq!(order, vec!["c", "b", "a"]);
    }

    #[test]
    fn test_diamond_dependency_manual() {
        // a depends on b and c
        // b and c both depend on d
        let mut graph = DiGraph::new();
        let mut node_map = HashMap::new();

        let idx_a = graph.add_node("a".to_string());
        let idx_b = graph.add_node("b".to_string());
        let idx_c = graph.add_node("c".to_string());
        let idx_d = graph.add_node("d".to_string());

        node_map.insert("a".to_string(), idx_a);
        node_map.insert("b".to_string(), idx_b);
        node_map.insert("c".to_string(), idx_c);
        node_map.insert("d".to_string(), idx_d);

        // Add edges
        graph.add_edge(idx_a, idx_b, ());
        graph.add_edge(idx_a, idx_c, ());
        graph.add_edge(idx_b, idx_d, ());
        graph.add_edge(idx_c, idx_d, ());

        let crate_graph = CrateGraph { graph, node_map };
        let order = crate_graph.topological_order().unwrap();

        // d must be first, a must be last
        assert_eq!(order[0], "d");
        assert_eq!(order[3], "a");
    }

    #[test]
    fn test_dependencies_of() {
        let mut graph = DiGraph::new();
        let mut node_map = HashMap::new();

        let idx_a = graph.add_node("a".to_string());
        let idx_b = graph.add_node("b".to_string());
        let idx_c = graph.add_node("c".to_string());

        node_map.insert("a".to_string(), idx_a);
        node_map.insert("b".to_string(), idx_b);
        node_map.insert("c".to_string(), idx_c);

        graph.add_edge(idx_a, idx_b, ());
        graph.add_edge(idx_a, idx_c, ());

        let crate_graph = CrateGraph { graph, node_map };
        let mut deps = crate_graph.dependencies_of("a").unwrap();
        deps.sort();

        assert_eq!(deps, vec!["b", "c"]);
        assert_eq!(
            crate_graph.dependencies_of("b").unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn test_circular_dependency_detection() {
        // Create circular dependency: a -> b -> c -> a
        let mut graph = DiGraph::new();
        let mut node_map = HashMap::new();

        let idx_a = graph.add_node("a".to_string());
        let idx_b = graph.add_node("b".to_string());
        let idx_c = graph.add_node("c".to_string());

        node_map.insert("a".to_string(), idx_a);
        node_map.insert("b".to_string(), idx_b);
        node_map.insert("c".to_string(), idx_c);

        graph.add_edge(idx_a, idx_b, ());
        graph.add_edge(idx_b, idx_c, ());
        graph.add_edge(idx_c, idx_a, ()); // Create cycle

        let crate_graph = CrateGraph { graph, node_map };

        // Should detect cycle
        assert!(crate_graph.has_cycles());
        assert!(crate_graph.topological_order().is_err());
    }
}
