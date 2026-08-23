---
"cargo-rail" = "patch"
---

Compiler-observation storage and acquisition failures now stop `unify` as operational errors instead of continuing
with graph-only analysis. Resource failures can no longer produce plausible but unsupported unused-dependency or
feature verdicts.
