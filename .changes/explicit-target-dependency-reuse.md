---
"cargo-rail" = "patch"
---

Registry and git dependencies reuse results under an explicit `CARGO_TARGET_DIR` or build directory.
Cargo compiles those packages inside their unpacked source,
so the wrapper found no enrolled workspace and every one of them bypassed;
it now uses the workspace Cargo started in (`PWD`).
