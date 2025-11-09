# cargo-rail

**Monorepo split/sync for Rust workspaces**

Split crates from a monorepo into standalone repositories while preserving full git history. Bidirectional sync keeps monorepo and split repos in perfect harmony.

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

## Quick Start

```bash
# Initialize cargo-rail in your workspace
cd your-workspace/
cargo rail init

# Configure a crate to split (edit rail.toml)
# Then split it out
cargo rail split your-crate

# Sync changes bidirectionally
cargo rail sync your-crate
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
