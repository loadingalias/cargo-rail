//! Heuristically detect Rust target triples in workspace configuration files.

use crate::error::{RailError, RailResult};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Detect all Rust target triples mentioned in any .toml file in the workspace
///
/// Scans workspace TOML and GitHub workflow files, matches against rustc's
/// canonical target list, and returns a deduplicated sorted result.
///
pub fn detect_targets(workspace_root: &Path) -> RailResult<Vec<String>> {
  detect_targets_excluding(workspace_root, &[])
}

/// Detect targets excluding specific files
///
/// Same as [`detect_targets`] but skips paths in `exclude`.
/// Useful when a caller needs to avoid circular self-detection.
pub fn detect_targets_excluding(workspace_root: &Path, exclude: &[&Path]) -> RailResult<Vec<String>> {
  // Get canonical target list from rustc (cached)
  let canonical_targets = get_rust_target_list()?;

  // Find all config files (TOML + GitHub workflow YAML)
  let mut config_files = find_toml_files(workspace_root);
  config_files.extend(find_github_workflow_files(workspace_root));

  // Track found targets (deduplicate)
  let mut found = HashSet::new();

  // For each config file, check if it mentions any canonical target
  for file_path in config_files {
    // Skip excluded files
    if exclude.iter().any(|e| file_path == *e) {
      continue;
    }

    if let Ok(content) = std::fs::read_to_string(&file_path) {
      for target in &canonical_targets {
        if contains_target_match(&content, target) {
          found.insert(target.clone());
        }
      }
    }
  }

  // Return sorted for deterministic output
  let mut targets: Vec<_> = found.into_iter().collect();
  targets.sort();

  Ok(targets)
}

/// Validate that the given targets are valid Rust target triples
///
/// Returns an error if any of the targets are not in rustc's canonical target list.
pub fn validate_targets(targets: &[String]) -> RailResult<()> {
  if targets.is_empty() {
    return Ok(());
  }

  let canonical_targets = get_rust_target_list()?;
  let mut invalid = Vec::new();

  for target in targets {
    if !canonical_targets.contains(target) {
      invalid.push(target.clone());
    }
  }

  if !invalid.is_empty() {
    return Err(RailError::with_help(
      format!("invalid target triple(s) in config: {}", invalid.join(", ")),
      "check spelling against `rustc --print target-list`. Common targets:\n  \
         - x86_64-unknown-linux-gnu\n  \
         - aarch64-apple-darwin\n  \
         - x86_64-pc-windows-msvc\n  \
         - wasm32-unknown-unknown"
        .to_string(),
    ));
  }

  Ok(())
}

/// Get canonical list of Rust target triples from rustc
///
/// Caches the result using OnceLock for efficiency (rustc call is ~5ms).
/// Returns ~320 target triples as of Rust 1.95.
pub fn get_rust_target_list() -> RailResult<Vec<String>> {
  static TARGETS: OnceLock<Option<Vec<String>>> = OnceLock::new();

  let targets = TARGETS.get_or_init(|| {
    // Run rustc --print target-list
    let output = Command::new("rustc").args(["--print", "target-list"]).output().ok()?;

    if !output.status.success() {
      return None;
    }

    // Parse output into Vec<String>
    let targets: Vec<String> = String::from_utf8_lossy(&output.stdout)
      .lines()
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .map(String::from)
      .collect();

    Some(targets)
  });

  targets
    .clone()
    .ok_or_else(|| RailError::message("Failed to get target list from rustc. Ensure rustc is installed and in PATH."))
}

/// Check if content contains a target triple as a complete word/token
///
/// This avoids false positives like matching "thumbv7em-none-eabi" when the
/// file only contains "thumbv7em-none-eabihf". Target triples in config files
/// are typically surrounded by:
/// - Whitespace
/// - Quotes (single or double)
/// - Brackets
/// - Commas
/// - Newlines
/// - Start/end of string
fn contains_target_match(content: &str, target: &str) -> bool {
  // Find all occurrences of the target string
  let mut start = 0;
  while let Some(pos) = content[start..].find(target) {
    let absolute_pos = start + pos;
    let end_pos = absolute_pos + target.len();

    // Check character before (if any)
    let char_before = if absolute_pos > 0 {
      content[..absolute_pos].chars().last()
    } else {
      None
    };

    // Check character after (if any)
    let char_after = content[end_pos..].chars().next();

    // A target triple character is: alphanumeric, hyphen, or underscore
    // Note: Some targets have dots (e.g., "thumbv8m.main-none-eabi") but treating
    // dots as target chars breaks TOML table detection like [target.x86_64-linux-gnu].
    // Since dotted targets are rare (only thumbv8m.* variants) and must be quoted
    // in TOML anyway, we treat dots as boundaries. This works because:
    // - In arrays: "thumbv8m.main-none-eabi" - the target is quoted, boundaries are quotes
    // - In tables: [target."thumbv8m.main-none-eabi"] - must be quoted due to the dot
    let is_target_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';

    // Valid boundary: start of string, or non-target character
    let valid_before = char_before.is_none_or(|c| !is_target_char(c));
    // Valid boundary: end of string, or non-target character
    let valid_after = char_after.is_none_or(|c| !is_target_char(c));

    if valid_before && valid_after {
      return true;
    }

    // Move past this match to find next occurrence
    start = absolute_pos + 1;
  }

  false
}

/// Recursively find all .toml files in workspace
///
/// - Searches up to depth 3 (avoids excessive traversal)
/// - Skips: target/, .git/, node_modules/
/// - Returns: absolute paths to .toml files
fn find_toml_files(workspace_root: &Path) -> Vec<PathBuf> {
  let mut toml_files = Vec::new();
  find_toml_files_recursive(workspace_root, 0, 3, &mut toml_files);
  toml_files
}

/// Recursive helper for find_toml_files
///
/// Walks the directory tree depth-first and appends discovered `.toml` files
/// to the provided accumulator.
fn find_toml_files_recursive(dir: &Path, current_depth: usize, max_depth: usize, toml_files: &mut Vec<PathBuf>) {
  // Stop at max depth
  if current_depth > max_depth {
    return;
  }

  // Read directory entries
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };

  for entry in entries.flatten() {
    let path = entry.path();

    // Skip known build/cache directories
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
      && matches!(name, "target" | ".git" | "node_modules" | ".cargo-rail")
    {
      continue;
    }

    if path.is_file() {
      // Check if it's a .toml file
      if path.extension() == Some(OsStr::new("toml")) {
        toml_files.push(path);
      }
    } else if path.is_dir() {
      // Recurse into subdirectories
      find_toml_files_recursive(&path, current_depth + 1, max_depth, toml_files);
    }
  }
}

/// Find GitHub workflow files (.yml and .yaml) in .github/workflows/
///
/// Many monorepos define their cross-compilation targets in GitHub Actions
/// workflow files rather than TOML configuration files. This function
/// finds all workflow files for target detection.
fn find_github_workflow_files(workspace_root: &Path) -> Vec<PathBuf> {
  let workflows_dir = workspace_root.join(".github").join("workflows");

  // Return empty if .github/workflows doesn't exist
  if !workflows_dir.is_dir() {
    return Vec::new();
  }

  let Ok(entries) = std::fs::read_dir(&workflows_dir) else {
    return Vec::new();
  };

  entries
    .flatten()
    .filter_map(|entry| {
      let path = entry.path();
      if path.is_file() {
        let ext = path.extension().and_then(|e| e.to_str());
        if matches!(ext, Some("yml") | Some("yaml")) {
          return Some(path);
        }
      }
      None
    })
    .collect()
}

/// Check if a workspace has GitHub workflow files
///
/// Used to provide helpful hints during `cargo rail init` when no targets
/// are detected from TOML files but workflows exist.
pub fn has_github_workflows(workspace_root: &Path) -> bool {
  let workflows_dir = workspace_root.join(".github").join("workflows");
  if !workflows_dir.is_dir() {
    return false;
  }

  // Check if there's at least one .yml or .yaml file
  if let Ok(entries) = std::fs::read_dir(&workflows_dir) {
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_file() {
        let ext = path.extension().and_then(|e| e.to_str());
        if matches!(ext, Some("yml") | Some("yaml")) {
          return true;
        }
      }
    }
  }

  false
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

  #[test]
  fn test_validate_targets_valid() {
    let valid_targets = vec![
      "x86_64-unknown-linux-gnu".to_string(),
      "wasm32-unknown-unknown".to_string(),
    ];
    assert!(validate_targets(&valid_targets).is_ok());
  }

  #[test]
  fn test_validate_targets_invalid() {
    let invalid_targets = vec!["invalid-target-triple".to_string()];
    let result = validate_targets(&invalid_targets);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("invalid-target-triple"));
  }

  #[test]
  fn test_validate_targets_mixed() {
    let mixed_targets = vec!["x86_64-unknown-linux-gnu".to_string(), "not-a-real-target".to_string()];
    let result = validate_targets(&mixed_targets);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not-a-real-target"));
    // Should NOT mention the valid target
    assert!(!err.contains("x86_64-unknown-linux-gnu"));
  }

  #[test]
  fn test_validate_targets_empty() {
    let empty: Vec<String> = vec![];
    assert!(validate_targets(&empty).is_ok());
  }

  #[test]
  fn test_get_rust_target_list() {
    let targets = get_rust_target_list().expect("rustc should be available");

    // Should have many targets (current rustc has ~285)
    assert!(targets.len() > 200, "Expected >200 targets, got {}", targets.len());

    // Should contain common targets
    assert!(targets.contains(&"x86_64-unknown-linux-gnu".to_string()));
    assert!(targets.contains(&"aarch64-apple-darwin".to_string()));
    assert!(targets.contains(&"wasm32-unknown-unknown".to_string()));
  }

  #[test]
  fn test_detect_targets_empty_workspace() {
    let temp = TempDir::new().unwrap();
    let targets = detect_targets(temp.path()).unwrap();
    assert_eq!(targets, Vec::<String>::new());
  }

  #[test]
  fn test_detect_targets_rust_toolchain() {
    let temp = TempDir::new().unwrap();

    // Create rust-toolchain.toml with targets
    fs::write(
      temp.path().join("rust-toolchain.toml"),
      r#"
[toolchain]
channel = "stable"
targets = ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();
    assert!(targets.contains(&"x86_64-unknown-linux-gnu".to_string()));
    assert!(targets.contains(&"wasm32-unknown-unknown".to_string()));
  }

  #[test]
  fn test_detect_targets_cargo_config() {
    let temp = TempDir::new().unwrap();
    let cargo_dir = temp.path().join(".cargo");
    fs::create_dir(&cargo_dir).unwrap();

    // Create .cargo/config.toml with target sections
    fs::write(
      cargo_dir.join("config.toml"),
      r#"
[target.x86_64-pc-windows-msvc]
linker = "lld-link.exe"

[target.aarch64-apple-darwin]
rustflags = ["-C", "link-arg=-fuse-ld=lld"]
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();
    assert!(targets.contains(&"x86_64-pc-windows-msvc".to_string()));
    assert!(targets.contains(&"aarch64-apple-darwin".to_string()));
  }

  #[test]
  fn test_detect_targets_cross_toml() {
    let temp = TempDir::new().unwrap();

    // Create Cross.toml (used by cross-rs)
    fs::write(
      temp.path().join("Cross.toml"),
      r#"
[target.aarch64-unknown-linux-gnu]
pre-build = ["apt-get update"]

[target.armv7-unknown-linux-gnueabihf]
image = "custom-image"
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();
    assert!(targets.contains(&"aarch64-unknown-linux-gnu".to_string()));
    assert!(targets.contains(&"armv7-unknown-linux-gnueabihf".to_string()));
  }

  #[test]
  fn test_detect_targets_dist_workspace() {
    let temp = TempDir::new().unwrap();

    // Create dist-workspace.toml (used by cargo-dist)
    fs::write(
      temp.path().join("dist-workspace.toml"),
      r#"
[dist]
targets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc"
]
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();
    assert!(targets.contains(&"x86_64-unknown-linux-gnu".to_string()));
    assert!(targets.contains(&"aarch64-apple-darwin".to_string()));
    assert!(targets.contains(&"x86_64-pc-windows-msvc".to_string()));
  }

  #[test]
  fn test_detect_targets_deduplication() {
    let temp = TempDir::new().unwrap();

    // Create multiple files mentioning the same targets
    fs::write(
      temp.path().join("rust-toolchain.toml"),
      r#"
[toolchain]
targets = ["x86_64-unknown-linux-gnu"]
"#,
    )
    .unwrap();

    let cargo_dir = temp.path().join(".cargo");
    fs::create_dir(&cargo_dir).unwrap();
    fs::write(
      cargo_dir.join("config.toml"),
      r#"
[target.x86_64-unknown-linux-gnu]
linker = "clang"
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();
    // Should only appear once despite being in 2 files
    assert_eq!(targets.iter().filter(|t| *t == "x86_64-unknown-linux-gnu").count(), 1);
  }

  #[test]
  fn test_find_toml_files_skips_target_dir() {
    let temp = TempDir::new().unwrap();

    // Create a target directory with a TOML file
    let target_dir = temp.path().join("target");
    fs::create_dir(&target_dir).unwrap();
    fs::write(target_dir.join("should-skip.toml"), "# ignored").unwrap();

    // Create a valid TOML file at root
    fs::write(temp.path().join("valid.toml"), "# found").unwrap();

    let toml_files = find_toml_files(temp.path());

    // Should find valid.toml but not target/should-skip.toml
    assert_eq!(toml_files.len(), 1);
    assert!(toml_files[0].ends_with("valid.toml"));
  }

  // GitHub Workflow YAML Tests

  #[test]
  fn test_find_github_workflow_files_empty() {
    let temp = TempDir::new().unwrap();
    let files = find_github_workflow_files(temp.path());
    assert!(files.is_empty());
  }

  #[test]
  fn test_find_github_workflow_files_finds_yml() {
    let temp = TempDir::new().unwrap();

    // Create .github/workflows directory
    let workflows_dir = temp.path().join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    // Create workflow files
    fs::write(workflows_dir.join("ci.yml"), "name: CI").unwrap();
    fs::write(workflows_dir.join("release.yaml"), "name: Release").unwrap();

    let files = find_github_workflow_files(temp.path());
    assert_eq!(files.len(), 2);
  }

  #[test]
  fn test_find_github_workflow_files_ignores_non_yaml() {
    let temp = TempDir::new().unwrap();

    // Create .github/workflows directory
    let workflows_dir = temp.path().join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    // Create mixed files
    fs::write(workflows_dir.join("ci.yml"), "name: CI").unwrap();
    fs::write(workflows_dir.join("README.md"), "# Workflows").unwrap();
    fs::write(workflows_dir.join("config.json"), "{}").unwrap();

    let files = find_github_workflow_files(temp.path());
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("ci.yml"));
  }

  #[test]
  fn test_detect_targets_from_github_workflows() {
    let temp = TempDir::new().unwrap();

    // Create .github/workflows/ci.yml with target matrix
    let workflows_dir = temp.path().join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    fs::write(
      workflows_dir.join("ci.yml"),
      r#"
name: CI
jobs:
  build:
    strategy:
      matrix:
        target:
          - x86_64-unknown-linux-gnu
          - aarch64-apple-darwin
          - x86_64-pc-windows-msvc
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();
    assert!(targets.contains(&"x86_64-unknown-linux-gnu".to_string()));
    assert!(targets.contains(&"aarch64-apple-darwin".to_string()));
    assert!(targets.contains(&"x86_64-pc-windows-msvc".to_string()));
  }

  #[test]
  fn test_detect_targets_combined_toml_and_yaml() {
    let temp = TempDir::new().unwrap();

    // Create rust-toolchain.toml with one target
    fs::write(
      temp.path().join("rust-toolchain.toml"),
      r#"
[toolchain]
targets = ["wasm32-unknown-unknown"]
"#,
    )
    .unwrap();

    // Create .github/workflows/ci.yml with different targets
    let workflows_dir = temp.path().join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    fs::write(
      workflows_dir.join("ci.yml"),
      r#"
name: CI
jobs:
  build:
    strategy:
      matrix:
        target: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();

    // Should find targets from both sources
    assert!(targets.contains(&"wasm32-unknown-unknown".to_string()));
    assert!(targets.contains(&"x86_64-unknown-linux-gnu".to_string()));
    assert!(targets.contains(&"aarch64-apple-darwin".to_string()));
    assert_eq!(targets.len(), 3);
  }

  #[test]
  fn test_has_github_workflows_true() {
    let temp = TempDir::new().unwrap();

    // Create .github/workflows with a workflow file
    let workflows_dir = temp.path().join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();
    fs::write(workflows_dir.join("ci.yml"), "name: CI").unwrap();

    assert!(has_github_workflows(temp.path()));
  }

  #[test]
  fn test_has_github_workflows_false_no_dir() {
    let temp = TempDir::new().unwrap();
    assert!(!has_github_workflows(temp.path()));
  }

  #[test]
  fn test_has_github_workflows_false_empty_dir() {
    let temp = TempDir::new().unwrap();

    // Create empty .github/workflows directory
    let workflows_dir = temp.path().join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();

    assert!(!has_github_workflows(temp.path()));
  }

  #[test]
  fn test_has_github_workflows_false_no_yaml() {
    let temp = TempDir::new().unwrap();

    // Create .github/workflows with non-YAML files only
    let workflows_dir = temp.path().join(".github").join("workflows");
    fs::create_dir_all(&workflows_dir).unwrap();
    fs::write(workflows_dir.join("README.md"), "# Workflows").unwrap();

    assert!(!has_github_workflows(temp.path()));
  }

  // Word Boundary Matching Tests

  #[test]
  fn test_contains_target_match_exact() {
    assert!(contains_target_match("thumbv7em-none-eabihf", "thumbv7em-none-eabihf"));
  }

  #[test]
  fn test_contains_target_match_quoted() {
    assert!(contains_target_match(
      r#""thumbv7em-none-eabihf""#,
      "thumbv7em-none-eabihf"
    ));
    assert!(contains_target_match(
      r#"'thumbv7em-none-eabihf'"#,
      "thumbv7em-none-eabihf"
    ));
  }

  #[test]
  fn test_contains_target_match_in_array() {
    let content = r#"targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]"#;
    assert!(contains_target_match(content, "x86_64-unknown-linux-gnu"));
    assert!(contains_target_match(content, "aarch64-apple-darwin"));
  }

  #[test]
  fn test_contains_target_match_in_table_key() {
    let content = r#"[target.thumbv7em-none-eabihf]
linker = "arm-none-eabi-gcc""#;
    assert!(contains_target_match(content, "thumbv7em-none-eabihf"));
  }

  #[test]
  fn test_contains_target_match_rejects_substring() {
    // This is the critical test - should NOT match shorter target when only longer exists
    let content = r#"targets = ["thumbv7em-none-eabihf"]"#;

    // Should match the actual target
    assert!(contains_target_match(content, "thumbv7em-none-eabihf"));

    // Should NOT match the shorter substring target
    assert!(
      !contains_target_match(content, "thumbv7em-none-eabi"),
      "Should not match 'thumbv7em-none-eabi' when file only contains 'thumbv7em-none-eabihf'"
    );
  }

  #[test]
  fn test_contains_target_match_rejects_all_substring_cases() {
    // Test several known substring cases from rustc target list
    let test_cases = [
      ("aarch64-apple-ios-sim", "aarch64-apple-ios"),
      ("arm-unknown-linux-gnueabihf", "arm-unknown-linux-gnueabi"),
      ("x86_64-unknown-linux-gnux32", "x86_64-unknown-linux-gnu"),
      ("wasm32-wasip1-threads", "wasm32-wasip1"),
      ("x86_64-pc-windows-gnullvm", "x86_64-pc-windows-gnu"),
    ];

    for (full_target, substring_target) in test_cases {
      let content = format!(r#"targets = ["{}"]"#, full_target);
      assert!(
        contains_target_match(&content, full_target),
        "Should match full target: {}",
        full_target
      );
      assert!(
        !contains_target_match(&content, substring_target),
        "Should NOT match substring '{}' when file only contains '{}'",
        substring_target,
        full_target
      );
    }
  }

  #[test]
  fn test_contains_target_match_allows_both_when_both_present() {
    // When both the short and long target are in the file, both should match
    let content = r#"targets = ["thumbv7em-none-eabi", "thumbv7em-none-eabihf"]"#;

    assert!(contains_target_match(content, "thumbv7em-none-eabi"));
    assert!(contains_target_match(content, "thumbv7em-none-eabihf"));
  }

  #[test]
  fn test_contains_target_match_yaml_format() {
    let content = r#"
matrix:
  target:
    - x86_64-unknown-linux-gnu
    - aarch64-apple-darwin
"#;
    assert!(contains_target_match(content, "x86_64-unknown-linux-gnu"));
    assert!(contains_target_match(content, "aarch64-apple-darwin"));
    // Should not false positive on similar targets
    assert!(!contains_target_match(content, "x86_64-unknown-linux-gnux32"));
  }

  #[test]
  fn test_contains_target_match_with_dots() {
    // Targets like thumbv8m.main-none-eabi have dots in them.
    // In TOML, these MUST be quoted because dots are path separators.
    // So we test the quoted form which is what users would actually write.
    let content = r#"[target."thumbv8m.main-none-eabihf"]"#;
    assert!(contains_target_match(content, "thumbv8m.main-none-eabihf"));
    assert!(!contains_target_match(content, "thumbv8m.main-none-eabi"));

    // Also test in array form
    let content2 = r#"targets = ["thumbv8m.main-none-eabihf"]"#;
    assert!(contains_target_match(content2, "thumbv8m.main-none-eabihf"));
    assert!(!contains_target_match(content2, "thumbv8m.main-none-eabi"));
  }

  #[test]
  fn test_detect_targets_no_false_positives() {
    let temp = TempDir::new().unwrap();

    // Create a file with ONLY the longer target
    fs::write(
      temp.path().join("rust-toolchain.toml"),
      r#"
[toolchain]
channel = "stable"
targets = ["thumbv7em-none-eabihf"]
"#,
    )
    .unwrap();

    let targets = detect_targets(temp.path()).unwrap();

    // Should find the actual target
    assert!(
      targets.contains(&"thumbv7em-none-eabihf".to_string()),
      "Should detect thumbv7em-none-eabihf"
    );

    // Should NOT find the substring target
    assert!(
      !targets.contains(&"thumbv7em-none-eabi".to_string()),
      "Should NOT detect thumbv7em-none-eabi (false positive)"
    );
  }
}
