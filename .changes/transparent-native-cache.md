---
"cargo-rail" = "major"
---

Added one safe machine setup for daemonless verified local compiler reuse beneath ordinary Cargo, nextest, Just, IDE,
CI, and Cargo-Rail commands. Cargo freshness and incremental compilation remain authoritative; disabled, incremental,
ambiguous-wrapper, and unsupported compiler shapes bypass before session or cache acquisition. Setup, status, repair,
opt-out, cleanup, and exact receipt-owned removal share one private bounded local authority. A minimal launcher
preserves the disabled compiler contract without starting the receipt-authenticated cache worker. Metadata/rlib actions,
including metadata-only proc-macro producers, exact native-static consumers, and certified Apple build-script
executables, proc-macro producer dylibs, and final linked artifacts use one action/witness/result pack with verified
atomic L1 restore. Native proc-macro consumers remain cold.
Removed runner-owned native cache activation and remote transfer rather than maintaining a second cache protocol;
retained remote targets are configuration-only until a later release transports this exact compiler-owned pack.
On the canonical five-sample local fixture, verified warm L1 was strictly faster than pinned sccache at p50 and p95 for
both check and release workloads while safely reusing more compiler actions and rejecting unsafe native proc-macro
consumer hits.
