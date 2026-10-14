---
cargo-rail = "patch"
---

Reuse crates that load a procedural macro that links an asynchronous runtime but never starts it.
Classify the event-queue, fork-handler, socket, and descriptor functions that the standard library,
tokio, and mio import.
These functions include glibc's `__register_atfork` and macOS `kqueue` and `kevent`.
A function with an effect records that effect only when the macro calls the function.
Treat every socket operation and every change of user, group, or session as an effect.
Require that the import table and the Linux system-call table classify each operation in the same
way.
