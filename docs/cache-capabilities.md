# Execution, Cache, and Performance Support

> Auto-generated from executable CI/release registries, native-cache production gates, and reviewed qualification
> manifests. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`. Support manifest schema: `4`;
> native-cache capability registry schema: `1`; qualification schema:
> `1`.

Use this matrix to answer one question: for this exact host, target, toolchain, and compiler class, does Cargo-Rail execute
normally, restore a verified result, or bypass reuse? **Fast when proven. Normal Cargo when not.**

Execution support, cache graduation, and performance qualification are independent. A cache bypass still executes
Cargo normally; it is not an execution-support failure.

Native-cache graduation is certificate-specific, not target-wide. Run `cargo rail doctor native-cache --format json`
to inspect the exact Cargo, rustc, rustdoc, sysroot, backend, host, and wrapper-protocol identity selected in the
captured workspace. A candidate host with no matching certificate executes normally with
`native_cache_capability_not_certified`.

## Host and target matrix

| Target | Native execution | Cross-target compilation | Release artifact | Native compiler-result cache | Performance qualification |
|---|---|---|---|---|---|
| `aarch64-apple-darwin` | Advertised; full-suite CI required (`macos-15`) | — | Native artifact required | Graduated `library_metadata_rlib` (Cargo `1.97.1`, rustc `1.97.1`; 1 exact certificate) | Qualified: check + release-build; 110 accepted, 0 false hits |
| `aarch64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-11-arm`) | — | Native artifact required | Bypass: `native_cache_platform_not_graduated` | Not qualified |
| `aarch64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04-arm`) | — | Native artifact required | Bypass: `native_cache_capability_not_certified` | Qualified: check + release-build; 110 accepted, 0 false hits |
| `aarch64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `thumbv7em-none-eabihf` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32-unknown-unknown` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32-wasip1` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `wasm32v1-none` | Not a native host | Required compatibility build | Fixture artifact required | Bypass: `cross_target_not_graduated` | Not qualified |
| `x86_64-apple-darwin` | Advertised; full-suite CI required (`macos-15-intel`) | — | Native artifact required | Bypass: `native_cache_platform_not_graduated` | Not qualified |
| `x86_64-pc-windows-msvc` | Advertised; full-suite CI required (`windows-2022`) | — | Native artifact required | Bypass: `native_cache_capability_not_certified` | Not qualified |
| `x86_64-unknown-linux-gnu` | Advertised; full-suite CI required (`ubuntu-24.04`) | — | Native artifact required | Bypass: `native_cache_capability_not_certified` | Qualified: check + release-build; 110 accepted, 0 false hits |
| `x86_64-unknown-linux-musl` | Not a native host | Required compatibility build | Cross-built artifact required | Bypass: `cross_target_not_graduated` | Not qualified |

Linux musl rows are required release cross-builds. They are not native Linux host evidence.

## Filesystem matrix

| Profile | Runner | Filesystem | Case behavior | Required evidence |
|---|---|---|---|---|
| Default `aarch64-apple-darwin` | `macos-15` | `apfs` | Insensitive | Full endpoint suite and native probe |
| Default `aarch64-pc-windows-msvc` | `windows-11-arm` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `ext4` | Sensitive | Full endpoint suite and native probe |
| Default `x86_64-apple-darwin` | `macos-15-intel` | `apfs` | Insensitive | Full endpoint suite and native probe |
| Default `x86_64-pc-windows-msvc` | `windows-2022` | `ntfs` | Insensitive | Full endpoint suite and native probe |
| Default `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `ext4` | Sensitive | Full endpoint suite and native probe |
| linux-tmpfs | `ubuntu-24.04` | `tmpfs` | Sensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |
| macos-apfs-case-sensitive | `macos-15` | `apfs` | Sensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |
| windows-ntfs-vhd | `windows-2022` | `ntfs` | Insensitive | Front-door corpus, CAS/atomicity suite, ENOSPC, and cleanup |

Alternate filesystem profiles use bounded temporary volumes and must cleanly detach them even after a failed test.

## Deferred native hosts

| Platform | Target | Execution status | Cache status | Performance status |
|---|---|---|---|---|
| IBM Power | `powerpc64le-unknown-linux-gnu` | Pass-through target execution remains unqualified | `native_cache_platform_not_graduated` | Not qualified |
| IBM Z | `s390x-unknown-linux-gnu` | Pass-through target execution remains unqualified | `native_cache_platform_not_graduated` | Not qualified |

IBM Power and IBM Z need native runners before cargo-rail can advertise native compatibility, cache graduation, or
performance qualification. Their ordinary Cargo target requests remain fail-closed for reuse.

## Linkers and codegen backends

| Capability | Advertised non-default implementations | Current contract |
|---|---|---|
| Linker | None | Current-stable native lanes prove the default, its explicit driver, and the bundled host LLD flavor as Cargo-owned pass-through execution. No alternate is advertised; selected linkers retain `configured_linker_not_graduated`. |
| Codegen backend | None | Native lanes prove stable LLVM, and the pinned-nightly lane proves Cranelift plus unknown-backend diagnostics as rustc-owned pass-through execution. No alternate is advertised; selected backends retain `codegen_backend_not_graduated`. |

No non-default implementation becomes advertised merely because cargo-rail preserves its invocation. It first needs a
named compatibility fixture on every applicable native host.

## Cache layers

| Layer | Current support | Authority boundary |
|---|---|---|
| Compiler-evidence cache | Workspace-only `unify` observations with complete revalidation | Diagnostic evidence; never restores Cargo artifacts |
| Hermetic whole-action cache | Current-host macOS pure-Rust `cargo check` class | Verified action/result manifest and isolated output tree |
| Native compiler-result cache | Eligible library metadata/rlib invocations listed above | Verified per-invocation action/result binding through Cargo's wrapper boundary |

## Native compilation classes

| Class | Reuse status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Graduated only for listed host/toolchain tuples | One declared crate root, complete observed Rust inputs, dep-info, `.rmeta`, optional `.rlib`, Rust-only dependency artifacts, no linker responsibility |
| Incremental compilation | Reuse bypassed; compiler executes | Requires `CARGO_INCREMENTAL=0`; forced incremental compilation also bypasses |
| Binary, test, example, and benchmark linking | Reuse bypassed; compiler/linker executes | Linker-producing invocations are not graduated |
| `dylib`, `cdylib`, and `staticlib` | Reuse bypassed; compiler/linker executes | Native linker, SDK, runtime, and archive boundaries are incomplete |
| Proc macros and their consumers | Reuse bypassed; compiler executes | Compile-time filesystem/process reads are not completely observed |
| Build scripts and generated output | Reuse bypassed; build script executes | Normal Cargo messages do not prove the ordered instruction stream, runtime reads, generated tree, or freshness |
| Native dependencies and `links` contracts | Reuse bypassed; native tools execute | External compiler, archiver, linker, SDK, and discovery inputs are incomplete |
| rustdoc and doctests | Reuse bypassed; rustdoc/test executes | Stable Cargo output does not enumerate the complete documentation tree; doctest execution is separate |
| Cross compilation and custom target specifications | Reuse bypassed; compiler executes | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing sccache or custom compiler wrappers | Preserved; cargo-rail reuse bypassed | The selected wrapper chain remains authoritative and is never double-cached |
| Cargo CLI `--config` and action-defined environments | Reuse bypassed; Cargo executes | Effective build configuration or environment is outside the graduated direct-action contract |

See [Caching](caching.md) for activation, telemetry, benchmark evidence, and the graduation rules behind this matrix.
