---
cargo-rail = "patch"
---

Build the compiler driver against nightlies that rename rustc's compiler entry point.
Select the entry point from the exact compiler identity and keep macro observation bound to
that same rustc function.
