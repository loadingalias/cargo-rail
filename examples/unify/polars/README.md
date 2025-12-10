# polars unify demo

Large data processing workspace (33 crates, pyo3 bindings). Aggressive settings.

## Config choices

```toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[unify]
msrv = true
detect_unused = true
remove_unused = true
prune_dead_features = true
pin_transitives = true
transitive_host = "root"
strict_version_compat = false
major_version_conflict = "bump"
```

| Option | Value | Why |
|--------|-------|-----|
| `msrv` | true | Compute floor for edition 2024 workspace |
| `detect_unused` | true | Find orphaned deps in large workspace |
| `remove_unused` | true | Auto-clean |
| `prune_dead_features` | true | Many optional features across crates |
| `pin_transitives` | true | **Key demo**: workspace-hack replacement |
| `transitive_host` | root | Pin transitives in root Cargo.toml |
| `strict_version_compat` | false | Allow version flexibility |
| `major_version_conflict` | bump | Force highest resolved version |

## What this shows

- Handling large workspace with 33 crates
- `pin_transitives` as cargo-hakari replacement
- Aggressive `major_version_conflict = "bump"` for leaner graph
- polars already uses `[workspace.dependencies]` - cargo-rail enhances it

## Demo

https://github.com/user-attachments/assets/31bf5ff5-7185-4e59-acaa-ea8edd3c6f48
