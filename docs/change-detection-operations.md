# Change Detection Operations Guide

Use this when a run/skip decision looks wrong.

## Fast triage

```bash
cargo rail plan --merge-base --explain
cargo rail plan --merge-base -f json
cargo rail run --merge-base --profile ci --dry-run --print-cmd --explain
```

Inspect:

- `surfaces.*.enabled`
- `surfaces.*.reasons`
- `impact.direct_crates`
- `impact.transitive_crates`
- `inputs.refs.resolved_base`

## Common causes

1. Baseline mismatch (`--since` vs `--merge-base`)
2. Profile/surface mismatch (`--profile`, `[run.profile.*]`)
3. Confidence profile mismatch (`confidence_profile`, `bot_pr_confidence_profile`)
4. Custom path classification (`[change-detection.custom]`)

## Rule

Use one planner contract everywhere:

- local: `plan` + `run`
- CI: planner outputs + `run`
