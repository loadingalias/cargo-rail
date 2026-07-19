---
"cargo-rail" = "major"
---

Replace hard-coded run surfaces with a bounded, snapshot-bound action graph. Built-in and repository actions now share
one shell-free expansion and stable topological order across local execution, JSON/GitHub CI plans, and version-2
decision receipts. Repository generators declare exclusive outputs plus separate check/regenerate commands; paths,
dependencies, tokens, environment capabilities, cycles, and portable ownership collisions fail closed before
execution. Ownership validation remains fast at the configured action/path limits.
