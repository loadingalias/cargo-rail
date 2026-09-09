---
"cargo-rail" = "patch"
---

Resolve Cargo workspace, package, dependency and target paths at capture so filesystem aliases preserve package ownership and planner selections. Index captured package roots for changed-file attribution. Preserve symlink parent traversal when resolving missing paths, including containment checks, and retain logical ownership for deleted source files.
