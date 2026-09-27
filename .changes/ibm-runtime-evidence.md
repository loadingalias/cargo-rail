---
"cargo-rail" = "patch"
---

Runtime evidence recognizes the `linux-vdso64.so.1` image that IBM Z and 64-bit IBM POWER kernels map,
so native reuse no longer rejects every executable probe on those hosts.
