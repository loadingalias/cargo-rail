# Contributing to Cargo-Rail

## Set up the repository

Install these tools:

- Rust through `rustup`; `rust-toolchain.toml` selects the repository toolchain and components.
- Python 3.11 or newer.
- `just`, `cargo-nextest`, and `cargo-deny`.

Run commands from the repository root. Use `just --list` to see the maintained command surface.

## Make a change

- Keep each change focused on one problem.
- Read [the architecture guide](docs/architecture.md) before changing an ownership or mutation boundary.
- Put user-visible behavior in the library. Keep `src/main.rs` limited to process setup, diagnostics, and dispatch.
- Add or update tests for changed behavior. The normal suite uses cargo-nextest; doctests run separately.
- Update documentation when commands, configuration, output, side effects, compatibility, or recovery behavior changes.
- Record user-visible intent in `.changes/`:

  ```bash
  cargo rail change add cargo-rail --bump patch --message "Describe the user-visible result."
  ```

  Choose `minor` or `major` when the compatibility impact requires it. Use `none` only to record a reviewed change
  that should not produce a release or changelog entry.

## Validate the worktree

Inspect the planner-selected work, then run the affected local lanes:

```bash
just plan
just check-affected
just build
just test
git diff --check
```

Run the complete lanes when a change crosses planner, mutation, cache, compiler, release, platform, or public-contract
boundaries:

```bash
just check
just test-all
```

`just check` is workspace-wide and read-only. `just fix` formats files and applies Clippy fixes; run it only when those
edits are intended. Use the contract-specific recipes listed by `just --list` for compiler-driver, Windows-target,
action-pin, benchmark, qualification, or remote-machine work.

## Regenerate owned documentation

Do not hand-edit `docs/caching.md`. It is generated from executable support authorities. Regenerate it after changing
its inputs:

```bash
just gen-docs
```

Inspect and commit the generated diff with the source change.

## Work on Surface compiler integration

General CLI, planner, release, dependency, and documentation work does not require compiler internals. When changing
Surface's compiler integration, install `rustc-dev` for the selected toolchain and validate the excluded driver:

```bash
rustup component add rustc-dev
just check-compiler-fact-driver
scripts/with-compiler-fact-driver.sh cargo build --locked --bin cargo-rail
```

The driver is built separately because it is tied to one exact Rust compiler toolchain.

## Support performance claims with evidence

Follow [the benchmarking guide](docs/benchmarking.md). State the workload, host, toolchain, command, sample count,
correctness checks, and before/after results. Preserve raw results. Compare cache implementations on the same host,
and do not generalize a result beyond the platforms and workload that were measured.

Native compiler-cache work must cover the repository's same-root and independent-root fixtures and an external
`CARGO_TARGET_DIR`. A same-size mutation of a selected repository input must miss. Compare native Cargo, Cargo-Rail
disabled, Cargo-Rail cold, Cargo-Rail warm, and the pinned sccache baseline when that comparison is relevant. Report
full source-capture and selected-input refresh costs separately, plus hits, misses, bypass reasons, bytes hashed and
restored, and exact output-byte equivalence.

## Open a pull request

- Explain the user-visible result and why the change is needed.
- List the exact validation commands and their results.
- Identify compatibility changes to CLI output, configuration, plan contracts, stored formats, or release state.
- Link the issue when one exists.

## Perform a maintainer release

Accumulate reviewed change files into a coherent minor release. Cut an immediate patch only for a regression, security
issue, broken package, or broken installer. Test release infrastructure through check mode or manual workflow dispatch;
do not create public tags as a test. Published tags and release assets are immutable.

From `main`, build and use the current checkout's binary:

```bash
cargo build --locked --bin cargo-rail
release_cli="$PWD/target/debug/cargo-rail"
set +e
"$release_cli" rail release check --all --bump auto --publication
release_check_status=$?
set -e
test "$release_check_status" -eq 1
"$release_cli" rail release run --all --publish --wait --yes
```

`release check --publication` proves the same positive registry and remote authority that `release run --publish`
requires, but executes no effects. It exits `1` when it finds a pending release; that is the expected preview result.
`release run --publish --wait` pushes one exact release commit, waits for its GitHub checks, including release
archives, and then completes the same durable transaction. Interrupting the wait is safe: resume the exact journal
reported by Cargo-Rail. Do not rerun `release run`, move a published tag, or replace a release asset.

## Report security issues privately

Do not open a public issue for a vulnerability. Follow [the security policy](SECURITY.md).
