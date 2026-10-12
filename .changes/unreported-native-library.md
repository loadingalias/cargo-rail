---
cargo-rail = "patch"
---

Store no linked result when the linker receives a library by path from a native search directory
and no native-library search that the compiler driver reported selected it.
A rustc that resolves more libraries itself than the driver reports then bypasses with `native_library_search_unreported` instead of
binding the file without the search that chose it.
