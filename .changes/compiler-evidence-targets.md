---
"cargo-rail" = "minor"
---

`unify.compiler_targets` accepts `"all"` (the default), an exact target list, or `"none"`.
`"none"` keeps every resolution target but acquires no compiler evidence,
so a host that cannot compile a foreign target can still run Unify without deleting that target.
An empty list, which earlier releases printed as the default, still selects every target,
and `config print` now writes `"all"`.
Unify checks each target that needs new evidence before acquisition: an uninstalled target library,
or a doctest target whose linker cannot link a probe, stops with the target, the cause,
and the recovery choices.
`unify doctor` reports per-target readiness and the targets left unobserved.
