---
"cargo-rail" = "major"
---

Expanded transparent compiler reuse to every exact compiler-owned Rust result supported by the current native
platform witness: metadata and rlib outputs, compiler-owned static archives, build-script and proc-macro producers,
ordinary binaries, tests, examples, benchmarks, dylibs, and cdylibs. Apple and Linux ELF linking share one typed
linker witness; COFF, explicit linkers, cross targets, incremental work products, alternate compilers, external
backends, Clippy diagnostics, rustdoc output, doctest execution, proc-macro consumers, and build-script execution keep
specific acquisition-free cold boundaries when complete authority is unavailable.

Added a root-independent operation inventory for Rust compilation, build-script execution, native compilation,
compiler-owned `asm!`, `global_asm!`, `naked_asm!`, included and target-feature-sensitive assembly results,
Rust-required external assembly, preprocessing, archives, probes, generated outputs, and downstream artifact edges.
Path-bearing and opaque compiler values retain typed shapes without embedding checkout roots in comparison identities;
raw compiler arguments remain in retained evidence.
Benchmark qualification now rejects ambiguous or unaccounted work, unsafe competitor hits, incomplete requested
outputs, and compiler modes that touch cache or remote state. IBM Power, IBM Z, and RISC-V claims remain blocked on
their exact hardware-access evidence gates. Local shared-hit superiority requires at least a 10% p50 and p95 reduction
against the pinned sccache lane, records the separate 15% target, and cannot be hidden by the aggregate overhead result.
Real-provider correctness now covers check, release build, and all-target test compilation and requires a retained
producer/consumer pair report proving exact root-independent action multisets, output bytes, read-only import, and
offline L1 behavior.
Provider fault qualification now shares one AWS S3, Azure Blob, and Cloudflare R2 harness with dedicated
run namespaces. Each backend must reject a corrupt object, fall back when that object is absent, and build through a
network-denied outage without changing exact outputs or writing remotely. Cleanup rejects broader authorities; Azure
qualification requires and removes one dedicated run container.
Native host qualification now binds the exact host and filesystem to the compatibility corpus, authenticated full
suite, doctests, complete cache coverage, and the retained performance decision on Linux and Windows x64/Arm64. Its
dedicated bootstrap profile installs both the full validation toolchain and the pinned benchmark tools.
