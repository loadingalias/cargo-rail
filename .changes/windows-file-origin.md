---
cargo-rail = "patch"
---

On Windows, accept a `file:///C:/...` origin for releases.
Cargo-Rail read the URL path as `/C:/...`, which is not an absolute Windows path,
so it refused the origin as relative.
