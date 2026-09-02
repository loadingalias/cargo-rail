---
"cargo-rail" = "patch"
---

Repositories with package-specific no-std or WASM support can now keep those
domains in target-aware dependency resolution without forcing Unify to invent a
workspace-wide default-feature compiler view for them.
