# Planning and Execution: With a Task Runner

Use this pattern when the repository already has `just`, `make`, `xtask`, or scripts. Cargo-Rail decides named work;
the task runner keeps command semantics.

```bash
cargo rail --config examples/planning/with-task-runner/rail.toml config validate --strict
cargo rail --config examples/planning/with-task-runner/rail.toml plan --json > target/plan-v8.json
```

## Local example

```bash
if [ "$(scripts/plan/read.py is-required target/plan-v8.json cargo.test)" = "true" ]; then
  CARGO_ARGS=()
  while IFS= read -r -d '' argument; do
    CARGO_ARGS+=("$argument")
  done < <(scripts/plan/read.py cargo-args target/plan-v8.json cargo.test)
  cargo xtask test "${CARGO_ARGS[@]}"
fi

if [ "$(scripts/plan/read.py is-required target/plan-v8.json themes)" = "true" ]; then
  cargo xtask validate-themes
fi
```

## CI boundary

Transfer the exact plan as an artifact. Every execution job validates contract v8, verifies checkout `HEAD` against
`inputs.head_commit`, and consumes only its registered work decision. Do not reconstruct work from changed paths or
evidence descriptions.
