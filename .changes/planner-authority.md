---
"cargo-rail" = "major"
---

Made the serialized planner the complete Cargo package-scope authority. Planner contract v6 and scope contract v4 use
one declared dependency universe across optional features, target predicates, and dependency kinds; every surface now
contains final Cargo arguments, and `run` no longer repairs package selection from private semantic state.
