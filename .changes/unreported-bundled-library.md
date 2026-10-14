---
cargo-rail = "patch"
---

An rlib or staticlib that rustc writes can hold a native-library member.
Compile without reuse when the compilation did not generate that member
and no reported native-library search selected it.
A future rustc might bundle a library that the compiler driver does not report.
With this rule, the archive then cannot be bound without the search that chose the library.
