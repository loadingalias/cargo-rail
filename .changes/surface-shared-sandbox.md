---
"cargo-rail" = "patch"
---

Compile each unit once when Surface acquires typed compiler facts.
Typed views, including doctest views, now share one sandbox and a stable workspace wrapper.
Before each view, Cargo-Rail removes only that view's package from the sandbox,
so Cargo recompiles the targets whose facts the view needs and keeps every other unit fresh.
Before, each view used a new wrapper path, which Cargo hashes into `-C metadata`,
so every view recompiled every workspace member in its graph.
On the 14-package D4 workspace,
a cold `surface` run fell from 287 to 148 compiler invocations and from 124 s to 84 s of CPU,
with an identical report.
