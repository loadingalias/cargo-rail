# Planner Machine Contract

`cargo rail plan -f json` is a versioned API for CI and other tooling. Validate it with the schema shipped in this repository:

```bash
cargo rail plan --schema > plan.schema.json
cargo rail plan --merge-base -f json > plan.json
```

The current output contains planner contract v5 and scope contract v3 inside machine envelope v1. [`schemas/plan-v5.schema.json`](../schemas/plan-v5.schema.json) defines the contract. `plan --schema` does not load workspace metadata, so consumers can validate compatibility before opening a repository.

Contract v5 adds `inputs.snapshot_id` for worktree plans, binding planner ownership to the same portable workspace
snapshot used by split, sync, and release. Historical object-range plans emit `null` because they intentionally do not
claim the current worktree snapshot.

Contract v4 separates build-transitive impact from development-only impact and gives every surface its own package
scope. `build` follows active normal, build, proc-macro, and host edges. `test` additionally includes active development
edges; `bench` remains enabled only by benchmark work, but uses development impact when selected. Optional and
target-conditioned edges are omitted only when an exact feature/target resolution proves them inactive.

Manifest and lockfile changes are compared semantically. Formatting and metadata-only manifest edits do not schedule
package work. Workspace dependency inheritance and resolved lockfile package/source/checksum/dependency-edge changes are localized to
the affected closure; unknown resolver, source-replacement, parse, or declaration evidence falls back to the workspace.
The exact changed bytes remain part of the authoritative snapshot.

Each trace reason records `selected_surfaces`. Semantic reasons may also include the changed input, exact Cargo
`PackageId`, dependency `PackageId`, edge kind, alias, target predicate, optional/default-feature state, explicit
features, host/proc-macro role, and conservative fallback codes. Human output exposes the same evidence with
`plan --explain`.

## Compatibility

- Adding an optional field is backward-compatible.
- Removing a field, changing its type or meaning, making an optional field required, or changing a reason code is breaking and requires a new planner contract version and schema file.
- Envelope and nested contracts version independently. Consumers must check every version they depend on.
- Trace `code` values are stable machine identifiers. `description` is for people and may be clarified without changing behavior.
- Unsupported output formats fail argument parsing with exit code `2`; commands never fall back to text.

Successful JSON commands write one JSON value to stdout and keep progress off stderr. Check modes retain the CLI exit convention: `0` means clean, `1` means changes are required, and `2` means an error.

## Portable Plan Identity

`cargo rail hash` emits `plan-v1:sha256:<digest>`. The digest uses canonical JSON over the execution-relevant planner fields:

- planner contract version;
- refs, config and toolchain fingerprints, and confidence profile;
- repository-relative files, impact, scope, surface decisions, and trace reasons.

Path separators are normalized to `/`. Absolute paths, drive-qualified paths, and `..` components are rejected. Path
package identities inside the workspace are normalized to logical workspace-relative identities. Local diagnostics
such as `inputs.workspace_root`, the machine envelope, and `reproducibility` metadata are excluded, so equivalent
clones at different checkout paths have the same identity. Config and toolchain fingerprints normalize LF and CRLF
line endings.

This identity compares planner decisions. It is **not a cache key**. A future cache identity must additionally bind source contents, the actual compiler, target, features, command, and an explicit environment allowlist before reuse can be safe.
