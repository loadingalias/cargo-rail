---
"cargo-rail" = "minor"
---

Add a bounded machine-local action/output cache for eligible macOS hermetic Cargo checks. Verified hits restore exact
declared outputs into a clean root without starting Cargo or rustc; changed inputs, corrupt objects, unsupported
classes, and other platforms remain fail-closed. Add `--no-cache` and extend `run --explain`, diagnostics, and
`clean --cache` with local-cache decisions.
