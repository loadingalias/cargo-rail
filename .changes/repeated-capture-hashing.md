---
cargo-rail = "patch"
---

On Linux and macOS, hash each unchanged input file once for each compiler wrapper.
Before, Cargo-Rail hashed a static library in the `OUT_DIR` of a `-sys` crate again when rustc selected it.
It hashed that library again before it restored a hit.
