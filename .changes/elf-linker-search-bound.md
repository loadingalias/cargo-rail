---
cargo-rail = "patch"
---

Avoid unnecessary ELF linker search probes for directly named archives and object files.
Linker scripts and binary metadata retain candidates for actual or uncertain searches,
and every selected file keeps its content binding.
Separate limits for found files
and absent search candidates prevent their combined count from causing unnecessary `elf_linker_witness_unavailable` bypasses on
large workspaces.
