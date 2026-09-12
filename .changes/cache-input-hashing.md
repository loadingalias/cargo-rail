---
"cargo-rail" = "patch"
---

Reduce repeated hashing during cold compiler-cache capture.
Concurrent sysroot memo misses share one capture when locking is available,
and linker aliases reuse a digest only while captured file generation evidence still matches.
Preserve every path witness, mutation check, and warm-hit validation;
fall back to normal capture when evidence or locking is unavailable.
