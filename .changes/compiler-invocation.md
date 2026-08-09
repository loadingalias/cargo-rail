---
"cargo-rail" = "major"
---

Consolidated compiler cache, rustc observation, and rustdoc proxy execution behind one exact pre-Clap invocation
boundary. Ambiguous roles now fail before workspace acquisition, analysis facts require a private run capability, and
disabled or clearly unsupported compiler shapes execute the original chain before session or CAS loading. Shared CAS
and output-manifest ownership moved out of the whole-action runner boundary.
