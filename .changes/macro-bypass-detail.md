---
cargo-rail = "patch"
---

Name every condition that stops a procedural macro unit in the cache report.
Before this change, the example named only the first condition.
For example, a macro that starts `cargo metadata` and reads `.env` files outside the repository showed only the process.
The example now names each effect, secret-named variable, process, and read.
The unit still has one reason, so the reason counts still add up to the bypassed units.
A process appears as its program and subcommand, such as `cargo metadata`, without its other arguments.
