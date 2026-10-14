---
cargo-rail = "patch"
---

Reuse crates whose build scripts emit a bare `rustc-link-search` or `-L all=` directory outside the repository.
An example is non-bundled `libsqlite3-sys` with a system library directory.
Rustc also searches that directory for Rust crates.
That search now binds by absolute host path.
Before, it bypassed with `compiler_native_input_evidence_unavailable`.
