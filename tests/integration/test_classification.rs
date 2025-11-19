//! Integration tests for file classification improvements
//!
//! These tests verify that the new classification system properly detects:
//! - Examples in any location
//! - Cargo config files
//! - Rust toolchain files
//! - And maintains backward compatibility

use std::path::Path;

// Re-export for easy testing
use cargo_rail::change_detection::classify::{ChangeKind, ConfigKind, TestKind, classify_file};

#[test]
fn test_examples_detection_at_workspace_root() {
  let path = Path::new("examples/demo.rs");
  let kind = classify_file(path);

  assert!(
    matches!(kind, ChangeKind::Example),
    "examples/demo.rs should be classified as Example, got: {:?}",
    kind
  );
}

#[test]
fn test_examples_detection_in_crate() {
  let path = Path::new("crates/my-crate/examples/demo.rs");
  let kind = classify_file(path);

  assert!(
    matches!(kind, ChangeKind::Example),
    "crates/*/examples/*.rs should be classified as Example, got: {:?}",
    kind
  );
}

#[test]
fn test_examples_detection_nested() {
  let path = Path::new("foo/bar/baz/examples/demo.rs");
  let kind = classify_file(path);

  assert!(
    matches!(kind, ChangeKind::Example),
    "*/examples/*.rs at any depth should be classified as Example, got: {:?}",
    kind
  );
}

#[test]
fn test_cargo_config_toml_detection() {
  let path = Path::new(".cargo/config.toml");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Config {
        kind: ConfigKind::CargoConfig
      }
    ),
    ".cargo/config.toml should be classified as Config(CargoConfig), got: {:?}",
    kind
  );
}

#[test]
fn test_cargo_config_detection() {
  let path = Path::new(".cargo/config");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Config {
        kind: ConfigKind::CargoConfig
      }
    ),
    ".cargo/config should be classified as Config(CargoConfig), got: {:?}",
    kind
  );
}

#[test]
fn test_rust_toolchain_toml_detection() {
  let path = Path::new("rust-toolchain.toml");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Config {
        kind: ConfigKind::RustToolchain
      }
    ),
    "rust-toolchain.toml should be classified as Config(RustToolchain), got: {:?}",
    kind
  );
}

#[test]
fn test_rust_toolchain_detection() {
  let path = Path::new("rust-toolchain");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Config {
        kind: ConfigKind::RustToolchain
      }
    ),
    "rust-toolchain should be classified as Config(RustToolchain), got: {:?}",
    kind
  );
}

#[test]
fn test_cargo_toml_detection() {
  let path = Path::new("Cargo.toml");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Config {
        kind: ConfigKind::CargoToml
      }
    ),
    "Cargo.toml should be classified as Config(CargoToml), got: {:?}",
    kind
  );
}

#[test]
fn test_cargo_lock_detection() {
  let path = Path::new("Cargo.lock");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Config {
        kind: ConfigKind::CargoLock
      }
    ),
    "Cargo.lock should be classified as Config(CargoLock), got: {:?}",
    kind
  );
}

#[test]
fn test_integration_tests_detection() {
  let path = Path::new("tests/integration.rs");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Test {
        kind: TestKind::Integration
      }
    ),
    "tests/*.rs should be classified as Test(Integration), got: {:?}",
    kind
  );
}

#[test]
fn test_integration_tests_in_crate() {
  let path = Path::new("crates/foo/tests/integration.rs");
  let kind = classify_file(path);

  assert!(
    matches!(
      kind,
      ChangeKind::Test {
        kind: TestKind::Integration
      }
    ),
    "crates/*/tests/*.rs should be classified as Test(Integration), got: {:?}",
    kind
  );
}

#[test]
fn test_benchmarks_detection() {
  let path = Path::new("benches/benchmark.rs");
  let kind = classify_file(path);

  assert!(
    matches!(kind, ChangeKind::Test { kind: TestKind::Bench }),
    "benches/*.rs should be classified as Test(Bench), got: {:?}",
    kind
  );
}

#[test]
fn test_source_files_detection() {
  let path = Path::new("src/lib.rs");
  let kind = classify_file(path);

  assert!(
    matches!(kind, ChangeKind::Source { .. }),
    "src/*.rs should be classified as Source, got: {:?}",
    kind
  );
}

#[test]
fn test_build_script_detection() {
  let path = Path::new("build.rs");
  let kind = classify_file(path);

  assert!(
    matches!(kind, ChangeKind::BuildScript),
    "build.rs should be classified as BuildScript, got: {:?}",
    kind
  );
}

#[test]
fn test_build_script_in_crate() {
  let path = Path::new("crates/foo/build.rs");
  let kind = classify_file(path);

  assert!(
    matches!(kind, ChangeKind::BuildScript),
    "crates/*/build.rs should be classified as BuildScript, got: {:?}",
    kind
  );
}

#[test]
fn test_documentation_detection() {
  let paths = vec!["README.md", "docs/guide.md", "LICENSE", "CHANGELOG.txt"];

  for path_str in paths {
    let path = Path::new(path_str);
    let kind = classify_file(path);

    assert!(
      matches!(kind, ChangeKind::Documentation),
      "{} should be classified as Documentation, got: {:?}",
      path_str,
      kind
    );
  }
}

// Note: test_change_kind_requires_rebuild() and test_change_kind_requires_retest() removed.
// The requires_rebuild/requires_retest methods were implementation details.
// The actual logic is tested via ChangeCategories in workspace::change_analyzer tests.

/// Regression test: verify we don't break existing behavior
#[test]
fn test_backward_compatibility_all_file_types() {
  let test_cases = vec![
    // (path, expected_category)
    ("src/lib.rs", "Source"),
    ("src/main.rs", "Source"),
    ("crates/foo/src/bar.rs", "Source"),
    ("tests/integration.rs", "Test"),
    ("crates/foo/tests/test.rs", "Test"),
    ("benches/bench.rs", "Test"),
    ("build.rs", "BuildScript"),
    ("Cargo.toml", "Config"),
    ("Cargo.lock", "Config"),
    ("README.md", "Documentation"),
    ("examples/demo.rs", "Example"),
    (".cargo/config.toml", "Config"),
  ];

  for (path_str, expected) in test_cases {
    let path = Path::new(path_str);
    let kind = classify_file(path);

    let actual = match kind {
      ChangeKind::Source { .. } => "Source",
      ChangeKind::Test { .. } => "Test",
      ChangeKind::Example => "Example",
      ChangeKind::BuildScript => "BuildScript",
      ChangeKind::Config { .. } => "Config",
      ChangeKind::Documentation => "Documentation",
      ChangeKind::Other => "Other",
    };

    assert_eq!(
      actual, expected,
      "Classification changed for {}: expected {}, got {}",
      path_str, expected, actual
    );
  }
}
