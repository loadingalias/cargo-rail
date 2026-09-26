---
"cargo-rail" = "minor"
---

Workspace-member `cargo clippy` results are now stored and replayed,
so Clippy after `cargo clean` restores the member diagnostics instead of linting every member again.
A Clippy result binds the `clippy-driver` bytes, the arguments Clippy appends, the environment it reads,
every configuration file candidate it checks up to the one it loads, and the manifests, lockfile,
and Cargo configuration that `clippy::cargo` lints read through `cargo metadata`.
A cold miss runs a new resolution phase of the compiler driver beside Clippy to record the crates it
selects.
That phase needs the new native input protocol,
so independent compiler adapter packs from earlier releases are rejected until they are republished.
Under `--root-portability remap`, a Clippy result is shared across checkout roots: paths below the repository, the Cargo home,
and the compiler sysroot are bound relative to those roots.
Linked Clippy output, such as a member's build script, a `SYSROOT` override,
and a configuration file outside the repository keep running Clippy normally.
