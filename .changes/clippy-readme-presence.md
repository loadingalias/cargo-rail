---
cargo-rail = "patch"
---

Bind whether each package has a default README in reused Clippy results.
`cargo metadata` reports `README.md`, `README.txt`, or `README` as the README of a package without a `readme` key.
`clippy::cargo_common_metadata` reads that name.
Before this change, a new or removed README did not change the Clippy action.
With `-D clippy::cargo`, a hit could then replay a pass after the README was removed.
The contents of a README are still not bound, so an edit to a README still hits.
