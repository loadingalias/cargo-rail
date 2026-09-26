---
"cargo-rail" = "minor"
---

Crates that depend directly on a procedural macro, such as `async-trait` or `paste`,
are now stored and reused like crates that use a macro through a re-export.
Every unit that loads a procedural macro now also binds its package build script's `rerun-if-changed` paths
and `rerun-if-env-changed` variables, so a hit stays valid exactly while Cargo itself would keep the unit.
This closes a stale reuse for macros that read declared files, such as migrations,
through a re-export.
A package whose build script declares no rerun input keeps compiling normally (`build_script_package_rerun_unmodeled`).
Cache hits are also faster: captured source trees
and dependency directories each keep one digest record, native search directories reuse it,
and Rust library selection resolves each directory once.
A workspace-member Clippy hit in this repository fell from a median of 330 ms to 130–185 ms.
