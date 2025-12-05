# helix-db unify demo

Emerging graph database workspace (6 crates). Demonstrates unification on a growing project.

## Config choices

```toml
[unify]
msrv = true
detect_unused = true
remove_unused = true
prune_dead_features = true
strict_version_compat = true
```

| Option | Value | Why |
|--------|-------|-----|
| `msrv` | true | Compute MSRV as project matures |
| `detect_unused` | true | Keep deps clean during rapid development |
| `remove_unused` | true | Auto-clean unused deps |
| `prune_dead_features` | true | Remove feature cruft |
| `strict_version_compat` | true | Safe defaults for new project |

## What this shows

- cargo-rail on a small, actively developed workspace
- Clean unification for projects heading toward v1
- Standard workflow: init → check → unify → verify

## Demo

https://github.com/user-attachments/assets/3520d254-e69c-460c-b894-eb126b42a1ea
