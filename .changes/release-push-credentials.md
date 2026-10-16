---
cargo-rail = "patch"
---

Keep the release push credential away from build scripts and procedural macros.
Release runners pass the push token as process-scoped Git configuration (`GIT_CONFIG_*` variables).
The companion Action and this repository's release workflow do this.
`cargo rail release` now removes these variables from `cargo package` and `cargo semver-checks`.
It already removed the registry and forge tokens there.
Before, code that these commands compiled and ran could read the push token.
