---
"cargo-rail" = "patch"
---

On hosts whose kernel stamps file changes with a coarse clock, such as IBM Z,
a capture waits until a just-changed input is settled,
and the digest memo measures age from the change time, which no user can set back.
A same-size rewrite within one clock tick can no longer reuse a stale digest
or hide from revalidation.
The same wait applies to the directories that bind a followed root link
and to the native compiler driver's runtime layout,
so a link replaced within one tick is rejected.
