# Change Detection Examples

Two approaches based on your project's tooling:

## Choose Your Pattern

| Pattern | For Projects With | Strategy |
|---------|-------------------|----------|
| [**with-task-runner/**](with-task-runner/) | just, make, xtask, scripts | `cargo rail plan` + your task runner |
| [**standalone/**](standalone/) | No task runner | `cargo rail run` handles everything |

## Measured Impact (Last 20 Commits per Repo)

Generated with `/Users/mr.wolf/loadingalias/cargo-rail-testing/scripts/measure-impact.sh`.

| Repository | Could Skip Build | Could Skip Tests | Targeted (Not Full Run) |
|---|---:|---:|---:|
| tokio | 10% | 0% | 95% |
| meilisearch | 35% | 35% | 60% |
| helix | 30% | 30% | 40% |
| helix-db | 10% | 10% | 75% |
| **Aggregate (80 commits)** | **21%** | **19%** | **68%** |

Interpretation:
- Task-runner repos (`meilisearch`, `helix`) show the largest immediate test/build skip savings.
- Standalone repos still gain deterministic targeting and explainable plan traces.

## With Task Runner (Recommended for Large Projects)

If you already have **just**, **make**, **xtask**, or shell scripts:

```bash
# cargo-rail provides change detection
PLAN=$(cargo rail plan --merge-base -f json)

# Your task runner handles execution
if echo "$PLAN" | jq -e '.surfaces.test.enabled' > /dev/null; then
  cargo xtask test        # or: just test, make test, ./scripts/test.sh
fi
```

**Why?**
- cargo-rail stays focused on change detection
- Your existing build logic doesn't change
- Full control over execution
- No lock-in

**Real examples:** [helix](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix), [meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/main/meilisearch)

## Standalone (Simpler, Less Flexible)

If you don't have a task runner:

```bash
# cargo-rail handles both detection and execution
cargo rail run --merge-base --surface test
cargo rail run --workflow ci
```

**Why?**
- Single command for plan + execute
- No scripting required
- Built-in surfaces: build, test, bench, docs

**Real examples:** [tokio](https://github.com/loadingalias/cargo-rail-testing/tree/main/tokio), [helix-db](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix-db)

## Configuration Reference

### Infrastructure Files

Files that trigger full workspace rebuild:

```toml
[change-detection]
infrastructure = [
  ".github/**",       # CI changes
  "scripts/**",       # Build scripts
  "justfile",         # Task runner
  "deny.toml",        # License/security
  "rust-toolchain.toml",
  "Cargo.toml",
  "Cargo.lock",
]
```

### Confidence Profiles

| Profile | Behavior |
|---------|----------|
| `strict` | Conservative — runs more, misses less |
| `balanced` | Default — good tradeoff |
| `fast` | Aggressive — skips transitive checks |

```toml
[change-detection]
confidence_profile = "balanced"
bot_pr_confidence_profile = "strict"  # Override for dependabot, etc.
```

### Custom Surfaces

Detect non-Rust asset changes:

```toml
[change-detection.custom]
themes = ["runtime/themes/**"]
queries = ["runtime/queries/**"]
workloads = ["workloads/**"]
```

Plan output includes `custom:themes`, `custom:queries`, etc.

## Validation

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail run --merge-base --dry-run --print-cmd
```

## See Also

- [Configuration Reference](../../docs/config.md)
- [Troubleshooting](../../docs/troubleshooting.md)
- [Validation Forks](https://github.com/loadingalias/cargo-rail-testing)
