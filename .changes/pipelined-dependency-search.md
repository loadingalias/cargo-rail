---
cargo-rail = "patch"
---

Reuse a library that Cargo compiles before the rlib of a dependency is complete.
Cargo starts a dependent from the rmeta of the dependency.
A compilation that reads only crate metadata never reads the rlib that has the same filename hash.
That rlib is no longer an input.
Its appearance during compilation no longer causes a bypass with `compiler_native_input_evidence_unavailable`.
As a result, cold and warm builds bind the same search.
