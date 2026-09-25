---
"cargo-rail" = "patch"
---

Keep Unify and Surface compiler evidence reusable in workspaces with Git dependencies.
A Git source pinned to its exact resolved commit now identifies its content
as a registry checksum does.
Before, one Git dependency made every view in the workspace miss with `external_source_digest_unavailable`.
Other external sources without a checksum still bypass reuse.
