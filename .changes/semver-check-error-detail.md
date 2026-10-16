---
cargo-rail = "patch"
---

When `cargo semver-checks` cannot produce a verdict, the release warning names its error.
Before, the warning showed the first line of output, such as `Cloning <commit>`.
The error came later, for example a baseline dependency that crates.io yanked.
