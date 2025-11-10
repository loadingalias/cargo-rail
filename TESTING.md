# Testing Documentation for Conflict Resolution

## Overview

This document explains the testing strategy for cargo-rail's conflict resolution feature, particularly highlighting the differences between unit tests (in the implementation) and integration tests (in the test suite).

## Unit Tests vs Integration Tests

### Unit Tests (`src/core/conflict.rs`)

**Purpose**: Test the `ConflictResolver` in isolation using Git's `merge-file` command.

**Key Characteristics**:

- Use `TempDir` to create isolated temporary directories
- Test individual conflict resolution strategies directly
- Verify Git's merge-file behavior with controlled inputs
- Fast execution (< 100ms per test)
- No dependency on workspace structure or cargo-rail commands

**Test Coverage**:

1. `test_strategy_from_str` - Parse strategy strings (ours, theirs, manual, union)
2. `test_clean_merge` - Non-conflicting changes merge successfully
3. `test_conflict_detection` - Conflicting changes create markers with manual strategy
4. `test_ours_strategy` - `--ours` keeps monorepo version
5. `test_theirs_strategy` - `--theirs` uses remote version
6. `test_union_strategy` - `--union` combines both versions line-by-line

**Example** (test_ours_strategy):

```rust
let temp = TempDir::new().unwrap();
let resolver = ConflictResolver::new(ConflictStrategy::Ours, temp.path().to_path_buf());

let current_file = temp.path().join("test.txt");
std::fs::write(&current_file, "line 1\nline 2 current\nline 3\n").unwrap();

let base = b"line 1\nline 2\nline 3\n";
let incoming = b"line 1\nline 2 incoming\nline 3\n";

let result = resolver.resolve_file(&current_file, base, incoming).unwrap();

match result {
  MergeResult::Success => {
    let content = std::fs::read_to_string(&current_file).unwrap();
    assert!(content.contains("line 2 current"));  // Kept ours
    assert!(!content.contains("line 2 incoming")); // Rejected theirs
  }
  _ => panic!("Expected clean merge with --ours"),
}
```

### Integration Tests (`tests/integration/test_sync.rs`)

**Purpose**: Test end-to-end conflict resolution through the full cargo-rail sync workflow.

**Key Characteristics**:

- Create full workspace with crates, git repos, and cargo-rail configuration
- Use actual `cargo rail` commands via subprocess
- Test realistic scenarios: split repos, bidirectional sync, conflict resolution
- Slower execution (1-3 seconds per test)
- Test the complete integration: CLI → SyncEngine → ConflictResolver → Git

**Test Coverage**:

1. `test_conflict_resolution_ours_strategy` - End-to-end `--strategy=ours`
2. `test_conflict_resolution_theirs_strategy` - End-to-end `--strategy=theirs`
3. `test_conflict_resolution_manual_strategy` - End-to-end `--strategy=manual`
4. `test_conflict_resolution_union_strategy` - End-to-end `--strategy=union`
5. `test_no_conflict_with_non_overlapping_changes` - Clean merge with no conflicts

**Example** (test_conflict_resolution_ours_strategy):

```rust
// Create workspace and split a crate
let workspace = TestWorkspace::new()?;
workspace.add_crate("my-crate", "0.1.0", &[])?;
workspace.commit("Add my-crate")?;

run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;
// ... configure and split ...

// Create divergent changes
workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version v2\npub fn mono() {}")?;
workspace.commit("Monorepo change v2")?;

std::fs::write(split_dir.join("src/lib.rs"), "// Split version\npub fn split() {}")?;
git(&split_dir, &["add", "."])?;
git(&split_dir, &["commit", "-m", "Split change"])?;

// Sync with conflict resolution
run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--from-remote", "--strategy=ours"])?;

// Verify monorepo version wins
let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
assert!(lib.contains("Monorepo version v2"));
assert!(!lib.contains("Split version"));
```

## Key Differences

### 1. Scope

| Aspect | Unit Tests | Integration Tests |
|--------|-----------|------------------|
| Scope | ConflictResolver only | Full cargo-rail workflow |
| Dependencies | Git merge-file | Git, cargo, filesystem, config |
| Isolation | Fully isolated | Multi-repo setup |
| Speed | ~50ms | ~1-3s |

### 2. Test Setup Complexity

**Unit Tests**: Minimal setup

```rust
let temp = TempDir::new().unwrap();
let resolver = ConflictResolver::new(strategy, temp.path().to_path_buf());
```

**Integration Tests**: Complex workspace setup

```rust
let workspace = TestWorkspace::new()?;  // Creates Cargo.toml, git repo
workspace.add_crate("my-crate", "0.1.0", &[])?;  // Creates crate structure
workspace.commit("Add my-crate")?;  // Git commit
run_cargo_rail(&workspace.path, &["rail", "init", "--all"])?;  // cargo-rail config
// Configure remote paths, disable protected branches
run_cargo_rail(&workspace.path, &["rail", "split", "my-crate"])?;  // Split operation
// Sync to establish baseline
// Create divergent changes in both repos
// Run sync with strategy
```

### 3. Conflict Creation

**Unit Tests**: Direct file content manipulation

```rust
let base = b"line 1\nline 2\nline 3\n";
let incoming = b"line 1\nline 2 incoming\nline 3\n";
let current = "line 1\nline 2 current\nline 3\n";
```

**Integration Tests**: Real git commits

```rust
// Monorepo change
workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version v2")?;
workspace.commit("Monorepo change v2")?;

// Remote change
std::fs::write(split_dir.join("src/lib.rs"), "// Split version")?;
git(&split_dir, &["add", "."])?;
git(&split_dir, &["commit", "-m", "Split change"])?;
```

### 4. Assertion Strategy

**Unit Tests**: Verify merge algorithm behavior

```rust
match result {
  MergeResult::Success => { /* ... */ }
  MergeResult::Conflicts(paths) => { /* ... */ }
  MergeResult::Failed(msg) => { /* ... */ }
}
```

**Integration Tests**: Verify end-to-end correctness

```rust
let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
assert!(lib.contains("expected content"));
assert!(!lib.contains("rejected content"));
```

## Critical Test Configuration Differences

### Protected Branches

**Issue**: Integration tests must disable protected branch enforcement.

**Why**: cargo-rail's security model protects `main`/`master` by default, creating PR branches for remote→monorepo syncs. This breaks conflict resolution tests because:

1. Conflict resolution happens on the PR branch
2. Tests read from `master`, which hasn't changed
3. Tests fail despite correct conflict resolution

**Solution**:

```rust
let updated_config = config
  .replace(r#"remote = """#, &format!(r#"remote = "{}""#, split_dir.display()))
  .replace(r#"protected_branches = ["main"]"#, r#"protected_branches = []"#);
```

### Sync Baseline Establishment

**Issue**: Tests need a common ancestor for 3-way merge.

**Why**: The ConflictResolver performs 3-way merge using:

- Base: Last synced version (common ancestor)
- Ours: Current monorepo content
- Theirs: Incoming remote content

Without a sync baseline, there's no common ancestor, and merge behavior is undefined.

**Solution**:

```rust
// Make initial change
workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version")?;
workspace.commit("Monorepo change")?;

// Sync to remote (establishes baseline)
run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--to-remote"])?;

// NOW create divergent changes for conflict test
workspace.modify_file("my-crate", "src/lib.rs", "// Monorepo version v2")?;
std::fs::write(split_dir.join("src/lib.rs"), "// Split version")?;
```

## Test Reliability Considerations

### Known Issues

1. **Test Flakiness**: Some integration tests fail intermittently due to:
   - Race conditions in git operations
   - Temporary file cleanup timing
   - Test isolation in parallel execution

2. **Sequential Execution**: Use `--test-threads=1` for reliable results:

   ```bash
   cargo test test_conflict_resolution --release -- --test-threads=1
   ```

3. **Test Independence**: Integration tests should be run in isolation when debugging:

   ```bash
   cargo test test_conflict_resolution_ours_strategy --release
   ```

### Debugging Tips

**Enable Debug Output**:

```rust
let output = run_cargo_rail(&workspace.path, &["rail", "sync", "my-crate", "--from-remote", "--strategy=ours"])?;
eprintln!("=== Sync command output ===\n{}", String::from_utf8_lossy(&output.stdout));
```

**Check Actual File Content**:

```rust
let lib = workspace.read_file("crates/my-crate/src/lib.rs")?;
eprintln!("=== File content ===\n{}", lib);
assert!(lib.contains("expected"), "File content:\n{}", lib);
```

**Verify Branch State**:

```rust
let output = git(&workspace.path, &["branch", "-a"])?;
eprintln!("=== Branches ===\n{}", String::from_utf8_lossy(&output.stdout));
```

## Running Tests

```bash
# Run all tests
cargo test --release

# Run only unit tests
cargo test --lib --release

# Run only integration tests
cargo test --test integration --release

# Run conflict resolution tests specifically
cargo test test_conflict_resolution --release

# Run tests sequentially (more reliable)
cargo test --release -- --test-threads=1

# Run with output
cargo test test_conflict_resolution --release -- --nocapture
```

## Summary

- **Unit tests** verify the ConflictResolver works correctly in isolation
- **Integration tests** verify the full cargo-rail sync workflow with conflict resolution
- Both are necessary: unit tests for algorithm correctness, integration tests for end-to-end behavior
- Integration tests require careful setup (disable protected branches, establish sync baseline)
- Test flakiness is a known issue being addressed separately from the core implementation
