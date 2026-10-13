---
cargo-rail = "patch"
---

Hash each unchanged input file once per compiler wrapper on Linux and macOS.
A `-sys` crate's static library in its `OUT_DIR` was hashed again when rustc selected it,
and again before a hit was restored.
