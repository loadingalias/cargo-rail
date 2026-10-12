---
cargo-rail = "patch"
---

Reuse a cached action when another compiler wrapper replaces the shared compiler session
while this one reads it.
The read now retries under the session lock instead of bypassing with `native_cache_session_unavailable`.
Add the error behind a session, installation, or local cache bypass to the `cargo rail cache report` details.
