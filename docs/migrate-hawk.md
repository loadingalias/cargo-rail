# Migrate from Hawk

`cargo rail surface` is Cargo Rail's native source-visibility workflow. It uses Cargo Rail's captured workspace,
compiler-fact protocol, planner, report, and mutation boundaries; it is not a Hawk compatibility wrapper. Qualify both
tools against the same compiler views before removing Hawk.

The checked-in conformance reference is pinned to Hawk 0.1.13 at commit
`a3b75f193b931d11cf8883c44bda3f9a79c8f19a`; its source archive SHA-256 is
`489f22d7df7e819273fa15c9558128e3a10f206672fb75c8544117e390dc095f`.

## Install the compiler-fact driver

`surface` requires a native release artifact with its matching compiler-fact driver. A source installation or
`cargo binstall` provides the general CLI, but surface analysis rejects it before workspace acquisition. Schema output
remains available.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/loadingalias/cargo-rail/main/scripts/install.sh \
  | sh -s -- X.Y.Z
cargo rail surface --schema >/dev/null
```

The checksum-verifying installer supports GNU Linux and macOS release archives. Windows users should verify the
published `SHA256SUMS`, extract the matching `.zip`, and keep `cargo-rail.exe`, the native compiler helpers, and
`cargo-rail-fact-driver.exe` together. Native musl archives deliberately omit the driver and do not support
surface analysis.

## Configure the native workflow

Cargo Rail does not carry a Hawk-specific configuration parser in its runtime. Start with the native opt-in:

```toml
[surface]
enabled = true
```

That is the complete configuration for a normal binary workspace. Cargo Rail discovers workspace binaries, compiler
targets, feature coverage, and doctest-enabled packages from the captured Cargo model. Add
`consumer_scope = "workspace"` only when non-publishable library, proc-macro, and build-script crates have no consumers
outside the workspace. Add the remaining fields only when the repository needs an exception or an exact compatibility
matrix.

Map an existing `hawk.toml` manually; the table is small and this keeps one analyzer's schema out of Cargo Rail's
product API:

| Hawk 0.1.13                               | Cargo-Rail                                                                                                    |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| omitted feature profiles                  | Cargo Rail's automatic feature coverage; use one explicit `all-features` profile only for an exact comparison |
| omitted doctest entries                   | `doctest_coverage = "automatic"`                                                                              |
| `[[production]]`                          | `[[surface.product]]`, including `bin`/`lib`, `target`, and `reason`                                          |
| `[[feature-profile]]`                     | `[[surface.feature-profile]]` with the same Cargo flags                                                       |
| `[[doctest]]`                             | `[[surface.doctest]]`; any entries replace automatic package selection                                        |
| `preserve-uniform-field-visibility`       | `surface.preserve_uniform_fields`                                                                             |
| `[[override]]`                            | `[[surface.override]]`, preserving Rust `crate`, item, mapped kind, target, level, and reason                 |
| `[[exclude]]`                             | `[[surface.exclude]]`, preserving Rust `crate`, module/file, target, level, and reason                        |
| `hawk::dead_public`                       | `dead-public`                                                                                                 |
| `hawk::unnecessary_public`                | `unnecessary-public`                                                                                          |
| `hawk::unnecessary_restricted_visibility` | `unnecessary-restricted-visibility`                                                                           |
| `hawk::unnecessary_crate_visibility`      | `unnecessary-crate-visibility`                                                                                |

Hawk's `--exclude-crate NAME` is command-line state, so record the boundary explicitly:

```toml
[[surface.external]]
crate = "supported_library"
reason = "supported outside this workspace"
```

Hawk `--target TRIPLE` maps to `surface.targets = ["TRIPLE"]`; the triple must also be present in the top-level
target policy. Per-entry Cargo target triples and `cfg(...)` selectors translate directly.

## Translate lint flags and commands

Hawk's ordered `-A`, `-W`, and `-D` flags map to ordered `[[surface.lint]]` entries. Strip the `hawk::` prefix and
replace underscores with hyphens. The `warnings` group keeps its name:

```toml
[[surface.lint]]
selector = "warnings"
level = "warn"

[[surface.lint]]
selector = "dead-public"
level = "deny"
```

Later matching entries win. Core findings deny by default; `unnecessary-crate-visibility` allows by default. A warning
is reported with exit 0. A deny finding, stale/unknown/ambiguous policy, or overlapping exact policy exits 1. Use the
same exact finding names with `--only`; filtering never hides configuration diagnostics.

```bash
# Hawk: cargo hawk check -D warnings --only dead-public
cargo rail surface --only dead-public --explain
cargo rail surface --check --only dead-public --explain
cargo rail surface --check --only dead-public -f json
```

The first command is read-only, reports findings, and exits 0. Add `--check` in CI to exit 1 for deny findings or
configuration diagnostics. Warnings remain successful in either mode.

JSON and GitHub projections use surface contract v2. It records compiler-crate authority, exact feature/target views,
configuration diagnostics, findings with levels, cache evidence, acquisition metrics, and mutation state. Operational
failure remains exit 2.

Preview fixes before granting write authority:

```bash
cargo rail surface --fix --dry-run --explain
cargo rail surface --fix --backup
```

The apply path binds replacements to captured bytes, rejects drift, writes only planned paths, recompiles every
configured view, rolls every touched file back on verification or receipt failure, and records a receipt after
success. Dead declarations remain report-only.

## Review the closed-world boundary

Hawk identifies its audited library boundary by Rust crate name. Cargo-Rail makes the full boundary visible in report
v2 as compiler crates: package, Cargo target, Rust crate name, target kind, and role.

- A selected binary product is closed even when its package is publishable.
- Non-publishable library, proc-macro, and build-script targets are closed only when
  `consumer_scope = "workspace"` asserts that the workspace contains every consumer.
- Publishable libraries and `[[surface.external]]` crates remain open.
- A physical declaration compiled into both an open and closed crate is preserved.
- A selected internal library derives production roots from actual cross-crate production consumers. Selecting the
  library does not make every public item live.

Inspect `authority.audited_targets`, `authority.open_targets`, `products`, `features`, `targets`, and
`completeness.complete` before accepting the findings.

## Route the gate through the planner

Set `[surface] enabled = true` to let Rust source, surface policy, compiler driver, schema, and relevant workflow
changes select the planner's whole-workspace `surface` gate. Consume the planner value directly:

```bash
PLAN_JSON=$(cargo rail plan --merge-base -f json)
if [ "$(jq -r '.surfaces.surface.enabled' <<<"$PLAN_JSON")" = "true" ]; then
  cargo rail surface --check
fi
```

The official action exports the same versioned `surfaces-json`; consume that value instead of recreating path rules in
workflow YAML. Surface execution still needs the release driver or the repository's exact source-built embedded
driver used by its bootstrap job.

## Compare before removing Hawk

Run the checked-in qualification harness against an extracted native Cargo-Rail release and the pinned reference
archive:

```bash
scripts/ci/qualify-surface-reference.py \
  --cargo-rail /path/to/extracted/cargo-rail \
  --reference-archive /path/to/hawk-a3b75f193b931d11cf8883c44bda3f9a79c8f19a.tar.gz \
  --output benchmark_results/surface-reference
```

The harness verifies Hawk's archive digest, uses Rust 1.98.0, interleaves the tools over the same multi-crate fixture,
normalizes paths, and accepts differences only from the exact checked-in allowlist with a reason and owner. Keep the
raw output and measurement files. A failing corpus is evidence; do not convert acquisition-view arithmetic into a
wall-time claim. See [Benchmarking](benchmarking.md#source-surface-analysis).
