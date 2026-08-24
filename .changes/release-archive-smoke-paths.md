---
"cargo-rail" = "patch"
---

Resolve release-archive executables from an absolute extraction root so smoke tests remain valid after changing their
working directory, and preserve the failing diagnostic when an archive violates its Surface capability contract.
