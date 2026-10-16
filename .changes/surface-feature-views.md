---
cargo-rail = "patch"
---

Compile one fewer view for a package that declares features but no `default` feature.
For that package, `--no-default-features` compiles the same configuration as the default selection.
Surface and the compiler evidence of `unify` compiled that configuration twice.
