---
cargo-rail = "patch"
---

Plan changes outside a nested workspace.
A changed Cargo configuration file
or toolchain file in a parent directory of the workspace root now requires every built-in Cargo item
for the whole workspace.
A changed `rustfmt.toml`, `clippy.toml`, or `deny.toml` file in that location requires `cargo.fmt`, `cargo.clippy`, or `dependency-policy`, respectively.
Any other changed file outside the workspace widens built-in Cargo work
until planning evidence shows that no compiler read it.
Before, the planner dropped these changes and skipped the work as if the evidence were complete.
