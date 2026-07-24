---
"cargo-rail" = "major"
---

Add fail-closed action-key diagnostics over exact source, resolution, toolchain, executable, Cargo configuration, argv,
typed environment, and verified dependency-result identities. Transparent rustdoc observation preserves the selected
tool and HTML output while recording stable dep-info. Build-script compilation separates its non-circular
pre-execution action identity from the ordered instructions, environment reads, generated tree, and execution evidence
in its result identity. Incomplete boundaries remain explicitly non-reusable while ordinary unsupported execution stays
available.

Add `cargo rail run --all --action build --hermetic` for the graduated pure-Rust Cargo-check class. It performs an
explicit locked fetch, captures immutable crates.io, remote registry-mirror, or Git dependency sources, then checks
locked/offline in fresh read-only source and isolated output roots with logical path remapping and a controlled
environment. macOS enforces filesystem and network denial and can issue a verified action/result manifest; other hosts
remain platform-limited. Build scripts, proc macros, docs, linked/native/cross-target work, custom tool boundaries, and
sccache fail closed. Cargo fingerprints and incremental state are never restored. Action plans and decision receipts
use schema version 4.
