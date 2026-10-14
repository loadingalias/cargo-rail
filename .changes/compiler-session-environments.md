---
cargo-rail = "patch"
---

Keep one compiler session for each compiler environment in a build, up to eight sessions.
Cargo runs build-script probes with different library search paths from its ordinary units.
A build no longer recaptures the session each time it switches between these environments.
Each recapture ran two `rustc` queries.
