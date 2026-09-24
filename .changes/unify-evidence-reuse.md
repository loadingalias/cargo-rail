---
"cargo-rail" = "minor"
---

Reuse Unify compiler evidence in workspaces with build scripts and proc macros.
Each view records the `rerun-if-changed` paths and `rerun-if-env-changed` values of every build script in its Cargo graph,
and is reused while Cargo would keep those scripts' output.
A changed declared input reruns only the affected views.
Views are stored as they complete, so a failed or interrupted run keeps them for the retry,
and a corrupt stored view no longer hides valid ones.
`evidence_cache` entries add `publication_bypasses`, which name why a view was not stored.
Stored compiler evidence starts cold once after upgrading.
