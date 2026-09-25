---
"cargo-rail" = "minor"
---

Add `cargo rail config migrate` to reduce a configuration file to intentional policy.
It removes a setting only when removing it leaves the effective policy unchanged,
which covers explicit defaults, saved `config print` output, and older spellings such as `unify.compiler_targets = []`.
Comments, ordering, and every other setting are kept.
A file containing only defaults is deleted when no other configuration file would take its place.
The command previews by default; `--check` exits 1 when a migration is pending,
and `config migrate apply` revalidates drift before writing.
`apply --plan` accepts the saved JSON preview.
