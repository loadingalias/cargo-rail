---
"cargo-rail" = "minor"
---

Surface fact reuse now follows the same input proof as Unify evidence.
A stored fact set records the files and environment variables rustc read for its units
and the declared rerun inputs of every build script in its view, and is reused only while they hold.
This fixes stale Surface reports after a change to a file included from outside the package,
and lets Surface reuse facts in workspaces with build scripts and proc macros.
Non-empty fact sets stored by earlier versions are not reused.
Unify and Surface also reuse evidence in a workspace below its repository root;
rustc invocations there were recorded against the wrong root and never matched their Cargo units.
