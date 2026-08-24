---
"cargo-rail" = "patch"
---

Preserve every benchmark compiler-coverage event when parallel rustc wrappers select the same initial event filename
instead of failing the compilation with an `EEXIST` error.
