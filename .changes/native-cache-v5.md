---
"cargo-rail" = "major"
---

Rebuilt native compiler reuse around complete source, environment, and physical-root identity, exact direct-Cargo
output bytes and modes, durable conflict and restore state, and optional bounded L2 reuse for controlled CI and managed
machines. Same-root clean-target, L1, and L2 reuse preserves rustc arguments and artifacts; moved checkouts safely
compile cold instead of restoring path-bearing metadata. Windows cache authority uses handle-bound NTFS identity and
write-through publication without a separately published helper package.
