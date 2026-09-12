# Contributing to Cargo-Rail

## Set up the repository

Install these tools:

- Rust through `rustup`; `rust-toolchain.toml` selects the repository toolchain and components.
- `just`, `cargo-nextest`, and `cargo-deny`.
- For `just build` and `just test`: Python 3.11 or newer and `rustc-dev` for the selected toolchain.
- For `just check` and `just ci-check`: `rustc-dev` for the selected toolchain.
- For `just check` on macOS: Zig, `cargo-zigbuild`, `cargo-xwin`, and LLVM tools required by cargo-xwin.
- For `just check-tooling`: Python 3.11 or newer, ShellCheck, Actionlint,
  and ripgrep (`rg`).
- For `just test` on macOS: clang with COFF support, `lld-link`, and `ld64.lld` on `PATH`, plus `cargo-xwin` with its x86_64 MSVC SDK already cached.
  The cross-link test runs offline.

Run commands from the repository root.
Use `just --list` to see the maintained command surface.

Native runner installers require an operation: for example, `scripts/tooling/x86_64-linux.sh ci` or `scripts/tooling/x86_64-win.ps1 -Operation package`.
The tooling catalog selects the operation's Cargo tools, Rust components, and native prerequisites.
Package provisioning retains the compiler development components and native build tools
while omitting check and test tools.
Unix release jobs use `scripts/tooling/package-unix.sh` with their native target; macOS packaging requires the runner's Xcode tools,
CMake, Python, and rustup.

## Make a change

- Keep each change focused on one problem.
- Read [the architecture guide](docs/architecture.md) before changing an ownership or mutation boundary.
- Put user-visible behavior in the library.
  Keep `src/main.rs` limited to process setup, diagnostics, and dispatch.
- Add or update tests for changed behavior.
  The normal suite uses cargo-nextest; doctests run separately.
- Update documentation when commands, configuration, output, side effects, compatibility,
  or recovery behavior changes.

## Validate the worktree

With the prerequisites installed, run the workspace build and test lanes:

```bash
just build
just test
git diff --check
```

Run the complete quality lane when a change crosses planner, mutation, cache, compiler, release,
platform, or public-contract boundaries:

```bash
just check
```

`just build` prepares the authenticated compiler driver and source bundle beside Cargo’s debug binaries.
`just test` also prepares source-installation authority for its integration fixtures.
Each recipe uses its prepared authority for the commands it runs.
Preparation runs offline and reuses exact unchanged components;
missing prerequisites stop the recipe.
Both recipes are workspace-wide and do not lower plan selectors.

On macOS, `just test` also runs the Cranelift production-cache contract.
`just test-cranelift` runs that contract alone.
Both recipes install missing components from [the pinned Cranelift toolchain](.config/cranelift-toolchain.toml),
including its matched development files and backend.
The first run may download these components; later runs reuse the installed toolchain.
Advance that separate nightly pin only after the focused lane passes.

Local work and CI use the same nonmutating `just ci-check` lane: formatting,
host Clippy with all Cargo targets and features, dependency policy using `deny.toml`'s target scope,
documentation, and the excluded compiler driver's dedicated checks.

On the macOS workstation, `just check` first runs `just fix`, then `just ci-check`,
followed by cross-target Clippy for Linux GNU/musl and Windows MSVC on x86-64 and ARM64,
and `cargo rail unify --check --explain` against the repository.
Fixing applies workspace Rustfmt and host Clippy repairs, including to dirty or staged files;
review the resulting diff.
Cross-target checks do not execute tests or prove final executable linking.
CI calls `just ci-check` for this lane; workstation cross-compilation and dogfooding remain outside it.

Run `just test` separately for runtime tests, including native cache tests and doctests.
Local work and CI use the default nextest profile and the same concurrency policy
for the full suite.
`just check-tooling` validates the installer and updater scripts
and runs Actionlint over the workflows without updating tooling.
CI runs it once on Linux x64 and runs `scripts/tooling/check.ps1`
once with Windows' existing PowerShell runtime to check PowerShell syntax.
Use `just test-cache-host` for the native local-cache, remote-storage, and mTLS distributed-worker qualification.
IBM Z and POWER qualification is manual and remains deferred until runner access is available;
a green default CI run does not qualify these hosts.
It builds the library test harness and the separate `cache` integration target with the `cache-host` Cargo profile
(debug information disabled; assertions retained),
then runs required cases serially and stops on the first failure.
The same cases remain in `just test`; the cache-only lane does not run doctests or unrelated integration tests.
CI invokes `scripts/check-cache-host.sh` directly so IBM Z and POWER do not need Just or Nextest installed.

RISC-V builds the same cache harnesses and Cargo binaries on x86-64,
then runs them through Nextest on the native runner.
Install `scripts/tooling/x86_64-linux.sh riscv-build`, source the emitted tooling environment, and run `just test-cache-host prepare riscv64gc-unknown-linux-gnu target/riscv-cache`.
Transfer that directory to the same source checkout on RISC-V, install its `ci` tooling,
and run `just test-cache-host run target/riscv-cache` locally or `scripts/check-cache-host.sh run target/riscv-cache` in CI.
The transfer requires the same source, compiler release and commit, Nextest build,
and exact test selection; missing or ignored cases fail.
The native runner installs prebuilt Nextest and retains Rust, compiler development components,
a linker, and OpenSSL for the cache fixtures.
The authenticated compiler-driver source travels in the archive
and bootstraps against the native compiler.
No doctests or unrelated integration tests enter this lane.
Use `just check-compiler-driver` to run only the excluded compiler driver's checks.

## Work on compiler integration

Plain `cargo build` can build the general CLI without compiler components.
Native cache reuse and Surface compiler facts use the separately built driver.
Install its selected-toolchain prerequisite and run its dedicated checks
when changing either compiler contract:

```bash
rustup component add rustc-dev
just check-compiler-driver
```

The driver is built separately because it is tied to one exact Rust compiler toolchain.

## Support performance claims with evidence

Follow [the benchmarking guide](docs/benchmarking.md).
State the workload, host, toolchain, command, sample count, correctness checks,
and before/after results.
Preserve raw results.
Compare cache implementations on the same host,
and do not generalize a result beyond the platforms and workload that were measured.

## Update command documentation

Edit the owning CLI help in `src/`, then run `just docs` to regenerate `docs/commands/`.
The generator reads the built executables' public `--help` output, including nested commands.
Use `just check-docs` to reject stale, missing, or obsolete reference pages without rewriting them.
Do not edit generated pages by hand.

## Record release intent

For user-visible changes, add a `.changes/<area>-<behavior>.md` file with a short lowercase, hyphenated name.
Name the behavior rather than a ticket, release number, or implementation detail.
Use `cargo rail change add --help` for the current command and `cargo rail change status` to review pending entries.

Each file contains TOML frontmatter and a release-note body:

```markdown
---
"cargo-rail" = "patch"
---

Preserve literal target keys when applying dependency repairs.
```

Describe the shipped result and any operator action it requires.
Keep one coherent change per file;
avoid recording intermediate implementations or test-only cleanup.
Use `none` when an entry needs tracking but no version bump.
Keep breaking-contract migration and recovery instructions together.

The [release workflow](.github/workflows/release.yml) requires prepared versions and changelog entries, consumed change files,
and successful CI for the exact commit being released.
Green CI alone does not prepare a release.

## Open a pull request

- Explain the user-visible result and why the change is needed.
- List the exact validation commands and their results.
- Identify compatibility changes to CLI output, configuration, plan contracts, stored formats,
  or release state.
- Link the issue when one exists.

## Report security issues privately

Do not open a public issue for a vulnerability.
Follow [the security policy](SECURITY.md).
