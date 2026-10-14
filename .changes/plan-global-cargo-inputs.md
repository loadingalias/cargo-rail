---
cargo-rail = "patch"
---

Plan every built-in Cargo item for the whole workspace when `rust-toolchain.toml`, `rust-toolchain`, or the root `.cargo/config.toml` changes.
Plan `cargo.clippy` for the whole workspace when a Clippy configuration file changes.
These files have an effect on every package.
Before, a change to these files together with a change to the source of one member selected only
that member and its dependents.

Emit exact test targets for a package only when its own target files are the sole reason
for its selection.
A package whose dependency also changed now keeps all of its targets.
For example, `b` depends on `a`, and both `crates/a/src/lib.rs` and `crates/b/tests/it.rs` change.
Package `b` now keeps all of its targets.
