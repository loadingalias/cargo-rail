---
"cargo-rail" = "patch"
---

Select a pinned nightly for RISC-V tooling so rscrypto SHA-2 can compile, and verify the active compiler and Cargo against that exact toolchain. Remove compiler-version guessing from crate-visibility capture and use the supported atomic update API on stable and nightly.
