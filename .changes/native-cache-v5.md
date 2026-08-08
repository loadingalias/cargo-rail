---
"cargo-rail" = "major"
---

Added verified native compiler-result reuse that survives `cargo clean` and can exchange exact results through an
optional S3 L2 across compatible CI runners and managed SSH build hosts. Every L1 or L2 hit revalidates complete source,
environment, toolchain, dependency, action, and stored-byte evidence before restore. Invocations preserve direct Cargo's
rustc arguments, output bytes, and file modes. Incomplete evidence runs Cargo normally. Moved checkouts, external target
directories, and incompatible hosts compile cold instead of restoring path-bearing artifacts. Durable conflict and
restore state fail closed. Windows authority uses handle-bound NTFS identity and write-through publication, and evidence
follows alternate spellings of the current Cargo output directory without a separately published helper package.
