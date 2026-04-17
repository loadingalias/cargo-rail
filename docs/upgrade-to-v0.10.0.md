# ADR 0001: Migrate `affected` / `test` to `plan` / `run`

- Status: Accepted
- Date: 2026-02-13

## Why this changed

The old model split planning and execution across command-specific flows (`affected`, `test`).
That created duplicate CI logic and local/CI drift.

`cargo-rail` now uses one planner (`plan`) and one executor (`run`).

## Migration at a glance

| Before | After |
|---|---|
| `cargo rail affected` | `cargo rail plan` |
| `cargo rail test` | `cargo rail run` |
| Per-workflow shell logic for changed files | `cargo rail plan -f github` outputs |

Removed commands are intentionally not aliased.

## CLI mapping

1. Build deterministic plan:

```bash
cargo rail plan --merge-base -f json
```

2. Execute only planner-selected work:

```bash
cargo rail run --merge-base --profile ci
```

3. Explain any run/skip decision:

```bash
cargo rail plan --merge-base --explain
cargo rail run --merge-base --dry-run --print-cmd --explain
```

## GitHub Actions mapping

Before (legacy pattern):
- Action or shell scripts compute changed files/crates directly.
- Jobs are gated by custom logic per workflow.

After (canonical pattern):

```yaml
- uses: actions/checkout@v4
  with:
    fetch-depth: 0

- name: Build planner outputs
  id: plan
  run: cargo rail plan --merge-base -f github >> "$GITHUB_OUTPUT"

- name: Run CI profile
  if: steps.plan.outputs.build == 'true' || steps.plan.outputs.test == 'true' || steps.plan.outputs.infra == 'true'
  run: cargo rail run --merge-base --profile ci
```

## Action migration (`cargo-rail-action`)

`cargo-rail-action` is a planner-first GitHub transport over `cargo rail plan`.

- Keep `mode: minimal` unless you explicitly need legacy projection outputs.
- Gate jobs from action outputs, then execute with `cargo rail run`.
- Do not re-implement changed-file logic in YAML.

## Exit code model

- `0`: success (or no-op)
- `1`: check-mode found changes that would be applied (expected for `--check` flows)
- `2`: operational error

## Non-goals

- No compatibility shim for removed commands.
- No dual command model.
