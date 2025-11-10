# Git-Notes Conflict Resolution

## Overview

cargo-rail uses git-notes to maintain bidirectional mappings between monorepo commits and split repository commits. When syncing between repositories, git-notes conflicts can occur when both sides have been updated independently.

## How Git-Notes Are Used

Git-notes store the mapping in the refs namespace:
```
refs/notes/rail/<crate-name>
```

Each note maps a monorepo commit SHA to its corresponding split repo commit SHA (or vice versa). This enables:
- Deduplication (skipping already-synced commits)
- Bidirectional sync tracking
- Merge conflict detection

## When Conflicts Occur

Git-notes conflicts happen when:

1. **Non-Fast-Forward Updates**: The remote git-notes have diverged from local notes
   ```
   ! [rejected]  refs/notes/rail/my-crate -> refs/notes/rail/my-crate  (non-fast-forward)
   ```

2. **Concurrent Syncs**: Multiple users/CI jobs sync the same crate simultaneously

3. **Manual Note Edits**: Direct modification of git-notes refs

## Automatic Conflict Resolution

cargo-rail automatically handles git-notes conflicts using the **union merge strategy**:

```bash
git notes merge --strategy=union refs/notes/rail/<crate-name>
```

### Union Strategy Behavior

The union strategy combines all notes from both sides:
- **No data loss**: All mappings from both local and remote are preserved
- **Automatic**: Merges without manual intervention
- **Safe**: Duplicate mappings are harmless (same SHA maps to same SHA)

### When Auto-Merge Works

✅ Most cases - union strategy succeeds automatically
✅ When mappings don't conflict (different commits mapped)
✅ When mappings are identical (same commits mapped to same SHAs)

### When Manual Resolution Is Needed

❌ Rare: When the same monorepo commit maps to different split commits
❌ When git-notes ref is corrupted or has invalid format

## Manual Resolution Steps

If automatic merge fails, cargo-rail provides clear instructions:

```
Error: Failed to merge git-notes automatically.

Manual resolution required:
1. Check conflicts: git notes show <sha>
2. Resolve manually: git notes edit <sha>
3. Re-run sync: cargo rail sync <crate> --from-remote
```

### Manual Resolution Process

1. **Inspect the conflict**:
   ```bash
   cd <split-repo>
   git notes --ref=refs/notes/rail/<crate-name> show <conflicting-sha>
   ```

2. **Choose resolution strategy**:
   - **Keep ours** (local mapping):
     ```bash
     git notes merge --strategy=ours refs/notes/rail/<crate-name>
     ```

   - **Keep theirs** (remote mapping):
     ```bash
     git notes merge --strategy=theirs refs/notes/rail/<crate-name>
     ```

   - **Manual edit**:
     ```bash
     git notes edit <sha>
     # Edit to correct mapping, save and exit
     ```

3. **Push resolved notes**:
   ```bash
   git push origin refs/notes/rail/<crate-name>
   ```

4. **Retry sync**:
   ```bash
   cargo rail sync <crate> --from-remote
   ```

## Best Practices

### Prevent Conflicts

1. **Sync regularly**: Frequent small syncs reduce divergence
2. **Coordinate CI**: Use locking or sequential sync jobs
3. **Protected refs**: Configure protected refs for notes in GitHub/GitLab settings

### Recovery

If git-notes become corrupted:

1. **Backup current notes**:
   ```bash
   git fetch origin refs/notes/rail/<crate>:refs/notes/rail/<crate>-backup
   ```

2. **Reset to remote**:
   ```bash
   git fetch origin refs/notes/rail/<crate>:refs/notes/rail/<crate>
   git push -f origin refs/notes/rail/<crate>
   ```

3. **Rebuild from scratch** (last resort):
   ```bash
   # Delete local notes
   git notes --ref=refs/notes/rail/<crate> remove $(git rev-list HEAD)

   # Delete remote notes
   git push origin :refs/notes/rail/<crate>

   # Re-split to rebuild mappings
   cargo rail split <crate>
   ```

## Troubleshooting

### "Non-fast-forward" Error

**Symptom**:
```
! [rejected]  refs/notes/rail/my-crate -> refs/notes/rail/my-crate  (non-fast-forward)
```

**Solution**: This is automatically handled by cargo-rail's union merge strategy. If you see this error, ensure you're using cargo-rail v1.0+ which includes automatic conflict resolution.

### "Conflicting notes" Error

**Symptom**:
```
error: Automatic notes merge failed. Fix conflicts in refs/notes/commits and commit the result with 'git notes merge --commit'
```

**Solution**:
1. Manual resolution required (rare)
2. Follow manual resolution steps above
3. Contact maintainers if issue persists

### Duplicate Mappings

**Symptom**: Same commit appears to be synced multiple times

**Cause**: Usually harmless - union merge combined identical mappings

**Solution**: No action needed - deduplication logic handles this

## Technical Details

### Git-Notes Format

Each note is a simple text mapping:
```
<monorepo-sha> <split-repo-sha>
```

### Fetch and Merge Flow

1. `git fetch origin refs/notes/rail/<crate>:refs/notes/rail/<crate>`
2. Detect non-fast-forward
3. Attempt `git notes merge --strategy=union`
4. On success: Continue sync
5. On failure: Provide manual resolution instructions

### Code References

- Conflict detection: `crates/cargo-rail/src/core/mapping.rs:fetch_notes()`
- Union merge: `crates/cargo-rail/src/core/mapping.rs:merge_notes()`
- Error handling: `crates/cargo-rail/src/core/mapping.rs:handle_merge_conflict()`

## See Also

- [Git Notes Documentation](https://git-scm.com/docs/git-notes)
- [User Guide](./USER_GUIDE.md)
- [Troubleshooting](./TROUBLESHOOTING.md)
