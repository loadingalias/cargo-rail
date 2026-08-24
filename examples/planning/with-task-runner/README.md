# Planning and Execution: With Task Runner

Use this pattern when your repo already has `just`, `make`, `xtask`, or scripts.

Cargo-Rail emits crate and surface scope. The task runner maps that scope to repository-specific commands.

## Local example

```bash
PLAN=$(cargo rail plan --merge-base -f json)
TEST_SCOPE=$(echo "$PLAN" | jq -c '.surfaces.test.scope')

if echo "$PLAN" | jq -e '.surfaces.test.enabled' > /dev/null; then
  if echo "$TEST_SCOPE" | jq -e '.mode == "workspace"' > /dev/null; then
    cargo xtask test --workspace
  elif echo "$TEST_SCOPE" | jq -e '.mode == "crates"' > /dev/null; then
    echo "$TEST_SCOPE" | jq -r '.crates[]' | xargs cargo xtask test
  fi
fi
```

## CI example

```yaml
- uses: loadingalias/cargo-rail-action@47e86bde928ce420b85efa5f8d3b5feb96fd0ffc # v7.0.0
  id: rail
  with:
    mode: debug
    version: 0.22.1

- name: Run targeted tests
  if: steps.rail.outputs.test == 'true'
  env:
    PLAN_FILE: ${{ steps.rail.outputs.plan-file }}
  run: |
    TEST_SCOPE=$(jq -c '.surfaces.test.scope' "$PLAN_FILE")
    MODE=$(echo "$TEST_SCOPE" | jq -r '.mode')
    if [ "$MODE" = "workspace" ]; then
      cargo xtask test --workspace
    elif [ "$MODE" = "crates" ]; then
      echo "$TEST_SCOPE" | jq -r '.crates[]' | xargs cargo xtask test
    fi
```

Use the selected surface's `scope` for execution. Do not rebuild execution scope from `impact`.
