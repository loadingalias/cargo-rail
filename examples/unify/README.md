# Dependency Unification

[`rail.toml`](rail.toml) shows every current `[unify]` field. Start with the defaults, then keep only the policy your
workspace needs.

From the repository root:

```bash
cargo rail --config examples/unify/rail.toml config validate --strict
cargo rail --config examples/unify/rail.toml unify --check --explain
```

`--check` does not edit manifests. It exits `1` when changes are pending. Review the explanation, then apply:

```bash
cargo rail --config examples/unify/rail.toml unify apply
```

Use `consumer_scope = "workspace"` only when no external consumer can activate private package features. Use
`cargo rail unify undo` to restore the latest manifest and lockfile backup.

See the [configuration reference](../../docs/config.md#unify) for every value and safety boundary.
