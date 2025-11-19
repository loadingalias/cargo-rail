//! File classification by type and impact
//!
//! Provides hierarchical classification of changed files to determine:
//! - What needs to be rebuilt
//! - What needs to be retested
//! - What can be safely skipped

use std::path::Path;

/// Hierarchical classification of file changes
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
  /// Source code that affects compilation
  Source {
    /// Whether this is a procedural macro crate
    is_proc_macro: bool,
  },

  /// Test code (does not affect downstream crates)
  Test { kind: TestKind },

  /// Examples (compile but don't affect dependencies)
  Example,

  /// Build scripts (affects build process, not code)
  BuildScript,

  /// Configuration files
  Config { kind: ConfigKind },

  /// Documentation only (no rebuild/retest needed)
  Documentation,

  /// Other files (scripts, CI configs, etc)
  Other,
}

/// Types of test files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
  /// Integration tests in tests/
  Integration,
  /// Benchmarks in benches/
  Bench,
}

/// Types of configuration files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
  /// Cargo.toml - affects dependencies
  CargoToml,
  /// Cargo.lock - affects resolution
  CargoLock,
  /// .cargo/config.toml - affects build flags
  CargoConfig,
  /// rust-toolchain.toml - affects compiler
  RustToolchain,
}

/// Classify a file by path
///
/// This is a fast, path-based classification that requires no I/O.
/// Proc-macro detection requires workspace context and is done separately.
pub fn classify_file(path: &Path) -> ChangeKind {
  let path_str = path.to_string_lossy();

  // Most specific patterns first

  // Build scripts
  if path_str.ends_with("build.rs") {
    return ChangeKind::BuildScript;
  }

  // Configuration files
  if let Some(config_kind) = classify_config(&path_str) {
    return ChangeKind::Config { kind: config_kind };
  }

  // Documentation
  if is_documentation(&path_str) {
    return ChangeKind::Documentation;
  }

  // Rust files - context-sensitive
  if path_str.ends_with(".rs") {
    return classify_rust_file(&path_str);
  }

  ChangeKind::Other
}

/// Classify configuration files
fn classify_config(path_str: &str) -> Option<ConfigKind> {
  if path_str.ends_with("Cargo.toml") {
    Some(ConfigKind::CargoToml)
  } else if path_str.ends_with("Cargo.lock") {
    Some(ConfigKind::CargoLock)
  } else if path_str.ends_with(".cargo/config.toml") || path_str.ends_with(".cargo/config") {
    Some(ConfigKind::CargoConfig)
  } else if path_str.ends_with("rust-toolchain.toml") || path_str.ends_with("rust-toolchain") {
    Some(ConfigKind::RustToolchain)
  } else {
    None
  }
}

/// Check if file is documentation
fn is_documentation(path_str: &str) -> bool {
  path_str.ends_with(".md")
    || path_str.ends_with(".txt")
    || path_str.ends_with(".adoc")
    || path_str.ends_with(".rst")
    || path_str.ends_with("LICENSE")
    || path_str.ends_with("README")
}

/// Classify Rust source files by location
fn classify_rust_file(path_str: &str) -> ChangeKind {
  // Examples - checked first as they can be in various locations
  if is_example_file(path_str) {
    return ChangeKind::Example;
  }

  // Tests - integration and benchmarks
  if is_test_file(path_str) {
    let kind = if path_str.contains("/benches/") || path_str.starts_with("benches/") {
      TestKind::Bench
    } else {
      TestKind::Integration
    };
    return ChangeKind::Test { kind };
  }

  // Default: source code
  // Note: is_proc_macro is determined by workspace context, not file path
  ChangeKind::Source { is_proc_macro: false }
}

/// Check if path is an example file
fn is_example_file(path_str: &str) -> bool {
  // Match patterns:
  // - examples/*.rs
  // - crates/*/examples/*.rs
  // - */examples/*.rs (any depth)
  path_str.contains("/examples/") || path_str.starts_with("examples/")
}

/// Check if path is a test file
fn is_test_file(path_str: &str) -> bool {
  // Match patterns:
  // - tests/*.rs
  // - crates/*/tests/*.rs
  // - */tests/*.rs (any depth)
  // - benches/*.rs
  // - */benches/*.rs
  (path_str.contains("/tests/") || path_str.starts_with("tests/"))
    || (path_str.contains("/benches/") || path_str.starts_with("benches/"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_classify_build_script() {
    assert_eq!(classify_file(Path::new("build.rs")), ChangeKind::BuildScript);
    assert_eq!(classify_file(Path::new("crates/foo/build.rs")), ChangeKind::BuildScript);
  }

  #[test]
  fn test_classify_config_files() {
    assert_eq!(
      classify_file(Path::new("Cargo.toml")),
      ChangeKind::Config {
        kind: ConfigKind::CargoToml
      }
    );
    assert_eq!(
      classify_file(Path::new("Cargo.lock")),
      ChangeKind::Config {
        kind: ConfigKind::CargoLock
      }
    );
    assert_eq!(
      classify_file(Path::new(".cargo/config.toml")),
      ChangeKind::Config {
        kind: ConfigKind::CargoConfig
      }
    );
    assert_eq!(
      classify_file(Path::new("rust-toolchain.toml")),
      ChangeKind::Config {
        kind: ConfigKind::RustToolchain
      }
    );
  }

  #[test]
  fn test_classify_documentation() {
    assert_eq!(classify_file(Path::new("README.md")), ChangeKind::Documentation);
    assert_eq!(classify_file(Path::new("docs/guide.md")), ChangeKind::Documentation);
    assert_eq!(classify_file(Path::new("LICENSE")), ChangeKind::Documentation);
  }

  #[test]
  fn test_classify_examples() {
    assert_eq!(classify_file(Path::new("examples/demo.rs")), ChangeKind::Example);
    assert_eq!(
      classify_file(Path::new("crates/foo/examples/demo.rs")),
      ChangeKind::Example
    );
    assert_eq!(
      classify_file(Path::new("foo/bar/examples/demo.rs")),
      ChangeKind::Example
    );
  }

  #[test]
  fn test_classify_integration_tests() {
    let result = classify_file(Path::new("tests/integration.rs"));
    assert!(matches!(
      result,
      ChangeKind::Test {
        kind: TestKind::Integration
      }
    ));

    let result = classify_file(Path::new("crates/foo/tests/integration.rs"));
    assert!(matches!(
      result,
      ChangeKind::Test {
        kind: TestKind::Integration
      }
    ));
  }

  #[test]
  fn test_classify_benchmarks() {
    let result = classify_file(Path::new("benches/benchmark.rs"));
    assert!(matches!(result, ChangeKind::Test { kind: TestKind::Bench }));

    let result = classify_file(Path::new("crates/foo/benches/benchmark.rs"));
    assert!(matches!(result, ChangeKind::Test { kind: TestKind::Bench }));
  }

  #[test]
  fn test_classify_source_files() {
    let result = classify_file(Path::new("src/lib.rs"));
    assert!(matches!(result, ChangeKind::Source { .. }));

    let result = classify_file(Path::new("crates/foo/src/main.rs"));
    assert!(matches!(result, ChangeKind::Source { .. }));
  }

  // Note: test_change_kind_requires_rebuild() and test_change_kind_requires_retest() removed.
  // Those methods were implementation details. The actual logic is tested via
  // ChangeCategories in workspace::change_analyzer tests.
}
