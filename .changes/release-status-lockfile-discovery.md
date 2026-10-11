---
cargo-rail = "patch"
---

Report disabled release effects as skipped instead of completed publication,
while preserving recovery instructions.
Reject undeclared tracked independent workspaces that resolve a released path package
before release preparation can invalidate their lockfiles.
