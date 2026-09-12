---
"cargo-rail" = "patch"
---

Preserve literal target-table keys, including dotted target names and `cfg(...)` expressions,
when applying dependency repairs.
Quote TOML keys and escape string control characters correctly.
Reject empty dotted-path components before mutation and preserve neighboring target tables.
