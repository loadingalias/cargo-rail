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

# Check which surfaces are enabled
echo "$PLAN" | jq '.surfaces | to_entries | map(select(.value.enabled)) | .[].key'

# Run xtask commands based on plan
if echo "$PLAN" | jq -e '.surfaces.test.enabled' > /dev/null; then
  cargo xtask test
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
  if echo "$PLAN" | jq -e '.surfaces.test.enabled' > /dev/null; then
    cargo nextest run
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
      themes: ${{ steps.custom.outputs.themes }}
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: loadingalias/cargo-rail-action@v3
        id: rail
        with:
          mode: full
          args: '--explain'

      - name: Extract custom surfaces
        id: custom
        run: |
          THEMES=$(echo '${{ steps.rail.outputs.custom-surfaces }}' | jq -r '.["custom:themes"] // "false"')
          echo "themes=$THEMES" >> "$GITHUB_OUTPUT"

  test:
    needs: plan
    if: needs.plan.outputs.test == 'true'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo xtask test

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
3. **Full control**: Complex build steps stay in your existing tooling
4. **Composable**: CI consumes plan output, runs your commands

## Real Examples

- [helix](https://github.com/loadingalias/cargo-rail-testing/tree/main/helix) — uses xtask
- [meilisearch](https://github.com/loadingalias/cargo-rail-testing/tree/main/meilisearch) — uses xtask
