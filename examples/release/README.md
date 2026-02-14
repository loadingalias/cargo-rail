# Release Example (Dry-Run Safe)

Purpose: demonstrate release planning/validation without publishing.

## Minimal config

Use `rail.toml` from this folder.

## Safe workflow

Validate release conditions:

```bash
cargo rail release check --all --json
```

Create release plan only (no publish):

```bash
cargo rail release run --all --check --json
```

## Do not publish in demo environments

Avoid running publish commands from test repos.
Use planning and checks only for demos.

## Suggested demo repo

Use a dedicated throwaway workspace for release examples.
