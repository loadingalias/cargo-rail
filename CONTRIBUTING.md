# Contributing to Cargo-Rail

## Set up

You need:

- Rust 1.97.1 (the repository toolchain installs automatically via `rust-toolchain.toml`)
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

`just check` is read-only and always workspace-wide. When you want rustfmt and Clippy to rewrite the worktree, run `just fix` explicitly.

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
is assembled from the native CI manifest, release target registry, native-cache capability certificates, runtime
gates, and performance qualifications. Regenerate both after changing commands, flags, defaults, support
inventories, or cache eligibility:

```bash
just gen-docs
```

## Performance changes

A performance claim needs evidence someone else can check. Identify the workload, host, toolchain, exact commands,
sample count, and before/after p50 and p95. Compare cache implementations on one host. Preserve the raw results,
and report any failed correctness checks alongside the numbers.

Native compiler-cache changes must run the checked-in cross-root fixture. When the change can affect those lanes,
measure native Cargo, Cargo-Rail disabled, Cargo-Rail cold, Cargo-Rail warm, and the pinned sccache comparator — reporting hits,
misses, bypasses, reasons, bytes hashed/restored, and output portability.

The repository benchmarks accept package and run counts:

```bash
just bench-unify 25 10
cargo build --release --locked
just bench-native-cache 10
```

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

Before opening a release PR:

```bash
cargo rail change status
cargo rail release check --all --extended
cargo rail release run --all --bump auto --pr --check
```

## Security

Do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md).
