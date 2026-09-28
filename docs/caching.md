# Caching

Cargo-Rail preserves Cargo as the executor.
Each layer may remove only the work it can prove reusable.

| Layer | Authority | Result |
| --- | --- | --- |
| Cargo L0 | Cargo fingerprints and incremental state | Cargo skips fresh work |
| Local L1 | Exact compiler action, result, and stored bytes | One compiler result is restored |
| Remote L2 | The same verified result pack under machine-owned provider authority | A result enters L1, then restores |
| Distributed miss | A pinned worker capability and validated response | One eligible miss executes remotely |
| Compiler evidence | Identity-matched, revalidated observations and typed facts | Unify or Surface skips the corresponding acquisition |

A lookup is not authority.
Missing, stale, ambiguous, unsupported,
or corrupt cache evidence falls back to compiler execution with a stable miss or bypass reason.
A compiler or required acquisition failure still fails the operation.

## Set up local reuse

Setup enrolls the current workspace for ordinary Cargo, nextest, Just, IDE, and CI commands.
Workspaces that share one effective Cargo home share the installed wrapper,
but each enrollment owns a separate cache profile:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache ready
cargo rail cache status --scope local
cargo rail doctor native-cache
```

`cache setup --check` previews changes; `cache setup` applies them.
Setup owns one global `build.rustc-wrapper`, private launcher and worker bytes, and an installation receipt.
It copies the compiler fact driver
and driver-source components declared by the build's embedded authority beside the worker.
Missing or changed declared files beside the real Cargo-Rail executable,
after resolving launcher symlinks, reject setup before writes.
Setup does not build or download these components.

Setup checks the three kinds of components differently:

| Component | Before installation | After installation |
| --- | --- | --- |
| Compiler fact driver and its source | Digest against the authority embedded in `cargo-rail` | Digest against the setup receipt |
| Wrapper and worker | Same Cargo-Rail release as `cargo-rail`, by version query | Digest against the setup receipt |
| `cargo-rail` itself | Trusted as the invoked executable | Not installed |

The embedded authority protects against a driver built for a different compiler
or from different source.
It is not a boundary against someone who can write beside `cargo-rail`, because they can also replace `cargo-rail`.
Verify the archive before extraction; `cache status` then reports any installed component that changes.
Native reuse requires an authenticated driver for the selected compiler.
An installation without embedded component authority, including an ordinary `cargo install`,
can use an independently authenticated adapter pack.
Without either authority, compiler work executes normally and bypasses reuse.

For development in this checkout, `just build` and `just test` prepare the authenticated driver and source bundle,
then build Cargo-Rail with that component authority.
Preparation requires `rustc-dev` for the selected toolchain
and the driver's locked dependencies in the local Cargo cache;
it does not download missing components or crates.
`just check-compiler-driver` runs the separate driver checks.
Plain Cargo builds do not perform this preparation.

`cache ready` runs one isolated uncached build, one cold cached build,
and one verified warm restore with the selected toolchain.
When that rustup toolchain lacks `rustc-dev`, `cache ready` installs it first;
compiler wrappers never install components during a build.
It requires an enrolled, authenticated profile and forces those three builds to use only L1.
An L2 selection remains installed but receives no requests from this probe.
A successful probe records readiness for that exact profile and rustc identity;
a profile or toolchain change makes the recorded readiness stale.
Qualify remote transport separately with `cache probe` and an explicit machine-owned authority.

### Stable, beta, and nightly compilers

Every stable, beta, or nightly compiler at or above the adapter's minimum reuses results.
The driver is built for the exact selected compiler and chooses that compiler's internal API
when it is built; CI builds and tests it against the current stable, beta, and nightly.
A compiler below the minimum is refused with the required release and commit date.

Nightly Cargo builds libraries without embedded metadata (`-Zembed-metadata=no`): each rlib is a stub,
its metadata is a separate rmeta,
and a crate that links a library receives both files under one extern name.
A result binds both outputs, and a result without its rmeta is refused.
Each file of a paired extern, and each rlib and rmeta found through `-L dependency=`,
enters the action identity separately, so a change to either one misses.
Nightly Cargo's per-unit build directories (`build/PACKAGE/HASH/out`) restore like the classic layout.

Limits:

- Distributed execution does not accept nightly Cargo's metadata split;
  those actions compile locally.
- With nightly Cargo's per-unit build directories and a target directory outside the repository,
  a crate that has dependencies compiles without reuse (`compiler_native_input_evidence_unavailable`):
  each dependency's directory is outside every root the input witness can name.
- A nightly newer than the last one CI built can change the compiler's internal API.
  When the driver no longer builds for it, `cache ready` fails, and compiler work runs without reuse.

## Select an independent compiler adapter

An adapter pack contains the closed,
vendored fact-driver source inventory and both compiler protocols.
Cargo-Rail authenticates the selected pack by an operator-supplied SHA-256 digest,
checks protocol compatibility, builds it with the exact selected compiler,
and runs a metadata-compilation calibration before use.
The pack sets a minimum compiler release and commit date.
It does not set an upper bound.
Build or calibration failure makes native cache work bypass to ordinary rustc execution;
a Surface command still fails when it requires compiler facts.

Publish a pack without publishing Cargo-Rail core:

```bash
just package-compiler-adapter ./adapter-assets
```

The command writes one content-addressed JSON pack and its adjacent `.sha256` file.
It refuses existing outputs and does not upload them.

After verifying the downloaded checksum, select the extracted JSON file with an absolute path:

```bash
export CARGO_RAIL_COMPILER_ADAPTER_PACK=/absolute/path/cargo-rail-compiler-adapter-DIGEST.json
export CARGO_RAIL_COMPILER_ADAPTER_PACK_SHA256=sha256:FULL_DIGEST
cargo rail surface --prepare
```

Set both variables together.
The digest is the explicit machine trust decision.
Cargo-Rail caches the calibrated executable by the pack digest, complete `rustc -vV` identity,
compiler-library digest, build target, and both protocol versions.

`just package-release OUTPUT_DIRECTORY` builds the native release components and writes a deflated ZIP, `cargo-rail-components-v1.tsv` inside that archive,
and an adjacent `SHA256SUMS`.
Use a separate output directory for each host.
The command verifies component digests and refuses existing release outputs;
it does not publish them.
The [packaging workflow](../.github/workflows/package.yml) produces these archives;
the [release transaction](releases.md) verifies the exact producer attempt and bytes before publication.
Extract the archive as a unit so the component files remain together.
The companion cache Action requires core, wrapper, worker,
matched driver and authenticated driver source components.
Build-host eligibility does not imply that the release workflow publishes an archive
for every eligible platform; see [installation](../README.md#installation).

Setup also binds the exact physical workspace root to one private profile with its own bounded CAS
trust domain.
Running setup in another workspace creates another profile; it does not replace the first profile.
An unenrolled workspace executes normally without L1 or L2 reuse.
Setup refuses another global wrapper, persistent shadowing, ambiguous Cargo homes,
linked authority paths, or changed owned state.

Cargo freshness and incremental compilation remain L0.
An L1 action binds the compiler, toolchain, arguments, target, environment, dependencies,
source topology and bytes, native-search inputs, and declared outputs.
Physical mode also binds the canonical workspace root.
Remap mode replaces that root only for certified portable result classes
while keeping the executor's physical output directory out of the portable operation identity.
Stored descriptors and every output byte are reverified before restore.
The bounded source capture deliberately over-invalidates when it cannot prove
that an unused path was irrelevant.
An unpacked registry package is captured whole,
so a crate that reads a package file outside its source directory, such as `#![doc = include_str!("../README.md")]`, is reusable,
and a change to any file in the package invalidates its results.
Other packages outside the workspace capture only the crate's source directory,
and a read outside it runs the compiler normally.

Eligible cold compilation uses the shared,
compiler-matched fact driver to record the Rust libraries rustc selects,
including transitive libraries, and the candidate filenames it searches.
This native-input witness is separate from diagnostic facts.
Reuse checks the selected files, matching alternatives,
and missing candidates before restoring outputs.
Metadata-only work records that no code generation occurred.
Code generation with inline or global assembly, or LTO modes that may import dependency assembly,
bypasses reuse because assembler inputs are incomplete.

Native reuse requires the selected `RUSTC` executable to resolve to the captured sysroot's compiler.
A custom compiler program or an opaque workspace wrapper executes normally,
preserving its flags and behavior.
A matching version response alone does not authorize replacing that program with the shared driver.

Compiler-selected environment values remain exact inputs, even when they contain workspace paths.
A different literal value produces a different action.
Dep-info path rebinding preserves rustc's trailing `# env-dep:` records byte-for-byte;
dependency rules interleaved after those records are unsupported.

A result is:

- a `hit` only after current inputs and stored bytes verify;
- a `miss` when no authoritative result exists and successful cold output may populate the CAS;
- a `bypass` when the invocation is outside the supported class and executes normally; or
- a `conflict` when one action produced distinct semantic results, in which case neither result restores.

Disable reuse for one process tree without changing setup:

```bash
CARGO_RAIL_CACHE=off cargo check --locked
```

## Reuse Clippy results

`cargo clippy` runs `clippy-driver` as Cargo's workspace wrapper for each workspace member.
Cargo-Rail reuses those results under the same exact rule as rustc results.
A Clippy action binds its rustc-equivalent action and every input that Clippy adds:

- the bytes of `clippy-driver`, which must be the matched sysroot's driver;
- whether Clippy runs lints, and the arguments it appends, including `CLIPPY_ARGS` and `--cfg clippy`;
- `CLIPPY_ARGS`, `CLIPPY_CONF_DIR`, `CLIPPY_DISABLE_DOCS_LINKS`, `CARGO_PKG_RUST_VERSION`, and `CARGO_MANIFEST_DIR`;
- each `.clippy.toml` and `clippy.toml` candidate that Clippy checks,
  from `CLIPPY_CONF_DIR`, else the package directory, up to the first directory that holds one,
  including every absent candidate;
- the inputs of `cargo metadata`, which `clippy::cargo` lints run during linting:
  the `cargo` executable, non-secret `CARGO_*` variables, `RUSTC`, `RUSTUP_TOOLCHAIN`,
  each `.cargo/config.toml` and `.cargo/config` that Cargo reads, including those in the Cargo home,
  the workspace manifests with every path dependency they reach, and `Cargo.lock`.

A source attribute can enable a `clippy::cargo` lint, so every Clippy action binds the Cargo inputs.
A change to a workspace manifest, the lockfile,
or Cargo configuration therefore misses every Clippy result in that workspace.
`CLIPPY_TERMINAL_WIDTH` is not bound: Clippy uses it only to lay out configuration errors,
and a failed result is never stored.

On a cold miss, the authenticated compiler driver runs a resolution phase beside Clippy with
Clippy's exact arguments.
It records the Rust libraries and search candidates rustc selects, stops when crate loading freezes,
and writes nothing.
That record is the same Rust-input witness that a rustc result keeps.
The phase ends before analysis,
so it adds parsing and macro expansion to a miss but no wall time on the critical path
when Clippy runs longer.

These Clippy invocations execute normally:

| Reason                                    | Condition |
| ----------------------------------------- | --------- |
| `clippy_linked_output_unavailable`        | Clippy links output, such as a workspace member's build script |
| `clippy_sysroot_override_unavailable`     | `SYSROOT` is set, so Clippy selects another sysroot |
| `clippy_driver_identity_unavailable`      | `clippy-driver` is not the matched sysroot's driver |
| `clippy_rustc_program_unavailable`        | The rustc argument's file stem is not `rustc`, so Clippy does not run in wrapper mode |
| `clippy_configuration_outside_repository` | Clippy loads a configuration file outside the repository |
| `clippy_configuration_unavailable`        | Clippy's configuration lookup cannot be observed |
| `clippy_cargo_inputs_unavailable`         | Cargo's inputs cannot be enumerated, for example a manifest that sets `package.workspace` |

The rustc bypass classes also apply.
Dep-info records `CLIPPY_ARGS` and `CLIPPY_CONF_DIR`, so an L2 selection shares Clippy results only
after setup approves both names with `--remote-environment`, as for every compiler environment read.

Under `--root-portability remap`, Clippy results are shared across checkout roots.
A bound path below the repository, the Cargo home,
or the compiler sysroot is spelled relative to that root,
and so is a bound variable whose value names such a path.
An absent candidate outside those roots is not bound: Cargo and Clippy read nothing there,
and every lookup captures the inputs again.
A present input outside them keeps its host path, so its result is reused only at its own root.
Cache setup writes the installed wrapper's absolute path into the Cargo home configuration.
A configuration file is bound without that entry when the entry selects the installed wrapper,
which passes Cargo's compiler queries through unchanged;
a file that selects any other wrapper is bound whole.

## Procedural macros

A procedural macro is a bound input: its exact library bytes enter the action,
whether the crate depends on the macro directly or reaches it through a re-export.
Rustc records what the expanded code includes and the environment it reads through `env!`, `option_env!`,
and tracked environment; the action binds those records.

A macro can also read files, list directories, and read variables that rustc never records.
On every miss, including Clippy's resolution phase,
the compiler driver observes what each loaded macro does through the C library and, on Linux,
through the kernel.
The action then binds each observed read:

- A variable read joins the compiler's own environment reads,
  so it binds and shares exactly as an `env!` read does.
- A repository path binds its state when the action was captured:
  the file's content and mode, the path's absence, or a directory's entry names and kinds.
- A read inside what the action already captures whole,
  such as the crate's own sources or its build-script output, is already bound.
- `cargo locate-project`, which `proc-macro-crate` runs to find the workspace,
  binds the Cargo inputs that answer it.

A hit is therefore stricter than Cargo's own freshness,
which misses a macro read that no build script declares.

These units execute normally:

| Reason                                     | Condition |
| ------------------------------------------ | --------- |
| `procedural_macro_read_outside_repository` | The macro reads a path outside the repository and outside the captured namespaces |
| `procedural_macro_read_through_symlink`    | The macro reads a repository path through a symbolic link |
| `procedural_macro_process_unmodeled`       | The macro starts a process other than a modeled `cargo locate-project` query |
| `procedural_macro_process_control`         | The macro forks, replaces its process, or starts a process with a changed environment, directory, or descriptors |
| `procedural_macro_secret_environment`      | The macro reads a variable with a secret-like name |
| `procedural_macro_environment_enumeration` | The macro walks the whole environment |
| `procedural_macro_environment_write`       | The macro changes the environment |
| `procedural_macro_file_write`              | The macro creates, writes, renames, or removes a file |
| `procedural_macro_network`                 | The macro opens a network connection |
| `procedural_macro_host_state`              | The macro reads host state, such as the host name, kernel identity, or file-system statistics |
| `procedural_macro_working_directory`       | The macro changes the working directory |
| `procedural_macro_dynamic_load`            | The macro loads a library at run time |
| `procedural_macro_executable_memory`       | The macro maps or generates executable memory |
| `procedural_macro_dynamic_dependency`      | The macro links a shared library other than the system C library |
| `procedural_macro_import_unclassified`     | The macro imports a C library function that the driver does not classify |
| `procedural_macro_initializer`             | The macro ran initialization code before the driver could observe it |
| `procedural_macro_raw_system_call`         | The macro issues a system call without the C library, and the platform cannot observe it |
| `procedural_macro_path_unavailable`        | A path the macro used cannot be resolved to a stable name |
| `procedural_macro_observation_limit`       | The macro reads more paths or variables than an action binds |
| `procedural_macro_observation_unavailable` | The platform cannot observe macros, as described below |

Observation depends on the host:

- Linux: the driver rebinds each macro image's C library imports after rustc loads it,
  and a per-thread seccomp user-notification filter reports system calls
  that the macro's own code issues.
  Kernels older than 5.5, and containers whose seccomp profile blocks that filter, report `procedural_macro_observation_unavailable`.
  An image whose initializers do more than the C runtime's
  and the Rust standard library's setup reports `procedural_macro_initializer`,
  because those initializers ran before instrumentation.
- macOS: the driver instruments each macro image
  before its initializers run. macOS has no unprivileged system-call observer,
  so an image that contains a system-call instruction bypasses.
  On Apple silicon the check is exact.
  On x86-64 it checks every byte offset, and ordinary code often contains those bytes,
  so most macro images bypass there.
- Windows and other hosts: every unit that loads a procedural macro reports `procedural_macro_observation_unavailable`.
  Windows native result reuse is not yet implemented, so this changes no Windows result.

Distributed workers never run procedural macros; those units stay local.

## Build-script execution

Cargo-Rail does not reuse the output of a build script's execution.
Compiling the build script is an ordinary rustc action and can hit.
Running it can compile native code, as `aws-lc-sys` does with cmake and a C compiler.
That output depends on everything that cmake, the C compiler, and the platform SDK read,
and `rerun-if-changed` and `rerun-if-env-changed` declare only part of it.
Exact reuse would need file tracing of the whole build-script process tree on every supported host.
Build scripts therefore run normally when Cargo decides they must run,
and Cargo's own freshness still skips them while their declared inputs are unchanged.
The crates that consume their output remain reusable,
because each consumer's action binds the generated files and native search directories it reads.

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
The exact rustc host target remains part of cache identity,
and distributed-worker identity also binds endianness.
Every other operating-system and architecture pair fails cache setup closed
and leaves Cargo execution unchanged.

This eligibility applies to local L1, remote L2, and the distributed client.
It does not provide a release archive:
the runner still needs a native Cargo-Rail build with the cache component binaries required by the
selected mode.
Remote objects also remain bound to the exact compiler action and platform identity.

Windows setup and compiler-fact acquisition are supported,
but native result reuse currently bypasses:
the native execution path is implemented only for Linux and macOS.
A Windows archive or passing Windows test suite does not establish positive native-cache reuse.
On Linux, shared compiler runtime libraries outside the sysroot remain an unqualified boundary,
including loader selection and earlier search candidates.
A successful driver readiness probe alone does not establish complete runtime input capture.

Explicit targets can reuse metadata and Rust library outputs
when Cargo-Rail captures the selected target definition, compiler distribution,
sysroot and target-library bytes.
Host-built dependencies retain their own compiler identity.
Linked reuse requires evidence for the selected linker and every owned output;
verified COFF results include PDBs and import libraries.
Missing backend, linker or auxiliary-output evidence runs the original compiler
and records the unavailable boundary.
Zig/cargo-zigbuild, MSVC `link.exe`, the rustc GCC backend,
packed Darwin debug output and post-link stripping are not qualified positive-reuse cases.
These paths retain normal compiler execution when complete cache evidence is unavailable.

Unpacked debug object files use the same verified file restore transaction.
Packed Darwin output and post-link stripping require additional tool inputs
that are not fully observed.
A distributed worker must match the client's architecture, endianness, operating system,
rustc host target, compiler, and sysroot.
Target identity is checked separately.
Cargo invocations with additional dependency-search directories remain local
until their complete search inputs can be transported and verified.

## Avoid repeated input hashing

A valid sysroot identity memo reuses the fingerprint only after checking the complete inventory
and current filesystem generation evidence.
On a miss, an optional per-memo lock lets a worker reuse a memo published by another worker.
The worker recaptures the inventory and revalidates after acquiring the lock;
unavailable locking or a five-second wait falls back to hashing.
The lock does not authorize reuse and does not prevent Windows cache deletion.

Within one ELF/GCC linker capture,
aliases of the same canonical file can share a digest while their generation evidence remains valid.
Each path spelling still enters the witness and consumes the path and entry budgets.
Replacement, retargeting, or unavailable generation evidence requires another hash.
Publication revalidation remains independent.
These mechanisms reduce repeated reads; they do not make a cold build equivalent to an L1 hit.

## Share results remotely

Remote selection is workspace-bound machine state, never repository configuration.
Persist L2 in the current workspace profile during setup, then use ordinary Cargo:

```bash
cargo rail cache setup --check --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
cargo rail cache setup --remote \
  's3://company-cargo-rail-cache/rust/team?region=us-east-1&owner=123456789012' \
  --remote-mode read-write
```

Accepted URL families are:

```text
s3://BUCKET/PREFIX?region=REGION&owner=AWS_ACCOUNT_ID
r2://ACCOUNT_ID/BUCKET/PREFIX
azure://ACCOUNT/CONTAINER/PREFIX
```

Native release archives include both providers.
A source build includes them only with Cargo features: `s3` covers S3 and R2, and `azure` covers Azure Blob,
as in `cargo install cargo-rail --locked --features s3,azure`.
A build without the provider rejects its URL during setup and names the missing feature.

### Cloudflare R2

Use a private, [default-jurisdiction bucket](https://developers.cloudflare.com/r2/reference/data-location/) with public access disabled.
Create an [R2 API token](https://developers.cloudflare.com/r2/api/tokens/) scoped to Object Read & Write for that bucket.
Its S3 access key ID and secret access key must be present for setup and every compiler process
that should use L2; a Wrangler login or general Cloudflare API token is not an S3 credential pair.

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

Use distinct bucket-scoped credential pairs for CI and developer machines even
when they share one R2 authority.
Keep the protocol marker at `native-v6/protocol`; an [object lifecycle rule](https://developers.cloudflare.com/r2/buckets/object-lifecycles/) may expire
`native-v6/entries/` without deleting the marker.
Scope the prefix relative to the selected URL root when the URL includes a prefix.

Cargo-Rail currently models only R2's default jurisdiction
and derives its standard account endpoint.
It deliberately has no jurisdiction or arbitrary-endpoint syntax.
Use a default-jurisdiction bucket until a real consumer requires a typed jurisdiction contract.

`cache probe` uses the persisted authority and standard AWS credential environment,
including a session token when present.
It proves authenticated marker compatibility and may initialize an absent marker only in `read-write` mode;
its JSON output contains the provider, mode, readiness, and marker state but no URL, object key,
or credential value.

Review current [R2 pricing](https://developers.cloudflare.com/r2/pricing/) and retention policy before selecting storage and operation budgets.

Use `cargo rail cache normalize URL` to validate a URL without resolving credentials or contacting storage.
Credentials stay outside URLs, repository configuration, result packs, diagnostics,
compiler arguments, and cache keys.
Prefer a machine or container role, OIDC, or a preconfigured profile.

`--remote-mode read` requires an existing compatible protocol marker and never writes.
`read-write` adds conditional protocol and entry publication.
For an authority rooted at `PREFIX`, provider permissions are bounded to:

| Mode         | Objects                                                   | Operations |
| ------------ | --------------------------------------------------------- | ---------- |
| `read`       | `PREFIX/native-v6/protocol`, `PREFIX/native-v6/entries/*` | Object read |
| `read-write` | The same objects                                          | Object read and conditional write |

Build credentials do not need list, delete, lifecycle, multipart-upload,
or administrative authority.
Keep provider cleanup and lifecycle policy outside build credentials.

L1 remains authoritative, so an L1 hit makes no remote request.
Absence, conflict, corruption, credential failure, throttling, or outage executes the compiler.
`--local-only` removes persisted L2 selection while preserving L1.
It is unnecessary when creating a fresh local profile because local reuse is already the default.
It cannot be combined with `--root-portability`: removing remote authority restores physical-root local reuse.

Physical-root mode is the default.
It shares only checkouts at the same canonical path.
Cross-root reuse requires `--root-portability remap`; that mode admits only certified workspace Rust metadata
and library results.
Rustc reads of regular repository files outside the package source root become a bounded dynamic
selector.
Before lookup, Cargo-Rail revalidates each selected path, file kind, byte length, content digest,
and executable mode.
Symlink or reparse crossings, inputs outside repository authority, generated namespaces,
native-search inputs, ambiguous roots, user-selected remaps,
and unsupported output classes bypass cross-root reuse.

External `CARGO_TARGET_DIR` locations are supported for eligible native results.
Cargo compiles registry and git packages inside their unpacked source,
which belongs to no workspace; under an explicit target or build directory,
those units use the workspace that Cargo started in, as the shell exports it in `PWD`.
Without `PWD`, they compile normally without reuse.
The cache identity uses one stable logical output directory,
while local compilation and restore continue to use Cargo's exact physical output parent.
Changing a checkout or target root preserves portable identity when selected input paths
and environment values stay unchanged.
Changing a selected input, including a same-size edit, produces a miss.

Additional L2 environment names must be reviewed and non-secret.
Select them with repeated `--remote-environment` options during setup.
Value digests enter action identity.
Rustc may also write those values into dep-info;
those environment records are stored and restored exactly.

## Distribute eligible misses

Distributed execution runs below Cargo L0, L1, and L2.
It accepts only bounded compiler-only Rust operations with complete source, dependency,
and selected repository inputs.
Linked outputs, build scripts, generated namespaces, native dependencies, unmodeled options,
and newly observed compiler environments remain local.

Worker protocol version 5 requires the actual compiler-selected native-input observation in each
successful response.
The client validates it against the transported files and matched toolchain
before admitting the result.
An input selected outside that transported set, incomplete assembly coverage,
or missing driver evidence rejects remote admission and falls back locally.
The operation retains ordered dependency-search directories
and the selected source-directory identity.
Physical-root operations read staged sources through the matched driver
while preserving ordinary compiler metadata paths; explicit remapping retains the virtual root.
Matching candidates and transitive libraries are transported
as exact files at their declared search paths under the worker’s existing input bounds.
Untransported inputs keep the invocation local before worker execution.

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

Run setup without `--check` only after reviewing the authority.
The default `automatic` policy stays local until fresh class-specific measurements predict a critical-path win.
`qualification` sends every eligible miss to collect evidence and may be slower.

Deploy the direct worker only on a dedicated single-tenant host or ephemeral VM.
Pin the compiler and worker capability, use mutual TLS,
run each attempt inside a resource-bounded sandbox, and inherit no operator or provider credentials.

A transport, worker, lease, sandbox,
or pre-commit validation failure executes the normalized operation locally once.
A successful response still passes the native-cache validation and restore transaction
before Cargo sees output.

## Compiler-evidence cache

Unify diagnostics and Surface's typed compiler facts are stored per view in the selected cache
profile's local store.
They are separate records with one reuse rule.
A view is reused after revalidating its compiler, sources, manifests, targets, features,
Cargo configuration, lockfile, and executable identity,
plus every file and environment variable that rustc reported reading for the view's units,
including files outside the package.
Unify preview, `--check`, `--explain`, and `apply`, and Surface runs and resumes, all apply this rule.
This store contains compiler evidence, not restorable Cargo artifacts.

A view also stays reusable only while Cargo would keep the output of every build script in it:

- Each path declared with `rerun-if-changed` keeps its content.
  A declared directory keeps every entry.
- Each variable declared with `rerun-if-env-changed` keeps its value.
  Only a digest of the value is stored.
- A script that declares no path depends on its package sources,
  which the lockfile checksum or the workspace source fingerprint already binds.

Files a build script generates and variables it sets with `rustc-env` are bound through those inputs.
Proc-macro reads are bound as Cargo binds them:
through the consuming unit's dep-info and tracked environment.
Changing one declared input reruns only the views whose Cargo graph ran that build script.
Several variants of a view can be stored, so returning to an earlier input reuses its evidence.

Unify does not store a view when a build script declares a secret-named variable,
declares a missing path, or declares a directory with more than 10,000 entries.
`evidence_cache[].publication_bypasses` names the reason.
A view is stored as soon as it completes, so a later failure or interruption keeps it for the retry.
A corrupt stored view is never reused, and it does not hide other stored views of the same key.
Runs under an unverified `RUSTC_WRAPPER` never reuse evidence, because the wrapper can read anything;
runs under Cargo-Rail's installed wrapper do.

Inspect Unify's `evidence_cache` for hits, misses, miss reasons, and publication bypasses,
and Surface's `metrics.acquisition` for fact-cache hits, misses, and bypass reasons.

## Local storage budget

Each enrolled profile has its own local store and budget.
`cache setup --max-size SIZE` sets the budget; otherwise setup keeps the profile's current budget,
and a new profile starts at 10 GiB.
Identical output files are stored once and shared by every result that produced them.
When a new result would exceed the budget,
Cargo-Rail evicts least-recently-used results until usage is at most 90% of the budget,
so a full store collects once per batch of new results rather than on every compilation.
A hit refreshes an entry's last-use time at most once per hour.
`cache setup` applies a lowered budget immediately.
Results held by an active reader are not evicted.

On Linux and macOS, the store also remembers each input file's SHA-256 while its device, inode,
size, and modification and change times are unchanged,
so dependents do not rehash the same dependency artifacts.
It keeps one record per captured source tree and per dependency directory,
so an invocation reads one record instead of one entry per file.
A record applies to a file only while that file's generation matches.
Files modified within the last two seconds are always rehashed, and entries expire after seven days.
Windows always hashes, because its file generation does not include a change time.

When collection evicts a result within a day of that result's last use,
the budget did not hold the working set in use, and the next build of that work misses again.
`cache status` then reports `recent_evictions` (results, bytes, and the time of the last such eviction)
and prints a budget-pressure line; raise the budget with `cargo rail cache setup --max-size SIZE`.
The count accumulates until the local cache is cleaned.
One validation lane of this repository, `just check` across seven targets, stores about 3 GiB.

`cache profiles` reports each profile's `bytes`, `max_bytes`, and `over_capacity_bytes`.
The installation storage in `cache status` totals every profile, so compare it with the sum of profile budgets,
not with one `max_bytes`.

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
Local cleanup removes only the current profile's CAS after validating ownership and waiting
for readers; rerun `cache setup` afterward.
It also removes that profile's store in the retired `local-cas-v2` layout, which current versions never read;
`cache status` counts retired stores as reclaimable, and `cache clean --scope local --check` includes them in its preview.
`cache detach` removes the current root binding but preserves the profile and CAS.
`cache drop-profile` accepts an opaque ID from `cache profiles` and removes only a detached profile with no enrolled roots.
`cache uninstall` removes the global wrapper, worker, receipt-owned compiler components, Cargo field,
and installation receipt while preserving profiles and their CAS data.

Setup repairs current installed component bytes from their authenticated source files.
Status reads stale receipt versions without activating them.
Setup quarantines the stale receipt and its owned installation state
before it installs the current component set and updates Cargo's wrapper entry.
Removal and reuse refuse changed, shadowed, linked, or unowned authority.
Do not edit profile records, individual CAS objects, or Cargo fingerprints by hand.

Status schema 18 reports installation integrity, component authentication,
selected-toolchain readiness, workspace enrollment, remote authority, observed reuse,
the selected profile and trust domain, and the redacted remote selection source as separate fields.
It also separates required installation bytes from quarantined reclaimable bytes.
It reports stable native failure-reason counters separately from the bounded 65,536-event usage
ledger, so capture, identity, and post-execution witness failures remain visible after that ledger
fills.
If the counter file cannot be validated, `failure_reason_counts_available` is `false` instead of reporting invented zeroes.
Verbose status also reports the fixed 64-shard native restore-lock namespace
and any staging residue.

## Benchmark evidence

Use [the benchmarking contract](benchmarking.md) for smoke, qualification, correctness, evidence retention,
and claim requirements.

### Aggregate cache measurements

`cargo rail cache report --start /absolute/recording.json -f json` creates a new private recording.
Set the machine-owned `CARGO_RAIL_CACHE_REPORT` environment variable to that path for the commands in the reporting interval.
After their compiler processes exit,
`cargo rail cache report --finish /absolute/recording.json -f json` closes the interval and emits the
[`cache-report-v1` contract](../schemas/cache-report-v1.schema.json).
Finishing again returns the recorded totals; starting over an existing file is rejected.

The recording contains bounded aggregate counters and reason counts.
Concurrent wrapper outcomes are serialized;
report storage never authorizes a restore or changes a cache key.
Known recording errors and counter overflow mark the measurements incomplete.
Corrupt or unreadable recordings fail collection.
These are observed wrapper outcomes, not Cargo freshness counts or an estimate of time saved.
Cache problems can cause fallback within a miss or bypass;
wrapper failures are a separate outcome and must not be added to reason counts
as disjoint categories.

The companion cache Action opens the interval during setup.
Add its `cache/collect` action after compilation and transfer each collected record to a job that runs `cache/report`;
setup alone does not publish a workflow report.
The report requires the expected job labels and identifies missing jobs and incomplete measurements.
Local storage remains per job; shared storage is not summed across runners.
See the [Action reporting example](https://github.com/loadingalias/cargo-rail-action#one-cache-report-for-the-workflow).
