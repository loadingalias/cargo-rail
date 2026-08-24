# Contributing to Cargo-Rail

## Set up

You need:

- Rust at the [public MSRV](Cargo.toml) (the repository toolchain installs automatically via `rust-toolchain.toml`)
- Python 3.11 or newer
- `just`
- `cargo-nextest`
- `cargo-deny`
- `cargo-audit`

Then run the same checks used during development:

```bash
just check
just test
```

`just check` is workspace-wide and read-only. Use `just fix` only when you intend to format files and apply Clippy's
automatic fixes; CI runs the same read-only checks through `just check-ci`.

After changing features, dependency resolution, or target-specific behavior, run the full suite:

```bash
just test-all
```

## What every change needs

- Keep the patch scoped to one problem.
- Put production behavior in the library; `main.rs` stays limited to argument handling and error reporting.
- Add or update tests for changed behavior. Tests run through `cargo-nextest` — this repository does not use
  `cargo test` for the normal suite.
- Update user documentation when commands, configuration, output, side effects, or exit codes change.
- Add a `.changes/*.md` file for any user-visible change — CLI, configuration, documentation, performance, safety,
  or release behavior. A few sentences aimed at users is enough; these files become the changelog.
- Run `just check && just test` before opening a pull request.

## Generated documentation

Two docs are generated, not hand-edited: `docs/commands.md` comes from CLI help, and `docs/caching.md`
is assembled from the native CI manifest, release target registry, and runtime cache contract. Regenerate both after
changing commands, flags, defaults, support inventories, or cache eligibility:

```bash
just gen-docs
```

## Performance changes

A performance claim needs evidence someone else can check. Identify the workload, host, toolchain, exact commands,
sample count, and before/after values. Report point measurements for one sample; report p50 or p95 only when the run
contains enough samples to support a distribution claim. Compare cache implementations on one host. Preserve the raw
results, and report failed correctness checks alongside the numbers.

Native compiler-cache changes must run the checked-in same-root output-directory and independent-root fixture. When a
change can affect those lanes, measure native Cargo, Cargo-Rail disabled, Cargo-Rail cold, Cargo-Rail warm, and the
pinned sccache comparator. Report hits, misses, bypasses, reasons, bytes hashed and restored, and exact output-byte
equivalence.

The repository benchmarks accept package and run counts:

```bash
just bench-unify 25 10
cargo build --release --locked
just bench-native-cache 1
```

Use `just bench-native-cache 10` only when the decision requires distribution or tail evidence.

## Pull requests

- Explain the user-visible result and the reason for the change.
- Include the commands you used to verify it.
- Call out compatibility changes to CLI output, configuration, planner contracts, or release state.
- Link the issue when one exists.

## Release policy

- Add the change file in the pull request that introduces the user-visible behavior.
- Accumulate reviewed change files into a coherent minor release instead of tagging each merged change.
- Cut an immediate patch only for a regression, security issue, broken package, or broken installer.
- Test release infrastructure with check mode or manual workflow dispatch. Never create public tags to test the
  pipeline.
- Published version tags and release assets are immutable.

For a direct maintainer release from `main`, use the current checkout's binary rather than an older globally installed
Cargo-Rail:

```bash
cargo build --locked --bin cargo-rail
release_cli="$PWD/target/debug/cargo-rail"
"$release_cli" rail release run --all --bump auto --check
"$release_cli" rail release run --all --bump auto --yes
```

The check command exits `1` when it finds a pending release. That is the expected preview result. The apply command
pushes one exact release commit and exits while GitHub checks, including all release archives, are pending. After they
pass, run the exact `"$release_cli" rail release resume <STATE>` command printed by the tool. Do not rerun `release
run`, move a published tag, or replace an existing release asset.

Before opening a release PR instead:

```bash
cargo rail change status
cargo rail release check --all --extended
cargo rail release run --all --bump auto --pr --check
```

## Security

Do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md).
