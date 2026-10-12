---
cargo-rail = "minor"
---

Bind each native search directory by the libraries that rustc selects from it, not by its content.
Symbolic links, unrelated files,
and subdirectories in a directory such as `/usr/lib/x86_64-linux-gnu` no longer block or invalidate reuse of the crates
that a `-sys` build script reaches.
The compiler driver reports each native-library search that rustc performs itself,
and the action binds every candidate through the selected library's spelling, canonical target,
and bytes.
Reuse `-l NAME` and `-l dylib=NAME` actions; a crate such as `pq-sys` no longer bypasses.

Name the cause of a capture bypass, such as a linked source entry or a capture bound,
and add one example of each of the first bypass reasons to `cargo rail cache report --finish`.
Count a failed cold admission under its own reason;
it was previously joined to the miss reason and left uncounted.
