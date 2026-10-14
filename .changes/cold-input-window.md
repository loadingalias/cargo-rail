---
cargo-rail = "patch"
---

Cargo-Rail captures some inputs after the compiler exits.
Store no cold result when one of these inputs changed while the compiler ran.
This rule covers linker inputs, failed linker lookups, native-library candidates,
procedural-macro reads, and selected repository inputs.
Otherwise, a cold result could bind a state that the compiler never read.
For example, a file could change and then return to its earlier content during compilation.
A candidate could also appear and then disappear during compilation.
The action bypasses with `cold_input_changed_during_execution`.
