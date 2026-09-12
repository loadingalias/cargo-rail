---
"cargo-rail" = "patch"
---

Observe GCC-selected Rust LLD inputs and verified process-image replacements for compiler reuse.
Recognize GNU and Darwin LLD flavors that use a C compiler driver.
Track parent traversal in ELF runtime searches while detecting symlink and candidate changes.
Normalize Windows executable paths consistently for compiler and wrapper verification.

Provisioned toolchains are checked by native compilation and execution.
Preserve MSVC linker precedence and use the shared nextest policy on native runners.
Linux CI installs perf for the running kernel, including its distribution flavor.
