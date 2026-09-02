---
"cargo-rail" = "minor"
---

Allow `cargo rail release run --wait` to keep one durable release invocation attached until exact-SHA checks settle,
and bind release-PR finalization to the exact merge that introduced its prepared transaction.
