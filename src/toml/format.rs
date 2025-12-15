//! TOML formatting utilities
//!
//! Provides consistent, opinionated formatting for all TOML output.
//! Uses template strings + toml_edit parsing (hybrid approach).

use crate::error::{RailError, RailResult};
use toml_edit::DocumentMut;

/// Formatting rules and utilities
#[derive(Debug, Clone)]
pub struct TomlFormatter {
  /// Number of items before breaking array into multiple lines
  pub inline_array_threshold: usize,
  /// Number of features before breaking into multiple lines
  pub inline_feature_threshold: usize,
  /// Indentation string (usually 2 spaces)
  pub indent: &'static str,
  /// Whether to sort dependencies alphabetically (true) or preserve order (false)
  pub sort_dependencies: bool,
}

impl Default for TomlFormatter {
  fn default() -> Self {
    Self {
      inline_array_threshold: 4,
      inline_feature_threshold: 10,
      indent: "  ",
      sort_dependencies: true,
    }
  }
}

impl TomlFormatter {
  /// Create a new formatter with default settings
  pub fn new() -> Self {
    Self::default()
  }

  /// Format string array with optional grouping
  pub fn array_string(&self, items: &[String], groups: Option<Vec<Group>>) -> String {
    if items.is_empty() {
      return "[]".to_string();
    }

    // If we have groups, always use multiline
    if let Some(groups) = groups {
      let mut output = String::from("[\n");
      for group in groups {
        if !group.items.is_empty() {
          output.push_str(&format!("{}# {}\n", self.indent, group.name));
          for item in &group.items {
            output.push_str(&format!("{}\"{}\",\n", self.indent, item));
          }
        }
      }
      output.push(']');
      return output;
    }

    // Check if we should inline
    if items.len() <= self.inline_array_threshold {
      let content = items
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
      return format!("[{}]", content);
    }

    // Multiline default
    let mut output = String::from("[\n");
    for item in items {
      output.push_str(&format!("{}\"{}\",\n", self.indent, item));
    }
    output.push(']');
    output
  }

  /// Format features array (for Cargo.toml dependencies)
  pub fn array_features(&self, features: &[String]) -> String {
    if features.is_empty() {
      return "[]".to_string();
    }

    if features.len() > self.inline_feature_threshold {
      let mut output = String::from("[\n");
      for feature in features {
        output.push_str(&format!("{}\"{}\",\n", self.indent, feature));
      }
      output.push(']');
      output
    } else {
      let content = features
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
      format!("[{}]", content)
    }
  }

  /// Format target array with tier grouping
  pub fn array_targets(&self, targets: &[String]) -> String {
    let groups = group_targets(targets);

    // Convert to generic groups
    let generic_groups = groups.into_iter().map(|(name, items)| Group { name, items }).collect();

    self.array_string(targets, Some(generic_groups))
  }

  /// Format array without grouping (simple multiline for readability)
  pub fn array_simple(&self, items: &[String]) -> String {
    if items.is_empty() {
      return "[]".to_string();
    }
    if items.len() <= self.inline_array_threshold {
      let content = items
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
      return format!("[{}]", content);
    }
    let mut output = String::from("[\n");
    for item in items {
      output.push_str(&format!("{}\"{}\",\n", self.indent, item));
    }
    output.push(']');
    output
  }

  /// Format section header with box comment
  pub fn section_header(&self, title: &str, description: &str) -> String {
    format!("# {}\n# {}\n[{}]\n", title.to_uppercase(), description, title)
  }

  /// Format inline table
  pub fn inline_table(&self, pairs: &[(String, TomlValue)]) -> String {
    if pairs.is_empty() {
      return "{}".to_string();
    }

    let content = pairs
      .iter()
      .map(|(k, v)| format!("{} = {}", k, v))
      .collect::<Vec<_>>()
      .join(", ");

    format!("{{ {} }}", content)
  }

  /// Validate TOML string parses correctly
  pub fn validate(&self, toml: &str) -> RailResult<()> {
    toml
      .parse::<DocumentMut>()
      .map(|_| ())
      .map_err(|e| RailError::message(format!("Invalid TOML generated: {}", e)))
  }

  /// Format a Cargo.toml document in-place
  ///
  /// Applies standard formatting rules:
  /// 1. Sorts dependencies (if dependency_sort is Alphabetical)
  /// 2. Standardizes table format (inline vs block)
  pub fn format_manifest(&self, doc: &mut DocumentMut) -> RailResult<()> {
    // 1. Sort dependencies (only if configured to sort alphabetically)
    self.sort_deps(doc);

    // 2. Standardize table formatting (inline vs block)
    self.standardize_tables(doc);

    Ok(())
  }

  /// Sort dependencies in all dependency sections
  ///
  /// Only sorts if `sort_dependencies` is true.
  /// When false, existing order is maintained.
  fn sort_deps(&self, doc: &mut DocumentMut) {
    // Skip sorting if configured to preserve existing order
    if !self.sort_dependencies {
      return;
    }

    let sections = [
      "dependencies",
      "dev-dependencies",
      "build-dependencies",
      "workspace.dependencies",
    ];

    for section in sections {
      if let Some(table) = self.get_table_mut(doc, section) {
        table.sort_values();
      }
    }

    // Also handle target-specific dependencies
    // [target.'cfg(...)'.dependencies]
    if let Some(target_table) = doc.get_mut("target").and_then(|t| t.as_table_mut()) {
      for (_, cfg_item) in target_table.iter_mut() {
        if let Some(cfg_table) = cfg_item.as_table_mut() {
          for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(deps) = cfg_table.get_mut(section).and_then(|d| d.as_table_mut()) {
              deps.sort_values();
            }
          }
        }
      }
    }
  }

  /// Standardize table formatting
  /// - Simple dependencies (version only or path only) -> Inline
  /// - Complex dependencies -> Inline if possible
  fn standardize_tables(&self, doc: &mut DocumentMut) {
    let sections = [
      "dependencies",
      "dev-dependencies",
      "build-dependencies",
      "workspace.dependencies",
    ];

    for section in sections {
      if let Some(table) = self.get_table_mut(doc, section) {
        for (_, item) in table.iter_mut() {
          if let Some(inline) = item.as_inline_table_mut() {
            // Ensure it stays inline
            inline.fmt();
          } else if let Some(t) = item.as_table_mut() {
            // Convert standard table to inline if it's a dependency
            // But only if it doesn't have sub-tables (which deps shouldn't have usually)
            t.set_implicit(true);
          }
        }
      }
    }
  }

  /// Helper to get a mutable table reference from a dotted path
  fn get_table_mut<'a>(&self, doc: &'a mut DocumentMut, path: &str) -> Option<&'a mut toml_edit::Table> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = doc.as_item_mut();

    for part in parts {
      if let Some(table) = current.as_table_mut() {
        if let Some(next) = table.get_mut(part) {
          current = next;
        } else {
          return None;
        }
      } else {
        return None;
      }
    }

    current.as_table_mut()
  }
}

/// Target tier classification
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetTier {
  /// Tier 1: Guaranteed to work
  Tier1,
  /// Tier 2: Guaranteed to build
  Tier2,
  /// Tier 3: No guarantees
  Tier3,
  /// Other/Unknown
  Other,
}

/// Classify a target triple into a tier
pub fn classify_target_tier(target: &str) -> TargetTier {
  match target {
    "x86_64-unknown-linux-gnu"
    | "aarch64-unknown-linux-gnu"
    | "x86_64-pc-windows-msvc"
    | "x86_64-apple-darwin"
    | "aarch64-apple-darwin" => TargetTier::Tier1,
    t if t.contains("musl") || t.contains("wasm") => TargetTier::Tier2,
    _ => TargetTier::Other,
  }
}

/// Group targets by tier
pub fn group_targets(targets: &[String]) -> Vec<(String, Vec<String>)> {
  let mut tier1 = Vec::new();
  let mut tier2 = Vec::new();
  let mut other = Vec::new();

  for target in targets {
    match classify_target_tier(target) {
      TargetTier::Tier1 => tier1.push(target.clone()),
      TargetTier::Tier2 => tier2.push(target.clone()),
      _ => other.push(target.clone()),
    }
  }

  tier1.sort();
  tier2.sort();
  other.sort();

  let mut groups = Vec::new();
  if !tier1.is_empty() {
    groups.push(("Tier 1 (Guaranteed)".to_string(), tier1));
  }
  if !tier2.is_empty() {
    groups.push(("Tier 2 (Common)".to_string(), tier2));
  }
  if !other.is_empty() {
    groups.push(("Other".to_string(), other));
  }

  groups
}

/// Value types for inline tables
#[derive(Debug, Clone)]
pub enum TomlValue {
  /// String value
  String(String),
  /// Boolean value
  Bool(bool),
  /// Integer value
  Integer(i64),
  /// Array of strings
  Array(Vec<String>),
}

impl std::fmt::Display for TomlValue {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TomlValue::String(s) => write!(f, "\"{}\"", s),
      TomlValue::Bool(b) => write!(f, "{}", b),
      TomlValue::Integer(i) => write!(f, "{}", i),
      TomlValue::Array(arr) => {
        let content = arr.iter().map(|s| format!("\"{}\"", s)).collect::<Vec<_>>().join(", ");
        write!(f, "[{}]", content)
      }
    }
  }
}

/// A named group of items for array formatting
#[derive(Debug, Clone)]
pub struct Group {
  /// Name of the group (displayed in comment)
  pub name: String,
  /// Items in the group
  pub items: Vec<String>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_inline_array() {
    let formatter = TomlFormatter::new();
    let items = vec!["a".to_string(), "b".to_string()];
    assert_eq!(formatter.array_string(&items, None), "[\"a\", \"b\"]");
  }

  #[test]
  fn test_multiline_array() {
    let formatter = TomlFormatter::new();
    let items = vec![
      "a".to_string(),
      "b".to_string(),
      "c".to_string(),
      "d".to_string(),
      "e".to_string(),
    ];
    let output = formatter.array_string(&items, None);
    assert!(output.contains("\n"));
    assert!(output.contains("  \"a\","));
  }

  #[test]
  fn test_grouped_targets() {
    let formatter = TomlFormatter::new();
    let targets = vec![
      "x86_64-unknown-linux-gnu".to_string(),
      "wasm32-unknown-unknown".to_string(),
    ];
    let output = formatter.array_targets(&targets);
    assert!(output.contains("# Tier 1"));
    assert!(output.contains("# Tier 2"));
  }

  #[test]
  fn test_inline_table() {
    let formatter = TomlFormatter::new();
    let pairs = vec![
      ("version".to_string(), TomlValue::String("1.0".to_string())),
      ("features".to_string(), TomlValue::Array(vec!["a".to_string()])),
    ];
    let output = formatter.inline_table(&pairs);
    assert_eq!(output, "{ version = \"1.0\", features = [\"a\"] }");
  }
}
