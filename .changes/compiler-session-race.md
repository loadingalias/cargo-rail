---
cargo-rail = "patch"
---

Reuse a cached action when another compiler wrapper replaces the shared compiler session
while this wrapper reads it.
The read now tries again under the session lock.
Before, it bypassed with `native_cache_session_unavailable`.
Add the error that causes a session, installation, or local cache bypass to the `cargo rail cache report` details.
