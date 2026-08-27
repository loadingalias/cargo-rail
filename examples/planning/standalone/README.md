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
  scripts/plan/read.py verify-checkout target/plan-v8.json
  cargo nextest run "${CARGO_ARGS[@]}" --locked
fi
```

## CI boundary

Create the plan once and transfer it as an artifact. In each execution workspace, validate contract v8, gate the job
with `required`, and lower only that work item's typed scope. Then run `verify-checkout` immediately before Cargo. It
delegates to the matching Cargo-Rail binary and verifies the complete saved execution authority; matching `HEAD` alone
is insufficient.

```bash
scripts/plan/read.py validate target/plan-v8.json
scripts/plan/read.py verify-checkout target/plan-v8.json
```

Cargo-Rail decides work and scope. Cargo, cargo-nextest, and CI retain command semantics and exit behavior. Evidence
records explain decisions but are not executable input.
