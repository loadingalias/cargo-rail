---
"cargo-rail" = "minor"
---

`cargo rail plan --cases FILE` compares reviewed path cases with planner decisions before a CI migration.
Each case is planned as an unreferenced commit on top of `HEAD`, so the worktree, index, refs,
and untracked files neither change nor affect the result.
The report marks each required item as direct or conservatively expanded and names the reasons.
A false negative names the missing work ID and exits `1`.
