---
"cargo-rail" = "minor"
---

`cargo rail plan evidence --work WORK_ID --output FILE -- CARGO_ARGS` records portable planning evidence from the ordinary build of `HEAD`:
the workspace files each compiled unit read, and each build script's rerun inputs.
With that evidence, `cargo rail plan` skips `cargo.build`, `cargo.clippy`, or `cargo.test` for a change that no recorded unit reads, such as a README,
and selects the readers of a changed input and their dependents.
Recording runs Cargo with a recorder as `RUSTC_WRAPPER` and changes neither outputs nor freshness,
so it can replace the job's build step; fresh units are read from the dep-info Cargo keeps.
An unobserved member or unit, an untracked read,
or a missing build-script input leaves the work item incomplete.

The evidence contract is now `planning-evidence-v2`,
which adds directory inputs for build-script rerun directories and package sources.
Plans list `planning-evidence-v2` identities, and v1 evidence is rejected.
`--evidence` can be repeated, one file per work item; each file is validated separately,
so a stale or foreign file widens only the work it describes.
An added or changed `clippy.toml` or `.clippy.toml` now selects `cargo.clippy`.
