---
"cargo-rail" = "patch"
---

Keep package-wide planner work when target selectors cover only part of the selected packages, and compare
`plan --all` against `HEAD`. Bind a `--config` override into release drift checks, and fail Surface closed when
compiler facts are absent.
