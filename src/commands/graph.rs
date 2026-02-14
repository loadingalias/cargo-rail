//! `cargo rail graph` - planner/action reasoning graph output.

use crate::commands::common::PlanOutputFormat;
use crate::commands::plan::{PlanOptions, build_plan_output};
use crate::error::{RailError, RailResult};
use crate::workspace::WorkspaceContext;
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::PathBuf;

/// Options for the `graph` command.
pub struct GraphOptions {
  /// Git ref to compare against.
  pub since: Option<String>,
  /// Start of SHA range (used with `to`).
  pub from: Option<String>,
  /// End of SHA range (used with `from`).
  pub to: Option<String>,
  /// Use merge-base with default branch.
  pub merge_base: bool,
  /// Planner confidence profile override.
  pub confidence_profile: Option<String>,
  /// Emit GraphViz DOT output.
  pub dot: bool,
  /// Optional output path.
  pub output: Option<PathBuf>,
}

#[derive(Serialize)]
struct GraphNode {
  id: String,
  kind: String,
  label: String,
}

#[derive(Serialize)]
struct GraphEdge {
  from: String,
  to: String,
  relation: String,
}

#[derive(Serialize)]
struct PlannerGraph {
  nodes: Vec<GraphNode>,
  edges: Vec<GraphEdge>,
}

/// Run the graph command.
pub fn run_graph(ctx: &WorkspaceContext, opts: GraphOptions) -> RailResult<()> {
  crate::output::set_json_mode(true);

  let plan = build_plan_output(
    ctx,
    &PlanOptions {
      since: opts.since,
      from: opts.from,
      to: opts.to,
      merge_base: opts.merge_base,
      format: PlanOutputFormat::Json,
      output: None,
      explain: false,
      confidence_profile: opts.confidence_profile,
    },
  )?;

  let graph = build_graph(&plan);
  let rendered = if opts.dot {
    to_dot(&graph)
  } else {
    serde_json::to_string_pretty(&graph).map_err(|e| RailError::message(format!("graph JSON failed: {}", e)))?
  };

  write_output(&rendered, opts.output.as_ref())
}

fn build_graph(plan: &crate::commands::plan::PlanOutput) -> PlannerGraph {
  let mut nodes = Vec::new();
  let mut edges = Vec::new();
  let mut seen = BTreeSet::new();

  for file in &plan.files {
    let file_id = format!("file:{}", file.path);
    if seen.insert(file_id.clone()) {
      nodes.push(GraphNode {
        id: file_id.clone(),
        kind: "file".to_string(),
        label: file.path.clone(),
      });
    }

    for owner in &file.owners {
      let crate_id = format!("crate:{}", owner);
      if seen.insert(crate_id.clone()) {
        nodes.push(GraphNode {
          id: crate_id.clone(),
          kind: "crate".to_string(),
          label: owner.clone(),
        });
      }
      edges.push(GraphEdge {
        from: file_id.clone(),
        to: crate_id,
        relation: "owned_by".to_string(),
      });
    }
  }

  for (surface, decision) in &plan.surfaces {
    let surface_id = format!("surface:{}", surface);
    if seen.insert(surface_id.clone()) {
      nodes.push(GraphNode {
        id: surface_id.clone(),
        kind: "surface".to_string(),
        label: format!(
          "{} ({})",
          surface,
          if decision.enabled { "enabled" } else { "disabled" }
        ),
      });
    }

    for reason_id in &decision.reasons {
      let reason = plan.trace.iter().find(|r| r.id == *reason_id);
      let reason_label = match reason {
        Some(r) => r.code.to_string(),
        None => format!("reason_{}", reason_id),
      };
      let reason_node_id = format!("reason:{}", reason_id);
      if seen.insert(reason_node_id.clone()) {
        nodes.push(GraphNode {
          id: reason_node_id.clone(),
          kind: "reason".to_string(),
          label: reason_label,
        });
      }
      edges.push(GraphEdge {
        from: reason_node_id,
        to: surface_id.clone(),
        relation: "enables".to_string(),
      });
    }
  }

  PlannerGraph { nodes, edges }
}

fn to_dot(graph: &PlannerGraph) -> String {
  let mut out = String::new();
  out.push_str("digraph rail_plan {\n");
  out.push_str("  rankdir=LR;\n");
  for node in &graph.nodes {
    let shape = match node.kind.as_str() {
      "file" => "box",
      "crate" => "ellipse",
      "surface" => "diamond",
      _ => "oval",
    };
    out.push_str(&format!(
      "  \"{}\" [label=\"{}\", shape={}];\n",
      escape_dot(&node.id),
      escape_dot(&node.label),
      shape
    ));
  }
  for edge in &graph.edges {
    out.push_str(&format!(
      "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
      escape_dot(&edge.from),
      escape_dot(&edge.to),
      escape_dot(&edge.relation)
    ));
  }
  out.push_str("}\n");
  out
}

fn escape_dot(input: &str) -> String {
  input.replace('\\', "\\\\").replace('"', "\\\"")
}

fn write_output(content: &str, output_file: Option<&PathBuf>) -> RailResult<()> {
  match output_file {
    Some(path) => {
      let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| RailError::message(format!("failed to open '{}': {}", path.display(), e)))?;
      writeln!(file, "{}", content)
        .map_err(|e| RailError::message(format!("failed to write '{}': {}", path.display(), e)))?;
      crate::progress!("output: {}", path.display());
      Ok(())
    }
    None => {
      println!("{}", content);
      Ok(())
    }
  }
}
