# Contributing to Cargo-Rail

## Set up the repository

Install these tools:

- Rust through `rustup`; `rust-toolchain.toml` selects the repository toolchain and components.
- `just`, `cargo-nextest`, and `cargo-deny`.
- For `just build` and `just test`: Python 3.11 or newer and `rustc-dev` for the selected toolchain.
- For local checks on macOS: Zig, `cargo-zigbuild`, `cargo-xwin`, and LLVM tools required by cargo-xwin.
- For `just test` on macOS: clang with COFF support, `lld-link`, and `ld64.lld` on `PATH`, plus `cargo-xwin`
  with its x86_64 MSVC SDK already cached. The cross-link test runs offline.

Run commands from the repository root. Use `just --list` to see the maintained command surface.

## Make a change

- Keep each change focused on one problem.
- Read [the architecture guide](docs/architecture.md) before changing an ownership or mutation boundary.
- Put user-visible behavior in the library. Keep `src/main.rs` limited to process setup, diagnostics, and dispatch.
- Add or update tests for changed behavior. The normal suite uses cargo-nextest; doctests run separately.
- Update documentation when commands, configuration, output, side effects, compatibility, or recovery behavior changes.
## Validate the worktree

With the prerequisites installed, run the workspace build and test lanes:

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

`just build` and `just test` prepare authenticated compiler components beside Cargo's debug binaries, then use that
authority for the whole recipe. Preparation runs offline and reuses exact unchanged components; missing prerequisites
stop the recipe. Both recipes are workspace-wide and do not lower plan selectors.

On macOS, `just test` also runs the Cranelift production-cache contract.
`just test-cranelift` runs that contract alone. Both recipes install missing components from
[the pinned Cranelift toolchain](.config/cranelift-toolchain.toml), including its matched development files and backend.
The first run may download these components; later runs reuse the installed toolchain.
Advance that separate nightly pin only after the focused lane passes.

On the local macOS workstation, `just check` first runs `just fix`, then validates the resulting worktree.
Both commands run host Clippy and
cross-target Clippy for Linux GNU/musl and Windows MSVC on x86-64 and ARM64, with all Cargo targets and features.
`just fix` applies Rustfmt and Clippy edits, including to dirty or staged files; review the resulting diff.
Cross-target checks do not execute tests or prove final executable linking.

CI uses `just ci-check` for native formatting, Clippy, dependency policy, and documentation checks without source
repairs. Dependency unification remains in the local check because it analyzes the repository's full target policy.
Run `just test` separately for runtime tests, including native cache tests. `just check-tooling` validates
the installer and updater scripts; it does not update tooling. Use `just check-compiler-driver` for the excluded
compiler driver.

## Work on compiler integration

Plain `cargo build` can build the general CLI without compiler components. Native cache reuse and Surface compiler
facts use the separately built driver. Install its selected-toolchain prerequisite and run its dedicated checks when
changing either compiler contract:

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
