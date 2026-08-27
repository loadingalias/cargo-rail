---
"cargo-rail" = "major"
---

Replaced planner surfaces with the v8 evidence-backed named-work contract, exact Cargo and CI selectors, sparse source capture, and one strict local/CI consumer. Cargo-scoped repository work now inherits exact selectors from subscribed Cargo decisions. Removed the retired classification policy, duplicate affected-work APIs, and planning-only hash, diff-hash, and graph commands.
Source-checkout consumers now build Cargo-Rail before invoking the binary directly, so plan creation and saved-plan verification observe the same Cargo environment.
Saved-plan consumers validate one canonical decision and bind it to the exact source checkout without recomputing the planner's Cargo, toolchain, target, or platform identities. Equivalent Cargo home locations no longer change planning identity by path alone.
