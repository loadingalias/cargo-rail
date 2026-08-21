---
"cargo-rail" = "minor"
---

Finalizing a merged release pull request now tags the already-proven merge commit without creating or pushing an empty
commit. Compiler-observation failures now stop `unify` as operational errors instead of continuing with a semantic
fallback. GitHub planner output now includes the deterministic `surfaces_json` projection for bounded CI routing.
