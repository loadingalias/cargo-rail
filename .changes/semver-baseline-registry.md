---
cargo-rail = "patch"
---

With `release.semver_check = "deny"`, a crate that has no baseline tag is checked against the registry that receives it.
The release asks that registry whether the crate has an earlier version.
Before, it always asked crates.io.
Thus, an unrelated crates.io crate with the same name could block a release to another registry.
