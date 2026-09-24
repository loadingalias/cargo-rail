---
"cargo-rail" = "minor"
---

Replace the local compiler cache with one deduplicated `local-cas-v3` store: identical outputs are stored once,
a miss no longer scans the whole store, and a full store collects to 90% of its budget, least recently used first.
`cargo build`, `cargo test`, and `cargo clippy` now share dependency results, and terminal width affects a result
only when it rendered diagnostics. On Linux and macOS, unchanged input files are not rehashed.
`cache profiles` reports each profile's usage against its budget, `cache setup` applies a lowered budget
immediately, and `cache clean --scope local` previews and removes the retired v2 store.
Cache keys changed, so the first build and remote caches start cold once after upgrading.
