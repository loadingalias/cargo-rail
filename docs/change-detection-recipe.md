# Change Detection Recipe (Local + GitHub Actions)

This is the shortest reliable setup for deterministic, explainable change detection.

## 1. Keep policy in `rail.toml`

Start from:

- `examples/change_detection/02-balanced-monorepo/rail.toml`

Avoid re-implementing changed-file logic in workflow YAML.

## 2. Local front door

```bash
just plan
just run
```

Equivalent direct commands:

```bash
cargo rail plan --merge-base -f json
cargo rail run --merge-base --profile local
```

## 3. GitHub Actions front door

```yaml
- uses: actions/checkout@v4
  with:
    fetch-depth: 0

- name: Build plan outputs
  id: plan
  run: cargo rail plan --merge-base -f github >> "$GITHUB_OUTPUT"

- name: Run CI profile
  if: steps.plan.outputs.build == 'true' || steps.plan.outputs.test == 'true' || steps.plan.outputs.infra == 'true'
  run: cargo rail run --merge-base --profile ci

- name: Run docs surface
  if: steps.plan.outputs.docs == 'true'
  run: cargo rail run --merge-base --surface docs

- name: Upload decision receipts
  if: always()
  uses: actions/upload-artifact@v4
  with:
    name: cargo-rail-receipts
    path: target/cargo-rail/receipts/*.json
    if-no-files-found: ignore
```

## 4. Verification commands

```bash
cargo rail config validate --strict -f json
cargo rail plan --merge-base --explain
cargo rail run --merge-base --profile ci --dry-run --print-cmd --explain
```
