//! Internal change interpretation shared by release attribution and planning.

use std::path::Path;

use crate::error::RailResult;
use crate::workspace::WorkspaceContext;
use rustc_hash::FxHashSet;

pub(crate) mod semantic;

/// Return workspace crates with changed inputs that require release-intent coverage.
pub(crate) fn changed_code_crate_names(ctx: &WorkspaceContext, base: Option<&str>) -> RailResult<FxHashSet<String>> {
    let mut crates = FxHashSet::default();
    for repository_path in ctx.source_paths_since(base)? {
        let Some(workspace_path) = ctx.to_workspace_path(&repository_path) else {
            continue;
        };
        if !is_release_coverage_input(&workspace_path) {
            continue;
        }
        if let Some(owner) = ctx.graph().file_to_crate(&workspace_path) {
            crates.insert(owner);
        }
    }
    Ok(crates)
}

pub(crate) fn is_release_coverage_input(path: &Path) -> bool {
    let path = path.to_string_lossy();

    if path.ends_with("build.rs")
        || path == "Cargo.lock"
        || path == "rust-toolchain"
        || path.ends_with("Cargo.toml")
        || path.ends_with(".cargo/config")
        || path.ends_with(".cargo/config.toml")
    {
        return true;
    }

    if path.starts_with(".github/") {
        return false;
    }

    if path.ends_with(".rs") {
        return !is_test_bench_or_example(&path);
    }

    path.ends_with(".toml")
}

fn is_test_bench_or_example(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.starts_with("benches/")
        || path.contains("/benches/")
        || path.starts_with("examples/")
        || path.contains("/examples/")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::is_release_coverage_input;

    #[test]
    fn release_coverage_preserves_rust_and_cargo_boundaries() {
        let cases = [
            ("crates/demo/src/lib.rs", true),
            ("crates/demo/build.rs", true),
            ("Cargo.toml", true),
            ("crates/demo/Cargo.toml", true),
            ("Cargo.lock", true),
            ("rust-toolchain", true),
            ("rust-toolchain.toml", true),
            (".cargo/config", true),
            (".cargo/config.toml", true),
            ("crates/demo/tooling.toml", true),
            ("crates/demo/tests/api.rs", false),
            ("crates/demo/benches/throughput.rs", false),
            ("crates/demo/examples/client.rs", false),
            (".github/workflows/check.rs", false),
            (".github/workflows/check.toml", false),
            ("scripts/generate.py", false),
            ("scripts/check.sh", false),
            ("docs/design.md", false),
            (".editorconfig", false),
            ("crates/demo/assets/input.bin", false),
        ];

        for (path, expected) in cases {
            assert_eq!(
                is_release_coverage_input(Path::new(path)),
                expected,
                "release coverage mismatch for {path}"
            );
        }
    }
}
