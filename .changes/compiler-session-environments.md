---
cargo-rail = "patch"
---

Keep one compiler session for each compiler environment in a build, up to eight.
Cargo runs build-script probes with other library search paths than its ordinary units,
so a build no longer recaptures the session, with two `rustc` queries, each time it switches between them.
