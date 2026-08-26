//! Path classification used by semantic Cargo diffs and release attribution.

use std::path::Path;

/// Detailed canonical path profile.
///
/// This preserves distinctions that older layers cared about (`build.rs`,
/// examples, lockfiles) while still projecting the planner's canonical
/// `kind` / `sub_kind` taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileProfile {
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
    /// Whether this profile seeds transitive build/test release attribution.
    pub(crate) fn seeds_build_test_transitive(self) -> bool {
        matches!(
            self,
            Self::RustSrc
                | Self::RustBuildScript
                | Self::TomlManifest
                | Self::TomlWorkspace
                | Self::TomlCargoConfig
                | Self::TomlRustToolchain
                | Self::TomlTooling
                | Self::CargoLock
        )
    }
}

/// Classify a file for semantic Cargo analysis and release attribution.
pub(crate) fn classify_path(path: &Path) -> FileProfile {
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
    use super::{FileProfile, classify_path};
    use std::path::Path;

    #[test]
    fn classifies_inputs_that_change_release_attribution() {
        assert_eq!(classify_path(Path::new("Cargo.lock")), FileProfile::CargoLock);
        assert_eq!(classify_path(Path::new("Cargo.toml")), FileProfile::TomlWorkspace);
        assert_eq!(
            classify_path(Path::new("crates/demo/Cargo.toml")),
            FileProfile::TomlManifest
        );
        assert!(classify_path(Path::new("crates/demo/src/lib.rs")).seeds_build_test_transitive());
        assert!(!classify_path(Path::new("crates/demo/tests/api.rs")).seeds_build_test_transitive());
    }
}
