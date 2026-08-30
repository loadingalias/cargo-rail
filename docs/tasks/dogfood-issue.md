# Cargo-Rail dogfood qualification

**Status:** Hosted remote consumer and cross-trust qualification open

**Reviewed:** 2026-08-29

**Cargo-Rail:** `e993b29ac7ce10a748c7daae227bd786c986ef77`

**Companion Action:** `69eadc85d8bd461c42eb16367d7ada2d2f26b7b9`

Do not describe the shared Cloudflare R2 cache as production-qualified until the hosted read-only consumer imports
the producer's verified result and both GitHub Actions and an ephemeral development machine restore one another's
eligible entries.

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

## Evidence retained on 2026-08-29

An ephemeral AWS AArch64 producer and clean read-only consumer proved authenticated R2 traffic across independent
fixture roots. A same-size mutation of a selected repository input produced a miss. The run deleted its disposable
objects, EBS volumes, and instances. No Azure or Windows host was started to duplicate already-covered provider or
platform evidence in that producer/consumer run.

Hosted run `33281444939` did not complete the same contract:

1. The producer's authenticated probe and verified-result publication passed.
2. The clean consumer's read-only authenticated probe passed.
3. The consumer reached `cargo check`, but the qualification parser could not recognize a root-portable hit because
   it compared the complete reason string with `verified_remote_result` instead of classifying its
   semicolon-delimited reason tokens.
4. The unconditional R2 cleanup job passed.

This is a fail-closed harness defect, not evidence of an authentication or provider failure. The failed job did not
upload its consumer events, so the run still cannot prove that every expected result imported. The corrected workflow
retains failed consumer evidence and must pass once before the hosted contract is complete.

## Remaining acceptance

- Rerun the exact hosted producer/read-only-consumer pair with token-aware reason classification to a passing evidence
  report.
- Prove one GitHub Actions entry restores on an ephemeral development machine and one remote-machine entry restores
  in GitHub Actions under the same normalized authority.
- Retain redacted authority, cache outcome, byte-identity, failure-fallback, and cleanup evidence for both trust
  domains.

Provider outage, absence, corruption, and rejected results must continue to execute the normal compiler path with a
stable observable reason. Qualification cannot trade that fail-open behavior for a higher hit rate.
