# Change Detection: With Task Runner

For projects using **just**, **make**, **xtask**, or shell scripts.

## Strategy

- **cargo-rail** handles change detection (`plan`)
- **Your task runner** handles execution

This keeps cargo-rail focused on what it does best (change detection) while letting your existing tooling handle execution.

## Usage

### Local Development

```bash
# Get plan as JSON
PLAN=$(cargo rail plan --merge-base -f json)
SCOPE=$(echo "$PLAN" | jq -c '.scope')

# Check which surfaces are enabled
echo "$PLAN" | jq '.surfaces | to_entries | map(select(.value.enabled)) | .[].key'

# Run xtask commands based on plan + scope
if echo "$PLAN" | jq -e '.surfaces.test.enabled' > /dev/null; then
  if echo "$SCOPE" | jq -e '.mode == "workspace"' > /dev/null; then
    cargo xtask test --workspace
  elif echo "$SCOPE" | jq -e '.mode == "crates"' > /dev/null; then
    mapfile -t CRATES < <(echo "$SCOPE" | jq -r '.crates[]')
    cargo xtask test "${CRATES[@]}"
  else
    echo "planner enabled test surface, but no package-scoped work was selected"
  fi
fi

if echo "$PLAN" | jq -e '.surfaces["custom:themes"].enabled' > /dev/null; then
  cargo xtask theme-check
fi
```

### In justfile

```just
plan:
  cargo rail plan --merge-base --explain

run-if-test:
  #!/usr/bin/env bash
  PLAN=$(cargo rail plan --merge-base -f json)
  SCOPE=$(echo "$PLAN" | jq -c '.scope')
  if echo "$PLAN" | jq -e '.surfaces.test.enabled' > /dev/null; then
    if echo "$SCOPE" | jq -e '.mode == "workspace"' > /dev/null; then
      cargo nextest run --workspace
    elif echo "$SCOPE" | jq -e '.mode == "crates"' > /dev/null; then
      mapfile -t CRATES < <(echo "$SCOPE" | jq -r '.crates[]')
      ARGS=()
      for crate in "${CRATES[@]}"; do
        ARGS+=(-p "$crate")
      done
      cargo nextest run "${ARGS[@]}"
    else
      echo "planner enabled test surface, but no package-scoped work was selected"
    fi
  else
    echo "test surface disabled, skipping"
  fi
```

### GitHub Actions

```yaml
jobs:
  plan:
    runs-on: ubuntu-latest
    outputs:
      build: ${{ steps.rail.outputs.build }}
      test: ${{ steps.rail.outputs.test }}
      scope_json: ${{ steps.rail.outputs.scope-json }}
      themes: ${{ steps.rail.outputs.custom_themes }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: loadingalias/cargo-rail-action@v3
        id: rail
        with:
          mode: minimal
          args: '--explain'

  test:
    needs: plan
    if: needs.plan.outputs.test == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run targeted tests
        env:
          SCOPE_JSON: ${{ needs.plan.outputs.scope_json }}
        run: |
          SCOPE_MODE=$(echo "$SCOPE_JSON" | jq -r '.mode')
          if [ "$SCOPE_MODE" = "workspace" ]; then
            cargo xtask test --workspace
          elif [ "$SCOPE_MODE" = "crates" ]; then
            mapfile -t CRATES < <(echo "$SCOPE_JSON" | jq -r '.crates[]')
            cargo xtask test "${CRATES[@]}"
          else
            echo "planner enabled test surface, but no package-scoped work was selected"
          fi

  themes:
    needs: plan
    if: needs.plan.outputs.themes == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo xtask theme-check
```

## Why This Approach?

1. **Separation of concerns**: cargo-rail knows what changed, your task runner knows how to build/test
2. **No lock-in**: If you stop using cargo-rail, your task runner still works
3. **Stable handoff**: use the planner-owned scope contract for package selection instead of rebuilding it from `impact`
4. **Composable**: CI consumes plan output, runs your commands

## Real Examples

- [helix](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix) — uses xtask
- [meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/main/meilisearch) — uses xtask
