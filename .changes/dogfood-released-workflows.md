---
"cargo-rail" = "major"
---

Add independently authenticated compiler adapter packs for toolchains beyond the embedded driver,
while keeping native cache failures fail-open and Surface acquisition fail-closed.

Strengthen release preparation with local extended checks, crate-qualified default tags,
linked GitHub changelog comparisons, and release-record contract v10.
Remove the no-op `release.require_changelog_entries` configuration field;
`release.require_release_notes` remains the release-prose gate.
Correct cache readiness, initialization, and dependency-unification edge cases.
