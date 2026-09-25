---
"cargo-rail" = "minor"
---

Unify and Surface compiler evidence now binds each view to the locked packages its package can
reach, instead of the whole `Cargo.lock`.
Updating a dependency that only one member uses reacquires that member's views and reuses the rest.
A lockfile whose references Cargo-Rail cannot resolve exactly, or a v1 lockfile,
still binds the whole file.
Stored evidence starts cold once after upgrading, because its keys changed.
