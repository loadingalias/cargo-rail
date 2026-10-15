---
cargo-rail = "patch"
---

Store no linked result when rustc passes a library by path from a native search directory
and no reported native-library search selected it, unless the compiler arguments name that path.
The action bypasses with `native_library_search_unreported` because the search that chose the library is unbound.

Startup objects and runtime files added by GCC
or Clang do not require a rustc native-library search report.
Their contents and driver-selected paths remain bound by the linker witness.
