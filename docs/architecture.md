# Architecture

`cargo-rail` is a library-backed CLI. Commands share one workspace snapshot and use explicit plans for filesystem, git, and release mutations.

## Workspace snapshot

Build `WorkspaceContext` once and pass it by reference everywhere else.

The context owns:

- git state
- resolved Cargo metadata
- workspace dependency graph
- loaded config

Commands do not reload metadata independently.

## Modules

| Layer | Purpose |
|---|---|
| `commands/` | CLI handlers and command dispatch |
| `action` | shell-free action declarations and deterministic request expansion |
| `hermetic` | isolated fetch/check execution and exact output proof |
| `build_script` | non-circular build-script action/result identities |
| `workspace/` | `WorkspaceContext` construction and shared state |
| `graph/` | dependency graph queries |
| `cargo/` | metadata, manifests, unify logic |
| `git/` | system git integration |
| `change_detection/` | file classification and planner taxonomy |
| `release/` | release planning and publishing |
| `split/` / `sync/` | repository extraction and bidirectional sync |
| `toml/` | lossless manifest editing |

## Command flow

1. Parse CLI arguments.
2. Initialize text or machine output.
3. Handle commands that do not need workspace metadata.
4. Build `WorkspaceContext` once.
5. Dispatch the command with a shared reference.

## Planner pipeline

`cargo rail plan` follows a fixed path:

1. Collect changed files.
2. Classify built-in and custom surfaces.
3. Resolve crate ownership.
4. Walk reverse dependencies to calculate impact.
5. Record stable reason codes and surface decisions.
6. Project the result into execution scope.

`impact` explains the graph calculation. `scope` carries the selected mode, crates, surfaces, and `cargo_args` used by runners.

`cargo rail run` maps planner surfaces to exact built-in or repository actions before starting the first action
process. Preview, local execution, CI JSON/GitHub plans, and the versioned decision receipt consume the same stable
topological expansion. Only the process boundary resolves and revalidates repository paths and applies typed
environment capabilities. The graph rejects missing snapshot identity, duplicate action IDs, unknown or repeated
dependencies, cycles, output overlap, and path escape before execution. Generated actions have one declared owner and
separate check/regenerate argv. Each expansion carries a versioned action-key analysis over exact declared source,
resolution, target, toolchain, configuration, argv, and environment identity. Incomplete ambient, process, build-script,
proc-macro, external-source, or dependency-result evidence is reported as `uncacheable`; cargo-rail does not issue an
authorizing key for it.

## Unify pipeline

`cargo rail unify`:

1. Load the host metadata once, then resolve configured targets in parallel into one indexed workspace model.
2. Derive the rustc target cfg sets and source-referenced feature selections needed to exercise conditional code.
3. Collect workspace-only compiler evidence, reusing diagnostics only when the compiler, source, manifest, target, and feature-selection identity still match.
4. Analyze versions, features, MSRV, undeclared features, and unused edges from the shared model.
5. Build a deterministic mutation plan with portable proof certificates for graph-removing decisions.
6. Revalidate exact declaration scopes and the resulting Cargo graph before applying lossless TOML edits.

The compiler wrapper passes dependency linting only to workspace compilation units. Registry, git, build-script, and proc-macro units keep Cargo's normal arguments, avoiding failures in third-party code. Open-world packages preserve public feature and optional-dependency surfaces; `consumer_scope = "workspace"` explicitly authorizes closed-world cleanup for non-published packages.

## Compilation observations

Compiler evidence uses a versioned compilation-unit identity, not a package identity. A unit binds its Cargo package,
typed target kind and name, crate types, host/target role, target specification, profile, features, `cfg`, emit modes,
link responsibility, normalized compiler argv, and exact dependency-artifact edges. Libraries, binaries, tests,
examples, benches, documentation, proc macros, build scripts, and native-link responsibility remain distinct even when
the class is not reusable.

During workspace-only `rustc` diagnostics, cargo-rail records argv-declared inputs before execution and correlates the
completed invocation with Cargo's stable JSON artifact messages. Rustc dep-info supplies observed file and environment
reads. Cargo has no rustdoc-wrapper setting, so the observation profile instead places a transparent proxy in Cargo's
selected `RUSTDOC` slot and retains the selected rustdoc as the inner executable. The proxy discovers the selected
rustdoc's supported default emit set, adds stable dep-info without dropping HTML output, and preserves response-file,
doctest, and unsupported-tool invocations unchanged with an explicit bypass.

The immutable result manifest keeps declared inputs, observed reads, dependency artifacts, emitted outputs, and
execution metadata in separate fields. Every file is SHA-256 digested from its bytes and re-digested before diagnostic
evidence can be reused. Cargo's `fresh` flag is retained only as execution metadata; it never authorizes a hit. Cargo's
documentation artifact message names an index file rather than the complete HTML tree, so documentation output remains
explicitly uncacheable until the isolated output boundary can enumerate and verify that tree.

Custom-build compilation retains Cargo's exact executable output separately from the other compiler artifacts. A
versioned `BuildScriptActionKey` can be issued only at the next process boundary, after the executable and source bytes
are revalidated and the relevant manifest/lock closure, toolchain, host and target identities, profile, features,
configuration, complete non-secret environment, exact declared dependency-action/result set, logical working
directory, and isolated launch layout are known. The stable action ID cannot appear in its own dependency set. The
script's instruction stream, runtime reads, and generated output tree are deliberately absent because they are results
of that action. Normal Cargo execution still inherits ambient state, so compiler observation records a stable
explanation but does not issue a build-script key. Cargo leaves the optional `executable` field empty for
custom-build artifacts, so cargo-rail accepts exactly one target-named program from `filenames`; zero or multiple
matches fail closed.

`BuildScriptResult` version 1 is the separate post-execution identity. Its digest frames the Cargo instruction lines
in emitted order, including rerun declarations, link libraries/search paths/arguments, `rustc-cfg`,
`rustc-check-cfg`, `rustc-env`, metadata, warnings, and errors. It also binds the sorted set of non-secret environment
reads by value digest, a canonical logical generated-output tree with file bytes, executable modes, and symlink
targets, plus execution success and platform identity. Modern `cargo::KEY=VALUE` and legacy `cargo:KEY=VALUE`
instructions follow Cargo's distinct parsing rules; legacy unreserved keys are metadata. Physical checkout paths,
escaping output symlinks, malformed observations, failed execution, secrets, or any missing domain withhold the
digest. Serialized analysis retains only counts, capability names, and stable reasons, never raw instruction or
environment values.

Cargo's stable `build-script-executed` JSON is useful but incomplete post-execution evidence. Cargo can replay the
message without running the script, and the message omits instruction order, rerun declarations, link arguments,
metadata, warnings, runtime environment reads, the generated tree, and execution freshness. The normal collector
therefore stores only redacted counts for linked libraries/paths, cfgs, `rustc-env` entries, and whether `OUT_DIR` was
reported. It records every missing proof domain and never issues a result digest from that subset. When a complete
result is available, its verified digest is attached to the producer package's ordinary compilation units and every
transitive consumer unit. The build-script unit never consumes its own result. Missing action/result evidence or an
incomplete dependency graph produces a stable bypass instead of omitting the edge. The current hermetic profile blocks
build scripts before execution, so normal Cargo remains executable while that complete runtime boundary is still
ungraduated.

Selected and underlying Cargo, rustc, and rustdoc implementations, wrappers, configured linkers and runners, and
repository executables are content-addressed when relevant. Scripts also bind direct interpreters. Response-file
expansion, dynamic libraries, SDK inputs, default linkers, incomplete platform images, and incomplete rustdoc output
trees produce explicit bypasses. Any observation bypass prevents diagnostic-evidence reuse; collector
semantics are versioned so older evidence cannot silently gain authority. An observation manifest never identifies its
own producing action; exact verified artifacts and result digests may become inputs only to later dependent actions.
Cargo-rail neither stores result artifacts nor writes or restores Cargo fingerprint state.

## Hermetic execution profile

`cargo rail run --all --action build --hermetic` is an explicit proof profile. It does not alter ordinary `run`
execution and it is not yet an action-result cache.

Actual hermetic execution currently requires the explicit built-in `--action build`. Default, profile, workflow, and
other action dispatch is rejected before workspace context or hermetic state is created; dry-run remains a planning
preview rather than an execution-boundary proof.

Trailing Cargo arguments may refine modeled features, targets, target kinds, and profiles. They may not replace the
workspace/lockfile, expand package scope outside cargo-rail's selection, redirect outputs, inject Cargo configuration,
enable unstable Cargo semantics, or pass raw rustc arguments; those boundary overrides fail before fetch state.

The profile requires an existing exact `Cargo.lock` and has one network boundary: `cargo fetch --locked`. Before that
boundary, cargo-rail captures and classifies Cargo configuration, rejects configured compiler/rustdoc wrappers and
ambient `RUSTC`/`RUSTDOC`, and performs only locked/offline local-package metadata preflight. Toolchain discovery
disables rustup auto-install/update behavior, ignores ambient compiler wrappers, and pins the exact sysroot Cargo,
rustc, and rustdoc; wrappers, rustup staging homes, and a newly downloaded toolchain cannot enter the fetch identity.
The fetch binds the lockfile, acquisition configuration, credential capability names, and exact Cargo implementation,
captures locked registry or Git packages as an immutable source inventory, and runs full metadata locked/offline
against that inventory. A warm run exactly revalidates and reuses this dependency inventory without contacting the
registry. It does not restore compiler output.

Each check runs with `--locked --offline` in a fresh root containing a streamed, byte-verified, read-only source tree,
a fresh mutable Cargo bookkeeping area, an isolated `target-dir`, stable `build.build-dir`, temporary and home
directories, and a controlled environment. Effective supported Cargo `[env]`, remote registry source replacement,
profile environment, target rustflags, and repository-relative values are materialized explicitly. Cargo dep-info and
rustc output paths are remapped to logical workspace, target, build, Cargo-home, toolchain, and run roots. The source,
dependency inventory, and exact toolchain/platform read boundary are revalidated after observation and immediately
before the manifest is published.

Cargo invokes cargo-rail as a global observation-only rustc wrapper for this profile, so registry and Git dependency
compilations are observed with the same exactness as workspace units. Rustc version/help/print probes are excluded
from the unit set. Before a key can be issued, the multiset of Cargo compiler artifacts must match the raw observed
crate invocations exactly, every invocation must have observed outputs, and dep-info outputs must survive into the
declared output manifest. Missing or extra coverage fails closed instead of producing a partial key.

On macOS, `sandbox-exec` denies network and defaults filesystem access to denied. The policy admits only the isolated
run, immutable dependency inventory, exact Cargo/rustc/driver and host sysroot inputs, the observer executable, and the
small sealed host file set required by the selected toolchain. Other operating systems still get offline Cargo and
isolated roots, but receive `platform_limited` and no authorizing action key until an equivalent filesystem/network
boundary is implemented.

The result is a versioned manifest of every declared compiler-output file, directory, symlink, mode, digest, and byte
count under the isolated Cargo build directory. Cargo's internal fingerprints and incremental state are intentionally
excluded: their layout is unstable, they are never synthesized, and P7 must not restore a whole Cargo build directory
as if it were valid state. After source, inventory, toolchain, and platform revalidation, every declared output is
re-read and compared with that manifest immediately before the report is published.

| Action class | macOS proof | Other hosts | Current contract |
|---|---|---|---|
| Pure current-host `cargo check` for libraries and binaries | `eligible` | `platform_limited` | Two-root action key and output manifest converge. |
| Pure test, example, and bench compilation via `cargo check --all-targets` | `eligible` | `platform_limited` | Compilation only; no test or benchmark process runs. |
| Locked crates.io, remote registry-mirror, or Git dependencies | input supported | input supported | Network only during fetch; full metadata/build are locked and offline, and a warm inventory performs zero registry requests. Local directory/source replacement remains ungraduated. |
| Build scripts, proc macros, native/generated code | `uncacheable` | `uncacheable` | Dynamic runtime/tool inputs are not yet sandboxed as complete action classes; normal execution remains available. |
| Documentation, actual test execution, linked build/package artifacts | `uncacheable` | `uncacheable` | Output/runtime boundaries are incomplete or not implemented by the profile. |
| Cross/custom targets, configured linker/runner, repository wrappers, sccache | `uncacheable` | `uncacheable` | Tool/SDK/wrapper coexistence has not passed the per-class proof gate. |

Physical checkout roots, unrelated files/packages/environment, and Cargo's mutable cache representation do not affect
the graduated key. Exact source, resolution, profile, feature, target, flags, environment, toolchain, platform, and
dependency-content changes do.

## Mutation authority

Mutation contract v2 binds `HEAD`, the dirty-path snapshot, declared read-only inputs, structured actions, and every authorized file mutation. Apply rechecks that state immediately before the first write. Release commits stage only changed authorized paths. Sync commits stage only paths owned by the source commit.

Split and sync resolve Cargo member names, dependency closure, release boundaries, and explicit non-Cargo assets from
the same `WorkspaceSnapshot` used by planning and release. Versioned `Rail-Origin` trailers bind synthesized commits
to the source repository, source commit, ownership snapshot, and transform version. Ordinary heads, remote refs, and
tags are the mapping database; Git notes are read only during lossless migration.

Source, target, and temporary roots are canonicalized before mutation. Overlapping repositories and symlink escapes
are rejected, and each destination is revalidated before writing. Exact Git trees preserve deletions, modes, symlinks,
parents, and separate author/committer identities without staging unrelated worktree state.

## Dependency choices

- System `git` handles repository operations and three-way merges.
- `petgraph` provides graph storage and traversal without a project-specific graph abstraction.
- `toml_edit` preserves manifest comments and formatting.
- `main.rs` handles startup and error reporting; production behavior remains in the library.

## Code map

| Change | File or module |
|---|---|
| CLI shape | `src/commands/cli.rs` |
| command dispatch | `src/commands/mod.rs` |
| context loading | `src/workspace/context.rs` |
| planner behavior | `src/commands/plan.rs` |
| graph algorithms | `src/graph/` |
| unify behavior | `src/cargo/` |
| config parsing | `src/config/` |
| release planning and state | `src/release/` |
| split and sync mutations | `src/split/`, `src/sync/` |
