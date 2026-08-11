# Share local compiler reuse across workspaces

Cargo-Rail's transparent compiler cache is a private, user-wide L1. One setup enables eligible reuse for ordinary
Cargo, nextest, Just, IDE, CI, and `cargo rail run` invocations that use the same effective Cargo home:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status --scope local --format json
```

The setup receipt selects one local CAS authority and byte bound. Workspaces under the same OS user and receipt can
share exact compiler results, but action identity remains bound to the canonical physical workspace root because Rust
metadata and diagnostics can contain source paths. A second checkout therefore misses first and then reuses only its
own root-bound variant.

## What is shared

The CAS can reuse graduated dependency and workspace-library `.rmeta`, metadata-only proc-macro producer `.rmeta`,
optional `.rlib`, dep-info, and captured diagnostic output, including actions that consume exact native-static search
namespaces. On macOS, certified default Apple-linker actions can additionally restore build-script executables,
proc-macro producer dylibs, ordinary final binaries, `dylib`, and `cdylib` outputs. Thin-LTO results require the adapter
to prove that reported intermediate objects were created after snapshotting an owner-controlled rustc temporary
namespace that is not writable by group or other users; temporary aggregate archives must be selected by the exact
rustc driver and absent from user compiler arguments. Every hit revalidates the action/witness/result binding and
stored bytes before replacing an output. Cargo still owns fingerprints and incremental compilation; an intact target
normally removes the compiler invocation at L0 before L1 is involved.

Native proc-macro consumers, build-script execution and generated output production, rustdoc, test mode, cross-target
work, incremental compilation, nonstandard target layouts, custom linkers, and ambiguous wrapper composition bypass
L1. A bypass executes the selected compiler chain and is not a build failure.

## Isolate a machine role

Use a separate Cargo home when CI, an untrusted job, and an interactive user must not share cache authority. For a
dedicated authority with the same Cargo home, select it during setup:

```bash
cargo rail cache setup --local-dir /var/cache/cargo-rail/ci --max-size 20GiB --check
cargo rail cache setup --local-dir /var/cache/cargo-rail/ci --max-size 20GiB
```

The directory must be a real private path. Cargo-Rail records it in the setup receipt; runtime environment variables
cannot silently redirect reuse. Do not copy a CAS between trust domains or expose it through a shared filesystem.

## Disable reuse for one process tree

```bash
CARGO_RAIL_CACHE=off cargo check
```

The wrapper immediately executes the original compiler command without loading installation context, session state,
or CAS data. It preserves argv, working directory, inherited environment—including `CARGO_RAIL_CACHE=off`—streams,
and exit status. `cargo rail run --no-cache` delegates with the same opt-out.

## Cleanup and removal

```bash
cargo rail cache clean --scope local --check
cargo rail cache clean --scope local
cargo rail cache setup                 # repair the selected empty authority

cargo rail cache remove --check
cargo rail cache remove
```

Local cleanup removes the validated receipt-selected CAS and leaves setup intentionally unhealthy until repaired.
Removal deletes only the Cargo field and private setup state named by the same receipt; it preserves the CAS. Both
commands revalidate bytes immediately before mutation and refuse changed, shadowed, linked, or unowned state.

## Remote sharing is deferred

Transparent reuse is local-only. The earlier runner-owned S3 import/publication coordinator was removed, and ordinary
Cargo invocations do not contact remote storage. Existing `[cache].l2` and machine target-map configuration can still
be validated by status and doctor commands, which report
`configuration_only_transparent_cache_is_local`; they do not activate transfer.

Do not build automation around the retained target-map schema as if it were a functioning remote data plane. Remote
reuse must later consume the same authenticated compiler-result protocol directly, without recreating runner ownership,
adding a daemon, or weakening exact local verification.
