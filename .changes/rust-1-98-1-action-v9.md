---
"cargo-rail" = "patch"
---

Build Cargo-Rail with Rust 1.98.1, which fixes the Rust 1.98.0 trait-object
vtable miscompilation, and align the GitHub Actions example with the native v9
Action and Cargo-Rail 0.26.0. Saved-plan consumers can now verify captured plan
bytes on standard input without reopening mutable path authority.
