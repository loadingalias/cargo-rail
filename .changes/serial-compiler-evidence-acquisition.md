---
"cargo-rail" = "minor"
---

Compile each shared dependency once when Unify and Surface acquire compiler evidence.
Views now run one Cargo process at a time in a shared sandbox, and Cargo applies its own job parallelism,
including `build.jobs`, `CARGO_BUILD_JOBS`, and an inherited jobserver.
Before, parallel views each rebuilt the shared dependency graph in a private sandbox.
On a 14-package workspace with 20 evidence views, a cold Unify check used about 5.5 times less CPU with the same decisions.
The `--diagnostics-file` schema is now version 17: it removes `configured_work_permits`, `max_nonwaiting_cargo_views`,
and the `work_permit_*` counters, and adds `dependency_compilations`, `repeated_dependency_compilations`,
and `artifact_high_water_bytes` to `compiler_acquisition`.
A repeated compilation now means the same unit configuration compiled again; a dependency rebuilt with other features
is a different unit.
`surface prepare` output is contract version 3: its `acquisition` object removes `work_permits`.
