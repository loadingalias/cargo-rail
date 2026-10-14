---
cargo-rail = "patch"
---

The linker can receive a library by path from a native search directory.
Store no linked result when none of the native-library searches
that the compiler driver reported selected that library.
As a result, a rustc that resolves more libraries itself than the driver reports causes a bypass
with `native_library_search_unreported`.
Without this rule, the file would be bound without the search that chose it.
