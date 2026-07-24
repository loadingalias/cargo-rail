---
"cargo-rail" = "minor"
---

Add portable, verified native compiler-result caching for non-incremental dependency and workspace library
metadata/rlib units on Apple Silicon macOS and ARM64 Linux with Cargo/rustc 1.97.1. Ordinary `cargo rail run` check
and build actions can reuse byte-exact outputs across clean roots without restoring or fabricating Cargo target state,
incremental state, or fingerprints.

Preserve custom wrappers and sccache, keep incremental builds and unproven linker/build-script/proc-macro classes
explicitly bypassed, and fail closed on input, toolchain, environment, SDK/linker, cache-object, and output mutations.
Add a representative registry/Git/native/proc-macro fixture plus reproducible cold/warm benchmarks and cache evidence.
