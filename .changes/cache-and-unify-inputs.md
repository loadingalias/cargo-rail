---
cargo-rail = "patch"
---

Read build-script rerun declarations the way Cargo does, after trimming each output line.
An indented `cargo:rerun-if-changed=PATH` line was ignored, so compiler evidence could stay reusable after that file changed.

Unify no longer treats `cfg(target_feature = "...")` or `cfg(panic = "...")` code as unreachable when bare `rustc --print cfg` omits the value;
rustflags and the profile can enable both.

Separate remote-cache coordinators by Azure credential selection, such as `AZURE_CLIENT_ID` or `AZURE_FEDERATED_TOKEN_FILE`,
as they already were for AWS credentials.

Planning-evidence identities must use lowercase hex digits, as the `planning-evidence-v2` schema requires.
