---
cargo-rail = "patch"
---

Reuse a library that Cargo compiles while a dependency's rlib is still being written.
Cargo starts a dependent from the dependency's rmeta,
and a compilation that reads only crate metadata never reads the rlib with the same filename hash.
That rlib is no longer an input, so it appearing during compilation no longer bypasses with `compiler_native_input_evidence_unavailable`,
and cold and warm builds bind the same search.
