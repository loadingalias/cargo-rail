---
"cargo-rail" = "patch"
---

`cargo rail plan --verify` now rejects a saved plan whose `inputs.platform` differs from the current host.
Before, a plan's platform label was checked only through its target identity,
so a plan re-signed with another platform label still verified.
