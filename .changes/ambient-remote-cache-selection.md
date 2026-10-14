---
cargo-rail = "patch"
---

Select remote cache storage for compilation only through `cache setup`.
Cargo gives repository `[env]` values and build-script `rustc-env` output to the compiler wrapper.
Before this change, a repository could use these values to set `CARGO_RAIL_CACHE_REMOTE` or `CARGO_RAIL_CACHE_MODE`.
It could then select a different bucket or change a read-only setup to read-write.
The compiler wrapper and `cache status` now ignore these variables.
They use the remote that setup saved.
Cargo does not set the environment of a Cargo-Rail command that you start directly.
For this reason, `cache probe` and the compiler analysis of `unify` and `surface` still read these variables.
