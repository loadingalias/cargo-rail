# Examples

Copy-paste-ready examples for cargo-rail workflows.

## By Workflow

| Workflow | Example | Description |
|----------|---------|-------------|
| **Config** | [config/](config/) | Full annotated `rail.toml` |
| **Change Detection** | [change_detection/](change_detection/) | Plan + run patterns |
| **Unify** | [unify/](unify/) | Dependency unification |
| **Split/Sync** | [split-sync/](split-sync/) | Crate extraction and sync |
| **Release** | [release/](release/) | Version, changelog, publish |

## Change Detection Patterns

| Pattern | For Projects With | Link |
|---------|-------------------|------|
| **With task runner** | just, make, xtask | [change_detection/with-task-runner/](change_detection/with-task-runner/) |
| **Standalone** | No task runner | [change_detection/standalone/](change_detection/standalone/) |

## Real-World Configs

Full production configs validated on real repositories:

| Repository | Config | Integration Guide |
|------------|--------|-------------------|
| [tokio](https://github.com/loadingalias/cargo-rail-testing/tree/main/tokio) | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/tokio/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/tokio/docs/cargo-rail-integration-guide.md) |
| [helix](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix) | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix/docs/cargo-rail-integration-guide.md) |
| [meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/main/meilisearch) | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/meilisearch/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/meilisearch/docs/cargo-rail-integration-guide.md) |
| [helix-db](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix-db) | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix-db/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix-db/docs/cargo-rail-integration-guide.md) |

**Validation forks**: [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing)

## Validation Results

- **Unify**: [unify-results.md](unify/unify-results.md) — 96 deps unified, 258 undeclared features fixed across 53 crates
- **Change Detection**: See each fork's `docs/CHANGE_DETECTION_METRICS.md`

## Quick Validation

```bash
cargo rail config validate --strict
cargo rail plan --merge-base --explain
cargo rail unify --check
```
