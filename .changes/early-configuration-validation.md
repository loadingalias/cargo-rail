---
"cargo-rail" = "minor"
---

Reject invalid configuration before any Git or Cargo subprocess starts,
for explicit and discovered policy alike.
`config validate`, `print`, `explain`, `locate`, and `clean` now read the Cargo workspace root's policy, like consuming commands,
and independent configuration errors are reported together.
