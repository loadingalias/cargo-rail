---
"cargo-rail" = "major"
---

Add fail-closed action-key diagnostics, exact compiler observations, executable identity, and stable Cargo
configuration contracts without enabling artifact reuse. Transparent rustdoc observation preserves the selected tool
and HTML output while recording stable dep-info; incomplete documentation output trees remain uncacheable.

Build-script compilation now retains the exact executable and separates the non-circular pre-execution action key from
the ordered instructions, environment reads, generated tree, and execution evidence in its result digest. Verified
results propagate to every dependent compilation unit; incomplete stable Cargo observations keep normal scripts
executable but explicitly non-reusable.

Add `cargo rail run --all --action build --hermetic` for the graduated pure-Rust Cargo-check class. An explicit locked
fetch action produces an exact immutable crates.io, remote registry-mirror, or Git dependency inventory from a reviewed
`Cargo.lock`; no full metadata call can acquire dependencies before that boundary, and warm inventories make zero
registry requests. Checks then run locked/offline in fresh read-only source and isolated output roots with logical path
remapping and a controlled environment. Exact preinstalled sysroot tools are pinned without invoking ambient wrappers
or allowing rustup to install/update a toolchain. An observation-only global rustc wrapper covers workspace and external
dependency units; Cargo artifacts, compiler observations, emitted outputs, and dep-info must agree exactly. macOS
enforces filesystem and network denial and can issue a verified action key/result manifest; other hosts report
platform-limited. Build scripts, proc macros, docs, linked/native/cross-target work, custom tool boundaries, and sccache
remain fail-closed. Workspace/package boundary overrides, redirected outputs, configured compiler wrappers, unstable
Cargo semantics, and raw rustc arguments fail before fetch state. Declared outputs are re-digested immediately before
publication. No compiler artifact or Cargo fingerprint is restored before P7. Action plans and decision receipts now
use schema version 4.
