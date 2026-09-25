---
"cargo-rail" = "patch"
---

Handle dependencies that only a feature enables when Unify and Surface acquire compiler evidence.
Cargo's default resolution omits them,
but a view that enables the feature compiles them and runs their build scripts.
Unify no longer fails with "absent from the captured package graph"
for an optional local path dependency,
and views that run such a build script are now stored and reused.
The extra all-features resolution is loaded only
when the lockfile names packages the default resolution lacks.
