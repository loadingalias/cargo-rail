# cargo-rail

**The Rust Monorepo Tool**

Split Rust crates from Cargo workspaces into standalone repositories while preserving full git history. Bidirectional sync keeps your monorepo and split repos in perfect harmony.

---

## ALPHA SOFTWARE - NOT PRODUCTION READY

**DO NOT USE IN PROD YET**

cargo-rail is under active development. The core split/sync functionality works, but critical features are still being ironed out:

- [ ] Security model hardening (in progress)
- [ ] Comprehensive error handling
- [ ] Production testing
- [ ] Full documentation

**Expected production-ready: v1.0 (2-3 weeks)**

Until then, use only for testing and experimentation.

---

## Key Features

- **Dry-run by default** - All operations show a plan first; `--apply` required to execute
- **Full git history preservation** - Every commit touching your crate is preserved with original author, date, and message
- **Bidirectional sync** - Changes flow both ways: monorepo ↔ split repo
- **Protected branch safety** - Never commits directly to `main`/`master` when syncing from remote; creates PR branches instead
- **Cargo.toml transforms** - Automatically converts workspace dependencies to version dependencies
- **Git-notes mapping** - Tracks commit relationships for perfect deduplication
- **Conflict resolution** - Multiple strategies: `ours`, `theirs`, `manual`, `union`

---

## Quick Start

```bash
# Initialize cargo-rail in your workspace
cd your-workspace/
cargo rail init

# Configure a crate to split (edit rail.toml)
# Preview what will happen (dry-run by default)
cargo rail split your-crate

# Actually perform the split
cargo rail split your-crate --apply

# Preview sync changes (dry-run by default)
cargo rail sync your-crate

# Actually sync changes bidirectionally
cargo rail sync your-crate --apply
```

## Security Model (v1.0)

**monorepo → remote repo:**

- Pushes automatically to `main` (or configured branch)
- Requires SSH key
- Optional: SSH signing key

**remote repo → monorepo:**

- **NEVER commits directly to main/master**
- Automatically creates PR branch: `rail/sync/{crate}/{timestamp}`
- Requires SSH key
- Optional: SSH signing key
- Always requires review before merging

This ensures your monorepo `main` branch is protected from accidental/malicious changes from split repos.
