---
"cargo-rail" = "patch"
---

Capture linker inputs under explicit file,
path-byte and content-byte limits instead of the source-discovery deadline.
Slow native hosts can complete linker evidence capture while retaining content hashing,
mutation checks and restore validation.
Source discovery keeps its existing time limit.
