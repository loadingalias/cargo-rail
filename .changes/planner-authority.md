---
"cargo-rail" = "major"
---

Made the serialized planner the complete Cargo package-scope authority. Planner contract v7 and scope contract v4 use
one declared dependency universe across optional features, target predicates, and dependency kinds. Every
package-scoped surface now contains its final Cargo argument array for direct use by Cargo, cargo-nextest, Just, and
CI. GitHub output also includes a deterministic `surfaces_json` projection for bounded job routing.
