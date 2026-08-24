# Planner Scope: Direct Cargo

Use this pattern when CI should pass planner-selected package arguments directly to Cargo without a repository-specific task runner.

## Local example

```bash
cargo rail plan --merge-base --explain
PLAN_JSON=$(cargo rail plan --merge-base -f json)
if [ "$(jq -r '.surfaces.test.enabled' <<<"$PLAN_JSON")" = "true" ]; then
  CARGO_ARGS=()
  while IFS= read -r argument; do
    CARGO_ARGS+=("$argument")
  done < <(jq -r '.surfaces.test.scope.cargo_args[]' <<<"$PLAN_JSON")
  cargo nextest run "${CARGO_ARGS[@]}"
fi
```

## CI example

```yaml
- uses: loadingalias/cargo-rail-action@47e86bde928ce420b85efa5f8d3b5feb96fd0ffc # v7.0.0
  id: rail
  with:
    mode: debug
    version: 0.22.1

- name: Test selected packages
  if: steps.rail.outputs.test == 'true'
  env:
    PLAN_FILE: ${{ steps.rail.outputs.plan-file }}
  run: |
    CARGO_ARGS=()
    while IFS= read -r argument; do
      CARGO_ARGS+=("$argument")
    done < <(jq -r '.surfaces.test.scope.cargo_args[]' "$PLAN_FILE")
    cargo nextest run "${CARGO_ARGS[@]}"

- name: Build selected packages
  if: steps.rail.outputs.build == 'true'
  env:
    PLAN_FILE: ${{ steps.rail.outputs.plan-file }}
  run: |
    CARGO_ARGS=()
    while IFS= read -r argument; do
      CARGO_ARGS+=("$argument")
    done < <(jq -r '.surfaces.build.scope.cargo_args[]' "$PLAN_FILE")
    cargo build "${CARGO_ARGS[@]}"
```

Cargo-Rail decides scope. Cargo, cargo-nextest, and CI retain command semantics and exit behavior.
