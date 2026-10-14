# Caching

Cargo-Rail keeps Cargo as the executor.
Each layer removes only the work that it can prove is reusable.

| Layer | Authority | Result |
| --- | --- | --- |
| Cargo L0 | Cargo fingerprints and incremental state | Cargo skips fresh work |
| Local L1 | Exact compiler action, result, and stored bytes | Cargo-Rail restores one compiler result |
| Remote L2 | The same verified result pack under machine-owned provider authority | The result enters L1, and then Cargo-Rail restores it |
| Distributed miss | A pinned worker capability and validated response | One eligible miss runs on a remote worker |
| Compiler evidence | Identity-matched, revalidated observations and typed facts | Unify or Surface skips the corresponding acquisition |

A lookup alone does not authorize reuse.
When cache evidence is missing, stale, ambiguous, unsupported, or corrupt,
Cargo-Rail runs the compiler and reports a stable miss or bypass reason.
If the compiler or a required acquisition fails, the operation still fails.

## Set up local reuse

Setup enrolls the current workspace for ordinary Cargo, nextest, Just, IDE, and CI commands.
Workspaces that use the same effective Cargo home share the installed wrapper.
Each enrollment owns a separate cache profile.

Set up and check local reuse:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache ready
cargo rail cache status --scope local
cargo rail doctor native-cache
```

`cache setup --check` shows the changes without applying them.
`cache setup` applies the changes.
Setup owns one global `build.rustc-wrapper`, private launcher and worker bytes, and an installation receipt.
Setup copies the compiler fact driver and driver-source components
that the build's embedded authority declares.
It puts these components beside the worker.
Setup resolves launcher symlinks to find the real Cargo-Rail executable.
If a declared file beside that executable is missing or changed,
setup fails before it writes anything.
Setup does not build or download these components.

Setup checks the three kinds of components differently:

| Component | Before installation | After installation |
| --- | --- | --- |
| Compiler fact driver and its source | Digest against the authority embedded in `cargo-rail` | Digest against the setup receipt |
| Wrapper and worker | Same Cargo-Rail release as `cargo-rail`, by version query | Digest against the setup receipt |
| `cargo-rail` itself | Trusted as the invoked executable | Not installed |

The embedded authority protects against a driver that was built for a different compiler
or from different source.
It does not protect against a person who can write files beside `cargo-rail`.
That person can also replace `cargo-rail`.
Verify the archive before you extract it.
After installation, `cache status` reports each installed component that changes.

Native reuse requires an authenticated driver for the selected compiler.
An installation without embedded component authority can use an independently authenticated adapter
pack.
An ordinary `cargo install` is such an installation.
Without either authority, compiler work runs without reuse.

In this checkout, `just build` and `just test` prepare the authenticated driver and source bundle.
Then they build Cargo-Rail with that component authority.
Preparation requires `rustc-dev` for the selected toolchain.
It also requires the locked dependencies of the driver in the local Cargo cache.
Preparation does not download missing components or crates.
`just check-compiler-driver` runs the separate driver checks.
Plain Cargo builds do not do this preparation.

`cache ready` runs three builds with the selected toolchain: one isolated uncached build, one cold cached build,
and one verified warm restore.
If the rustup toolchain does not have `rustc-dev`, `cache ready` installs it first.
Compiler wrappers never install components during a build.
`cache ready` requires an enrolled, authenticated profile.
It makes the three builds use only L1.
An L2 selection stays installed, but this probe sends no requests to it.
A successful probe records readiness for that exact profile and rustc identity.
A change to the profile or toolchain makes the recorded readiness stale.
To qualify remote transport, use `cache probe` separately with an explicit machine-owned authority.

### Stable, beta, and nightly compilers

Cargo-Rail reuses results for every stable, beta,
or nightly compiler at or above the adapter's minimum.
The driver is built for the exact selected compiler.
At build time, the driver selects the internal API of that compiler.
CI builds and tests the driver against the current stable, beta, and nightly compilers.
Cargo-Rail refuses a compiler below the minimum and reports the required release and commit date.

Nightly Cargo builds libraries without embedded metadata (`-Zembed-metadata=no`).
Each rlib is a stub, and its metadata is in a separate rmeta file.
A crate that links a library receives both files under one extern name.
A result binds both outputs.
Cargo-Rail refuses a result that does not have its rmeta.
Each file of a paired extern enters the action identity separately.
Each rlib and rmeta found through `-L dependency=` also enters the action identity separately.
Thus, a change to either file causes a miss.

A compilation reads only crate metadata when every output is an rlib or
when nothing is compiled to object code.
Such a compilation never reads the rlib beside an rmeta that rustc selected.
That rlib is not an input.
Cargo can write that rlib while a pipelined dependent compiles.
This write does not block or invalidate reuse.
Nightly Cargo's per-unit build directories (`build/PACKAGE/HASH/out`) restore the same way as the classic layout.

Limits:

- Distributed execution does not accept nightly Cargo's metadata split.
  Those actions compile locally.
- Nightly Cargo's per-unit build directories can be used with a target directory outside the
  repository.
  In that case, a crate that has dependencies compiles without reuse (`compiler_native_input_evidence_unavailable`).
  The cause is that each dependency directory is outside every root that the input witness can name.
- A nightly compiler that is newer than the last nightly
  that CI built can change the internal API of the compiler.
  If the driver does not build for that nightly, `cache ready` fails, and compiler work runs without reuse.

## Select an independent compiler adapter

An adapter pack contains the closed,
vendored fact-driver source inventory and both compiler protocols.
Before Cargo-Rail uses the selected pack, it does these operations:

- It authenticates the pack with a SHA-256 digest that the operator supplies.
- It checks protocol compatibility.
- It builds the pack with the exact selected compiler.
- It runs a metadata-compilation calibration.

The pack sets a minimum compiler release and commit date.
It does not set an upper bound.
If the build or the calibration fails, native cache work bypasses reuse and runs ordinary rustc.
A Surface command that requires compiler facts still fails.

To publish a pack without publishing Cargo-Rail core, package the pack with this command:

```bash
just package-compiler-adapter ./adapter-assets
```

The command writes one content-addressed JSON pack and its adjacent `.sha256` file.
It refuses to replace existing outputs.
It does not upload its outputs.

Verify the downloaded checksum.
Then select the extracted JSON file with an absolute path:

```bash
export CARGO_RAIL_COMPILER_ADAPTER_PACK=/absolute/path/cargo-rail-compiler-adapter-DIGEST.json
export CARGO_RAIL_COMPILER_ADAPTER_PACK_SHA256=sha256:FULL_DIGEST
cargo rail surface --prepare
```

Set both variables together.
The digest is the explicit trust decision for the machine.
Cargo-Rail caches the calibrated executable.
The cache key for that executable has these parts: the pack digest, the complete `rustc -vV` identity,
the compiler-library digest, the build target, and both protocol versions.

`just package-release OUTPUT_DIRECTORY` builds the native release components.
It writes a deflated ZIP archive with `cargo-rail-components-v1.tsv` inside the archive.
It also writes an adjacent `SHA256SUMS` file.
Use a separate output directory for each host.
The command verifies component digests and refuses to replace existing release outputs.
It does not publish its outputs.
The [packaging workflow](../.github/workflows/package.yml) produces these archives.
The [release transaction](releases.md) verifies the exact producer attempt and bytes before publication.
Extract the complete archive so that the component files stay together.
The companion cache Action requires these components: core, wrapper, worker, matched driver,
and authenticated driver source.
Build-host eligibility does not mean that the release workflow publishes an archive
for every eligible platform.
See [installation](../README.md#installation).

Setup also binds the exact physical workspace root to one private profile.
Each profile is a separate trust domain with its own bounded content-addressed store (CAS).
If you run setup in another workspace, setup creates another profile.
It does not replace the first profile.
In a workspace that is not enrolled, compilation runs without L1 or L2 reuse.
Setup fails if it finds another global wrapper, persistent shadowing, ambiguous Cargo homes,
linked authority paths, or changed owned state.

Cargo freshness and incremental compilation stay L0.
An L1 action binds these items:

- the compiler and toolchain;
- the arguments, target, and environment;
- the dependencies;
- the source topology and bytes;
- the [native libraries](#native-libraries) that rustc resolves itself;
- the declared outputs.

Physical mode also binds the canonical workspace root.
Remap mode replaces that root only for certified portable result classes.
Remap mode keeps the physical output directory of the executor out of the portable operation
identity.
Before restore, Cargo-Rail verifies the stored descriptors and every output byte again.
If the bounded source capture cannot prove that an unused path was irrelevant,
it intentionally invalidates more than necessary.

Cargo-Rail captures an unpacked registry package whole.
Thus, a crate that reads a package file outside its source directory is reusable.
An example of such a read is `#![doc = include_str!("../README.md")]`.
A change to any file in the package invalidates the results for that package.
For other packages outside the workspace,
Cargo-Rail captures only the source directory of the crate.
A read outside that directory makes the compiler run without reuse.

An eligible cold compilation uses the shared, compiler-matched fact driver.
The driver records the Rust libraries that rustc selects, including transitive libraries.
It also records the candidate filenames that rustc searches.
This native-input witness is separate from diagnostic facts.
Before Cargo-Rail restores outputs, it checks the selected files, matching alternatives,
and missing candidates.
Metadata-only work records that no code generation occurred.
Code generation bypasses reuse when it uses inline or global assembly,
or an LTO mode that can import dependency assembly.
The cause is that the assembler inputs are incomplete.

Native reuse requires that the selected `RUSTC` executable resolves to the compiler of the captured
sysroot.
A custom compiler program or an opaque workspace wrapper runs without reuse and keeps its flags
and behavior.
A matching version response alone does not authorize Cargo-Rail to replace
that program with the shared driver.

Compiler-selected environment values stay exact inputs, even when they contain workspace paths.
A different literal value produces a different action.
Dep-info path rebinding keeps the trailing `# env-dep:` records of rustc byte-for-byte.
Cargo-Rail does not support dependency rules that are interleaved after those records.

A result is:

- a `hit` only after the current inputs and stored bytes verify;
- a `miss` when no authoritative result exists; successful cold output can then populate the CAS;
- a `bypass` when the invocation is outside the supported class and runs without reuse; or
- a `conflict` when one action produced different semantic results; in that case, neither result restores.

If Cargo-Rail cannot capture the inputs of an action, the action bypasses reuse.
The bypass reason names the cause:

| Reason                                  | Condition |
| --------------------------------------- | --------- |
| `source_namespace_entry_unsupported`    | The source directory of the crate contains a symbolic link, reparse point, or special file |
| `generated_namespace_entry_unsupported` | The `OUT_DIR` of the build script contains a symbolic link, reparse point, or special file |
| `action_capture_bound_exceeded`         | The input capture reached an entry, path, byte, depth, or time bound |
| `native_library_capture_unavailable`    | Cargo-Rail cannot observe a native-library candidate that rustc tested, or the candidate changed while rustc searched |
| `cold_input_changed_during_execution`   | An input that Cargo-Rail captured after the compiler exited changed after the compiler started. Examples are a linker input, a native-library candidate, and a macro read |
| `native_library_search_unreported`      | The linker received a file by path from a native search directory that no reported native-library search selected |
| `complete_action_capture_unavailable`   | Any other capture error. `cache status` counts it as a failure |

A [cache report](#aggregate-cache-measurements) names the entry, bound, or candidate that caused each of these reasons.

To disable reuse for one process tree without a change to setup, run:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

## Reuse Clippy results

`cargo clippy` runs `clippy-driver` as Cargo's workspace wrapper for each workspace member.
Cargo-Rail reuses those results with the same exact rule that it uses for rustc results.
A Clippy action binds its rustc-equivalent action and every input that Clippy adds:

- the bytes of `clippy-driver`, which must be the driver of the matched sysroot;
- whether Clippy runs lints, and the arguments that it appends, including `CLIPPY_ARGS` and `--cfg clippy`;
- `CLIPPY_ARGS`, `CLIPPY_CONF_DIR`, `CLIPPY_DISABLE_DOCS_LINKS`, `CARGO_PKG_RUST_VERSION`, and `CARGO_MANIFEST_DIR`;
- each `.clippy.toml` and `clippy.toml` candidate that Clippy checks, including every absent candidate,
  from `CLIPPY_CONF_DIR` or otherwise the package directory, up to the first directory that contains one;
- the inputs of `cargo metadata`, which `clippy::cargo` lints run during linting:
  - the `cargo` executable;
  - non-secret `CARGO_*` variables;
  - `RUSTC` and `RUSTUP_TOOLCHAIN`;
  - each `.cargo/config.toml` and `.cargo/config` that Cargo reads, including those in the Cargo home;
  - the workspace manifests and every path dependency that they reach;
  - `Cargo.lock`.

A source attribute can enable a `clippy::cargo` lint.
Thus, every Clippy action binds the Cargo inputs.
A change to a workspace manifest, the lockfile,
or Cargo configuration therefore causes a miss for every Clippy result in that workspace.
`CLIPPY_TERMINAL_WIDTH` is not bound.
Clippy uses it only to lay out configuration errors, and Cargo-Rail never stores a failed result.

On a cold miss, the authenticated compiler driver runs a resolution phase beside Clippy.
This phase uses the exact arguments of Clippy.
The phase records the Rust libraries and search candidates that rustc selects.
It stops when crate loading freezes, and it writes nothing.
That record is the same Rust-input witness that a rustc result keeps.
The phase ends before analysis.
Thus, it adds parsing and macro expansion work to a miss.
When Clippy runs longer than the phase, the phase adds no wall time to the critical path.

These Clippy invocations run without reuse:

| Reason                                    | Condition |
| ----------------------------------------- | --------- |
| `clippy_linked_output_unavailable`        | Clippy links output, such as the build script of a workspace member |
| `clippy_sysroot_override_unavailable`     | `SYSROOT` is set, so Clippy selects another sysroot |
| `clippy_driver_identity_unavailable`      | `clippy-driver` is not the driver of the matched sysroot |
| `clippy_rustc_program_unavailable`        | The file stem of the rustc argument is not `rustc`, so Clippy does not run in wrapper mode |
| `clippy_configuration_outside_repository` | Clippy loads a configuration file outside the repository |
| `clippy_configuration_unavailable`        | Cargo-Rail cannot observe the configuration lookup of Clippy |
| `clippy_cargo_inputs_unavailable`         | Cargo-Rail cannot list the inputs of Cargo, for example when a manifest sets `package.workspace` |

The rustc bypass classes also apply.
Dep-info records `CLIPPY_ARGS` and `CLIPPY_CONF_DIR`.
Thus, an L2 selection shares Clippy results only after setup approves both names with `--remote-environment`.
The same rule applies to every compiler environment read.

With `--root-portability remap`, Clippy results are shared across checkout roots.
A bound path below the repository, the Cargo home,
or the compiler sysroot is spelled relative to that root.
A bound variable whose value names such a path is also spelled relative to that root.
An absent candidate outside those roots is not bound.
Cargo and Clippy read nothing there, and every lookup captures the inputs again.
An input that exists outside those roots keeps its host path.
Thus, its result is reused only at its own root.
Cache setup writes the absolute path of the installed wrapper into the Cargo home configuration.
When that entry selects the installed wrapper, the configuration file is bound without the entry.
The installed wrapper passes the compiler queries of Cargo through unchanged.
A configuration file that selects any other wrapper is bound whole.

## Procedural macros

A procedural macro is a bound input.
Its exact library bytes enter the action.
This applies when the crate depends on the macro directly and
when the crate reaches the macro through a re-export.
Rustc records what the expanded code includes.
It also records the environment that the code reads through `env!`, `option_env!`, and tracked environment.
The action binds those records.

A macro can also read files, list directories, and read variables that rustc never records.
On every miss, including the Clippy resolution phase,
the compiler driver observes what each loaded macro does through the C library.
On Linux, the driver also observes what the macro does through the kernel.
The action then binds each observed read:

- A variable read joins the environment reads of the compiler.
  It binds and shares in exactly the same way as an `env!` read.
- A repository path binds its state at the time when the action was captured:
  the content and mode of a file, the absence of the path,
  or the entry names and kinds of a directory.
- A read inside a namespace that the action already captures whole is already bound.
  Examples are the sources of the crate and the output of its build script.
- `proc-macro-crate` runs `cargo locate-project` to find the workspace.
  That query binds the Cargo inputs that answer it.

Thus, a hit is stricter than Cargo freshness.
Cargo freshness does not detect a macro read that no build script declares.

These units run without reuse:

| Reason                                     | Condition |
| ------------------------------------------ | --------- |
| `procedural_macro_read_outside_repository` | The macro reads a path outside the repository and outside the captured namespaces |
| `procedural_macro_read_through_symlink`    | The macro reads a repository path through a symbolic link |
| `procedural_macro_process_unmodeled`       | The macro starts a process other than a modeled `cargo locate-project` query |
| `procedural_macro_process_control`         | The macro forks, replaces its process, changes its user, group, or session, or starts a process with a changed environment, directory, or descriptors |
| `procedural_macro_secret_environment`      | The macro reads a variable with a secret-like name |
| `procedural_macro_environment_enumeration` | The macro lists all environment variables |
| `procedural_macro_environment_write`       | The macro changes the environment |
| `procedural_macro_file_write`              | The macro creates, writes, renames, or removes a file |
| `procedural_macro_network`                 | The macro creates, connects, or uses a socket |
| `procedural_macro_host_state`              | The macro reads host state, such as the host name, kernel identity, or file-system statistics |
| `procedural_macro_working_directory`       | The macro changes the working directory |
| `procedural_macro_dynamic_load`            | The macro loads a library at run time |
| `procedural_macro_executable_memory`       | The macro maps or generates executable memory |
| `procedural_macro_dynamic_dependency`      | The macro links a shared library other than the system C library |
| `procedural_macro_import_unclassified`     | The macro imports a C library function that the driver does not classify. A cache report names the function |
| `procedural_macro_initializer`             | The macro ran initialization code before the driver could observe it |
| `procedural_macro_raw_system_call`         | The macro issues a system call without the C library, and the platform cannot observe the call |
| `procedural_macro_path_unavailable`        | A path that the macro used cannot be resolved to a stable name |
| `procedural_macro_observation_limit`       | The macro reads more paths or variables than an action binds |
| `procedural_macro_observation_unavailable` | The platform cannot observe macros, as the host list below describes |

Linking a function is not calling it.
A macro library often links code that expansion never runs.
Examples are the event queue of an asynchronous runtime, fork handlers, and sockets.
The driver classifies every function that the standard library and common runtimes import.
The driver hooks each function that has an effect.
The hook records that effect only when the macro calls the function.
The import tables and the Linux system-call table classify each operation in the same way.
A test enforces this rule.

Observation depends on the host:

- Linux: After rustc loads each macro image, the driver rebinds the C library imports of that image.
  A per-thread seccomp user-notification filter reports the system calls
  that the macro's own code issues.
  Kernels older than 5.5, and containers whose seccomp profile blocks that filter, report `procedural_macro_observation_unavailable`.
  An image can have initializers that do more than the setup of the C runtime
  and the Rust standard library.
  Such an image reports `procedural_macro_initializer`, because those initializers ran before instrumentation.
- macOS: The driver instruments each macro image before its initializers run.
  There is no unprivileged system-call observer on macOS.
  Thus, an image that contains a system-call instruction bypasses reuse.
  On Apple silicon, the check is exact.
  On x86-64, the check examines every byte offset, and ordinary code often contains those bytes.
  Thus, most macro images bypass reuse on x86-64.
- Windows and other hosts: Every unit that loads a procedural macro reports `procedural_macro_observation_unavailable`.
  Native result reuse is not implemented on Windows yet, so this does not change a Windows result.

Distributed workers never run procedural macros.
Those units stay local.

## Build-script execution

Cargo-Rail does not reuse the output that a build script makes when it runs.
The compilation of the build script is an ordinary rustc action, and it can hit.
When the build script runs, it can compile native code.
For example, `aws-lc-sys` does this with cmake and a C compiler.
That output depends on all the inputs that cmake, the C compiler, and the platform SDK read.
`rerun-if-changed` and `rerun-if-env-changed` declare only part of these inputs.
Exact reuse would need file tracing of the full process tree of the build script on every supported
host.
Thus, build scripts run when Cargo decides that they must run.
Cargo freshness still skips them while their declared inputs do not change.
The crates that use their output stay reusable.
The action of each consumer binds the generated files that it reads.
It also binds the [native libraries](#native-libraries)
that rustc selects from the search directories of the build script.

## Native libraries

The `rustc-link-search` directory of a build script reaches every crate that depends on it.
It is often a system library directory, such as `/usr/lib/x86_64-linux-gnu`.
An action binds what each native search directory selects.
It never binds the content of the directory:

| Role of a `-L` directory                                          | What the action binds |
| ----------------------------------------------------------------- | --------------------- |
| Rustc looks up Rust crates in `-L all=` and bare `-L` directories | The candidate filenames of each search and the files that rustc selects |
| The linker resolves the libraries of a linked output              | The linker witness: each file that the linker read and each lookup that failed |
| Rustc resolves a native library itself                            | Each candidate that rustc tests, in search order, up to and including the first candidate that exists |

Rustc resolves a native library itself to bundle a static library into an rlib or staticlib.
Rustc also resolves a native library itself when it gives the library to the linker by path.
This applies to an Apple whole-archive static library, a static library whose symbols a `cdylib` exports,
and every MSVC library.
On every miss, the compiler driver reports each such search,
including libraries that `#[link]` attributes and macro output name.
Each candidate before the match must stay absent.
The match must keep its spelling, canonical target, and bytes.
Each of these changes causes a miss: a new library in an earlier directory, a changed archive,
or a retargeted link.
After rustc writes an rlib or staticlib, the driver checks each member of the archive.
If the compilation did not generate a member, and no reported library or upstream rlib contains it,
the search stays unbound.
Then the action compiles without reuse (`compiler_native_input_evidence_unavailable`).

A directory that selects no such library binds only its spelling.
The compiler arguments contain that spelling.
In such a directory, symbolic links, unrelated files,
and subdirectories do not block or invalidate reuse.

`-l NAME`, `-l dylib=NAME`, and `-l static=NAME`, with or without modifiers, are reusable.
If an action does not link, it only names a dynamic library in its metadata.
Thus, a `-sys` crate such as `pq-sys` is reusable.
Framework libraries bypass reuse with `dynamic_native_library_search_evidence_unavailable`.
Other library kinds bypass reuse with `native_library_input_evidence_unavailable`.
Actions that name a native search directory stay local.
Distributed reuse and root-portable reuse do not model these directories.
A `-L all=` or bare `-L` directory can be outside the repository and the target directory.
A system library directory is an example.
Such a directory binds its Rust crate search by absolute host path.
A `-L dependency=` or `-L crate=` directory in such a location still bypasses reuse,
because those actions can reach distributed reuse.

## Native host eligibility

Cache setup accepts these operating-system and architecture pairs:

| Operating system | Native architecture | Rust architecture |
| ---------------- | ------------------- | ----------------- |
| Linux            | x86-64              | `x86_64`          |
| Linux            | Arm64               | `aarch64`         |
| Linux            | RISC-V 64           | `riscv64`         |
| Linux            | IBM Z               | `s390x`           |
| Linux            | IBM POWER           | `powerpc64`       |
| macOS            | x86-64              | `x86_64`          |
| macOS            | Apple silicon       | `aarch64`         |
| Windows          | x86-64              | `x86_64`          |
| Windows          | Arm64               | `aarch64`         |

Rust reports the architecture of a `powerpc64le-unknown-linux-gnu` host as `powerpc64`.
The exact rustc host target stays part of the cache identity.
The distributed-worker identity also binds endianness.
For every other operating-system and architecture pair,
cache setup fails closed and does not change Cargo execution.

This eligibility applies to local L1, remote L2, and the distributed client.
Eligibility does not provide a release archive.
The runner still needs a native Cargo-Rail build with the cache component binaries
that the selected mode requires.
Remote objects also stay bound to the exact compiler action and platform identity.

Cargo-Rail supports Windows setup and compiler-fact acquisition.
On Windows, native result reuse currently bypasses,
because the native execution path is implemented only for Linux and macOS.
A Windows archive or a passing Windows test suite does not prove positive native-cache reuse.
On Linux, shared compiler runtime libraries outside the sysroot stay an unqualified boundary.
This boundary includes loader selection and earlier search candidates.
A successful driver readiness probe alone does not prove complete capture of runtime inputs.

Explicit targets can reuse metadata and Rust library outputs
when Cargo-Rail captures the selected target definition, the compiler distribution,
and the sysroot and target-library bytes.
Host-built dependencies keep their own compiler identity.
Linked reuse requires evidence for the selected linker and every owned output.
Verified COFF results include PDBs and import libraries.
If backend, linker, or auxiliary-output evidence is missing,
Cargo-Rail runs the original compiler and records the unavailable boundary.
These cases are not qualified positive-reuse cases: Zig/cargo-zigbuild, MSVC `link.exe`,
the rustc GCC backend, packed Darwin debug output, and post-link stripping.
When complete cache evidence is not available, these paths run the compiler without reuse.

Unpacked debug object files use the same verified file restore transaction.
Packed Darwin output and post-link stripping require additional tool inputs
that are not fully observed.
A distributed worker must match the client's architecture, endianness, operating system,
rustc host target, compiler, and sysroot.
Target identity is checked separately.
Cargo invocations with additional dependency-search directories stay local
until Cargo-Rail can transport and verify their complete search inputs.

## Avoid repeated input hashing

A valid sysroot identity memo reuses the fingerprint only after it checks the complete inventory
and the current filesystem generation evidence.
On a miss, an optional lock for each memo lets a worker reuse a memo that another worker published.
After the worker gets the lock, it captures the inventory again and repeats the validation.
If locking is not available, or if the wait lasts five seconds, the worker falls back to hashing.
The lock does not authorize reuse.
It does not prevent cache deletion on Windows.

Within one ELF/GCC linker capture,
aliases of the same canonical file can share a digest while their generation evidence stays valid.
Each path spelling still enters the witness and uses part of the path and entry budgets.
If a file is replaced or retargeted, or if its generation evidence is not available,
Cargo-Rail must hash it again.
Publication revalidation stays independent.
These mechanisms reduce repeated reads.
They do not make a cold build equivalent to an L1 hit.

## Share results remotely

Remote selection is machine state that is bound to a workspace.
It is never repository configuration.
During setup, save the L2 selection in the current workspace profile.
Then use ordinary Cargo.
Compilation uses only the remote that setup saved.
Cargo gives repository `[env]` values and build-script output to the compiler wrapper.
For this reason, the wrapper and `cache status` ignore `CARGO_RAIL_CACHE_REMOTE`, `CARGO_RAIL_CACHE_MODE`, and `CARGO_RAIL_CACHE_REMOTE_ENVIRONMENT`.
Cargo does not set the environment of a Cargo-Rail command that you start directly.
For this reason, `cache probe` and the compiler analysis of `unify` and `surface` still read these variables.

Select a remote during setup:

```bash
cargo rail cache setup --check --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo rail cache setup --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
```

Cargo-Rail accepts these URL families:

```text
s3://BUCKET/PREFIX?region=REGION&owner=AWS_ACCOUNT_ID
r2://ACCOUNT_ID/BUCKET/PREFIX
azure://ACCOUNT/CONTAINER/PREFIX
```

Native release archives include both providers.
A source build includes them only when you enable Cargo features.
The `s3` feature covers S3 and R2, and the `azure` feature covers Azure Blob.
For example, use `cargo install cargo-rail --locked --features s3,azure`.
A build without a provider rejects the URL of that provider during setup
and names the missing feature.

### Cloudflare R2

Use a private, [default-jurisdiction bucket](https://developers.cloudflare.com/r2/reference/data-location/) with public access disabled.
Create an [R2 API token](https://developers.cloudflare.com/r2/api/tokens/) scoped to Object Read & Write for that bucket.
The S3 access key ID and secret access key of the token must be available to setup.
They must also be available to every compiler process that you want to use L2.
A Wrangler login or a general Cloudflare API token is not an S3 credential pair.

```bash
export AWS_ACCESS_KEY_ID='<R2 access key ID>'
export AWS_SECRET_ACCESS_KEY='<R2 secret access key>'

cargo rail cache normalize \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache'
cargo rail cache setup --check --remote \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache' \
  --remote-mode read-write --root-portability remap
cargo rail cache setup --remote \
  'r2://0123456789abcdef0123456789abcdef/cargo-rail-cache' \
  --remote-mode read-write --root-portability remap
cargo rail cache probe --json
```

Use different bucket-scoped credential pairs for CI and for developer machines,
even when they share one R2 authority.
Keep the protocol marker at `native-v6/protocol`.
An [object lifecycle rule](https://developers.cloudflare.com/r2/buckets/object-lifecycles/) can expire `native-v6/entries/`, but the rule must not delete the marker.
If the URL includes a prefix, scope the prefix relative to the selected URL root.

Cargo-Rail currently models only the default jurisdiction of R2.
It derives the standard R2 account endpoint.
It intentionally has no syntax for a jurisdiction or an arbitrary endpoint.
Use a default-jurisdiction bucket until a real consumer requires a typed jurisdiction contract.

`cache probe` uses the saved authority and the standard AWS credential environment.
This environment includes a session token when one is present.
`cache probe` proves authenticated marker compatibility.
It can initialize an absent marker only in `read-write` mode.
Its JSON output contains the provider, mode, readiness, and marker state.
The output does not contain a URL, object key, or credential value.

Before you select storage and operation budgets, review current [R2 pricing](https://developers.cloudflare.com/r2/pricing/) and retention policy.

Use `cargo rail cache normalize URL` to validate a URL.
This command does not resolve credentials or contact storage.
Credentials stay outside URLs, repository configuration, result packs, diagnostics,
compiler arguments, and cache keys.
When possible, use a machine or container role, OIDC, or a preconfigured profile.

`--remote-mode read` requires an existing compatible protocol marker.
It never writes.
`read-write` adds conditional publication of the protocol marker and entries.
For an authority with the root `PREFIX`,
the provider permissions are limited to these objects and operations:

| Mode         | Objects                                                   | Operations |
| ------------ | --------------------------------------------------------- | ---------- |
| `read`       | `PREFIX/native-v6/protocol`, `PREFIX/native-v6/entries/*` | Object read |
| `read-write` | The same objects                                          | Object read and conditional write |

Build credentials do not need list, delete, lifecycle, multipart-upload,
or administrative authority.
Keep provider cleanup and lifecycle policy outside build credentials.

L1 stays authoritative.
Thus, an L1 hit makes no remote request.
The compiler runs if any of these conditions occur: absence, conflict, corruption,
credential failure, throttling, or outage.
`--local-only` removes the saved L2 selection and keeps L1.
You do not need it when you create a new local profile, because local reuse is already the default.
You cannot use it with `--root-portability`.
When Cargo-Rail removes remote authority, it restores physical-root local reuse.

Physical-root mode is the default.
It shares results only between checkouts at the same canonical path.
Cross-root reuse requires `--root-portability remap`.
That mode admits only certified workspace Rust metadata and library results.
When rustc reads regular repository files outside the package source root,
those reads become a bounded dynamic selector.
Before lookup, Cargo-Rail revalidates each selected path, file kind, byte length, content digest,
and executable mode.
These cases bypass cross-root reuse:

- symlink or reparse crossings;
- inputs outside repository authority;
- generated namespaces;
- native-search inputs;
- ambiguous roots;
- user-selected remaps;
- unsupported output classes.

Cargo-Rail supports external `CARGO_TARGET_DIR` locations for eligible native results.
Cargo compiles registry and git packages inside their unpacked source,
which belongs to no workspace.
With an explicit target or build directory, those units use the workspace that Cargo started in.
That workspace is the `PWD` value that the shell exports.
If `PWD` is not set, those units compile without reuse.
The cache identity uses one stable logical output directory.
Local compilation and restore continue to use the exact physical output parent of Cargo.
A change to the checkout root or target root keeps the portable identity
when the selected input paths and environment values do not change.
A change to a selected input causes a miss.
This includes an edit that keeps the same size.

You must review each additional L2 environment name.
Each name must not be secret.
Select these names with repeated `--remote-environment` options during setup.
The digests of their values enter the action identity.
Rustc can also write those values into dep-info.
Cargo-Rail stores and restores those environment records exactly.

## Distribute eligible misses

Distributed execution runs below Cargo L0, L1, and L2.
It accepts only bounded compiler-only Rust operations with complete source, dependency,
and selected repository inputs.
These items stay local: linked outputs, build scripts, generated namespaces, native dependencies,
unmodeled options, and newly observed compiler environments.

Worker protocol version 5 requires each successful response to include the actual native-input
observation that the compiler selected.
Before the client admits the result,
it validates the observation against the transported files and the matched toolchain.
The client rejects remote admission and falls back to local execution
if any of these conditions occur: an input selected outside the transported set,
incomplete assembly coverage, or missing driver evidence.
The operation keeps the ordered dependency-search directories
and the identity of the selected source directory.
Physical-root operations read staged sources through the matched driver
and keep the ordinary compiler metadata paths.
Explicit remapping keeps the virtual root.
Cargo-Rail transports matching candidates and transitive libraries
as exact files at their declared search paths.
The existing input bounds of the worker apply.
An input that is not transported keeps the invocation local before the worker runs.

The client requires one complete mTLS worker authority:

```bash
cargo rail cache setup --check \
  --distributed-endpoint '10.0.0.20:39443' \
  --distributed-server-name worker.example.internal \
  --distributed-capability 'worker-capability-v5:sha256:CAPABILITY_DIGEST' \
  --distributed-authority /etc/cargo-rail/server-ca.pem \
  --distributed-client-certificate /etc/cargo-rail/client.pem \
  --distributed-client-private-key /etc/cargo-rail/client.key
```

Review the authority before you run setup without `--check`.
The default `automatic` policy stays local
until fresh class-specific measurements predict a gain on the critical path.
The `qualification` policy sends every eligible miss to collect evidence.
This policy can be slower.

Deploy the direct worker only on a dedicated single-tenant host or ephemeral VM.
Also do these steps:

- Pin the compiler and the worker capability.
- Use mutual TLS.
- Run each attempt inside a resource-bounded sandbox.
- Make sure that the worker inherits no operator or provider credentials.

If a transport, worker, lease, sandbox, or pre-commit validation failure occurs,
the normalized operation runs locally one time.
A successful response still goes through the native-cache validation and restore transaction
before Cargo sees output.

## Compiler-evidence cache

Cargo-Rail stores Unify diagnostics and the typed compiler facts of Surface per view.
It stores them in the local store of the selected cache profile.
They are separate records, but one reuse rule applies to both.
Before Cargo-Rail reuses a view, it revalidates these items:

- the compiler, sources, manifests, targets, and features;
- the Cargo configuration and lockfile;
- the executable identity;
- every file and environment variable that rustc reported reading for the units of the view,
  including files outside the package.

These operations apply this rule: Unify preview, `--check`, `--explain`, and `apply`, and Surface runs and resumes.
This store contains compiler evidence, not restorable Cargo artifacts.

A view also stays reusable only while Cargo would keep the output of every build script in it:

- Each path declared with `rerun-if-changed` keeps its content.
  A declared directory keeps every entry.
- Each variable declared with `rerun-if-env-changed` keeps its value.
  Only a digest of the value is stored.
- A script that declares no path depends on its package sources.
  The lockfile checksum or the workspace source fingerprint already binds these sources.

The files that a build script generates and the variables
that it sets with `rustc-env` are bound through those inputs.
Proc-macro reads are bound in the same way that Cargo binds them:
through the dep-info and tracked environment of the consuming unit.
A change to one declared input reruns only the views whose Cargo graph ran that build script.
Cargo-Rail can store several variants of a view.
Thus, a return to an earlier input reuses the evidence for that input.

Unify does not store a view when a build script declares a secret-named variable,
declares a missing path, or declares a directory with more than 10,000 entries.
`evidence_cache[].publication_bypasses` names the reason.
Cargo-Rail stores a view as soon as the view completes.
Thus, if a later failure or interruption occurs, the stored view is available for the retry.
Cargo-Rail never reuses a corrupt stored view.
A corrupt view does not hide other stored views of the same key.
Runs under an unverified `RUSTC_WRAPPER` never reuse evidence, because that wrapper can read anything.
Runs under the installed Cargo-Rail wrapper reuse evidence.

To see hits, misses, miss reasons, and publication bypasses for Unify, inspect `evidence_cache`.
To see fact-cache hits, misses, and bypass reasons for Surface, inspect `metrics.acquisition`.

## Local storage budget

Each enrolled profile has its own local store and budget.
`cache setup --max-size SIZE` sets the budget.
Without that option, setup keeps the current budget of an existing profile,
and a new profile starts at 10 GiB.
Cargo-Rail stores identical output files one time.
Every result that produced such a file shares it.
If a new result would exceed the budget,
Cargo-Rail evicts the least-recently-used results until usage is at most 90% of the budget.
Thus, a full store collects one time for each batch of new results, not on every compilation.
A hit updates the last-use time of an entry at most one time each hour.
`cache setup` applies a lowered budget immediately.
Cargo-Rail does not evict results that an active reader holds.

On Linux and macOS, the store also keeps the SHA-256 of each input file.
It uses that digest only while the device, inode, size, modification time,
and change time of the file do not change.
Thus, dependents do not hash the same dependency artifacts again.
The store keeps one record for each captured source tree and each dependency directory.
Thus, an invocation reads one record instead of one entry for each file.
A record applies to a file only while the generation of that file matches.
Cargo-Rail always hashes a file again if the file was modified in the last two seconds.
Entries expire after seven days.
On Windows, Cargo-Rail always hashes,
because the Windows file generation does not include a change time.

If collection evicts a result within one day of the last use of that result,
the budget did not hold the working set that was in use.
The next build of that work misses again.
`cache status` then reports `recent_evictions`: the results, the bytes, and the time of the last such eviction.
It also prints a budget-pressure line.
To increase the budget, run `cargo rail cache setup --max-size SIZE`.
The count does not decrease until the local cache is cleaned.
For example, one validation lane of this repository stores about 3 GiB.
That lane is `just check` across seven targets.

`cache profiles` reports `bytes`, `max_bytes`, and `over_capacity_bytes` for each profile.
The installation storage in `cache status` is the total for all profiles.
Compare it with the sum of all profile budgets, not with one `max_bytes` value.

## Inspect, clean, detach, or uninstall

```bash
cargo rail cache status --scope local --json
cargo rail cache profiles --json
cargo rail cache clean --scope workspace --check
cargo rail cache clean --scope local --check
cargo rail cache detach --check
cargo rail cache drop-profile --profile PROFILE_ID --check
cargo rail cache uninstall --check
```

Workspace cleanup removes reconstructible state for the current checkout.
Local cleanup validates ownership and waits for readers.
Then it removes only the CAS of the current profile.
After local cleanup, run `cache setup` again.
Local cleanup also removes the store of that profile in the retired `local-cas-v2` layout.
Current versions never read that layout.
`cache status` counts retired stores as reclaimable.
`cache clean --scope local --check` includes them in its preview.
`cache detach` removes the current root binding but keeps the profile and its CAS.
`cache drop-profile` accepts an opaque ID from `cache profiles`.
It removes a profile only if the profile is detached and has no enrolled roots.
`cache uninstall` removes the global wrapper, the worker, the receipt-owned compiler components, the Cargo field,
and the installation receipt.
It keeps the profiles and their CAS data.

Setup repairs the current installed component bytes from their authenticated source files.
`cache status` reads stale receipt versions but does not activate them.
Before setup installs the current component set and updates the wrapper entry of Cargo,
it quarantines the stale receipt and the installation state that the receipt owns.
Removal and reuse refuse authority that is changed, shadowed, linked, or unowned.
Do not edit profile records, individual CAS objects, or Cargo fingerprints by hand.

Status schema 18 reports each of these items as a separate field:

- installation integrity;
- component authentication;
- selected-toolchain readiness;
- workspace enrollment;
- remote authority;
- observed reuse;
- the selected profile and trust domain;
- the redacted remote selection source.

It also separates required installation bytes from quarantined reclaimable bytes.
It reports stable native failure-reason counters separately from the usage ledger,
which has a bound of 65,536 events.
Thus, capture, identity, and post-execution witness failures stay visible after the ledger is full.
If Cargo-Rail cannot validate the counter file, `failure_reason_counts_available` is `false`.
Status does not report invented zeroes.
Verbose status also reports the fixed 64-shard native restore-lock namespace
and any staging residue.

## Benchmark evidence

Use [the benchmarking contract](benchmarking.md) for smoke, qualification, correctness, evidence retention,
and claim requirements.

### Aggregate cache measurements

`cargo rail cache report --start /absolute/recording.json -f json` creates a new private recording.
Set the machine-owned `CARGO_RAIL_CACHE_REPORT` environment variable to that path for the commands in the reporting interval.
After the compiler processes of those commands exit, run `cargo rail cache report --finish /absolute/recording.json -f json`.
This command closes the interval and returns a report that follows the [`cache-report-v1` contract](../schemas/cache-report-v1.schema.json).
If you finish the interval again, the command returns the recorded totals.
If you start a recording over an existing file, the command rejects it.

The recording contains bounded aggregate counters and reason counts.
In `details`, the finished interval also names one example for each of the first eight bypass reasons.
An example can be the source entry, native-library candidate, macro import,
or session or admission error that caused the reason.
Examples can name host paths.
Examples never enter the measurements, a cache key, or restore authority.
Text output lists each bypass reason with its example.
Concurrent wrapper outcomes are serialized.
Report storage never authorizes a restore or changes a cache key.
Known recording errors and counter overflow mark the measurements incomplete.
A corrupt or unreadable recording makes collection fail.
The measurements are observed wrapper outcomes.
They are not Cargo freshness counts or an estimate of time saved.
Cache problems can cause fallback within a miss or bypass.
Wrapper failures are a separate outcome.
Do not add them to reason counts as disjoint categories.

The companion cache Action opens the interval during setup.
To publish a workflow report, do these steps:

1. Add the `cache/collect` action of the companion cache Action after compilation.
1. Transfer each collected record to a job that runs `cache/report`.

Setup alone does not publish a workflow report.
The report requires the expected job labels.
It identifies missing jobs and incomplete measurements.
Local storage stays separate for each job.
Shared storage is not summed across runners.
See the [Action reporting example](https://github.com/loadingalias/cargo-rail-action#one-cache-report-for-the-workflow).
