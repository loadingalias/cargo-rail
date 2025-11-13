# cargo-rail Pre-v1.0 Checklist

**Current Version**: v0.1.0
**Target Version**: v1.0.0
**Last Updated**: 2025-11-13

**Focus**: Split & sync for Rust monorepos. Lean, fast, trustworthy.

---

## ✅ Already Complete

- ✅ Core split/sync functionality (single + combined modes)
- ✅ Bidirectional sync with PR branch security model
- ✅ Git-notes commit mapping (rebase-tolerant)
- ✅ Cargo.toml transforms (path ↔ version deps)
- ✅ Conflict resolution (ours, theirs, manual, union)
- ✅ Health checks (`cargo rail doctor`)
- ✅ CI coverage (4 platforms: Linux/Windows x86_64/ARM64)
- ✅ **Release system removed** (delegated to cargo-release/release-plz)
- ✅ **114 dependencies removed** (394→280 crates, -29%)
- ✅ E2E testing phases 1-5 completed
- ✅ Documentation (README, USER_GUIDE, SECURITY)
- ✅ All 53 tests passing, zero warnings

---

## 🎯 Critical Path to v1.0

### 1. Complete E2E Testing (Phases 7-12)

**Remaining test phases** from E2E_TESTING_SETUP.md:

#### Phase 7: Edge Cases & Error Handling (~1 day)
- [ ] **Test 7.1**: Missing config error handling
- [ ] **Test 7.2**: Invalid crate name errors
- [ ] **Test 7.3**: Network error handling (simulated)

**Priority**: MEDIUM (tests existing functionality)

---

#### Phase 8: Performance & Robustness (~1 day)
- [ ] **Test 8.1**: Large history (50+ commits)
  - Verify completes in <30s
  - Progress indicators work
- [ ] **Test 8.2**: Rebase handling
  - Create feature branch, rebase, sync
  - Git-notes update correctly
  - No duplicate commits

**Priority**: HIGH (validates production scenarios)

---

#### Phase 9: Additional Edge Cases (~0.5 day)
- [ ] **Test 9.1**: Large file handling (10MB+)
  - Optional: Suggest git-lfs
- [ ] **Test 9.2**: Empty crate error handling
- [ ] **Test 9.3**: Unicode in commit messages
  - Preserve emoji, non-ASCII correctly
- [ ] **Test 9.4**: Force-push detection
  - Warn about rewritten history
  - Do NOT auto force-push

**Priority**: MEDIUM

---

#### Phase 10: Platform-Specific (~1 day)
- [ ] **Test 10.1**: Help commands on all platforms
- [ ] **Test 10.2**: Install from source (Linux/macOS/Windows)
- [ ] **Test 10.3**: macOS Keychain integration
- [ ] **Test 10.4**: Windows path handling (C:\, backslashes)

**Priority**: HIGH (cross-platform validation)

---

#### Phase 11: Performance Benchmarks (~0.5 day)
- [ ] **Test 11.1**: Sync dry-run performance
  - Small crate: <50ms
  - Medium crate: <150ms
  - Large crate: <500ms
- [ ] **Test 11.2**: Split performance
  - 100 commits: <5s
  - 500 commits: <30s
  - 1000 commits: <60s

**Priority**: MEDIUM (optimization targets)

---

#### Phase 12: Regression Testing (~0.5 day)
- [ ] **Test 12.1**: Re-run all core tests (Phases 1-5)
  - Verify no regressions
- [ ] **Test 12.2**: Data loss verification
  - Compare monorepo vs split repo files
  - Ensure no missing/extra files

**Priority**: HIGH (quality gate)

**Total E2E Effort**: ~4-5 days

---

### 2. VCS Abstraction + System Git Backend (~3-4 days)

**Goal**: Replace gix (200+ deps) with system git (zero deps)

**Why**:
- gix dependency tree: 200+ crates (heavy)
- We use <5% of functionality
- System git is battle-tested, zero deps
- Enables jj (jujutsu) support for free

#### Phase 2.1: VCS Trait Design (~0.5 day)
- [ ] Create `src/core/vcs/mod.rs` with VCS trait
- [ ] Define capabilities (notes, partial_clone, open_pr)
- [ ] Minimal trait surface (head, rev_list_paths, cat_blob, etc.)

#### Phase 2.2: SystemGit Backend (~1.5 days)
- [ ] Implement `src/core/vcs/system_git.rs`
- [ ] Safe subprocess wrapper with env isolation
- [ ] Map all VCS trait methods to git plumbing commands:
  - `head()` → `git rev-parse HEAD`
  - `rev_list_paths()` → `git rev-list --no-merges <range> -- <paths>`
  - `cat_blob()` → `git show <commit>:<path>`
  - `notes_get/set()` → `git notes --ref=refs/notes/rail/<name>`
- [ ] Security hardening (no global config, whitelisted env vars)
- [ ] Add timeouts and offline mode

#### Phase 2.3: jj (Jujutsu) Support (~0.5 day)
- [ ] Implement `src/core/vcs/jj.rs`
- [ ] Auto-detect `.jj/` directory
- [ ] Wrap SystemGit with jj export/import hooks
- [ ] Zero-friction: same UX for git and jj users

#### Phase 2.4: Integration (~1 day)
- [ ] Update all call sites to use VCS trait
- [ ] Remove gix from Cargo.toml
- [ ] Add `cargo rail doctor` check for git >= 2.33
- [ ] Run full test suite (use mock VCS for tests)
- [ ] Test with real jj repository

#### Phase 2.5: Optional git2 Feature (Post-v1.0)
- [ ] Feature-gate libgit2 backend for performance
- [ ] Binary distribution: default (system-git), optional (git2)

**Impact**: ~200 crates removed (280→80), 71% reduction
**Priority**: HIGH (biggest dependency win)

---

### 3. Workspace Mode for Combined Splits (~1 day)

**Current**: Combined mode creates standalone crates in one repo
**Goal**: Add option to create proper workspace structure

#### Implementation
- [ ] Support `workspace_mode = "workspace"` in config
- [ ] **Option A** (Standalone): Current behavior ✅ Already works
- [ ] **Option B** (Workspace): New behavior
  - [ ] Copy workspace Cargo.toml from monorepo
  - [ ] Preserve `[workspace.dependencies]`
  - [ ] Create proper workspace structure
  - [ ] Update path deps to workspace deps

**Config field already exists**, implementation needed.

**Priority**: MEDIUM (enhancement, not blocker)

---

### 4. Documentation Updates (~0.5 day)

#### Remove Release References
- [x] ~~README.md~~ ✅ Already updated
- [ ] docs/USER_GUIDE.md - Remove release sections
- [ ] docs/RELEASE_GUIDE.md - **DELETE** (no longer relevant)
- [ ] Update docs to recommend cargo-release/release-plz

#### Update for New Focus
- [ ] Add "Publishing Workflow" section to USER_GUIDE
- [ ] Document jj (jujutsu) support once implemented
- [ ] Update comparison table (focus on split/sync)

**Priority**: HIGH (user-facing)

---

## 📊 Post-v1.0 Enhancements

**Not required for v1.0, but valuable:**

### Checks System (~2-3 days)
Framework for policy enforcement and validation:
- [ ] `cargo rail check` command
- [ ] `transform.reproducible` - Byte-exact round-trip
- [ ] `policy.deny_paths` - Block secrets (*.pem, *.key, .env*)
- [ ] `policy.protected_branches` - Enforce PR workflow
- [ ] `build.test` - Optional cargo check/test
- [ ] Configurable in rail.toml
- [ ] JSON output for CI
- [ ] `--explain` flag

### Exit Code Taxonomy (~0.5 day)
Structured error reporting for CI:
- 0 = success
- 1 = user error
- 2 = system error
- 3 = validation error
- 10 = policy failure
- 20 = transform failure

### GitHub Actions Security (~1-2 days)
Port from rail:
- [ ] `.github/actions-lock.yaml` - Pin actions to SHAs
- [ ] `cargo rail ci lock` - Generate lock file
- [ ] `cargo rail ci verify` - Enforce pinning
- [ ] `cargo rail ci audit` - Run zizmor
- [ ] `cargo rail ci update` - Update SHAs, open PR

### Change Detection (~1 day)
Smart CI optimization:
- [ ] Three-tier strategy (docs/infra/source)
- [ ] BFS transitive dependency analysis
- [ ] `dorny/paths-filter` integration

---

## 🚀 v1.0 Release Checklist

When all critical path items complete:

### Pre-Release
1. [ ] All E2E tests pass (Phases 1-12)
2. [ ] VCS abstraction complete + gix removed
3. [ ] Documentation updated
4. [ ] All 53+ tests passing on 4 CI platforms
5. [ ] Zero compiler + clippy warnings
6. [ ] Dependencies: <100 crates (target: ~80)

### Release Process
7. [ ] Update version to `1.0.0` in Cargo.toml
8. [ ] Update CHANGELOG.md
9. [ ] Commit: `git commit -m "chore: Release v1.0.0"`
10. [ ] Tag: `git tag -a v1.0.0 -m "Release v1.0.0"`
11. [ ] Push: `git push origin main --tags`
12. [ ] Verify GitHub Release builds
13. [ ] Publish: `cargo publish`
14. [ ] Announce: r/rust, Discord, Twitter

---

## 📈 Metrics

**Current State:**
- Dependencies: 280 unique crates
- Tests: 53 passing (19 integration + 34 unit)
- Commands: 6 core (init, split, sync, doctor, status, mappings)
- Warnings: 0

**v1.0 Target:**
- Dependencies: <80 unique crates (-71% from current)
- Tests: 60+ passing (additional E2E coverage)
- Commands: Same 6 core + optional checks/ci (post-v1.0)
- Warnings: 0

---

## 🎯 Timeline Estimate

**Critical Path (Required for v1.0):**
- E2E Testing: 4-5 days
- VCS Abstraction: 3-4 days
- Workspace Mode: 1 day
- Documentation: 0.5 day
- **Total: 9-11 days**

**Post-v1.0 Features:**
- Checks System: 2-3 days
- GitHub Actions Security: 1-2 days
- Change Detection: 1 day
- Exit Codes: 0.5 day

---

## 🧭 Philosophy

**cargo-rail is infrastructure for Rust shops**

- **Minimal attack surface** - <80 dependencies (71% reduction from 280)
- **Battle-tested tools** - Prefer system git over pure-Rust libs
- **Single responsibility** - Split/sync only, delegate releases
- **Zero friction** - jj (jujutsu) support with zero config
- **Trust through simplicity** - Auditable in hours, not days

---

**Ready to ship v1.0 when critical path complete.**
