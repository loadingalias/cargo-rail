# tokio unify demo

Foundational async runtime (5 public + 5 internal crates). Conservative settings.

## Config choices

```toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[unify]
msrv = true
detect_unused = true
remove_unused = true
prune_dead_features = true
strict_version_compat = true
exact_pin_handling = "warn"
major_version_conflict = "warn"
```

| Option | Value | Why |
|--------|-------|-----|
| `msrv` | true | Compute buildable floor from deps |
| `detect_unused` | true | Ecosystem crate - should be clean |
| `remove_unused` | true | Auto-apply cleanup |
| `prune_dead_features` | true | tokio has many conditional features |
| `strict_version_compat` | true | Conservative - don't break ecosystem |
| `exact_pin_handling` | warn | Report any exact pins but don't fail |
| `major_version_conflict` | warn | Skip conflicts, don't force bump |

## What this shows

- Safe unification on core ecosystem library
- Conservative version handling (warn, don't break)
- Multi-crate workspace with internal test crates
