# Troubleshooting Planner and Runner Decisions

This guide helps answer two operational questions quickly:

- Why did this run?
- Why didn't this run?

## Why Did This Run?

1. Inspect the planner explanation:

```bash
cargo rail plan --merge-base --explain
```

2. Inspect machine-readable decisions:

```bash
cargo rail plan --merge-base -f json
```

Look at:

- `surfaces.*.enabled`
- `surfaces.*.reasons`
- `impact.direct_crates`
- `impact.transitive_crates`

3. Preview executor behavior without running commands:

```bash
cargo rail run --merge-base --dry-run --print-cmd --explain
```

## Why Didn't This Run?

Check these in order:

1. Wrong base ref:

```bash
cargo rail plan --since <expected-base> --explain
```

2. Surface/profile mismatch:

- `run` default targets `test` surface.
- built-in `--profile ci` expands to `build` + `test`.
- `run.default_profile` can change default profile selection.
- `--workflow <name>` resolves through `[run.workflow]` to a profile.
- Explicit `--surface ...` overrides profile mapping.

3. Planner confidence settings:

- `change-detection.confidence_profile`
- `change-detection.bot_pr_confidence_profile`

4. Path classification or custom rules:

- `change-detection.infrastructure`
- `change-detection.custom`

5. Crate target filtering:

- `--ignore-bin-crates` may remove binary-only crates from package-scoped surfaces.

## CI vs Local Mismatch

Use the same base-ref strategy and profile in both places:

```bash
# Local
cargo rail run --merge-base --profile ci

# CI
cargo rail run --since "$RAIL_SINCE" --profile ci
```

If behavior differs, compare planner JSON outputs from both environments first.
