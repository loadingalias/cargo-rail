# Split/Sync Example

Purpose: demonstrate split/sync flow without hidden complexity.

## Recommendation

Use a dedicated sandbox repo for split/sync demos.
Do not run first-time demos against production repos.

## Minimal config shape

Use `rail.toml` from this folder and replace placeholder paths/remotes.

## Split (check mode first)

```bash
cargo rail split run <crate> --check
```

Then run for real when output is correct:

```bash
cargo rail split run <crate>
```

## Sync (check mode first)

```bash
cargo rail sync <crate> --check
```

Then run for real:

```bash
cargo rail sync <crate>
```

## Notes

- Keep source repo clean before running split/sync.
- Keep mapping artifacts under versioned control where appropriate.
- Prefer one crate at a time for first adoption.
