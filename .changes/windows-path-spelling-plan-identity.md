---
"cargo-rail" = "patch"
---

Resolve PATH programs to one spelling per file, so a saved plan verifies from every launch.
On Windows, a PATH entry with a doubled separator or an uppercase `PATHEXT` extension changed the
Cargo configuration identity. `cargo rail plan --verify` then rejected a plan that the Action had
created and verified, whenever a workflow shell launched Cargo-Rail directly.
