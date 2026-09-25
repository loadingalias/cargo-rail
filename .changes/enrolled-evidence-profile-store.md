---
"cargo-rail" = "patch"
---

Unify and Surface keep compiler evidence
and the sysroot fingerprint memo in the enrolled workspace's cache profile instead of rewriting the
unbound default store on every run.
An explicit `CARGO_RAIL_CACHE_DIR` still selects its own store,
a workspace without a profile still uses the default store,
and a command that cannot read its enrollment rehashes instead of writing either store.
