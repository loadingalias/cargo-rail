# Configuration

`rail.toml` contains sparse repository policy. An empty file is valid; omitted fields use coded defaults. Keep
commands, credentials, timeouts, setup, and execution in their owning tools.

## Find and inspect configuration

Cargo-Rail uses the first file found:

1. `rail.toml`
2. `.rail.toml`
3. `.cargo/rail.toml`
4. `.config/rail.toml`

`--config PATH` bypasses discovery. Relative paths are resolved from `--workspace-root`.

```bash
cargo rail init --dry-run
cargo rail init
cargo rail config locate
cargo rail config print
cargo rail config explain --all -f json
cargo rail config validate --strict
cargo rail config migrate --check
```

`config print` emits canonical effective TOML, including defaults. `config explain` reports each configured and
effective value, its default, source, classification, reason, and deprecation state. Use those commands instead of
copying defaults from documentation.

`config migrate --check` is read-only and exits `1` when migration is pending. Run `cargo rail config migrate` only
after reviewing its semantic actions.

## Start sparse

Keep only choices that differ from defaults:

```toml
targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]

[unify]
include_renamed = true
major_version_conflict = "bump"

[surface]
enabled = true

[release]
remote_effects = "auto"

[plan.work.verification]
scope = "repository"
paths = ["verification/**"]
```

## Policy families

| Family | Owns |
|---|---|
| `targets` | Additional target triples used by target-aware workspace decisions |
| `[unify]` | Dependency and manifest policy |
| `[surface]` | Compiler-derived reachability and visibility policy |
| `[release]` | Release intent, changelogs, and external-effect authority |
| `[plan.work.NAME]` | Positive inputs and selector shape for repository-owned work |
| `[crates.NAME]` | Per-crate split, release, and changelog policy |

### Dependency policy

`cargo rail unify` previews one dependency decision. `--check` makes the same decision and exits `1` when edits are
pending. `cargo rail unify apply` revalidates and writes the planned manifest changes.

Use `consumer_scope = "workspace"` only when the workspace contains every consumer of the affected private packages.
Published packages remain open-world. A major-version unification requires explicit `major_version_conflict = "bump"`
authority. Exact pins, renamed dependencies, MSRV policy, preserved features, and backup retention remain independent
choices; inspect their current defaults with `config explain --all`.

### Surface policy

`surface.enabled = true` adds the whole-workspace Surface gate to planning. Direct `cargo rail surface` inspection is
available even when the gate is disabled.

Keep `consumer_scope = "open"` unless the workspace owns every consumer of its non-publishable compiler crates.
Declare products, external crates, feature profiles, doctest coverage, lint levels, overrides, and exclusions only
when Cargo-Rail's automatic workspace model does not express the intended boundary.

`cargo rail surface --schema` exposes the report contract. `surface --check` is read-only; `surface --fix --dry-run`
previews exact source edits before `surface --fix` receives write authority.

### Release policy

Reviewed `.changes/*.md` files are the default source for bumps and release prose. `cargo rail release check` previews
the local plan and exits `1` when a release is pending. It performs no mutation or external effect.

`remote_effects` controls Git and forge effects. Registry publication is separate and requires both
`registry_publication = "crates-io"` and `--publish` on the exact release invocation. Remote modes persist
transaction state and stop at readiness or registry-convergence boundaries; resume the recorded state instead of
replanning.

To replace commit-driven changelog automation:

1. Set `release.source = "commits"` temporarily.
2. Match the existing changelog filters and compare `cargo rail release check --all --bump auto` with the old plan.
3. Add reviewed intent to new changes with `cargo rail change add`.
4. Switch to `source = "changes"` only after every pending release has reviewed change files.

Cargo-Rail uses fixed changelog placeholders rather than a template engine. Put prose that needs custom logic in the
reviewed change file.

### Repository work

`[plan.work.NAME]` declares inputs, not commands:

```toml
[plan.work.compatibility]
scope = "variants"
paths = ["tests/compatibility/**"]
config = ["targets"]
cargo = ["cargo.build", "cargo.test"]
variant_catalog = "distribution/compatibility-plan-variants.json"
```

`scope` is `repository`, `cargo`, or `variants`. Paths are positive repository-relative globs. `config` names exact
effective fields. `cargo` subscribes the work item to built-in Cargo decisions. Variant work requires a checked-in
catalog conforming to [`plan-variants-v1.schema.json`](../schemas/plan-variants-v1.schema.json).

Absolute paths, parent traversal, negative globs, commands, unknown fields, and malformed catalogs are rejected. See
[Planning](planning.md) for selector and consumer rules.

### Per-crate policy

`[crates.NAME.split]` maps package-owned history and explicit non-Cargo assets to one split destination. `include` adds
non-Cargo files; `exclude` can only narrow that explicit set. Cargo-owned package files cannot be excluded.

`[crates.NAME.release]` may narrow publication. `[crates.NAME.changelog]` overrides workspace changelog policy for one
crate.

## Rejected ownership

Repository `[cache]` configuration is rejected. Remote URLs, credentials, provider environment, local CAS placement,
and distributed-worker selection are machine-owned cache setup.

Execution tables and the retired `[change-detection]` policy are rejected. Keep commands in Cargo, nextest, Just,
scripts, or CI, and register only their positive inputs under `[plan.work.NAME]`.

Deprecated fields remain readable only for explicit semantic migration. `config migrate` preserves unrelated TOML and
does not materialize coded defaults.

## Exit behavior

| Command | Exit `0` | Exit `1` | Exit `2` |
|---|---|---|---|
| `config migrate --check` | No migration | Migration pending | Invalid input or operation |
| Mutation command `--check` | No mutation | Mutation pending | Invalid input or operation |
| `config validate` | Valid at selected strictness | — | Invalid or unreadable configuration |

`config validate` enables strict mode in common CI environments. Use `--no-strict` only when warnings are deliberately
non-blocking.
