---
"cargo-rail" = "minor"
---

Every stable, beta, and nightly compiler at
or above the compiler adapter's minimum now reuses results:
the compiler driver builds against each compiler's internal API,
and CI builds and tests it against the current stable, beta, and nightly.
`cargo rail cache ready` installs `rustc-dev` for the workspace-selected rustup toolchain when it is missing,
so the compiler driver can be prepared for that toolchain.
Nightly Cargo builds rlibs without embedded metadata
(`-Z embed-metadata=no`)
and passes each linked dependency as an rlib and its rmeta; the native cache binds that pair,
so crates that link a library reuse results instead of bypassing,
and a result without its rmeta is refused.
Planning evidence locates a fresh binary's own dep-info in nightly Cargo's per-unit build
directories instead of widening to Cargo's dependency superset.
