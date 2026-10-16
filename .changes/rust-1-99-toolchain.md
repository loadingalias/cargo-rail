---
cargo-rail = "patch"
---

Build Cargo-Rail release archives and their bundled compiler driver with Rust 1.99.0.
The bundled driver serves only the exact compiler that built it.
For a workspace on another compiler, such as Rust 1.98.1,
Cargo-Rail builds the driver from its authenticated source with that compiler.
That build needs the `rustc-dev` component of the workspace toolchain.
