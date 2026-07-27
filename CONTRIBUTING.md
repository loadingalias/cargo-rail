# Contributing to cargo-rail

## Before changing code

Required tools:

- Rust stable
- Python 3.11 or newer
- `just`
- `cargo-nextest`
- `cargo-deny`

Install the repository toolchain, then run the same focused checks used during development:

```bash
just check
just test
```

`just check` is read-only. Run `just fix` when you explicitly want rustfmt and
Clippy to rewrite the worktree.

`just check` is always workspace-wide. Run the full test suite after changing features, dependency resolution, or
target-specific behavior:

```bash
just test-all
```

`docs/commands.md` is generated from CLI help. `docs/cache-capabilities.md` joins the native CI manifest, release
target registry, exact native-cache capability certificates, runtime gates, and performance qualifications.
Regenerate them after changing commands, flags, defaults, support inventories, or cache eligibility:

```bash
just gen-docs
```

Performance changes must identify the workload, host, toolchain, exact commands, sample count, and before/after p50 and
p95. Compare cache implementations within one host. Preserve raw results and report failed correctness checks.

Native compiler-cache changes must run the checked-in cross-root fixture. Measure native Cargo, cargo-rail disabled,
cargo-rail cold, cargo-rail warm, and the pinned sccache comparator when the change can affect those lanes. Report hits,
misses, bypasses, reasons, bytes hashed/restored, and output portability.

The repository benchmarks accept package and run counts:

```bash
just bench-unify 25 10
cargo build --release --locked
just bench-native-cache 10
```

## Change requirements

- Keep the patch scoped to one problem.
- Put production behavior in the library and keep `main.rs` limited to argument handling and error reporting.
- Add or update tests for changed behavior. Use `cargo-nextest`; this repository does not use `cargo test` for the normal test suite.
- Update user documentation when commands, configuration, output, side effects, or exit codes change.
- Add a `.changes/*.md` file for user-visible CLI, configuration, documentation, performance, safety, or release behavior.
- Run `just check && just test` before opening a pull request.

## Pull requests

- Explain the user-visible result and the reason for the change.
- Include the commands used to verify it.
- Call out compatibility changes to CLI output, configuration, planner contracts, or release state.
- Link the issue when one exists.

## Release policy

- Add a change file in the pull request that introduces user-visible behavior.
- Accumulate reviewed change files into a coherent minor release instead of tagging each merged change.
- Cut an immediate patch only for a regression, security issue, broken package, or broken installer.
- Test release infrastructure with check mode or manual workflow dispatch. Do not create public tags to test the pipeline.
- Keep published version tags and release assets immutable.

Before opening a release PR:

```bash
cargo rail change status
cargo rail release check --all --extended
cargo rail release run --all --bump auto --pr --check
```

## Security

Do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md).
