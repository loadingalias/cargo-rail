---
"cargo-rail" = "major"
---

Add independently authenticated compiler adapter packs for toolchains beyond the embedded driver,
while keeping native cache failures fail-open and Surface acquisition fail-closed.

Strengthen release preparation with local extended checks, crate-qualified default tags,
linked GitHub changelog comparisons, and release-record contract v10.
Make Cargo-Rail's hosted release enforce the existing package workflow, crates.io publication,
GitHub release, release assets, and historical `v{version}` tag namespace.
Remove the no-op `release.require_changelog_entries` configuration field;
`release.require_release_notes` remains the release-prose gate.
Keep the remote active-release lease readable across record-schema upgrades without accepting older transaction records.
Correct cache readiness, initialization, and dependency-unification edge cases.
