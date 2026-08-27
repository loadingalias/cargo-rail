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
  scripts/plan/read.py verify-checkout target/plan-v8.json
  cargo xtask test "${CARGO_ARGS[@]}"
fi

if [ "$(scripts/plan/read.py is-required target/plan-v8.json themes)" = "true" ]; then
  scripts/plan/read.py verify-checkout target/plan-v8.json
  cargo xtask validate-themes
fi
```

## CI boundary

Transfer the exact plan as an artifact. Every execution job validates contract v8 and consumes only its registered
work decision. Immediately before each task-runner command, `verify-checkout` must use the matching Cargo-Rail binary
to revalidate the complete saved execution authority. Matching `HEAD` alone is insufficient. Do not reconstruct work
from changed paths or evidence descriptions.
