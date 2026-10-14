---
cargo-rail = "major"
---

Use native input protocol 5 for the compiler driver.
The driver now reports the libraries that rustc selects from each native search directory.
Cargo-Rail rejects independent compiler adapter packs from 0.30 and 0.31, which use protocol 4.
Publish these packs again for this release.
