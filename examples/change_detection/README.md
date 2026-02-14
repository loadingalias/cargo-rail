# Change Detection Examples

Canonical, copy-paste-ready examples for planner-first change detection.

## Structure

- `01-minimal/rail.toml`: smallest usable setup.
- `02-balanced-monorepo/rail.toml`: practical baseline for most teams.
- `03-strict-bot-pr/rail.toml`: stricter policy for bot-authored PRs.
- `04-github-actions/workflow.yaml`: minimal planner-first GHA workflow.
- `05-custom-profiles/rail.toml`: custom profiles/workflow mapping without command sprawl.

## Fast validation

```bash
cargo rail config validate --strict -f json
cargo rail plan --merge-base --explain
cargo rail run --merge-base --profile ci --dry-run --print-cmd
```

## Trust checklist

- `plan` is the source of truth for CI gating.
- `run` uses the same baseline semantics (`--merge-base` or `--since <ref>`).
- Receipts are uploaded from `target/cargo-rail/receipts/*.json`.
- CI YAML does not re-implement changed-file logic.

See `docs/change-detection-recipe.md` for the full operating recipe.
