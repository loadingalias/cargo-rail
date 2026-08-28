---
"cargo-rail" = "patch"
---

Install each CI Rust toolchain into a job-private Rustup home, require Cargo explicitly, and bind downstream steps to
the verified host-qualified toolchain. This removes runner-image Rustup state from compatibility and release archive
bootstrap.
