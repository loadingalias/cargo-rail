---
cargo-rail = "patch"
---

Reuse crates that load a procedural macro which links an asynchronous runtime but never starts it.
Classify the event-queue, fork-handler, socket, and descriptor functions that the standard library,
tokio, and mio import, including glibc's `__register_atfork` and macOS `kqueue` and `kevent`.
A function with an effect records it only when the macro calls it.
Treat every socket operation and every change of user, group, or session as an effect,
and enforce that the import and Linux system-call tables classify each operation alike.
