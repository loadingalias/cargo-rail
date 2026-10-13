---
cargo-rail = "patch"
---

Plan every built-in Cargo item for the whole workspace when `rust-toolchain.toml`, `rust-toolchain`, or the root `.cargo/config.toml` changes,
and plan `cargo.clippy` for the whole workspace when a Clippy configuration file changes.
These files reach every package,
but a change to them alongside one member's source selected only that member and its dependents.

Emit exact test targets only for a package whose own target files are the sole reason it was
selected.
A package whose dependency also changed, such as `b` depending on `a` when both `crates/a/src/lib.rs` and `crates/b/tests/it.rs` change,
now keeps all of its targets.
