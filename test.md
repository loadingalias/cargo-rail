# cargo-rail Test Results

## Summary

| Repo        | Members | Lock Deps | Rust LoC | Demo Focus              | Status  |
|-------------|---------|-----------|----------|-------------------------|---------|
| tokio       | 10      | 214       | 167K     | unify + affected        | ✅ PASS |
| polars      | 33      | 579       | 433K     | unify --check           | ✅ PASS |
| ruff        | 43      | 514       | 547K     | affected --since HEAD~5 | ✅ PASS |
| vello       | 26      | 537       | 66K      | unify                   | ✅ PASS |
| helix       | 13      | 327       | 125K     | affected                | ✅ PASS |
| tikv        | 83      | 758       | 632K     | unify                   | ✅ PASS |
| iced        | 71      | 772       | 97K      | unify + test            | ✅ PASS |
| meilisearch | 19      | 758       | 210K     | affected                | ✅ PASS |
| jj          | 5       | 505       | 218K     | N/A                     | ⚠️ N/A  |
| ripgrep     | 10      | 61        | 52K      | unify                   | ✅ PASS |

---

## Real-World Impact Metrics

### Unification Impact (Verified After Hard Reset)

| Repo        | Deps Unified | Member Edits | Transitives Pinned | Manual Effort Saved |
|-------------|--------------|--------------|--------------------|--------------------|
| tokio       | 10           | 35           | 0                  | ~35 file edits     |
| polars      | 2            | 4            | 0                  | ~4 file edits      |
| vello       | 7            | 17           | 0                  | ~17 file edits     |
| helix       | 16           | 66           | 0                  | ~66 file edits     |
| tikv        | 56           | 514          | 0                  | ~514 file edits    |
| iced        | 6            | 20           | 164                | ~184 file edits    |
| meilisearch | 46           | 209          | 0                  | ~209 file edits    |
| ripgrep     | 9            | 35           | 5                  | ~40 file edits     |
| ruff        | 0            | 0            | 86                 | N/A (use affected) |
| jj          | 0            | 0            | 107                | N/A (well-maintained) |

### CI Speedup Estimates (affected command)

| Repo        | Total Crates | Affected | Test Targets | Skip Ratio | Estimated Speedup |
|-------------|--------------|----------|--------------|------------|-------------------|
| ruff        | 43           | 22       | 23           | 47%        | ~2x               |
| helix       | 13           | 4        | 5            | 62%        | ~2.6x             |
| meilisearch | 19           | 9        | 10           | 47%        | ~2x               |

**Note:** Actual speedup depends on test duration per crate. Crates with heavy tests see larger gains.

---

## Detailed Results

### 1. tokio ✅

**Demo Focus:** unify + affected

```
Codebase:
  - Members: 10
  - Lock dependencies: 214
  - Rust LoC: 167K

Unification:
  - Dependencies unified: 10
  - Member edits: 35
  - Transitives pinned: 0
  - Build: ✅ Verified
```

**Dependencies unified:** loom, rand, parking_lot, bytes, slab, tracing, tempfile, tokio, libc, futures

**Ideal rail.toml:** Default (use `cargo rail init`)

---

### 2. polars ✅

**Demo Focus:** unify --check

```
Codebase:
  - Members: 33
  - Lock dependencies: 579
  - Rust LoC: 433K

Unification:
  - Dependencies unified: 2
  - Member edits: 4
  - Transitives pinned: 0
  - Build: ✅ Verified (polars-core)
```

**Dependencies unified:** getrandom, lz4

**Note:** polars is already well-maintained - few unification opportunities.

**Ideal rail.toml:** Default (use `cargo rail init`)

---

### 3. ruff ✅

**Demo Focus:** affected --since HEAD~5

```
Codebase:
  - Members: 43
  - Lock dependencies: 514
  - Rust LoC: 547K

Affected Analysis:
  - Changed files: 31
  - Direct affected: 9
  - Transitive affected: 22
  - Test targets: 23 (vs 43 total = 47% skip)

Unification:
  - Dependencies unified: 0 (already well-maintained)
  - Transitives to pin: 86
```

**CI Impact:** Instead of testing all 43 crates, only 23 need testing - **47% reduction**.

**Note:** ruff is already well-maintained - no unification needed. Use `affected` for demos.

**Ideal rail.toml:** Default (use `cargo rail init`)

---

### 4. vello ✅

**Demo Focus:** unify

```
Codebase:
  - Members: 26
  - Lock dependencies: 537
  - Rust LoC: 66K

Unification:
  - Dependencies unified: 7
  - Member edits: 17
  - Transitives pinned: 0
  - Build: ✅ Verified
```

**Dependencies unified:** guillotiere, hashbrown, roxmltree, env_logger, naga, getrandom, parley

**ISSUE:** Virtual workspace with internal path dependencies (scenes, with_winit) not published to crates.io. Transitive pinning fails.

**REQUIRED rail.toml:**

```toml
[unify]
pin_transitives = false  # REQUIRED - path deps can't be resolved from crates.io
```

---

### 5. helix ✅

**Demo Focus:** affected

```
Codebase:
  - Members: 13
  - Lock dependencies: 327
  - Rust LoC: 125K

Affected Analysis:
  - Changed files: 13
  - Direct affected: 2 (helix-term, helix-vcs)
  - Transitive affected: 4
  - Test targets: 5 (vs 13 total = 62% skip)
```

**CI Impact:** Instead of testing all 13 crates, only 5 need testing - **62% reduction**.

**Bonus:** Also detects infrastructure changes (CI workflows).

**Ideal rail.toml:** Default (use `cargo rail init`)

---

### 6. tikv ✅

**Demo Focus:** unify

```
Codebase:
  - Members: 83
  - Lock dependencies: 758
  - Rust LoC: 632K

Unification:
  - Dependencies unified: 56
  - Member edits: 514
  - Transitives pinned: 0
```

**MASSIVE IMPACT:** 514 manual file edits automated!

**ISSUE:** Multiple major versions of uuid, semver, derive_more, nom must be excluded.

**REQUIRED rail.toml:**

```toml
[unify]
exclude = ["uuid", "semver", "derive_more", "nom"]  # Multi-version deps
exact_pin_handling = "skip"
```

**Alternative demo:** Show `--check` first as diagnostic, then fix with exclusions.

---

### 7. iced ✅

**Demo Focus:** unify + test

```
Codebase:
  - Members: 71
  - Lock dependencies: 772
  - Rust LoC: 97K

Unification:
  - Dependencies unified: 6
  - Member edits: 20
  - Transitives pinned: 164
  - Build: ✅ Verified
```

**Dependencies unified:** reqwest, serde, rand, webbrowser, iced, serde_json

**Note:** 164 transitives pinned = workspace-hack replacement!

**Ideal rail.toml:** Default (use `cargo rail init`)

---

### 8. meilisearch ✅

**Demo Focus:** affected

```
Codebase:
  - Members: 19
  - Lock dependencies: 758
  - Rust LoC: 210K

Affected Analysis:
  - Changed files: 34
  - Direct affected: 5
  - Transitive affected: 9
  - Test targets: 10 (vs 19 total = 47% skip)

Unification (bonus):
  - Dependencies to unify: 46
  - Member edits: 209
```

**CI Impact:** 47% test reduction. Plus 209 manual edits saved with unify!

**Ideal rail.toml:** Default (use `cargo rail init`)

---

### 9. jj ⚠️

**Demo Focus:** N/A (nothing to unify)

```
Codebase:
  - Members: 5
  - Lock dependencies: 505
  - Rust LoC: 218K

Unification:
  - Dependencies unified: 0
  - Transitives to pin: 107
```

**ISSUE:** No direct dependencies need unification - already well-maintained.

**Recommendation:** Remove from demo list or show as "best practices" example.

---

### 10. ripgrep ✅

**Demo Focus:** unify

```
Codebase:
  - Members: 10 (NOT 1!)
  - Lock dependencies: 61
  - Rust LoC: 52K

Unification:
  - Dependencies unified: 9
  - Member edits: 35
  - Transitives pinned: 5
```

**Dependencies unified:** serde_json, regex, serde, bstr, memchr, termcolor, globset, walkdir, log

**CORRECTION:** ripgrep is NOT a single crate! It benefits from unification.

**Ideal rail.toml:** Default (use `cargo rail init`)

---

## Demo Recommendations

| Original Plan                 | Recommendation                                   |
|-------------------------------|--------------------------------------------------|
| ripgrep: "overkill (1 crate)" | ❌ Incorrect - has 10 crates, show unify         |
| tikv: unify                   | ✅ Show with exclusions (514 edits saved!)       |
| jj: unify                     | ⚠️ Remove or replace - nothing to unify          |
| vello: unify + test           | ⚠️ unify only, needs `pin_transitives = false`   |
| ruff: affected                | ✅ Great demo - 47% CI reduction                 |
| meilisearch: affected         | ✅ Can also show unify (209 edits!)              |

---

## Compelling Demo Metrics

### For README "Real-World Impact" Section

| Monorepo    | Crates | Deps Unified | Edits Saved | CI Reduction |
|-------------|--------|--------------|-------------|--------------|
| tikv        | 83     | 56           | 514         | N/A          |
| meilisearch | 19     | 46           | 209         | 47%          |
| helix       | 13     | 16           | 66          | 62%          |
| tokio       | 10     | 10           | 35          | N/A          |
| ruff        | 43     | N/A          | N/A         | 47%          |

### Key Talking Points

1. **tikv:** "56 dependencies unified, 514 file edits automated"
2. **helix:** "62% CI time savings - only test what changed"
3. **meilisearch:** "46 deps unified + 47% faster CI"
4. **iced:** "164 transitives pinned - replaces workspace-hack"

---

## Pre-Tape Checklist

For each repo before recording:

1. `git reset --hard origin/<branch> && git clean -fd`
2. `rm -rf .config/rail.toml target/cargo-rail`
3. Copy required rail.toml (vello, tikv only)
4. Run demo command once to verify output
5. Clear terminal, start recording

---

## Special Configurations Reference

### vello - Virtual workspace with unpublished path deps

```toml
[unify]
pin_transitives = false
```

### tikv - Multiple major version deps to exclude

```toml
[unify]
exclude = ["uuid", "semver", "derive_more", "nom"]
exact_pin_handling = "skip"
```

---

## Issues Found

### 1. Virtual Workspace Transitive Pinning

**Repos affected:** vello

When a virtual workspace has path dependencies not published to crates.io, transitive pinning fails.

**Workaround:** Set `pin_transitives = false`

**Potential fix:** Auto-detect unpublished path deps and skip/warn.

### 2. Multiple Major Versions Detection

**Repos affected:** tikv

cargo-rail correctly detects multiple major versions (e.g., uuid 0.x and 1.x) as an anti-pattern.

**Workaround:** Add to `exclude` list.

**Note:** This is tikv's technical debt, not a cargo-rail bug.

### 3. Well-Maintained Workspaces

**Repos affected:** jj, polars, ruff (for unify)

Some workspaces already follow best practices - nothing to unify.

**Solution:** Use `affected` command instead for CI optimization demos.

---

## Comparison: cargo-rail vs cargo-hakari

| Feature                    | cargo-hakari        | cargo-rail unify     |
|----------------------------|---------------------|----------------------|
| Requires extra crate       | Yes (workspace-hack)| No                   |
| Preserves TOML comments    | No                  | Yes                  |
| Multi-target aware         | No                  | Yes                  |
| Feature computation        | Union (bloated)     | Intersection (lean)  |
| CI affected detection      | No                  | Yes (`affected`)     |
| Works on any workspace     | Needs setup         | Zero config          |

---

## VHS Demo Recording

### Setup

```bash
brew install vhs
brew install gifsicle  # for optimization
```

### Recording

```bash
# Record a demo
vhs demos/tikv.tape

# Optimize file size
gifsicle -O3 --lossy=80 demos/tikv.gif -o demos/tikv-optimized.gif
```

### Available Tape Scripts

| Script | Focus | Key Metric |
|--------|-------|------------|
| `demos/readme-hero.tape` | Core loop for README | All features |
| `demos/tikv.tape` | Massive unification | 514 edits saved |
| `demos/helix.tape` | CI reduction | 62% faster |
| `demos/meilisearch.tape` | Both features | 46 deps + 47% CI |
| `demos/iced.tape` | Transitives | 164 pinned |
| `demos/tokio.tape` | Recognizable | 10 deps |
| `demos/ruff.tape` | CI savings | 47% reduction |
| `demos/vello.tape` | Config flexibility | Virtual workspace |
| `demos/ripgrep.tape` | Surprise | 10 crates! |
| `demos/polars.tape` | Well-maintained | 2 deps |

---

## Suggested GIF Order (Most Impressive First)

1. **tikv unify** - 514 edits saved, massive workspace
2. **helix affected** - 62% CI reduction, clean output
3. **meilisearch unify + affected** - Both features, enterprise use case
4. **iced unify** - 164 transitives, workspace-hack replacement
5. **tokio unify + affected** - Recognizable project
6. **ruff affected** - Large workspace, CI savings
7. **vello unify** - Graphics, shows config flexibility
8. **ripgrep unify** - Surprise factor (not a single crate!)
9. **polars unify --check** - Shows "already good" state
