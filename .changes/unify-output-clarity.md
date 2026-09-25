---
"cargo-rail" = "minor"
---

`cargo rail unify` names every pending edit class on one screen: dependencies, inherited package fields,
pruned features, and the `rust-version` it writes.
Declarations that Unify keeps on purpose are no longer reported as warnings; `--explain` groups them by reason,
and JSON reports them with kind `Preserved` and severity `Info`.
A build script that cannot find a native tool through the `cc`, `cmake`, or `pkg-config` crates,
or source that reads a missing file, now names that tool or file and the recovery.
Identical compiler errors from one view appear once.
After `unify apply`, the next step no longer names this repository's own test profile.
