# Planner Scope: Direct Cargo

Use this pattern to pass one named work decision directly to Cargo or cargo-nextest.

```bash
cargo rail --config examples/planning/standalone/rail.toml config validate --strict
cargo rail --config examples/planning/standalone/rail.toml plan --explain
```

## Local example

```bash
cargo rail --config examples/planning/standalone/rail.toml plan --json > target/plan-v8.json

if [ "$(scripts/plan/read.py is-required target/plan-v8.json cargo.test)" = "true" ]; then
  CARGO_ARGS=()
  while IFS= read -r -d '' argument; do
    CARGO_ARGS+=("$argument")
  done < <(scripts/plan/read.py cargo-args target/plan-v8.json cargo.test)
  cargo nextest run "${CARGO_ARGS[@]}" --locked
fi
```

## CI boundary

Create the plan once, transfer it as an artifact, and verify `inputs.head_commit` against checkout `HEAD` before
execution. Gate a job with `required`, then read only that work item's scope. Reject an unknown contract or malformed
selector before starting Cargo.

```bash
scripts/plan/read.py validate target/plan-v8.json
scripts/plan/read.py verify-checkout target/plan-v8.json
```

Cargo-Rail decides work and scope. Cargo, cargo-nextest, and CI retain command semantics and exit behavior. Evidence
records explain decisions but are not executable input.
