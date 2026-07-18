---
"cargo-rail" = "minor"
---

Preserve every Cargo package as an exact `PackageId`-keyed graph node and build dependency edges from Cargo's resolved graph, retaining distinct versions, renamed dependencies, dependency kinds, and target conditions while keeping inactive declarations out of resolved topology and confining package-name lookup to ambiguity-aware workspace selection.
