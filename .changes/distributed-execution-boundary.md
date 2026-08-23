---
"cargo-rail" = "major"
---

Added optional distributed execution for a deliberately bounded class of compiler-owned Rust operations. Requests use
a versioned typed protocol with canonical source namespaces, exact Rust dependency inputs, fixed compiler options, and
metadata or library outputs. Unsupported inputs, native or dynamic dependencies, linking, incomplete environment
evidence, and unknown compiler shapes stay local.

`cache setup` can pin one mutually authenticated worker and its exact compiler, sysroot, platform, endpoint, trust
root, client identity, and execution policy. The qualified Linux policy runs each attempt in an empty-root Bubblewrap
sandbox with private namespaces and an exact cgroup-v2 envelope for CPU, memory, processes, scratch space, time,
streams, and outputs. Startup probes require observed CPU throttling, an OOM kill, process-limit enforcement, and an
idle hierarchy before the worker accepts normal work. The direct worker remains for dedicated single-tenant or
ephemeral machines; it is not a multi-tenant service or general remote runner.

Distribution runs only after local L1 and remote L2 miss. Transport, protocol, capability, lease, and pre-effect worker
failures fall back to the exact local compiler command. Compiler failures retain their exit state and bounded
diagnostics without returning partial artifacts. Successful responses enter private staging and must pass the same
live recapture, action/result verification, and atomic restore transaction as local cache results before any output is
published. Workers never receive cache-provider credentials or cache write authority.

Automatic placement uses bounded, expiring, source-free cost history and delegates only when at least three local and
remote observations predict a critical-path win. In the retained same-shape `c8i.large` qualification, a six-crate
dependency DAG completed in 10.098 seconds p50: 29.57% below local Cargo and 28.84% below pinned distributed sccache.
Small, single-large-unit, and parallel-check workloads lost and remain local, so this result is intentionally limited
to the qualified dependency-DAG topology.
