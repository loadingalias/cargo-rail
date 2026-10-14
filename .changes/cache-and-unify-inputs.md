---
cargo-rail = "patch"
---

Read build-script rerun declarations as Cargo reads them: trim each output line first.
Before, Cargo-Rail ignored an indented `cargo:rerun-if-changed=PATH` line.
Compiler evidence could then stay reusable after that file changed.

Unify no longer treats `cfg(target_feature = "...")` or `cfg(panic = "...")` code as unreachable when bare `rustc --print cfg` omits the value.
Rustflags and the profile can enable both values.

Separate remote-cache coordinators by Azure credential selection, such as `AZURE_CLIENT_ID` or `AZURE_FEDERATED_TOKEN_FILE`.
AWS credentials already separated coordinators in the same way.

Planning-evidence identities must use lowercase hexadecimal digits, as the `planning-evidence-v2` schema requires.
