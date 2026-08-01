---
"cargo-rail" = "minor"
---

Add automatic verified compiler reuse for eligible clean Cargo profiles while preserving active incremental profiles,
explicit incremental requests, and existing wrappers. Move compiler evidence into the typed, user-wide
content-addressed store and retain the bounded legacy file only for one-time import.

Add separately scoped workspace/local cache status, preview, and cleanup commands while keeping `clean --cache` as the
combined compatibility alias. Coordinate restore, publication, garbage collection, status, and cleanup through one
validated lifecycle lock; reclaim crash staging and evict least-recently-used unleased results under the configured
byte bound. Local status and cleanup remain exact after `cargo clean` without a workspace-local pointer to user-wide
state.

Delegate the exact normal all-workspace build and distribution actions directly to unchanged Cargo when an active
profile and its target location are statically unambiguous. This preserves Cargo and wrapper ownership while avoiding
metadata, Git, tool hashing, action-key construction, and cache setup; other shapes retain captured planning.
Default text execution reports one concise native-cache decision; `--explain` retains the full stable reason and
per-unit evidence. Stop synchronously flushing observational run receipts because they are not recovery authority.
