---
cargo-rail = "minor"
---

Bind each native search directory by the libraries that rustc selects from it, not by its content.
A directory such as `/usr/lib/x86_64-linux-gnu` can contain symbolic links, unrelated files, and subdirectories.
These entries no longer block or invalidate reuse of the crates that a `-sys` build script reaches.
The compiler driver reports each native-library search that rustc performs itself.
The action binds every candidate through the spelling, canonical target,
and bytes of the selected library.
Reuse `-l NAME` and `-l dylib=NAME` actions.
A crate such as `pq-sys` no longer bypasses.

Name the cause of a capture bypass, such as a linked source entry or a capture bound.
Add one example for each of the first recorded bypass reasons to `cargo rail cache report --finish`.
Count a failed cold admission under its own reason.
Before, Cargo-Rail joined it to the miss reason and did not count it.
