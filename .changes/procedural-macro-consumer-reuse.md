---
"cargo-rail" = "minor"
---

Crates that depend directly on a procedural macro, such as `async-trait` or `paste`,
are now stored and reused like crates that use a macro through a re-export.
On every miss, the compiler driver observes what each loaded macro reads through the C library and,
on Linux, through the kernel.
A result then binds every file, directory listing, absent path, and variable the macro read,
so a hit stays valid even when the macro reads an input that no build script declares,
which Cargo's own freshness misses.
A macro that writes files, opens network connections, starts an unmodeled process,
reads outside the repository, or cannot be observed keeps compiling normally
and names the reason (`procedural_macro_*`).
Linux kernels older than 5.5, containers that block seccomp user notification,
and hosts other than Linux and macOS keep macro consumers on the normal compiler path.
This needs native input protocol 4,
so independent compiler adapter packs from earlier releases are rejected until they are republished,
and results of units that load a procedural macro start cold once.
Cache hits are also faster: captured source trees
and dependency directories each keep one digest record, native search directories reuse it,
and Rust library selection resolves each directory once.
A workspace-member Clippy hit in this repository fell from a median of 330 ms to 130–185 ms.
