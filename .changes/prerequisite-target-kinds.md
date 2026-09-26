---
"cargo-rail" = "major"
---

A Cargo prerequisite edge can require every workspace target of a kind:
`require_target_kinds = ["cdylib"]` builds each plugin library that a test loads at run time,
so a new plugin needs no configuration edit.
A kind that selects no workspace target is rejected.
The library's `CargoPrerequisiteConfig` adds the public `require_target_kinds` field, which breaks struct literals.
