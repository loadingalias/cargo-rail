---
title: "Cargo Already Knows Your Workspace. Stop Rebuilding It."
description: "Cargo-Rail 0.22 removes its task runner and turns Cargo’s resolved model into a shared authority for affected CI, verified compiler reuse, dependency coherence, exact-SHA releases, and crate synchronization."
date: 2026-08-21
tags:
  - rust
  - cargo
  - open-source
  - ci
  - build-systems
---

Today I’m releasing Cargo-Rail 0.22.0. Its most important feature is one I deleted:

```text
cargo rail run
```

That may sound like a strange direction for something called a workspace engine. It is the opposite.

Cargo already owns build semantics. cargo-nextest already knows how to run tests. Just already knows how to run repository recipes. CI already owns scheduling, credentials, isolation, and logs. Cargo-Rail had no business becoming another task runner between those tools and the work they already do well.

What a large Rust workspace is usually missing is not another executor. It is one authoritative answer to a harder set of questions:

- What actually changed?
- Which packages and repository surfaces can that change affect?
- Is a compiler result still valid for these exact inputs?
- Which dependency edits preserve the Cargo graph?
- What release intent was reviewed, and which exact commit is authorized to publish?
- Which files belong to a crate when it crosses a repository boundary?

Cargo-Rail 0.22 narrows around that responsibility.

It captures Cargo’s resolved model and the relevant source state once for an operation, derives decisions from that view, emits explicit plans, and leaves execution to Cargo, nextest, Just, Git, and CI.

**It is an authority layer, not an orchestration layer.**

## Rust build performance starts before rustc

Rust compile times matter. But the first performance question is not how quickly rustc handles a unit of work.

It is whether that unit of work should have reached rustc at all.

A serious Rust workspace usually grows a second toolchain around Cargo: path filters, package maps, dependency linters, feature auditors, cache wrappers, changesets, changelog generators, publish scripts, split-repository scripts, and CI glue.

Each tool addresses a real problem. The failure is architectural: every one reconstructs a different partial model of the same repository.

That means repeated metadata loading, manifest parsing, Git queries, filesystem walks, dependency analysis, and policy encoded in shell or YAML. More importantly, it means disagreement.

The tool deciding whether a test job should run may not agree with the script selecting Cargo packages. Neither may agree with the release tool’s dependency order. A cache may identify an action using less evidence than the compiler actually consumed. A split script may define crate ownership differently from Cargo.

The cost is not merely another binary in CI. It is another source of truth.

Cargo-Rail’s target is simple:

> Adopt a workflow when it deletes a second source of truth.

A small crate with fast builds does not need this entire system. The strongest fit is a multi-crate workspace where affected scope, compiler cost, dependency coherence, coordinated releases, or monorepo-to-standalone synchronization is already an operational problem.

## Why removing `run` matters

Earlier Cargo-Rail releases grew a generalized execution layer. It could describe repository actions, expand them, order them, wrap Cargo, and project the same action model into CI.

That was technically coherent. It was still the wrong boundary.

A second execution language creates permanent pressure to absorb more of Cargo, nextest, Just, shell, CI, and every repository-specific tool. It also makes adoption all-or-nothing: a team must trust the new runner before it can benefit from the workspace model beneath it.

Cargo-Rail 0.22 removes the top-level `run` command and its configuration. The planner’s versioned package arguments are now the handoff. Existing tools consume them directly.

```bash
PLAN_JSON=$(cargo rail plan --merge-base -f json)

if [ "$(jq -r '.surfaces.test.enabled' <<<"$PLAN_JSON")" = "true" ]; then
  CARGO_ARGS=()
  while IFS= read -r argument; do
    CARGO_ARGS+=("$argument")
  done < <(jq -r '.surfaces.test.scope.cargo_args[]' <<<"$PLAN_JSON")

  cargo nextest run "${CARGO_ARGS[@]}" --all-features --locked
fi
```

Cargo-Rail does not join those arguments into a shell command. It does not reinterpret nextest flags. It does not own the command’s output or exit status. It decides scope; the domain tool executes it.

That is both leaner and safer.

It also makes adoption incremental. A repository can start with one read-only plan, keep every existing command, and decide whether the result is useful before changing anything else.

## One operation, one authoritative workspace view

“One graph” is a useful slogan, but it is not quite the implementation.

Cargo-Rail is not a daemon holding an eternal global graph across every developer and CI process. For an operation that needs workspace state, it builds one `WorkspaceContext`. Depending on the command, that context captures the source tree, manifests, lockfile, Cargo configuration, toolchain identity, metadata, repository boundary, and base dependency graph.

Narrower target and feature views are derived from those inputs rather than recapturing the repository for convenience.

```text
Git history + captured source + Cargo inputs
                    │
                    ▼
             WorkspaceContext
                    │
       ┌────────────┼─────────────┬──────────────┐
       ▼            ▼             ▼              ▼
     plan         unify         release       split/sync
 affected work   graph edits   exact-SHA      crate boundary
                    │
                    ▼
           revalidate before effect
```

This does not mean every command secretly invokes every workflow. `plan` does not run `unify`. `release` does not synchronize a split repository. The workflows share infrastructure and invariants, not hidden control flow:

- one captured authority for the operation;
- deterministic, inspectable plans;
- exact authorized paths and effects;
- conservative widening when evidence is incomplete; and
- durable recovery evidence where an external effect cannot be rolled back.

That shared boundary is why these workflows belong in one project. They all fail in the same way when they make decisions from stale or partial workspace models.

## Affected CI is a graph query, not a directory match

`cargo rail plan` is the clearest expression of the model.

It compares Git state, interprets manifest and lockfile changes semantically, maps changed files to Cargo packages, propagates impact through a declared dependency universe, and emits separate decisions for build, test, benchmark, documentation, infrastructure, and configured custom surfaces.

That produces useful distinctions that path filters cannot express safely:

- A formatting-only `Cargo.toml` edit can select no package work.
- A library change can select the affected dependent closure.
- A documentation change can enable docs without manufacturing crate ownership.
- A CI configuration change can enable an infrastructure job without pretending that the job belongs to a Cargo package.
- Missing or ambiguous evidence widens the scope instead of guessing narrowly.

The output is a versioned machine contract. Each surface carries stable reason codes and an exact Cargo argument vector. Text explanations, JSON, GitHub Actions output, and the plan hash are projections of the same decision.

The optimization question changes from:

> How can this test command run faster?

To:

> Why is this test command running against this work at all?

The fastest test job is the one a conservative plan proves does not need to run.

This is already useful outside Cargo-Rail’s own repository. [Apache Iggy merged Cargo-Rail into its pre-merge CI](https://github.com/apache/iggy/pull/3095) to derive affected Rust packages from the dependency DAG and scope Cargo and nextest work. It keeps full-workspace check and Clippy coverage as an independent safety layer, and falls back to the full suite when it cannot obtain a plan. [Iggy later reused the planner to gate Docker edge-image refreshes](https://github.com/apache/iggy/commit/f139f0e5b8bde5dde5e7a507b82f26db8fbbeb2e).

[Prosody’s CI](https://github.com/prosody-events/prosody/blob/main/.github/workflows/quality.yaml) also uses the Cargo-Rail GitHub Action to produce build, test, and infrastructure decisions before its jobs run.

Those integrations matter more than a synthetic claim about “developer velocity.” They show that the contract can fit existing CI without replacing its executor.

## Compiler reuse below ordinary Cargo

Planning removes work before a job starts. Caching removes eligible compiler work from the jobs that remain.

Cargo-Rail installs a private compiler wrapper with four distinct layers:

1. Cargo freshness and incremental compilation remain L0.
2. A bounded machine-local content-addressed store provides L1.
3. An optional machine-owned S3, Azure Blob Storage, or Cloudflare R2 authority provides L2.
4. Optional distributed execution handles a deliberately bounded class of compiler-only misses.

This is not a cached `target/` directory.

The local store lives outside `target/`, so eligible results can survive `cargo clean` and move across target directories within the same physical source root. Cargo still owns fingerprints, incremental state, command semantics, output placement, and the final process status.

Before restoring a result, Cargo-Rail revalidates the compiler and sysroot, arguments, source topology and bytes, dependency artifacts, compiler-visible environment, supported native inputs, the action/result binding, and the exact stored outputs.

A lookup is not authority.

The proof boundary is intentionally incomplete. Incremental invocations, Clippy, rustdoc, cross targets, unsupported linkers, native proc-macro consumers, unmodeled compiler shapes, and other cases without complete evidence run through normal Cargo. Missing credentials, a remote outage, corrupt objects, protocol conflicts, and pre-commit distributed failures also fall back to local compilation.

A restore failure after output replacement begins fails closed rather than compiling over a partial restore.

The rule is deliberately boring:

> **Fast when proven. Normal Cargo when not.**

Setup is explicit and previewable:

```bash
cargo rail cache setup --check
cargo rail cache setup
cargo rail cache status
```

After setup, ordinary Cargo, nextest, Just, IDE, and CI processes using that Cargo home can use L1. Repository configuration is not allowed to choose a network write destination; L2 and distributed authorities are machine-owned policy. `CARGO_RAIL_CACHE=off` disables reuse for a process tree without uninstalling anything.

Remote reuse and distributed execution are separate decisions. L2 transports results that must pass the same native verification as L1. Distributed execution is a later miss path with its own capability, sandbox, transfer-cost, and placement model.

The default automatic distributed policy stays local until bounded, expiring history contains enough successful local and remote observations to predict a critical-path win conservatively.

Remote is not automatically faster.

## The benchmarks include the losses

I do not want Cargo-Rail’s performance story to depend on a friendly workload and an omitted failure case.

The retained benchmark process separates correctness from timing, interleaves compared lanes, pins the comparison tool, verifies exact output bytes, rejects ambiguous work, and does not convert a failed corpus into a speed claim.

The current retained results support these scoped statements:

| Retained corpus                                          | Result                                                                                      |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| Linux x86-64 local L1 vs pinned sccache                  | 77.55–89.25% p95 wall-time reduction while restoring more compiler actions                  |
| Empty-L1 AWS S3, `cargo check`, vs pinned remote sccache | 43.58% p95 wall-time reduction                                                              |
| Empty-L1 AWS S3, release build, vs pinned remote sccache | 73.27% p95 wall-time reduction                                                              |
| Six-crate dependency DAG on `c8i.large`                  | 10.098s p50; 29.57% lower than local Cargo and 28.84% lower than pinned distributed sccache |

Azure Blob Storage and R2 passed the retained correctness and failure qualification, but I am not claiming performance superiority for them.

The distributed results are more important for what lost. Small work, a single large unit, and a parallel-check workload were slower remotely. Automatic placement keeps those measured losing classes local.

These numbers are not a universal ranking. They apply to the retained worktree, toolchain, host, topology, cache state, provider, sample contract, and acceptance rules. They do not prove that every workspace gets the same result.

The [benchmarking documentation](https://github.com/loadingalias/cargo-rail/blob/main/docs/benchmarking.md) publishes the measurement contract. Every team should measure its own graph and feedback loop.

An optimizer that cannot say “stay local” is not an optimizer. It is a remote-execution sales pitch.

## Dependency coherence is one problem, not five linters

Rust dependency hygiene is commonly split across version alignment, workspace inheritance, feature analysis, unused-edge detection, and MSRV checks.

`cargo rail unify --check --explain` treats them as views of one Cargo graph. It can diagnose and plan repairs for:

- dependency-version drift;
- hidden feature coupling;
- unused dependency edges where the evidence is complete;
- workspace-inheritance drift;
- MSRV mismatches; and
- optional host-owned pins for fragmented transitive features.

The apply path uses lossless TOML editing, validates the resulting Cargo graph, can create bounded backups, and supports undo for the latest Cargo-Rail-owned backup.

The separate `surface` workflow analyzes Rust declaration reachability and can preview exact visibility reductions before applying them.

The value is not “one binary has many checks.” The value is that graph-removing decisions, mutation authority, explanations, and recovery do not come from unrelated approximations of the workspace.

## Release intent should survive to publication

Release automation often reconstructs intent from commit history after code has merged. That is a weak place to recover a decision that reviewers could have made explicitly.

Cargo-Rail records release intent beside the change:

```bash
cargo rail change add rail-core \
  --bump minor \
  --message "Added graph-aware release planning."
```

The resulting `.changes/*.md` file records the affected crate, bump, and release note. CI can require reviewed intent before merge.

The release workflow carries that input through versioning, changelog generation, dependency-ordered publication, and tags. Remote modes bind readiness and publication to the exact release commit rather than a moving branch head. The transaction records durable state before external effects, observes registry publication, and creates tags last.

Interrupted work is inspected and resumed explicitly:

```bash
cargo rail release status
cargo rail release resume <STATE>
```

This is recovery, not fictional rollback. A published crate cannot be unpublished by restoring a local file, and Cargo-Rail does not pretend otherwise.

## A monorepo can publish standalone repositories without losing origin

A canonical monorepo can be the right internal architecture while standalone repositories are better for external users and contributors.

`cargo rail split` extracts a selected crate’s relevant Git history and rewrites workspace-relative manifests. `cargo rail sync` maps later changes in either direction. Inbound changes land on a review branch. Synchronization uses Git’s three-way merge, and a manual conflict stops before commit with a resumable receipt.

This is intentionally not a general repository-transformation language. It is Cargo-aware crate synchronization with a narrow ownership and recovery model.

That narrowness is a feature.

## The workflows compound without becoming a platform

Cargo-Rail removes work in the order it appears:

```text
unify
  └─ removes dependency-graph waste and hidden coupling

plan
  └─ removes unaffected jobs and packages

Cargo L0
  └─ removes compiler invocations Cargo already knows are fresh

verified L1/L2 reuse
  └─ restores eligible results for the invocations that remain

measured distributed placement
  └─ delegates only qualified critical-path misses
```

The effects can compound because the workflows use compatible workspace and authority boundaries. They remain independently adoptable and workload-dependent. There is no defensible universal multiplier for engineering velocity.

The useful measurements are concrete:

- How many jobs did the plan remove?
- How many crates disappeared from selected jobs?
- How many compiler actions hit L1 or L2, missed, or bypassed?
- Which distributed operation classes won after transfer and admission cost?
- Which dependency and release tools became unnecessary?
- How long did it take to move from a code change to a trustworthy result?

## What Cargo-Rail does not replace

Cargo-Rail does not replace Cargo, cargo-nextest, Just, Git, or CI.

It is not a general polyglot build system. It is not a hosted cache service. It is not a daemon with a global view of every checkout. It is not an excuse to send every compiler invocation over a network. It is not a reason for a small crate to acquire monorepo machinery.

It is also not finished.

The proof boundary should expand only when the evidence is complete. The planner needs more real workspaces and adversarial diffs. Cross-platform and provider qualification should grow through public, reproducible corpora—not confidence. Release and synchronization recovery deserve review from people who have operated those failure modes at scale.

The 0.22 hardening pass also closed more than 1,000 lint findings and tightened the codebase, but lint cleanliness is not the result. The result is a smaller public boundary: decide and prove; do not reimplement execution.

## Try the read-only path first

Install the release and inspect a real branch:

```bash
cargo install cargo-rail --version 0.22.0 --locked
cargo rail plan --merge-base --explain
```

`plan` is read-only. It does not edit tracked files or execute selected work. Inspect its reasoning and typed scopes, then pass those scopes to the Cargo, nextest, Just, or CI commands you already trust.

If the plan is useful, add the dependency checks. If verified reuse fits the graduated compiler classes on your machines, preview cache setup. Adopt release or split/sync only after reviewing their plans, external effects, and recovery boundaries.

Do not migrate everything because a launch post told you to. Delete one duplicate model at a time.

## I am looking for evidence and maintainers

Cargo-Rail is MIT-licensed, and I want its next phase to be community-maintained rather than dependent on one person’s context.

Stars help discovery. The project needs more concrete contributions:

- Run the read-only planner on large, awkward workspaces and report false positives, false negatives, and unclear reason chains.
- Reproduce the cache and distributed benchmark contracts on different hosts, toolchains, and providers, including rejected and losing samples.
- Review the Cargo-resolution, compiler-proof, release-transaction, and split/sync recovery boundaries.
- Help own a domain instead of requiring every subsystem to route through one maintainer.

I am also applying for open-source funding and infrastructure/API credits to sustain that work. Infrastructure support will fund public compatibility, failure, and benchmark qualification. API credits will be used for maintainer tooling—issue triage, fixture reduction, review support, and release work—not for Cargo-Rail’s runtime decisions. Planning, cache admission, and release authority will remain deterministic, auditable, and model-independent.

The thesis of Cargo-Rail is not “put another orchestrator around Rust.”

It is the opposite:

> Cargo already knows the workspace. Keep that model authoritative, stop rebuilding partial copies of it, and remove work before asking how to make the remaining work faster.

- [Cargo-Rail on GitHub](https://github.com/loadingalias/cargo-rail)
- [Planning documentation](https://github.com/loadingalias/cargo-rail/blob/main/docs/planning.md)
- [Caching documentation](https://github.com/loadingalias/cargo-rail/blob/main/docs/caching.md)
- [Benchmarking contract](https://github.com/loadingalias/cargo-rail/blob/main/docs/benchmarking.md)
- [Contributing](https://github.com/loadingalias/cargo-rail/blob/main/CONTRIBUTING.md)
