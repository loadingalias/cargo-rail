---
"cargo-rail" = "minor"
---

Make root-portable native reuse exact for compiler-selected repository files, retain bounded failure telemetry and
restore synchronization state, and accept rustc's boolean `linker-plugin-lto` spellings. The remote native-object
contract advances to `native-v6`; old `native-v5` objects remain cleanly unreachable.
