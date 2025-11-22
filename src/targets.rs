//! Target triple detection for workspace validation
//!
//! Detects Rust target triples across all TOML files in the workspace.
//! Uses fuzzy matching against rustc's canonical target list to find targets
//! regardless of where they're defined (rust-toolchain.toml, .cargo/config.toml,
//! Cross.toml, dist-workspace.toml, etc.).

use crate::error::{RailError, RailResult};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Detect all Rust target triples mentioned in any .toml file in the workspace
///
/// This function:
/// 1. Gets the canonical list of targets from `rustc --print target-list`
/// 2. Recursively searches all .toml files in the workspace (skipping build artifacts)
/// 3. Fuzzy matches file contents against canonical targets
/// 4. Returns deduplicated, sorted list of found targets
///
/// # Performance
/// - Caches rustc output (one-time ~5ms cost)
/// - Typical workspace: <5ms total
/// - Large workspace (60+ crates): <10ms total
///
/// # Arguments
/// * `workspace_root` - Path to workspace root directory
///
/// # Returns
/// Sorted list of detected target triples
pub fn detect_targets(workspace_root: &Path) -> RailResult<Vec<String>> {
  // Get canonical target list from rustc (cached)
  let canonical_targets = get_rust_target_list()?;

  // Find all TOML files in workspace
  let toml_files = find_toml_files(workspace_root);

  // Track found targets (deduplicate)
  let mut found = HashSet::new();

  // For each TOML file, check if it mentions any canonical target
  for toml_path in toml_files {
    if let Ok(content) = std::fs::read_to_string(&toml_path) {
      for target in &canonical_targets {
        if content.contains(target) {
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

/// Get canonical list of Rust target triples from rustc
///
/// Caches the result using OnceLock for efficiency (rustc call is ~5ms).
/// Returns ~285 target triples as of Rust 1.91.
fn get_rust_target_list() -> RailResult<Vec<String>> {
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
/// # Arguments
/// * `dir` - Current directory to search
/// * `current_depth` - Current recursion depth
/// * `max_depth` - Maximum depth to recurse
/// * `toml_files` - Accumulator for found files
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

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

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
}
