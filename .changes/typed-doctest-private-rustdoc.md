---
"cargo-rail" = "patch"
---

Typed doctest collection stages a private copy of `rustdoc` instead of a symlink.
Where `rustdoc` finds its compiler library through an ELF `DT_RPATH`, as on IBM Z,
the symlink loaded the shared toolchain's library,
so doctests compiled with the shared `rustc` and produced no compiler facts.
