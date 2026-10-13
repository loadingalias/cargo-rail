---
cargo-rail = "patch"
---

Compile without reuse when an rlib or staticlib that rustc writes holds a native-library member
that the compilation did not generate and that no reported native-library search selected.
A future rustc that bundles a library the compiler driver does not report then cannot bind the
archive without the search that chose the library.
