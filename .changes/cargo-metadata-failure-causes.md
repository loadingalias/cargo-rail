---
"cargo-rail" = "minor"
---

Report every Cargo metadata failure with one named cause, one recovery,
and the exact reproduction command.
Named causes are a stale `Cargo.lock`, an unloadable manifest, an unavailable target or `rustc`,
and a missing Cargo executable.
Cargo's output stays withheld when a credential capability is active, except for manifest failures,
which Cargo reports before it contacts a registry.
`config validate` now resolves an existing lockfile with `--locked`, so a stale lockfile fails validation.
It reports whether it checked the Cargo workspace or only the schema,
in text and in the JSON `evidence` field, and adds `help` to each issue.
