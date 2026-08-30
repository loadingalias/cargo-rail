# Cargo-Rail dogfood qualification

**Status:** Hosted R2 qualification complete; development-machine cross-trust qualification open

**Reviewed:** 2026-08-29

**Cargo-Rail:** `271fb81e5530d81763d8825bb12319f16e06673c`

**Companion Action:** `78ad385a85627484a5b634cf1bc3caa1de872c6a`

Cargo-Rail v0.25.0's rscrypto release acceptance is complete. The hosted producer/read-only-consumer R2 contract is
also complete. Do not describe the shared cache as production-qualified across CI and developer trust domains until
GitHub Actions and an ephemeral development machine restore one another's eligible entries.

## Completed product contracts

- Surface report v3 distinguishes raw compiler observations from merged declarations, gives bounded examples, and
  measures omit-one-reason counterfactuals only under `--explain`.
- Compiler fact protocol v4 uses rustc's complete definition-path identity and rejects incompatible older facts.
- The companion cache Action exposes typed root portability, an authenticated strict probe, and redacted readiness
  outputs through one setup transaction.
- Cloudflare R2 uses a canonical default-jurisdiction authority, private credentials, and the `native-v6` object
  protocol. Read mode cannot initialize or publish objects.
- Native root-portable identity covers compiler-selected repository files and external `CARGO_TARGET_DIR` locations.
  Selected inputs are revalidated by path, type, size, digest, and executable mode before lookup.
- The external machine adapter selects the same normalized authority, uses separate credentials, and destroys its
  credentials, instances, and block volumes with the machine lease.

## Provider and platform evidence

An ephemeral AWS AArch64 producer and clean read-only consumer proved authenticated R2 traffic across independent
fixture roots. A same-size mutation of a selected repository input produced a miss. The run deleted its disposable
objects, EBS volumes, and instances. No Azure or Windows host was started to duplicate already-covered provider or
platform evidence in that producer/consumer run.

The hosted repair used one automatic R2 workflow per pushed candidate. It did not start EC2, EBS, Azure, or another
Windows host. Every failed candidate's unconditional R2 cleanup job passed.

## Hosted failures and resolution

The hosted failures were qualification defects or correct fail-open misses. None showed that R2 authentication,
transport, read-only enforcement, or exact Cargo-Rail result validation had accepted a wrong result.

| Run | Observed failure | Resolution |
|---|---|---|
| `33281444939` | The parser compared the complete semicolon-delimited reason with `verified_remote_result`. | Classify exact reason tokens and retain failed consumer evidence. |
| `33285276472` | The consumer inherited the producer's remote selector, and fixture Git source identity differed across roots. | Isolate phase authority and use one stable fixture Git URL. |
| `33287777821` | The broad fixture imported no actions because generated and external dependency artifacts contained root-bound bytes. | Preserve the safe misses; qualify exact supported action keys instead of claiming every publication is portable. |
| `33289842723` | `check` and `build` imported the dependency-free control, but `test` compiled it only as an intentionally ineligible test executable. | Add a tiny dependent fixture so `cargo test` also compiles the control as a supported Rust library. |
| [`33290652950`](https://github.com/loadingalias/cargo-rail/actions/runs/33290652950) | No failure. | The producer, fresh-root read-only consumer, strict pair validator, and cleanup job passed. |

The final pair report has schema version 2 and records distinct fixture roots, complete compiler-class coverage, exact
producer keys for every imported action, and identical consumer outputs after offline L1 replay:

| Workload | Producer publications | Fresh-root remote hits | Safe read-only misses | Offline L1 hits |
|---|---:|---:|---:|---:|
| `check` | 8 | 2 | 6 | 8 |
| `build` | 5 | 2 | 3 | 5 |
| `test` | 4 | 1 | 3 | 4 |

The read-only consumer wrote zero remote bytes and reported no remote errors in every workload. Offline L1 replay
made zero remote requests and read and wrote zero remote bytes. The final cleanup job deleted the disposable R2
prefix in 10 seconds.

The producer and consumer broad output manifests may differ because unsupported generated and external dependency
closures compile normally in each root. The contract requires byte-identical consumer and offline-replay outputs and
binds each actual import to an exact published producer key; it does not weaken cache keys to turn safe misses into
hits.

## Remaining acceptance

- Prove one GitHub Actions entry restores on an ephemeral development machine and one remote-machine entry restores
  in GitHub Actions under the same normalized authority.
- Retain redacted authority, cache outcome, byte-identity, failure-fallback, and cleanup evidence for both trust
  domains.

This remaining cross-trust exercise gates the shared-cache production-qualified label. It does not reopen the
completed rscrypto acceptance for Cargo-Rail v0.25.0.

Provider outage, absence, corruption, and rejected results must continue to execute the normal compiler path with a
stable observable reason. Qualification cannot trade that fail-open behavior for a higher hit rate.
