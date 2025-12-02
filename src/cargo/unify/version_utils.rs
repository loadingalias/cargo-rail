//! Pure version parsing and comparison utilities
//!
//! These functions are stateless and used across multiple unification components.

use crate::cargo::manifest_analyzer::DepUsage;
use std::collections::HashSet;

/// Check if a version string is an exact pin ("=x.y.z")
pub fn is_exact_pin(version: &str) -> bool {
  version.starts_with('=') && !version.starts_with(">=")
}

/// Extract major version from a declared version string
///
/// Returns the major version number, handling various version formats:
/// - "1.0" -> Some(1)
/// - "^2.3.0" -> Some(2)
/// - ">=0.5" -> Some(0)
pub fn extract_major_version(version: &str) -> Option<u32> {
  let cleaned = strip_version_op(version);
  let parts: Vec<&str> = cleaned.split('.').collect();
  parts.first().and_then(|s| s.parse().ok())
}

/// Check for major version conflicts in declared versions
///
/// Returns the set of unique major versions found. If the set has more than
/// one element, there's a conflict that cannot be safely unified.
pub fn find_major_version_conflicts(usages: &[&DepUsage]) -> HashSet<u32> {
  usages
    .iter()
    .filter_map(|u| u.declared_version.as_ref())
    .filter_map(|v| extract_major_version(v))
    .collect()
}

/// Check if two version requirements are compatible
///
/// This is a heuristic check - it compares the major.minor portions
/// to detect obvious incompatibilities like "0.11" vs "0.13".
pub fn versions_compatible(member_version: &str, workspace_version: &str) -> bool {
  // Strip leading operators for comparison
  let member_ver = strip_version_op(member_version);
  let workspace_ver = strip_version_op(workspace_version);

  // Parse into parts
  let member_parts: Vec<&str> = member_ver.split('.').collect();
  let workspace_parts: Vec<&str> = workspace_ver.split('.').collect();

  // For semver compatibility, major must match (or be 0)
  // For 0.x versions, minor must also match
  if member_parts.is_empty() || workspace_parts.is_empty() {
    return true; // Can't determine, assume compatible
  }

  let member_major = member_parts[0].parse::<u32>().unwrap_or(0);
  let workspace_major = workspace_parts[0].parse::<u32>().unwrap_or(0);

  if member_major != workspace_major {
    return false;
  }

  // For 0.x versions, minor must match
  if member_major == 0 {
    let member_minor = member_parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let workspace_minor = workspace_parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    if member_minor != workspace_minor {
      return false;
    }
  }

  true
}

/// Strip version operators from a version string
pub fn strip_version_op(version: &str) -> &str {
  version
    .trim_start_matches(">=")
    .trim_start_matches("<=")
    .trim_start_matches('>')
    .trim_start_matches('<')
    .trim_start_matches('=')
    .trim_start_matches('^')
    .trim_start_matches('~')
    .trim()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_extract_major_version() {
    // Simple versions
    assert_eq!(extract_major_version("1.0"), Some(1));
    assert_eq!(extract_major_version("2.3.4"), Some(2));
    assert_eq!(extract_major_version("0.99.3"), Some(0));

    // With operators
    assert_eq!(extract_major_version("^1.0"), Some(1));
    assert_eq!(extract_major_version("~2.0"), Some(2));
    assert_eq!(extract_major_version(">=3.0"), Some(3));
    assert_eq!(extract_major_version("=4.0.0"), Some(4));

    // Edge cases
    assert_eq!(extract_major_version(""), None);
    assert_eq!(extract_major_version("invalid"), None);
  }

  #[test]
  fn test_is_exact_pin() {
    assert!(is_exact_pin("=1.0.0"));
    assert!(is_exact_pin("=0.8.0"));
    assert!(!is_exact_pin(">=1.0.0"));
    assert!(!is_exact_pin("^1.0.0"));
    assert!(!is_exact_pin("1.0.0"));
    assert!(!is_exact_pin("~1.0.0"));
  }

  #[test]
  fn test_strip_version_op() {
    assert_eq!(strip_version_op("=1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("^1.0.0"), "1.0.0");
    assert_eq!(strip_version_op(">=1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("~1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("1.0.0"), "1.0.0");
    assert_eq!(strip_version_op("<=2.0"), "2.0");
    assert_eq!(strip_version_op(">0.5"), "0.5");
  }

  #[test]
  fn test_versions_compatible_major_match() {
    // Same major version (>=1.0) should be compatible
    assert!(versions_compatible("1.0", "1.5"));
    assert!(versions_compatible("^1.0", "1.5"));
    assert!(versions_compatible("2.0", "2.3.1"));
  }

  #[test]
  fn test_versions_compatible_major_mismatch() {
    // Different major versions are incompatible
    assert!(!versions_compatible("1.0", "2.0"));
    assert!(!versions_compatible("^2.0", "3.0"));
  }

  #[test]
  fn test_versions_compatible_zero_major() {
    // For 0.x versions, minor must also match
    assert!(versions_compatible("0.11", "0.11.5"));
    assert!(versions_compatible("^0.11", "0.11"));
    assert!(!versions_compatible("0.11", "0.13"));
    assert!(!versions_compatible("^0.8", "0.9"));
  }

  #[test]
  fn test_versions_compatible_exact_pins() {
    // Exact pins with same version are compatible
    assert!(versions_compatible("=1.0.0", "1.0.0"));
    assert!(versions_compatible("=0.8.0", "0.8"));
    // Different versions are incompatible
    assert!(!versions_compatible("=1.0.0", "2.0.0"));
  }
}
