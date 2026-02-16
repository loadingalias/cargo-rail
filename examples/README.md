# Examples

Simple, copy-paste-first examples for cargo-rail workflows.

## Change Detection

Progressive configs from minimal to full-featured:

| Example | Description |
|---------|-------------|
| [01-minimal](change_detection/01-minimal/) | Smallest usable setup |
| [02-balanced-monorepo](change_detection/02-balanced-monorepo/) | Practical baseline (recommended) |
| [03-strict-bot-pr](change_detection/03-strict-bot-pr/) | Stricter mode for bot PRs |
| [04-github-actions](change_detection/04-github-actions/) | Minimal planner-first GHA workflow |
| [05-custom-profiles](change_detection/05-custom-profiles/) | Custom profiles and workflow mapping |

## Unify

- [unify/](unify/) — Config + validation results

**Validated on 4 production repos (53 crates):** [VALIDATION_RESULTS.md](unify/VALIDATION_RESULTS.md)

## Split/Sync

- [split-sync/](split-sync/) — Config template (sandbox first)

## Release

- [release/](release/) — Config template (check mode only)

---

## Real-World Configs

Full production configs validated on real repositories:

| Repository | Config | Integration Guide | Metrics |
|------------|--------|-------------------|---------|
| tokio | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/tokio/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/tokio/docs/cargo-rail-integration-guide.md) | [Metrics](https://github.com/loadingalias/cargo-rail-testing/blob/main/tokio/docs/CHANGE_DETECTION_METRICS.md) |
| helix | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix/docs/cargo-rail-integration-guide.md) | [Metrics](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix/docs/CHANGE_DETECTION_METRICS.md) |
| meilisearch | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/meilisearch/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/meilisearch/docs/cargo-rail-integration-guide.md) | [Metrics](https://github.com/loadingalias/cargo-rail-testing/blob/main/meilisearch/docs/CHANGE_DETECTION_METRICS.md) |
| helix-db | [rail.toml](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix-db/.config/rail.toml) | [Guide](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix-db/docs/cargo-rail-integration-guide.md) | [Metrics](https://github.com/loadingalias/cargo-rail-testing/blob/main/helix-db/docs/CHANGE_DETECTION_METRICS.md) |

**Validation forks**: [cargo-rail-testing](https://github.com/loadingalias/cargo-rail-testing)

---

All examples are intentionally minimal and designed to avoid ornamental complexity.
