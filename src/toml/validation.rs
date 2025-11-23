//! TOML validation and safety checks
#![allow(clippy::collapsible_if)]

use crate::error::{RailError, RailResult};
use toml_edit::DocumentMut;

/// Validate TOML string
pub fn validate_toml(content: &str) -> RailResult<()> {
  content
    .parse::<DocumentMut>()
    .map(|_| ())
    .map_err(|e| RailError::message(format!("Invalid TOML: {}", e)))
}

/// Validate workspace Cargo.toml structure
pub fn validate_workspace_manifest(doc: &DocumentMut) -> RailResult<()> {
  if doc.get("workspace").is_none() {
    return Err(RailError::message("Missing [workspace] section"));
  }

  if let Some(workspace) = doc.get("workspace").and_then(|i| i.as_table()) {
    if workspace.get("members").is_none() {
      return Err(RailError::message("Missing [workspace.members] array"));
    }
  }

  Ok(())
}

/// Validate member Cargo.toml structure
pub fn validate_member_manifest(doc: &DocumentMut) -> RailResult<()> {
  if doc.get("package").is_none() {
    return Err(RailError::message("Missing [package] section"));
  }

  if let Some(package) = doc.get("package").and_then(|i| i.as_table()) {
    if package.get("name").is_none() {
      return Err(RailError::message("Missing package name"));
    }
    if package.get("version").is_none() {
      return Err(RailError::message("Missing package version"));
    }
  }

  Ok(())
}

/// TOML Lint Warning
#[derive(Debug)]
pub struct LintWarning {
  /// The warning message
  pub message: String,
  /// Line number if available
  pub line: Option<usize>,
}

/// Check for common TOML errors/issues
pub fn lint_toml(doc: &DocumentMut) -> Vec<LintWarning> {
  let mut warnings = Vec::new();

  // Check for mixed array types (toml_edit handles this mostly, but good to check)
  // Check for empty tables that might be leftovers

  if let Some(deps) = doc.get("dependencies").and_then(|i| i.as_table()) {
    if deps.is_empty() {
      warnings.push(LintWarning {
        message: "Empty [dependencies] section".to_string(),
        line: None,
      });
    }
  }

  warnings
}

/// Verify workspace dependencies are valid
pub fn validate_workspace_deps(deps: &toml_edit::Table) -> RailResult<()> {
  for (name, item) in deps.iter() {
    if !item.is_str() && !item.is_inline_table() && !item.is_table() {
      return Err(RailError::message(format!(
        "Invalid dependency format for '{}': expected string or table",
        name
      )));
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_validate_workspace() {
    let toml = r#"
    [workspace]
    members = ["crates/*"]
    "#;
    let doc = toml.parse::<DocumentMut>().unwrap();
    assert!(validate_workspace_manifest(&doc).is_ok());
  }

  #[test]
  fn test_validate_workspace_missing_members() {
    let toml = r#"
    [workspace]
    "#;
    let doc = toml.parse::<DocumentMut>().unwrap();
    assert!(validate_workspace_manifest(&doc).is_err());
  }

  #[test]
  fn test_validate_member() {
    let toml = r#"
    [package]
    name = "foo"
    version = "0.1.0"
    "#;
    let doc = toml.parse::<DocumentMut>().unwrap();
    assert!(validate_member_manifest(&doc).is_ok());
  }
}
