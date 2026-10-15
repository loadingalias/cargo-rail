# Contributing to Cargo-Rail

## Validate a change

Run commands from the repository root.
Run `just --list` to show the maintained commands.
Start with the smallest lane that tests the changed behavior:

| Change                    | Start with |
| ------------------------- | ---------- |
| Markdown                  | `git diff --check` |
| Rustdoc                   | `just check-docs` |
| CLI or mutation           | `just test` |
| Planning                  | `just test`, then `just plan` |
| Compiler                  | `just test`; add `just check-compiler-driver` for driver changes |
| Cache                     | `just test-cache-host` |
| Release                   | `just test`; add `just release-check` to preview the next release |
| Cross-target compilation  | `just check-cross` |
| Tooling or GitHub Actions | `just check-tooling` |

`just ci-check` runs the shared checks that do not change source.
It runs on any development host.
On the macOS maintainer workstation, `just check` adds tests and cross-target checks.
`just check` does not repair source.
Run `just fix` as a separate step, then review its diff.

Cross-compilation proves that code builds for a target.
To prove runtime, filesystem, architecture, or performance behavior, run the code on that target.
Maintainers use the `just ssh-*` recipes to run native Linux and Windows lanes on temporary remote hosts.
These recipes require the maintainer's private `dev-machines` tool.
They delete each host after its run.

## Set up the selected lane

Install these tools:

- Rust through `rustup`.
  `rust-toolchain.toml` selects the repository toolchain and components.
- `just`, `cargo-nextest`, and `cargo-deny`.
- uv, which runs each repository Python script with the Python that `.python-version` names
  and the packages that `uv.lock` pins.
- For `just build` and `just test`: `rustc-dev` for the selected toolchain.
- For `just check` and `just ci-check`: `rustc-dev` for the selected toolchain.
- For `just check` on macOS: Zig, `cargo-zigbuild`, `cargo-xwin`, and the LLVM tools that cargo-xwin requires.
- For `just check-tooling`: ShellCheck, Actionlint, and ripgrep (`rg`).
- For `just test` on macOS: clang with COFF support, `lld-link`, and `ld64.lld` on `PATH`.
  Also install `cargo-xwin` with its x86_64 MSVC SDK already cached.
  The cross-link test runs offline.

To verify the binary installation of a native archive, run `uv run python scripts/check-release-install.py OUTPUT_DIRECTORY` after `just package-release OUTPUT_DIRECTORY`.
This check requires cargo-binstall.
The check uses a temporary installation.
It disables source fallback.
The packaging workflow requires this check to pass on every release host before it uploads assets.

Each native runner installer requires an operation, for example `scripts/tooling/x86_64-linux.sh ci` or `scripts/tooling/x86_64-win.ps1 -Operation package`.
The tooling catalog selects the Cargo tools, Rust components,
and native prerequisites for the operation.
Package provisioning keeps the compiler development components and the native build tools.
It does not include the check and test tools.
Each installer first installs the pinned uv, which provides Python.
Ubuntu packages come from the archive of the host.
An installer skips each tool that is already at its pin.
A complete run records the fingerprint of the files that the installer reads.
While that fingerprint matches, a rerun only publishes the environment again.
`scripts/tooling/x86_64-linux.sh fingerprint ci` prints the fingerprint on any host.
Unix release jobs use `scripts/tooling/package-unix.sh` with their native target. macOS packaging requires the Xcode tools, CMake,
and rustup of the runner.

## Make a change

- Limit each change to one problem.
- Before you change an ownership or mutation boundary, read [the architecture guide](docs/architecture.md).
- Put user-visible behavior in the library.
  Keep `src/main.rs` limited to process setup, diagnostics, and dispatch.
- Add or update tests for changed behavior.
  The normal suite uses cargo-nextest.
  Doctests run separately.
- When commands, configuration, output, side effects, compatibility,
  or recovery behavior change, update the documentation.

## Lane details

`just build` prepares the authenticated compiler driver and source bundle beside Cargo’s debug binaries.
`just test` also prepares source-installation authority for its integration fixtures.
Each recipe uses its prepared authority for the commands that it runs.
Preparation runs offline.
It reuses each component that is exactly unchanged.
If a prerequisite is missing, the recipe stops.
Both recipes apply to the full workspace.
They do not lower plan selectors.

On macOS, `just test` also runs the Cranelift production-cache contract.
`just test-cranelift` runs only that contract.
Both recipes install missing components from [the pinned Cranelift toolchain](.config/cranelift-toolchain.toml),
including its matched development files and backend.
The first run can download these components.
Later runs reuse the installed toolchain.
Move that separate nightly pin to a newer nightly only after the focused lane passes.

`just ci-check` runs these checks:

- formatting;
- Markdown lint;
- host Clippy with all Cargo targets and features;
- dependency policy with the target scope from `deny.toml`;
- documentation; and
- the dedicated checks of the excluded compiler driver.

CI runs the same recipes one at a time, when the plan for the change selects them.
Markdown lint runs only on the workstation.
The pinned `rumdl` is installed there.

For runtime tests, run `just test` separately.
These tests include native cache tests and doctests.
Local work and CI use the default nextest profile and the same concurrency policy
for the full suite.
`just check-tooling` validates the installer and updater scripts.
It also runs Actionlint on the workflows.
It does not update tooling.
CI runs it once on Linux x64.
To check PowerShell syntax, CI also runs `scripts/tooling/check.ps1` once with the existing PowerShell runtime on Windows.

Use `just test-cache-host` for the native local-cache, remote-storage, and mTLS distributed-worker qualification.
It uses the `cache-host` Cargo profile to build the library test harness and the separate `cache` integration target.
This profile disables debug information and keeps assertions.
The recipe then runs the required cases one after another and stops at the first failure.
`just test` still includes the same cases.
The cache-only lane does not run doctests or unrelated integration tests.

IBM Z, IBM POWER, and RISC-V have no release archives.
Their runners prove native caching on the real architecture, not a Cargo-Rail release for it.
On every push to `main` and on every dispatched run, an x86-64 runner does this work for each target:

- It cross-builds the test archive for the target and `cargo-nextest` for the native runner.
- It runs the Clippy and documentation checks for the target.

The checks of these runs gate a release.
The native runner installs only its compiler, Clippy, and `rustc-dev`, which is the `cache-host` tooling.
It builds and tests the compiler driver.
It runs the library and `cache` suites from the archive.
The archive must include every required qualification case.
The Planning, Unify, release, and other integration suites run on x86-64, arm64, and Windows.
Doctests also run on these hosts.
Pull requests do not run on the IBM Z, IBM POWER, and RISC-V hosts.

To reproduce that transfer, do these steps:

1. Install the cross-build tooling with `scripts/tooling/x86_64-linux.sh cross-build PLATFORM`.
   Set `PLATFORM` to `riscv64-linux`, `s390x-linux`, or `powerpc64le-linux`.
1. Source the emitted tooling environment.
1. Run `just test-cache-host check TARGET`.
1. Run `just test-cache-host prepare TARGET target/transfer`.
1. Run `just test-cache-host runner-tools TARGET target/runner-tools`.
1. Move both directories to the same source checkout on the native host.
1. Install the `cache-host` tooling of the native host.
1. Put `target/runner-tools/bin` on `PATH`.
1. Run `scripts/check-compiler-fact-driver.sh --channel "$RUSTUP_TOOLCHAIN"`.
1. Run `scripts/check-cache-host.sh run-suite target/transfer`.

The archive is built with the optimized `cache-host` profile.
It contains the compiler driver and the source-installation CLI.
The transfer requires the same source, the same compiler release and commit,
and the same Nextest build.
A missing or ignored qualification case causes a failure.

To run only the checks of the excluded compiler driver, use `just check-compiler-driver`.
`just check-compiler-driver-channel TOOLCHAIN` builds and tests the driver with another installed toolchain that has `rustc-dev`.
CI runs it for the current stable, beta, and nightly.
When a new nightly changes the internal API of the compiler, gate the driver on that change in `tools/compiler-fact-driver/build.rs`.

## Work on compiler integration

Plain `cargo build` can build the general CLI without compiler components.
Native cache reuse and Surface compiler facts use the separately built driver.
When you change either compiler contract,
install the driver prerequisite for the selected toolchain.
Then run the dedicated driver checks:

```bash
rustup component add rustc-dev
just check-compiler-driver
```

The driver is built separately because it is bound to one exact Rust compiler toolchain.
To package the closed driver source inventory
for another compatible toolchain without publishing Cargo-Rail core, run:

```bash
just package-compiler-adapter OUTPUT_DIRECTORY
```

The recipe writes one content-addressed adapter pack and its checksum.
It does not replace existing outputs.
It does not upload the outputs.

## Support performance claims with evidence

Follow [the benchmarking guide](docs/benchmarking.md).
State the workload, host, toolchain, command, sample count, correctness checks,
and before/after results.
Keep the raw results.
Compare cache implementations on the same host.
Do not apply a result to platforms or workloads that you did not measure.

## Update command documentation

Edit the CLI help in `src/`.
Then examine the `--help` output of the built command.
In guides, describe decisions and complete tasks.
Do not repeat flag descriptions in guides.

## Record release intent

For a user-visible change, add a `.changes/<area>-<behavior>.md` file.
Give the file a short, lowercase, hyphenated name.
Make the name identify the behavior, not a ticket, release number, or implementation detail.
Run `cargo rail change add --help` to see the current command.
Run `cargo rail change status` to review pending entries.

Each file contains TOML frontmatter and a release-note body:

```markdown
---
"cargo-rail" = "patch"
---

Preserve literal target keys when applying dependency repairs.
```

Describe the shipped result and any operator action that it requires.
Record one coherent change in each file.
Do not record intermediate implementations or test-only cleanup.
When an entry needs tracking but no version bump, use `none`.
Keep the migration and recovery instructions for a breaking contract together.

The [release workflow](.github/workflows/release.yml) runs the source-built Cargo-Rail engine in this sequence:

1. The first job commits versions, changelogs, and consumed change files.
   It also tags, pushes, and opens the draft release.
1. [Native packaging](.github/workflows/package.yml) builds, attests, and attaches each archive from the tag.
1. The last job verifies the assets and publishes.

To finish an interrupted release, run the workflow again.
For recovery and the Cargo-Rail/Action bootstrap order, see [releasing a workspace](docs/releases.md).

## Open a pull request

- Explain the user-visible result and the reason for the change.
- List the exact validation commands and their results.
- Identify compatibility changes to CLI output, configuration, plan contracts, stored formats,
  or release state.
- Link the issue if one exists.

## Report security issues privately

Do not open a public issue for a vulnerability.
Follow [the security policy](SECURITY.md).
