---
cargo-rail = "major"
---

Release a package while unpublished workspace members depend on it.
Validate and retain exact Cargo manifest and lockfile writes before preparation,
without bumping or tagging the unpublished dependents.
Published dependents retain the strict release closure.

Give each package an optional release tag format and include configuration warnings in JSON.
Required semver checks reject a missing baseline unless the registry proves a first release.

Analyze declared independent Cargo workspaces in Unify
and move proven test-only normal dependencies into their matching development tables.
Restore a deleted cache installation from its exact managed wrapper path
and diagnose stale authenticated compiler components.

Check cross-target Surface production code without linking or requiring std,
and report host-only test and doctest coverage.
Publish an authenticated archive-host manifest
so unsupported native archive hosts can select source installation.

Release records use v11 with release-plan v10, and Surface reports use v5.
Finish older active release transactions with their originating executable.
The companion Action must adopt its independently validated v11 reader
before consuming these release records.
