---
cargo-rail = "patch"
---

Reuse crates whose build scripts emit a bare `rustc-link-search` or `-L all=` directory outside the repository,
such as non-bundled `libsqlite3-sys` with a system library directory.
Rustc also searches that directory for Rust crates;
the search now binds by absolute host path instead of bypassing with `compiler_native_input_evidence_unavailable`.
