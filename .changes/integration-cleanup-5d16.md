---
"cargo-rail" = "minor"
---

Make cache installation upgrades recoverable,
add an explicit local readiness proof and schema 18 status,
and keep remote authority separate from local health.
Add bounded Surface preflight, progress, cancellation, storage accounting, and target scope;
Surface reports now use contract v4 and planner consumers use v9 attribution.
Regenerate older Surface reports and update strict cache-status consumers
before adopting this release.
