# vello + cargo-rail

**Repo:** <https://github.com/linebender/vello>
**Commit:** `56820883` (2025-11-29)

## Demo

![vello demo](./demo.gif)

## Commands

```bash
# Clone and enter
git clone https://github.com/linebender/vello
cd vello

# Initialize cargo-rail
cargo rail init

# Configure for virtual workspace (REQUIRED)
echo '[unify]' > .config/rail.toml
echo 'pin_transitives = false' >> .config/rail.toml

# Check and apply unification
cargo rail unify --check
cargo rail unify
```

## Impact Summary

| Metric | Value |
|--------|-------|
| Workspace members | 26 |
| Dependencies unified | 7 |
| Member edits | 17 |
| Transitives pinned | 0 |

**Dependencies unified:** guillotiere, hashbrown, roxmltree, env_logger, naga, getrandom, parley

## Configuration

**Special configuration required** - vello has internal path dependencies (scenes, with_winit) not published to crates.io. Transitive pinning would fail trying to resolve these.

```toml
# .config/rail.toml
[unify]
include_paths = true
include_renamed = false
pin_transitives = false  # REQUIRED - path deps can't be resolved from crates.io
transitive_host = "root"
exclude = []
include = []
msrv = false
strict_version_compat = true
exact_pin_handling = "warn"
detect_unused = false
```

## Notes

- GPU-accelerated 2D rendering library
- Virtual workspace with unpublished internal crates
- Shows cargo-rail's configuration flexibility
- Cross-platform graphics workspace

Details in [`summary.toml`](./summary.toml).
