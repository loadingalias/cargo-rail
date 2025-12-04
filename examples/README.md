# Examples

Demo videos and configuration examples for cargo-rail workflows.

## Quick Start

```bash
# Record all demo videos
./examples/record-demos.sh

# Record specific workflow
./examples/record-demos.sh unify
./examples/record-demos.sh split
./examples/record-demos.sh sync
./examples/record-demos.sh release
```

Requires: `vhs` and `ffmpeg` (`brew install vhs ffmpeg`)

## Demo Videos

### Unify Workflow

Dependency unification with `[workspace.dependencies]` management.

| Demo | Repository | Highlights |
|------|------------|------------|
| [polars.mp4](./unify/polars.mp4) | [pola-rs/polars](https://github.com/pola-rs/polars) (33 crates) | Dead feature pruning, 214 features removed |
| [tokio.mp4](./unify/tokio.mp4) | [tokio-rs/tokio](https://github.com/tokio-rs/tokio) (10 crates) | MSRV-aware unification |
| [ripgrep.mp4](./unify/ripgrep.mp4) | [BurntSushi/ripgrep](https://github.com/BurntSushi/ripgrep) (10 crates) | Unused dependency detection |

Each demo shows:
1. Repository baseline
2. `cargo rail init` - generate config
3. Configure unify options
4. `cargo rail unify --check` - preview changes
5. `cargo rail affected --explain` - see impact
6. `cargo rail unify` - apply changes
7. `cargo check` - verify non-breaking

### Split Workflow

Extract crates with preserved git history.

| Demo | Description |
|------|-------------|
| [split.mp4](./split/split.mp4) | Extract tokio-util from tokio monorepo |

Shows:
1. Configure split target
2. `cargo rail split run --check` - preview
3. `cargo rail split run` - execute with history
4. Verify standalone repo has commit history

### Sync Workflow

Bidirectional sync between monorepo and split repos.

| Demo | Description |
|------|-------------|
| [sync.mp4](./sync/sync.mp4) | Sync tokio-util between monorepo and split repo |

Shows:
1. Configure sync target
2. `cargo rail sync --check` - check sync status
3. `cargo rail sync --to-remote --check` - preview push
4. `cargo rail sync --from-remote --check` - preview pull

### Release Workflow

Version bumping, changelog generation, and publishing.

| Demo | Description |
|------|-------------|
| [release.mp4](./release/release.mp4) | Release workflow preview on ripgrep |

Shows:
1. Configure release settings
2. `cargo rail release check` - validate readiness
3. `cargo rail release run --check` - preview release
4. Different bump types (patch, minor, explicit version)

## Test Repository Summary

Full `cargo rail unify` results across tested repositories:

| Repository | Crates | Deps Unified | Member Edits |
|------------|--------|--------------|--------------|
| [tikv](https://github.com/tikv/tikv) | 83 | 57 | 516 |
| [meilisearch](https://github.com/meilisearch/meilisearch) | 19 | 46 | 209 |
| [polars](https://github.com/pola-rs/polars) | 33 | 0 | 214 dead features |
| [helix](https://github.com/helix-editor/helix) | 13 | 16 | 66 |
| [tokio](https://github.com/tokio-rs/tokio) | 10 | 10 | 35 |
| [ripgrep](https://github.com/BurntSushi/ripgrep) | 10 | 9 | 35 |
| [iced](https://github.com/iced-rs/iced) | 71 | 6 | 20 |
| [vello](https://github.com/linebender/vello) | 26 | 7 | 17 |
| [codex](https://github.com/openai/codex) | 48 | 2 | 19 |
| [helix-db](https://github.com/helixdb/helix-db) | 6 | 16 | 44 |
| [jj](https://github.com/martinvonz/jj) | 5 | 0 | minimal |
| [ruff](https://github.com/astral-sh/ruff) | 43 | 0 | minimal |

## Directory Structure

```
examples/
├── unify/                 # Dependency unification demos
│   ├── polars.tape        # VHS recording script
│   ├── polars.mp4         # Rendered video
│   ├── tokio.tape
│   ├── tokio.mp4
│   ├── ripgrep.tape
│   └── ripgrep.mp4
├── split/                 # Crate extraction demo
│   ├── split.tape
│   └── split.mp4
├── sync/                  # Bidirectional sync demo
│   ├── sync.tape
│   └── sync.mp4
├── release/               # Release workflow demo
│   ├── release.tape
│   └── release.mp4
├── record-demos.sh        # Recording script
└── README.md
```

## Note

No third-party source code is vendored. Only CLI transcripts and demo recordings
produced by running cargo-rail against public repositories.

Results may vary as upstream repositories evolve.
