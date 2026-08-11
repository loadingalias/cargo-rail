---
"cargo-rail" = "major"
---

Removed the top-level `cargo rail run` command and `[run]` configuration. The planner's versioned per-surface Cargo
arguments are now the only workspace-scope handoff; Cargo, cargo-nextest, Just, and CI execute that scope directly.
Runner-owned profiles, workflows, actions, hermetic command wrapping, whole-action caching, and dry-run projections are
gone. Transparent verified compiler reuse remains available beneath ordinary Cargo invocations.
