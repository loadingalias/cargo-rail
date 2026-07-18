---
"cargo-rail" = "minor"
---

Add lazy exact Cargo resolution views keyed by package, feature, target, toolchain, and sanitized Cargo configuration; replace filename heuristics with deterministic PackageId ownership; and introduce opt-in immutable workspace snapshots over exact source, manifests, lockfile, configuration, toolchain, and target inputs without slowing native/default commands.
