---
"cargo-rail" = "patch"
---

Raised the repository and package toolchain to Rust 1.97.1, updated Rust dependencies, the native-cache fixture
dependency graph, GitHub Actions, the CI planner's cargo-rail version, and the native-cache comparator's sccache
installation to their current releases. Release rebuilds now use the exact package toolchain, immutable assets are
verified instead of overwritten, and action verification binds every workflow pin to the action lock.
