# Cache Capability Matrix

> Auto-generated from the native-cache production gates. Do not edit manually.
>
> Regenerate with: `./scripts/docs/generate.sh`

## Cache layers

| Layer | Current support | Authority boundary |
|---|---|---|
| Compiler-evidence cache | Workspace-only `unify` observations with complete revalidation | Diagnostic evidence; never restores Cargo artifacts |
| Hermetic whole-action cache | Current-host macOS pure-Rust `cargo check` class | Verified action/result manifest and isolated output tree |
| Native compiler-result cache | Eligible library metadata/rlib invocations listed below | Verified per-invocation action/result binding through Cargo's wrapper boundary |

## Native hosts and toolchains

| Host | Cargo | rustc | Status |
|---|---:|---:|---|
| `aarch64-apple-darwin` | `1.97.1` | `1.97.1` | Shipped |
| `aarch64-unknown-linux-gnu` | `1.97.1` | `1.97.1` | Shipped |
| Every other host or toolchain release | — | — | Deliberately bypassed |

## Native compilation classes

| Class | Status | Boundary |
|---|---|---|
| Dependency and workspace library metadata/rlib | Shipped | One declared crate root, complete observed Rust inputs, dep-info, `.rmeta`, optional `.rlib`, Rust-only dependency artifacts, no linker responsibility |
| Incremental compilation | Deliberately bypassed | Requires `CARGO_INCREMENTAL=0`; forced incremental compilation also bypasses |
| Binary, test, example, and benchmark linking | Deliberately bypassed | Linker-producing invocations are not graduated |
| `dylib`, `cdylib`, and `staticlib` | Deliberately bypassed | Native linker, SDK, runtime, and archive boundaries are incomplete |
| Proc macros and their consumers | Deliberately bypassed | Compile-time filesystem/process reads are not completely observed |
| Build scripts and generated output | Deliberately bypassed | Normal Cargo messages do not prove the ordered instruction stream, runtime reads, generated tree, or freshness |
| Native dependencies and `links` contracts | Deliberately bypassed | External compiler, archiver, linker, SDK, and discovery inputs are incomplete |
| rustdoc and doctests | Deliberately bypassed | Stable Cargo output does not enumerate the complete documentation tree; doctest execution is separate |
| Cross compilation and custom target specifications | Deliberately bypassed | Host/target tools, runners, SDKs, and target specifications are not graduated |
| Existing sccache or custom compiler wrappers | Preserved; cargo-rail bypasses | The selected wrapper chain remains authoritative and is never double-cached |
| Cargo CLI `--config` and action-defined environments | Deliberately bypassed | Effective build configuration or environment is outside the graduated direct-action contract |

See [Caching](caching.md) for activation, telemetry, benchmark evidence, and the graduation rules behind this matrix.
