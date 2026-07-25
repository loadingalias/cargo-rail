---
"cargo-rail" = "patch"
---

Hardened exact-SHA release readiness to reject all-skipped GitHub rollups and run release commits through normal CI. `cargo rail config migrate` now removes the inert `release.require_clean` and `release.publish_delay` fields, and release previews no longer claim to delay between publishes. Added explicit cache capability and evaluation guidance.
