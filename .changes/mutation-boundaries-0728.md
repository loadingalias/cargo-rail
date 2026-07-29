---
"cargo-rail" = "major"
---

Hardened backup, release, split, and sync mutation boundaries against path escape, symlink traversal, cross-operation
plan reuse, and exact-release checkout drift. Split and sync check modes now distinguish clean state from pending work,
configuration validation rejects malformed unify globs and empty split branches, and release checks report skipped
evidence separately from passed checks.
