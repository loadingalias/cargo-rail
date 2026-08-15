---
"cargo-rail" = "major"
---

Added complete workspace Rust source-surface analysis from authenticated compiler facts. `cargo rail surface --check`
separates production, non-production, and required-public reachability across configured targets and reports dead or
unnecessarily broad visibility only for explicitly closed, non-publishable workspace packages. Versioned text, JSON,
GitHub, schema, reason, fragment, and cache evidence share one deterministic report.

Added exact visibility repair with snapshot-bound byte spans, deterministic mutation plans, drift rejection, bounded
backups, atomic file replacement, rollback, receipts, and post-write recompilation of every configured view. Public
declaration deletion remains report-only. Planner contract v7 adds a whole-workspace `surface` decision without
claiming package-scoped Cargo arguments.
