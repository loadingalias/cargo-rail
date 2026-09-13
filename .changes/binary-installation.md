---
"cargo-rail" = "minor"
---

Fix prebuilt installation through cargo-binstall by making the source-only benchmark executable opt-in with the
`bench` feature. Release packaging now rejects archives that omit mandatory Cargo binaries.
To install the benchmark from source, add `--features bench`.
