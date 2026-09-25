---
"cargo-rail" = "minor"
---

Compiler results for registry crates that read a package file outside their source directory,
such as `#![doc = include_str!("../README.md")]`, are now stored and reused instead of bypassing,
because the whole unpacked package is part of their key.
Registry dependency results start cold once.
`cache status` reports `recent_evictions` and a budget-pressure line when collection evicts results within a day of their last use,
which means the budget did not hold the working set in use.
