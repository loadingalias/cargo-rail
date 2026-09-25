---
"cargo-rail" = "minor"
---

Make the remote cache providers and the cache benchmark optional Cargo features.
`cargo install cargo-rail` no longer builds the AWS and Azure SDKs: 88 of 327 dependency packages existed only for them.
On Apple silicon a clean release build of `cargo-rail` used 32% less CPU,
and the binary shrank from 45 MB to 31 MB.
Native release archives still include both providers.
Add `--features s3` for S3 and R2 or `--features azure` for Azure Blob to a source build that needs a remote cache;
a build without the provider rejects its URL during setup and names the missing feature.
The `benchmark` library module and its embedded workload now require the `bench` feature,
as the benchmark binary already did.
