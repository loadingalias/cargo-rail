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
That phase needs native input protocol 3,
so independent compiler adapter packs from earlier releases are rejected until they are republished.
Linked Clippy output, such as a member's build script, a `SYSROOT` override,
a configuration file outside the repository, and `--root-portability remap` profiles keep running Clippy normally.
