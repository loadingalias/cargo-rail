---
"cargo-rail" = "major"
---

Replaced repository-selected `[cache]` aliases with one strict machine-owned remote URL authority. Setup now persists
explicit read or read-write AWS S3, Azure Blob Storage, and Cloudflare R2 destinations, can return to local-only mode,
and reports only redacted authority. A new network-free normalization command validates provider URLs before
credentials or storage are consulted. Existing repository `[cache]` configuration is rejected with migration guidance.

Added transparent result sharing through one conditional object protocol and a private bounded coordinator that reuses
provider credentials, clients, and connections without retaining build results in memory. Verified local packed results
remain authoritative and network-free; absence, conflict, corruption, credential failure, coordinator failure, or
provider outage falls back to exact cold compilation. Qualified Linux ELF linker evidence also expands safe reuse to
linked build-script, proc-macro, and final executable outputs.

On the retained Linux x64 corpus, local L1 was 77.55–89.25% faster than pinned sccache at p95 while restoring more
compiler actions. The empty-L1 AWS S3 corpus was 43.58% faster for check and 73.27% faster for release build at p95.
Azure Blob and R2 passed independent live producer/consumer, read-only, offline-L1, corruption/outage, and cleanup
qualification; these results do not claim Azure or R2 performance superiority.
