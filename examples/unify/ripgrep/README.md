# ripgrep unify demo

Small, focused CLI workspace (9 crates). Demonstrates baseline unification.

## Config choices

```toml
targets = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]

[unify]
msrv = true
detect_unused = true
remove_unused = true
prune_dead_features = true
strict_version_compat = true
```

| Option | Value | Why |
|--------|-------|-----|
| `msrv` | true | ripgrep already sets `rust-version`; verify cargo-rail computes same |
| `detect_unused` | true | Find any deps declared but not resolved |
| `remove_unused` | true | Auto-clean them |
| `prune_dead_features` | true | Remove empty feature no-ops |
| `strict_version_compat` | true | Default safety - no surprises |

## What this shows

- Basic unification flow on a well-maintained workspace
- MSRV computation matching existing `rust-version = "1.85"`
- Fast execution on small workspace

## Demo

https://github.com/user-attachments/assets/93f34633-aa0e-4cde-8723-c81f3f474bac
