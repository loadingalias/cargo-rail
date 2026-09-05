# Contributing to Cargo-Rail

## Set up the repository

Install these tools:

- Rust through `rustup`; `rust-toolchain.toml` selects the repository toolchain and components.
- `just`, `cargo-nextest`, and `cargo-deny`.

Run commands from the repository root. Use `just --list` to see the maintained command surface.

## Make a change

- Keep each change focused on one problem.
- Read [the architecture guide](docs/architecture.md) before changing an ownership or mutation boundary.
- Put user-visible behavior in the library. Keep `src/main.rs` limited to process setup, diagnostics, and dispatch.
- Add or update tests for changed behavior. The normal suite uses cargo-nextest; doctests run separately.
- Update documentation when commands, configuration, output, side effects, compatibility, or recovery behavior changes.
## Validate the worktree

Run the direct workspace build and test lanes:

```bash
just build
just test
git diff --check
```

Run the complete quality lane when a change crosses planner, mutation, cache, compiler, release, platform, or
public-contract boundaries:

```bash
just check
```

`just build` and `just test` are workspace-wide and do not lower plan selectors. `just check` is workspace-wide and
read-only. `just fix` formats files and applies Clippy fixes; run it only when those edits are intended. Use the
contract-specific recipes listed by `just --list` for compiler-driver, Windows-target, benchmark, or remote-machine
work.

## Work on Surface compiler integration

General CLI, planner, release, dependency, and documentation work does not require compiler internals. When changing
Surface's compiler integration, install `rustc-dev` for the selected toolchain and validate the excluded driver:

```bash
rustup component add rustc-dev
just check-compiler-driver
```

The driver is built separately because it is tied to one exact Rust compiler toolchain.

## Support performance claims with evidence

Follow [the benchmarking guide](docs/benchmarking.md). State the workload, host, toolchain, command, sample count,
correctness checks, and before/after results. Preserve raw results. Compare cache implementations on the same host,
and do not generalize a result beyond the platforms and workload that were measured.

## Open a pull request

- Explain the user-visible result and why the change is needed.
- List the exact validation commands and their results.
- Identify compatibility changes to CLI output, configuration, plan contracts, stored formats, or release state.
- Link the issue when one exists.

## Report security issues privately

Do not open a public issue for a vulnerability. Follow [the security policy](SECURITY.md).
