---
cargo-rail = "patch"
---

Store no cold result when an input captured after the compiler exits changed while the compiler ran.
This covers linker inputs and failed linker lookups, native-library candidates,
procedural-macro reads, and selected repository inputs.
A file edited and restored, or a candidate that appeared and disappeared,
during compilation could otherwise bind a state that the compiler never read.
The action bypasses with `cold_input_changed_during_execution`.
