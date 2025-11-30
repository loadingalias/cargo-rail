# tikv + cargo-rail

**Repo:** <https://github.com/tikv/tikv>
**Commit:** `9f88068b3` (2025-11-27)

## Demo

![tikv demo](./demo.gif)

## Commands

```bash
# Clone and enter
git clone https://github.com/tikv/tikv
cd tikv

# Initialize cargo-rail
cargo rail init

# Configure exclusions (REQUIRED)
cat >> .config/rail.toml << 'EOF'
[unify]
exclude = ["uuid", "semver", "derive_more", "nom"]
exact_pin_handling = "skip"
EOF

# Check and apply unification
cargo rail unify --check
cargo rail unify
```

## Impact Summary

| Metric | Value |
|--------|-------|
| Workspace members | 83 |
| Dependencies unified | 56 |
| Member edits | **514** |
| Transitives pinned | 0 |

**MASSIVE IMPACT:** 514 manual file edits automated!

## Configuration

**Special configuration required** - tikv has multiple major versions of uuid, semver, derive_more, and nom (e.g., uuid 0.x and 1.x). These must be excluded to proceed.

```toml
# .config/rail.toml
[unify]
pin_transitives = false  # No workspace-hack
msrv = true
detect_unused = true
exclude = ["uuid", "semver", "derive_more", "nom"]  # Multi-version deps
exact_pin_handling = "skip"
```

## Notes

- Distributed transactional key-value store (PingCAP)
- Largest workspace tested (83 crates)
- Shows cargo-rail's diagnostic capabilities (detects multi-version conflicts)
- 514 file edits saved - massive enterprise impact
- Exclusion config demonstrates flexibility

Details in [`summary.toml`](./summary.toml).
