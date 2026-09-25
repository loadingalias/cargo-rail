---
"cargo-rail" = "patch"
---

Run build scripts during Unify and Surface compiler evidence with the environment plain Cargo gives them.
Cargo-Rail's private session variables, such as `CARGO_RAIL_COMPILER_OBSERVATION_DIRECTORY`, no longer reach build scripts;
each view's wrappers read a private context file instead.
Unify views also keep a stable workspace-wrapper path, which Cargo hashes into each unit's `-C metadata`,
so compiled units and native cache results stay reusable across runs.
