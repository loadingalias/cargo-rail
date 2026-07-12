---
"cargo-rail" = "minor"
---

Made `cargo rail unify` faster and more exact with shared indexed Cargo metadata, workspace-only compiler evidence, source-derived feature checks, and compilation-unit cache reuse. Analysis now covers configured targets, default/no-default/all-feature builds, conditional feature selections, generated and macro-expanded source, every Cargo target kind, and target-scoped declarations.

Graph-removing decisions now carry deterministic proof certificates. Apply verifies the exact declaration edits and resulting portable Cargo graph before writing. Closed-world cleanup of dormant private features and optional dependencies requires the explicit `consumer_scope = "workspace"` contract; published feature APIs remain preserved.
