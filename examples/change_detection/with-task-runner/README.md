# Change Detection: With Task Runner

Use this pattern when your repo already has `just`, `make`, `xtask`, or scripts.

`cargo-rail` emits crate and surface scope. The task runner maps that scope to repository-specific commands.

## Local Example

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

## CI Example

```yaml
- uses: loadingalias/cargo-rail-action@v5.1.0
  id: rail
  with:
    version: 0.19.0

- name: Run targeted tests
  if: steps.rail.outputs.test == 'true'
  env:
    SCOPE_JSON: ${{ steps.rail.outputs.scope-json }}
  run: |
    MODE=$(echo "$SCOPE_JSON" | jq -r '.mode')
    if [ "$MODE" = "workspace" ]; then
      cargo xtask test --workspace
    elif [ "$MODE" = "crates" ]; then
      echo "$SCOPE_JSON" | jq -r '.crates[]' | xargs cargo xtask test
    fi
```

Use the selected surface's `scope` for execution. Do not rebuild execution scope from `impact`.
