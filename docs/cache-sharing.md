# Share native compiler results across CI and SSH

Cargo-Rail removes compiler work at two boundaries. The planner first removes actions that a change cannot affect. The
native cache then restores eligible rustc results inside the actions that still need to run. Local reuse requires no
configuration. An optional S3 L2 lets CI runners and SSH build hosts exchange the same verified results.

This is compiler-result reuse, not a copied `target/` directory. Cargo still owns fingerprints, build scripts still
run, and linker-producing invocations still execute. Every imported result must pass the same local proof as an L1 hit
before Cargo-Rail restores it.

## The sharing contract

L2 is deliberately strict. Two machines can share an action only when all action inputs match, including:

- the canonical physical source root;
- Cargo, rustc, rustdoc, sysroot, host, target, flags, and wrapper identity;
- bounded source and dependency contents; and
- every selected compiler-visible environment name and value.

Use the same canonical checkout path on compatible CI and SSH hosts. A symlink does not make different physical roots
equivalent. Different operating systems, architectures, toolchains, source roots, flags, features, or selected
environments produce different actions and compile normally.

One practical topology is:

| Machine | Canonical checkout | L1 | L2 authority |
|---|---|---|---|
| Developer laptop | Developer-owned path | Default user-wide CAS | Optional |
| Linux CI runner | `/srv/rust/acme` | Runner-local CAS | `read_write` |
| Linux SSH builder | `/srv/rust/acme` | Host-local CAS | `read` or `read_write` |

The CI and SSH rows can exchange results when their remaining action inputs match. The laptop still gets automatic L1
reuse, but its different physical root correctly prevents it from importing path-bearing CI artifacts.

## Use the local cache

Run an ordinary build action. Cargo-Rail enables native reuse automatically when it can prove the invocation is in the
graduated class:

```bash
cargo rail doctor native-cache --format json
cargo rail run --all --action build --explain

cargo clean

cargo rail run --all --action build --explain
cargo rail cache status --scope local --format json
```

The second run starts with an empty Cargo target directory but can restore eligible compiler results from L1. A bypass
is not a cache failure: it means Cargo-Rail could not prove that unit safe to reuse and ran the original compiler.

The local CAS defaults to `cargo-rail/local-cas-v2` below the effective Cargo home. Set `CARGO_RAIL_CACHE_DIR` to move
it or `CARGO_RAIL_CACHE_MAX_BYTES` to change its positive result-byte limit. Treat L1 as one machine-owned authority;
share verified results through L2 instead of copying that directory between machines.

## Configure one shared L2

Select a machine target by alias in the tracked Cargo-Rail configuration:

```toml
[cache]
l2 = "team"
```

On each machine, set `CARGO_RAIL_CACHE_TARGETS_FILE` to an absolute JSON file outside the checkout. The file identifies
the target but contains no credentials:

```json
{
  "version": 1,
  "targets": {
    "team": {
      "protocol": "s3",
      "region": "us-east-1",
      "expected_bucket_owner": "123456789012",
      "bucket": "company-cargo-rail-cache",
      "prefix": "rust/team",
      "role": "read_write",
      "shareable_environment": ["CARGO", "CARGO_CRATE_NAME", "LANG", "PATH"]
    }
  }
}
```

The complete schema, immutable protocol marker, conditional-write requirement, and minimum S3 authority are defined in
[Caching](caching.md#shared-native-cache-l2). Provision those controls before granting a build principal access.

`shareable_environment` permits named inputs to enter L2 identity; it does not ignore their values. The list above is
an example, not a universal allowlist. Use the smallest non-secret set required by the workspace. An action with a
selected name absent from the list stays local. Normalize every listed value across the machine images, including
`PATH`, or the actions will remain distinct and compile cold.

Credentials come from the standard AWS SDK chain. Use workload identity in CI and an instance profile on the SSH
host. Do not write AWS credentials into the target map, repository, workflow arguments, or SSH command. The two
machines may use different target maps for the same alias: for example, CI can use `read_write` while a production SSH
host uses `read`.

Validate configuration and credentials before the first build:

```bash
export CARGO_RAIL_CACHE_TARGETS_FILE=/etc/cargo-rail/cache-targets.json

cargo rail cache status --scope local --format json
cargo rail doctor native-cache --format json
```

`cache status` validates the selected target without resolving credentials. `doctor native-cache` resolves credentials
and validates the remote protocol marker.

## Run it in CI

Give every compatible runner the same canonical checkout path and Rust toolchain. The checkout mechanism is
CI-specific; assert the physical root before spending build time:

```bash
set -eu

expected_root=/srv/rust/acme
actual_root=$(pwd -P)
test "$actual_root" = "$expected_root" || {
  echo "expected canonical checkout $expected_root, got $actual_root" >&2
  exit 2
}

export CARGO_INCREMENTAL=0
export CARGO_RAIL_CACHE_TARGETS_FILE=/etc/cargo-rail/cache-targets.json

cargo rail doctor native-cache --format json
cargo rail run --merge-base --profile ci --explain
```

For GitHub-hosted or self-hosted Actions, record `${GITHUB_WORKSPACE}` after resolving it physically and provision the
SSH checkout at that exact path. If a runner manager cannot guarantee one canonical path, keep its actions separate;
Cargo-Rail will use L1 locally and compile a distinct L2 variant instead of making different roots appear compatible.

The repository's [cache qualification workflow][cache-qualification] is a complete GitHub Actions example of workload
identity, a machine-owned target map, publication, empty-L1 import, and unavailable-L2 fallback. Keep third-party
actions pinned when adapting it.

## Run it on an SSH build host

Provision the same repository at the CI root, install the same Rust toolchain and Cargo-Rail release, and attach an AWS
instance profile scoped to the selected cache prefix. Then invoke the build through SSH:

```bash
ssh rust-builder -- 'set -eu
  cd /srv/rust/acme
  test "$(pwd -P)" = /srv/rust/acme
  export CARGO_INCREMENTAL=0
  export CARGO_RAIL_CACHE_TARGETS_FILE=/etc/cargo-rail/cache-targets.json
  cargo rail doctor native-cache --format json
  cargo rail run --all --action build --explain'
```

Run the same commit when demonstrating a transfer. A different commit may still reuse unchanged dependency actions,
but it is not a clean end-to-end proof of CI-to-SSH sharing.

## Prove CI-to-SSH sharing

Use this sequence after the target and principals are provisioned:

1. Select a new empty `CARGO_RAIL_CACHE_DIR` on each machine. This proves the transfer without deleting either
   machine's normal L1.
2. On CI, run the build with a `read_write` target. The command admits verified local results before publishing
   immutable result packs and action associations.
3. On the SSH host, use the same commit, canonical source root, toolchain, flags, and selected environment. Start with
   an empty host-local L1 and run the same action.
4. Inspect `--explain`. The remote summary records authenticated requests and verified result-pack payload bytes; the
   unit evidence distinguishes hits, misses, and deliberate bypasses.
5. Run the command again on the SSH host with the same proof directory. Accepted L1 hits are network-free.

Give the SSH target `read_write` when results should also flow back to later CI runners. Use `read` when that host may
consume shared results but must not publish. A remote miss or unavailable service compiles cold. An integrity conflict
or malformed object restores nothing and requires operator investigation; build clients should not have list or delete
authority.

The first empty-L1 import includes S3 latency and payload transfer. Measure its remote request and payload counters on
the intended hosts. Do not present local warm-L1 timing as remote first-import timing.

## Know what remains cold

The native cache currently reuses graduated dependency and workspace-library compiler results. It deliberately bypasses
linker-producing binaries, tests, examples, and benchmarks; build scripts and generated output; proc-macro linking;
native dependencies and `links` contracts; rustdoc; cross compilation; and unmodeled configuration or environments.

That boundary is the point. Cargo-Rail claims a hit only when it can reconstruct and verify the exact result. Everything
else stays on Cargo's normal path with a stable explanation.

[cache-qualification]: https://github.com/loadingalias/cargo-rail/blob/main/.github/workflows/cache-qualification.yaml
