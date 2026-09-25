---
"cargo-rail" = "patch"
---

Cold compilations on macOS start rustc sooner.
Each cache miss used to stage a fresh copy of the native input driver, probe it,
and rehash the compiler library before running the compiler;
macOS also scanned every new driver copy on its first launch.
The driver is now staged once per store and re-authenticated on every use,
one successful probe is recorded under the exact driver and compiler-library digests,
and the library digest is memoized by its stable file generation.
Across cross-target Clippy, the wrapper's work
before rustc fell from a median of 1,069 ms to 137 ms per miss.
