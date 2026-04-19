//! Canonical file taxonomy and planning-oriented classification helpers.
//!
//! This module is the single source of truth for path-based change detection.
//! The planner's `kind` / `sub_kind` taxonomy is derived from [`FileProfile`],
//! and every other layer should consume the same profile instead of
//! re-implementing path heuristics.

use crate::config::ChangeDetectionConfig;
use glob::Pattern;
use std::path::Path;

/// Trace code for planner-classified Rust source files.
pub const RC_FILE_KIND_RUST_SRC: &str = "FILE_KIND_RUST_SRC";
/// Trace code for planner-classified Rust test files.
pub const RC_FILE_KIND_RUST_TEST: &str = "FILE_KIND_RUST_TEST";
/// Trace code for planner-classified Rust bench files.
pub const RC_FILE_KIND_RUST_BENCH: &str = "FILE_KIND_RUST_BENCH";
/// Trace code for planner-classified crate manifests.
pub const RC_FILE_KIND_TOML_MANIFEST: &str = "FILE_KIND_TOML_MANIFEST";
/// Trace code for planner-classified workspace manifests.
pub const RC_FILE_KIND_TOML_WORKSPACE: &str = "FILE_KIND_TOML_WORKSPACE";
/// Trace code for planner-classified tooling TOML files.
pub const RC_FILE_KIND_TOML_TOOLING: &str = "FILE_KIND_TOML_TOOLING";
/// Trace code for planner-classified Cargo.lock files.
pub const RC_FILE_KIND_TOML_LOCKFILE: &str = "FILE_KIND_TOML_LOCKFILE";
/// Trace code for planner-classified CI files.
pub const RC_FILE_KIND_CI: &str = "FILE_KIND_CI";
/// Trace code for planner-classified script files.
pub const RC_FILE_KIND_SCRIPT: &str = "FILE_KIND_SCRIPT";
/// Trace code for planner-classified documentation files.
pub const RC_FILE_KIND_DOCS: &str = "FILE_KIND_DOCS";
/// Trace code for planner-classified repository config files.
pub const RC_FILE_KIND_REPO_CONFIG: &str = "FILE_KIND_REPO_CONFIG";
/// Trace code emitted when configured infrastructure patterns match a path.
pub const RC_FILE_KIND_INFRA_PATTERN: &str = "FILE_KIND_INFRA_PATTERN";
/// Trace code emitted when configured custom surfaces match a path.
pub const RC_FILE_KIND_CUSTOM: &str = "FILE_KIND_CUSTOM";
/// Trace code for planner-classified unknown files.
pub const RC_FILE_KIND_UNCLASSIFIED: &str = "FILE_KIND_UNCLASSIFIED";

const BUILD_TEST_SURFACES: &[&str] = &["build", "test"];
const TEST_ONLY_SURFACES: &[&str] = &["test"];
const BENCH_ONLY_SURFACES: &[&str] = &["bench"];
const INFRA_BUILD_TEST_SURFACES: &[&str] = &["infra", "build", "test"];
const INFRA_ONLY_SURFACES: &[&str] = &["infra"];
const DOCS_ONLY_SURFACES: &[&str] = &["docs"];
const NO_SURFACES: &[&str] = &[];

/// Coarse legacy classification retained for compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
  /// Source code that affects compilation.
  Source {
    /// Whether this is a procedural macro crate.
    is_proc_macro: bool,
  },
  /// Test code.
  Test {
    /// Type of test.
    kind: TestKind,
  },
  /// Example code.
  Example,
  /// Build script.
  BuildScript,
  /// Configuration files.
  Config {
    /// Type of configuration file.
    kind: ConfigKind,
  },
  /// Documentation only.
  Documentation,
  /// Other files.
  Other,
}

/// Types of test files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
  /// Integration tests in `tests/`.
  Integration,
  /// Benchmarks in `benches/`.
  Bench,
}

/// Types of configuration files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
  /// `Cargo.toml`.
  CargoToml,
  /// `Cargo.lock`.
  CargoLock,
  /// `.cargo/config.toml` or `.cargo/config`.
  CargoConfig,
  /// `rust-toolchain.toml` or `rust-toolchain`.
  RustToolchain,
}

/// Detailed canonical path profile.
///
/// This preserves distinctions that older layers cared about (`build.rs`,
/// examples, lockfiles) while still projecting the planner's canonical
/// `kind` / `sub_kind` taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileProfile {
  /// Regular Rust source file.
  RustSrc,
  /// Rust test file under `tests/`.
  RustTest,
  /// Rust benchmark file under `benches/`.
  RustBench,
  /// Rust example file under `examples/`.
  RustExample,
  /// Rust `build.rs` file.
  RustBuildScript,
  /// Crate-local `Cargo.toml`.
  TomlManifest,
  /// Workspace-root `Cargo.toml`.
  TomlWorkspace,
  /// `.cargo/config.toml` or `.cargo/config`.
  TomlCargoConfig,
  /// `rust-toolchain.toml` or `rust-toolchain`.
  TomlRustToolchain,
  /// Other TOML tooling/config files.
  TomlTooling,
  /// Workspace `Cargo.lock`.
  CargoLock,
  /// CI or workflow file.
  Ci,
  /// Script or task-runner file.
  Script,
  /// Documentation file.
  Docs,
  /// Root-level repository config file.
  RepoConfig,
  /// Unclassified file.
  Unknown,
}

impl FileProfile {
  /// Planner-visible file kind.
  pub fn planned_kind(self) -> &'static str {
    match self {
      Self::RustSrc | Self::RustTest | Self::RustBench | Self::RustExample | Self::RustBuildScript => "rust",
      Self::TomlManifest
      | Self::TomlWorkspace
      | Self::TomlCargoConfig
      | Self::TomlRustToolchain
      | Self::TomlTooling
      | Self::CargoLock => "toml",
      Self::Ci => "ci",
      Self::Script => "script",
      Self::Docs => "docs",
      Self::RepoConfig => "config",
      Self::Unknown => "unknown",
    }
  }

  /// Planner-visible file sub kind.
  pub fn planned_sub_kind(self) -> Option<&'static str> {
    match self {
      Self::RustSrc | Self::RustExample | Self::RustBuildScript => Some("src"),
      Self::RustTest => Some("test"),
      Self::RustBench => Some("bench"),
      Self::TomlManifest => Some("manifest"),
      Self::TomlWorkspace => Some("workspace"),
      Self::TomlCargoConfig | Self::TomlRustToolchain | Self::TomlTooling => Some("tooling"),
      Self::CargoLock => Some("lock"),
      Self::RepoConfig => Some("repo"),
      Self::Ci | Self::Script | Self::Docs | Self::Unknown => None,
    }
  }

  /// Planner trace reason for the builtin classification.
  pub fn reason_code(self) -> &'static str {
    match self {
      Self::RustSrc | Self::RustExample | Self::RustBuildScript => RC_FILE_KIND_RUST_SRC,
      Self::RustTest => RC_FILE_KIND_RUST_TEST,
      Self::RustBench => RC_FILE_KIND_RUST_BENCH,
      Self::TomlManifest => RC_FILE_KIND_TOML_MANIFEST,
      Self::TomlWorkspace => RC_FILE_KIND_TOML_WORKSPACE,
      Self::TomlCargoConfig | Self::TomlRustToolchain | Self::TomlTooling => RC_FILE_KIND_TOML_TOOLING,
      Self::CargoLock => RC_FILE_KIND_TOML_LOCKFILE,
      Self::Ci => RC_FILE_KIND_CI,
      Self::Script => RC_FILE_KIND_SCRIPT,
      Self::Docs => RC_FILE_KIND_DOCS,
      Self::RepoConfig => RC_FILE_KIND_REPO_CONFIG,
      Self::Unknown => RC_FILE_KIND_UNCLASSIFIED,
    }
  }

  /// Builtin planner surfaces implied by this profile.
  pub fn default_surfaces(self) -> &'static [&'static str] {
    match self {
      Self::RustSrc | Self::RustExample | Self::RustBuildScript | Self::TomlManifest => BUILD_TEST_SURFACES,
      Self::RustTest => TEST_ONLY_SURFACES,
      Self::RustBench => BENCH_ONLY_SURFACES,
      Self::TomlWorkspace | Self::TomlCargoConfig | Self::TomlRustToolchain | Self::TomlTooling | Self::CargoLock => {
        INFRA_BUILD_TEST_SURFACES
      }
      Self::Ci | Self::Script => INFRA_ONLY_SURFACES,
      Self::Docs | Self::RepoConfig => DOCS_ONLY_SURFACES,
      Self::Unknown => NO_SURFACES,
    }
  }

  /// Whether this profile seeds transitive build/test impact in planner mode.
  pub fn seeds_build_test_transitive(self) -> bool {
    matches!(
      self,
      Self::RustSrc
        | Self::RustExample
        | Self::RustBuildScript
        | Self::TomlManifest
        | Self::TomlWorkspace
        | Self::TomlCargoConfig
        | Self::TomlRustToolchain
        | Self::TomlTooling
        | Self::CargoLock
    )
  }

  /// Whether this profile should count as docs-only by default.
  pub fn is_docs_only(self) -> bool {
    matches!(self, Self::Docs | Self::RepoConfig)
  }

  /// Compatibility projection to the legacy change kind enum.
  pub fn legacy_change_kind(self) -> ChangeKind {
    match self {
      Self::RustSrc => ChangeKind::Source { is_proc_macro: false },
      Self::RustTest => ChangeKind::Test {
        kind: TestKind::Integration,
      },
      Self::RustBench => ChangeKind::Test { kind: TestKind::Bench },
      Self::RustExample => ChangeKind::Example,
      Self::RustBuildScript => ChangeKind::BuildScript,
      Self::TomlManifest | Self::TomlWorkspace => ChangeKind::Config {
        kind: ConfigKind::CargoToml,
      },
      Self::TomlCargoConfig => ChangeKind::Config {
        kind: ConfigKind::CargoConfig,
      },
      Self::TomlRustToolchain => ChangeKind::Config {
        kind: ConfigKind::RustToolchain,
      },
      Self::TomlTooling => ChangeKind::Other,
      Self::CargoLock => ChangeKind::Config {
        kind: ConfigKind::CargoLock,
      },
      Self::Ci | Self::Script | Self::Unknown => ChangeKind::Other,
      Self::Docs | Self::RepoConfig => ChangeKind::Documentation,
    }
  }
}

/// Classify a file by path using the canonical planner taxonomy.
pub fn classify_path(path: &Path) -> FileProfile {
  let path_str = path.to_string_lossy();

  if path_str.ends_with("build.rs") {
    return FileProfile::RustBuildScript;
  }

  if path_str == "Cargo.lock" {
    return FileProfile::CargoLock;
  }

  if path_str == "Cargo.toml" {
    return FileProfile::TomlWorkspace;
  }

  if path_str.ends_with("Cargo.toml") {
    return FileProfile::TomlManifest;
  }

  if path_str == "rust-toolchain.toml" || path_str == "rust-toolchain" {
    return FileProfile::TomlRustToolchain;
  }

  if path_str.ends_with(".cargo/config") || path_str.ends_with(".cargo/config.toml") {
    return FileProfile::TomlCargoConfig;
  }

  if is_documentation(&path_str) {
    return FileProfile::Docs;
  }

  if is_ci_file(&path_str) {
    return FileProfile::Ci;
  }

  if is_script(&path_str) {
    return FileProfile::Script;
  }

  if is_repo_config(&path_str) {
    return FileProfile::RepoConfig;
  }

  if path_str.ends_with(".rs") {
    return classify_rust_file(&path_str);
  }

  if path_str.ends_with(".toml") {
    return FileProfile::TomlTooling;
  }

  FileProfile::Unknown
}

/// Backward-compatible file classification wrapper.
pub fn classify_file(path: &Path) -> ChangeKind {
  classify_path(path).legacy_change_kind()
}

/// Compile configured infrastructure patterns, using defaults when config is absent.
pub fn compile_infrastructure_patterns(config: Option<&ChangeDetectionConfig>) -> Vec<Pattern> {
  let patterns = config
    .map(|cfg| cfg.infrastructure.clone())
    .unwrap_or_else(|| ChangeDetectionConfig::default().infrastructure);

  patterns
    .into_iter()
    .filter_map(|pattern| Pattern::new(&pattern).ok())
    .collect()
}

/// Compile configured custom patterns sorted by category name.
pub fn compile_custom_patterns(config: Option<&ChangeDetectionConfig>) -> Vec<(String, Pattern)> {
  let Some(config) = config else {
    return Vec::new();
  };

  let mut names: Vec<String> = config.custom.keys().cloned().collect();
  names.sort();

  let mut patterns = Vec::new();
  for name in names {
    let Some(globs) = config.custom.get(&name) else {
      continue;
    };

    for glob in globs {
      if let Ok(pattern) = Pattern::new(glob) {
        patterns.push((name.clone(), pattern));
      }
    }
  }

  patterns
}

/// Return sorted, deduplicated custom surface names.
pub fn custom_surface_names(custom_patterns: &[(String, Pattern)]) -> Vec<String> {
  let mut names = Vec::new();

  for (name, _) in custom_patterns {
    let surface = format!("custom:{}", name);
    if !names.iter().any(|existing| existing == &surface) {
      names.push(surface);
    }
  }

  names
}

/// Match custom surfaces for a path.
pub fn custom_surfaces_for_path(path: &str, custom_patterns: &[(String, Pattern)]) -> Vec<String> {
  let mut surfaces = Vec::new();

  for (name, pattern) in custom_patterns {
    if pattern.matches(path) {
      let surface = format!("custom:{}", name);
      if !surfaces.iter().any(|existing| existing == &surface) {
        surfaces.push(surface);
      }
    }
  }

  surfaces
}

/// Whether a path matches configured infrastructure patterns.
pub fn matches_infrastructure_patterns(path: &str, infrastructure_patterns: &[Pattern]) -> bool {
  infrastructure_patterns.iter().any(|pattern| pattern.matches(path))
}

fn classify_rust_file(path_str: &str) -> FileProfile {
  if path_str.contains("/examples/") || path_str.starts_with("examples/") {
    return FileProfile::RustExample;
  }

  if path_str.contains("/benches/") || path_str.starts_with("benches/") {
    return FileProfile::RustBench;
  }

  if path_str.contains("/tests/") || path_str.starts_with("tests/") {
    return FileProfile::RustTest;
  }

  FileProfile::RustSrc
}

fn is_ci_file(path_str: &str) -> bool {
  path_str.starts_with(".github/")
}

fn is_script(path_str: &str) -> bool {
  path_str.ends_with(".sh")
    || path_str.ends_with(".bash")
    || path_str.ends_with(".zsh")
    || path_str.ends_with(".ps1")
    || path_str.ends_with(".py")
    || path_str.ends_with(".rb")
    || path_str.ends_with(".pl")
    || path_str == "justfile"
    || path_str == "Justfile"
    || path_str == "Makefile"
    || path_str == "makefile"
    || path_str == "GNUmakefile"
}

fn is_documentation(path_str: &str) -> bool {
  path_str.ends_with(".md")
    || path_str.ends_with(".txt")
    || path_str.ends_with(".adoc")
    || path_str.ends_with(".rst")
    || path_str.ends_with("LICENSE")
    || path_str.ends_with("README")
}

fn is_repo_config(path_str: &str) -> bool {
  if path_str.contains('/') {
    return false;
  }

  matches!(
    path_str,
    ".gitignore"
      | ".gitattributes"
      | ".editorconfig"
      | ".dockerignore"
      | ".prettierrc"
      | ".prettierignore"
      | ".eslintrc"
      | ".eslintignore"
      | ".npmrc"
      | ".nvmrc"
      | ".node-version"
      | ".python-version"
      | ".ruby-version"
      | ".tool-versions"
  )
}

#[cfg(test)]
mod tests {
  use super::{
    FileProfile, classify_file, classify_path, compile_infrastructure_patterns, custom_surface_names,
    custom_surfaces_for_path, matches_infrastructure_patterns,
  };
  use serde::Deserialize;
  use std::path::Path;

  const CLASSIFICATION_CORPUS: &str = include_str!("../../tests/fixtures/change_detection/path_corpus.json");

  #[derive(Debug, Deserialize)]
  struct CorpusCase {
    path: String,
    kind: String,
    #[serde(default)]
    sub_kind: Option<String>,
    #[serde(default)]
    default_surfaces: Option<Vec<String>>,
    surfaces: Vec<String>,
  }

  fn corpus_cases() -> Vec<CorpusCase> {
    serde_json::from_str(CLASSIFICATION_CORPUS).expect("classification corpus should parse")
  }

  #[test]
  fn test_classification_corpus_matches_canonical_profile() {
    for case in corpus_cases() {
      let profile = classify_path(Path::new(&case.path));
      assert_eq!(profile.planned_kind(), case.kind, "kind mismatch for {}", case.path);
      assert_eq!(
        profile.planned_sub_kind().map(str::to_string),
        case.sub_kind,
        "sub kind mismatch for {}",
        case.path
      );
      assert_eq!(
        profile
          .default_surfaces()
          .iter()
          .map(|surface| (*surface).to_string())
          .collect::<Vec<_>>(),
        case.default_surfaces.clone().unwrap_or_else(|| case.surfaces.clone()),
        "surface mismatch for {}",
        case.path
      );
    }
  }

  #[test]
  fn test_classify_file_preserves_legacy_examples_build_scripts_and_lockfiles() {
    assert!(matches!(
      classify_file(Path::new("examples/demo.rs")),
      super::ChangeKind::Example
    ));
    assert!(matches!(
      classify_file(Path::new("build.rs")),
      super::ChangeKind::BuildScript
    ));
    assert!(matches!(
      classify_file(Path::new("Cargo.lock")),
      super::ChangeKind::Config {
        kind: super::ConfigKind::CargoLock
      }
    ));
    assert!(matches!(
      classify_file(Path::new("rust-toolchain.toml")),
      super::ChangeKind::Config {
        kind: super::ConfigKind::RustToolchain
      }
    ));
  }

  #[test]
  fn test_configured_pattern_helpers() {
    let patterns = compile_infrastructure_patterns(None);
    assert!(matches_infrastructure_patterns(".github/workflows/ci.yml", &patterns));

    let custom_patterns = vec![
      (
        "verify".to_string(),
        glob::Pattern::new("verify/**").expect("glob should compile"),
      ),
      (
        "verify".to_string(),
        glob::Pattern::new("models/**").expect("glob should compile"),
      ),
      (
        "docs_pipeline".to_string(),
        glob::Pattern::new("docs/**").expect("glob should compile"),
      ),
    ];

    assert_eq!(
      custom_surface_names(&custom_patterns),
      vec!["custom:verify".to_string(), "custom:docs_pipeline".to_string()]
    );
    assert_eq!(
      custom_surfaces_for_path("verify/state.rs", &custom_patterns),
      vec!["custom:verify".to_string()]
    );
    assert_eq!(
      custom_surfaces_for_path("docs/guide.md", &custom_patterns),
      vec!["custom:docs_pipeline".to_string()]
    );
  }

  #[test]
  fn test_cargo_lock_projects_to_lock_sub_kind() {
    let profile = classify_path(Path::new("Cargo.lock"));
    assert_eq!(profile, FileProfile::CargoLock);
    assert_eq!(profile.planned_kind(), "toml");
    assert_eq!(profile.planned_sub_kind(), Some("lock"));
  }
}
