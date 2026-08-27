# Releases

[`rail.toml`](rail.toml) shows every workspace release field, changelog field, filter, custom group, and per-crate
publish switch. It keeps `remote_effects = "none"`, so copying it cannot push or publish.

From the repository root:

```bash
cargo rail --config examples/release/rail.toml config validate --strict
# Writes one reviewed file under .changes/.
cargo rail --config examples/release/rail.toml change add cargo-rail --bump patch \
  --message "Describe the user-visible change."
cargo rail --config examples/release/rail.toml release check --all --bump auto
```

`release check` previews the exact version, files, tag, and external effects. Change `remote_effects` only after the
local plan is correct. Registry publication additionally requires `registry_publication = "crates-io"` in this file
and `--publish` on the exact invocation; neither authority implies the other. With remote effects enabled, `release run`
pushes the exact release commit and stops while its checks run. Resume the state file printed by the command after that
SHA is green.

See the [configuration reference](../../docs/config.md#release) for every value and the
[contributor release runbook](../../CONTRIBUTING.md#release-policy) for this repository's direct-main flow.
