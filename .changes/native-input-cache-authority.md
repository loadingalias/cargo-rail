---
"cargo-rail" = "minor"
---

Require authenticated compiler-selected Rust inputs for local and distributed cache reuse.
Capture supported cross-target linker outputs, debug objects, PDBs,
and import libraries as part of verified results.
Incomplete backend or linker evidence falls back to ordinary compiler execution.
Native archives include the matched compiler driver and its authenticated source bundle.
