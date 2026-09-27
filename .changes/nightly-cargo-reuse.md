---
"cargo-rail" = "patch"
---

`cargo rail cache ready` installs `rustc-dev` for the workspace-selected rustup toolchain when it is missing,
so the compiler driver can be prepared for that toolchain.
Nightly Cargo builds rlibs without embedded metadata
(`-Z embed-metadata=no`)
and passes each dependency as an rlib and its rmeta;
the native cache now binds that pair instead of bypassing every such compilation.
