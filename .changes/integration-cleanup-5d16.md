---
"cargo-rail" = "minor"
---

Make cache installation upgrades recoverable,
prevent registry reads from observing concurrent profile publication,
add an explicit local readiness proof and schema 18 status,
keep remote authority separate from local health,
and qualify native stable and pinned nightly compilers against independent
release and date bounds for authenticated driver sources.
Add bounded Surface preflight, progress, cancellation, storage accounting, and target scope;
Surface reports now use contract v4 and planner consumers use v9 attribution.
Regenerate older Surface reports and update strict cache-status consumers
before adopting this release.
