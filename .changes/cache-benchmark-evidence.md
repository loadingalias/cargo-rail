---
"cargo-rail" = "minor"
---

Add `cargo-rail-bench` to compare native Cargo, Cargo-Rail, and sccache on a bundled mixed Rust/native workload.
Retain per-sample correctness evidence for cold, empty-target rebuild, Cargo freshness,
and source-edit scenarios.
Fixture preparation is portable; local timing requires Unix.
Run the benchmark from source with `just bench`.
