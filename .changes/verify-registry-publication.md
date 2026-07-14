---
"cargo-rail" = "patch"
---
Fixed crates.io publication checks so local workspace packages cannot masquerade as published versions. Release publishing now targets crates.io explicitly, requires the committed lockfile, rejects dirty package contents, and excludes Finder metadata.
