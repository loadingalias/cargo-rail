# Cargo-Rail 0.22 Launch and Open-Source Funding Kit

Prepared for the Cargo-Rail 0.22 launch on August 21, 2026.

## 1. The positioning that should lead everything

**Primary thesis**

> Cargo already knows the workspace. Cargo-Rail gives one operation one captured, revalidated workspace authority, then lets Cargo, nextest, Just, Git, and CI keep executing the work they already own.

**Launch hook**

> Cargo-Rail 0.22’s most important feature is one I deleted: `cargo rail run`.

**Category**

Cargo-Rail is a **Rust workspace authority layer**, not a task runner, generic build system, or hosted cache service.

**What makes the project coherent**

The workflows look broad only when described by feature name. They are one problem when described by failure mode:

- affected CI fails when it uses an approximate ownership/dependency model;
- compiler reuse fails when it authorizes output from incomplete evidence;
- dependency cleanup fails when edits are made from a partial Cargo graph;
- releases fail when intent and publication refer to different commits;
- split/sync fails when repository ownership diverges from Cargo ownership.

The common responsibility is **decision authority over one captured workspace view**.

## 2. Direct critique of the original draft

The technical content is strong. The original presentation is too much like combined architecture and reference documentation. It gives every subsystem equal weight before the reader has accepted the central idea.

The revised post should do four things in order:

1. Lead with deleting `run`.
2. Establish authority versus orchestration.
3. prove the design with affected CI, external adoption, and fail-closed caching;
4. show the remaining workflows as consequences of the same invariant.

### Claims to remove entirely

Do not publish these claims from the tail of the original draft:

- **“It replaces Astral’s Hawk and is 2–3x more performant.”** You explicitly do not have a retained benchmark yet. A manual result is useful for deciding what to benchmark, not for a public comparison.
- **“I am very close to fixing a large chunk of Cargo’s issues.”** This is too broad and will trigger justified skepticism.
- **Plans to replace Tokio with a private async runtime/networking stack.** This is unrelated to the launch thesis and reads as uncontrolled scope expansion. Remove dependencies only when a measured design need justifies it.
- **“The DX is head and shoulders above any competitor.”** Demonstrate the command/config surface. Do not declare victory over unnamed competitors.

### Claims that can stay, but need disciplined wording

- **More than 1,000 lint findings closed:** present this as a hardening detail, not proof that the architecture is correct.
- **Benchmark numbers:** call them “p95 wall-time reductions” and preserve the exact corpus, comparator, host, and losing workloads.
- **One graph:** use “one authoritative captured view per operation.” Cargo-Rail is not a global daemon with one eternal graph.
- **Replaces tools:** say a workflow can retire a duplicate model when it is adopted. Do not imply every repository deletes the same stack.
- **Massive potential:** let external adoption, contracts, and evidence establish leverage. Do not use the phrase.

## 3. Evidence audit

| Claim                                                           | Assessment                                              | Launch treatment                                 |
| --------------------------------------------------------------- | ------------------------------------------------------- | ------------------------------------------------ |
| Top-level `cargo rail run` and `[run]` config were removed      | Verified on current `main`                              | Lead with it                                     |
| Cargo/nextest/Just/CI now consume typed planner scope directly  | Verified in the removal change and config documentation | Core architectural claim                         |
| One `WorkspaceContext` is captured and passed into a workflow   | Verified in architecture/code                           | Say “per operation,” not “one eternal graph”     |
| Planner emits separate surfaces, reason codes, and `cargo_args` | Verified                                                | Show one concrete nextest handoff                |
| Compiler cache restores only after evidence revalidation        | Verified in code/docs/tests                             | Use “a lookup is not authority”                  |
| Unsupported or incomplete compiler classes bypass to Cargo      | Verified                                                | Make this prominent                              |
| Retained L1/S3/distributed benchmark figures                    | Present in checked-in evidence/change records           | Publish as scoped results, not universal ranking |
| Some distributed workloads lose and stay local                  | Verified in retained results                            | Keep; this materially increases credibility      |
| Apache Iggy uses Cargo-Rail in pre-merge CI                     | Verified in a merged Apache PR and current workflow     | Put in the article and every application         |
| Prosody projects use cargo-rail-action                          | Verified in current workflows                           | Secondary adoption evidence                      |
| More than 1,000 lint improvements                               | Maintainer-reported; strict lint policy is visible      | One sentence at most                             |
| 2–3x faster than Hawk                                           | Not backed by retained public evidence                  | Omit until benchmarked                           |
| Cargo-Rail will fix a large part of Cargo                       | Unsupported/overbroad                                   | Omit                                             |
| DX is superior to every competitor                              | Subjective                                              | Omit                                             |

## 4. Release-blocking checks

As of the review, GitHub and crates.io still show **v0.21.0** as the latest public release. Do not publish “Today I’m releasing 0.22.0” or the pinned install command until the tag, GitHub release, crates.io package, and assets are live.

### Fix the stale active documentation

Current `src/commands/cli.rs` still says:

```text
Most teams should start with 'plan', 'run', and 'unify'
```

Change it to something current, such as:

```text
Most teams should start with 'plan', 'cache', and 'unify'
```

Then regenerate generated command documentation.

```bash
just gen-docs
```

Search active documentation and examples for obsolete execution references. Historical changelog entries and the 0.22 removal change should remain intact.

```bash
rg -n \
  --glob '!CHANGELOG.md' \
  --glob '!.changes/**' \
  'cargo rail run|\[run\]|plan / run|plan and run|plan.*,.*run.*,.*unify' \
  .
```

### Run the repository gates

Use the repository’s own documented workflow:

```bash
just check
just test
just test-all
just gen-docs

git diff --check
git status --short
```

Run the release checks and preview before creating external effects:

```bash
cargo rail change status
cargo rail release check --all --extended
cargo rail release run --all --bump auto --pr --check
```

### Verify the public release from a clean environment

After publication:

```bash
cargo install cargo-rail --version 0.22.0 --locked
cargo rail --version
cargo rail --help
cargo rail plan --help
cargo rail cache --help
```

Also verify:

- the signed Git tag points to the intended release commit;
- the GitHub release assets match the tag;
- crates.io reports 0.22.0;
- the release notes explicitly call out removal of `run` and `[run]`;
- cargo-rail-action supports the 0.22 planner contract;
- README, generated command docs, examples, and social preview text contain no live `run` recommendation;
- the benchmark links and retained artifacts are publicly accessible.

## 5. Launch sequence

1. Publish and verify v0.22.0.
2. Merge/push the active-documentation cleanup.
3. Publish the article at `loadingalias.dev`.
4. Post the Rust Forum announcement first; it gives technical readers a durable discussion location.
5. Submit the link to Lobste.rs with a concise author comment.
6. Submit to the Rust subreddit with a technical self-post introduction.
7. Post the X thread and link back to the article.
8. Stay in the discussions and answer technical objections directly. Correct the post quickly if someone finds a factual error.

Do not lead any community post with funding. Lead with the architectural subtraction, evidence, and invitation to test it.

## 6. X launch thread

Replace `[URL]` with the final post URL.

**1/6**

> Cargo-Rail 0.22’s biggest feature is one I deleted: `cargo rail run`.
>
> Cargo, nextest, Just, and CI should execute. Cargo-Rail should decide affected scope, prove compiler reuse, and authorize releases/sync from one captured workspace view.
>
> [URL]

**2/6**

> Why delete the runner?
>
> A second execution language would force Cargo-Rail to absorb Cargo, nextest, Just, shell, and CI semantics forever.
>
> 0.22 emits versioned package scope and exact Cargo args. The tools you already trust execute them.

**3/6**

> Affected CI is a graph query, not a directory match.
>
> Cargo-Rail interprets Cargo.toml/Cargo.lock changes semantically, maps files to packages, propagates dependent impact, and widens conservatively when evidence is incomplete.

**4/6**

> Apache Iggy already uses Cargo-Rail’s dependency-DAG planner to scope Cargo/nextest work in pre-merge CI, with full-workspace check/Clippy as a separate safety layer and full-suite fallback when planning fails.

**5/6**

> The cache follows the same rule: a lookup is not authority.
>
> Cargo-Rail revalidates compiler, sysroot, argv, sources, dependencies, environment, supported native inputs, and exact outputs.
>
> Fast when proven. Normal Cargo when not.

**6/6**

> The benchmark report includes losing remote workloads. Small, single-large, and parallel-check cases stayed local.
>
> I’m looking for real workspaces, adversarial diffs, reproduced corpora, and domain co-maintainers—not blanket speed claims.
>
> [URL]

## 7. Reddit submission

**Title**

> Cargo-Rail 0.22: I removed the task runner; Cargo, nextest, and Just now consume affected-work plans directly

**Body**

> Project author here. Cargo-Rail 0.22 removes the top-level `cargo rail run` command and `[run]` configuration.
>
> The reason is architectural: Cargo already owns build semantics, nextest owns test execution, Just owns repository recipes, and CI owns scheduling and isolation. Cargo-Rail should not become a second execution language.
>
> It now concentrates on one captured workspace authority per operation: semantic affected-work planning, verified compiler-result reuse, dependency coherence, exact-SHA releases, and Cargo-aware split/sync. The planner emits versioned per-surface `cargo_args`; existing tools execute them unchanged.
>
> Apache Iggy is already using the dependency-DAG planner to scope Cargo/nextest work in pre-merge CI, with full-workspace check/Clippy as an independent safety layer and a full-suite fallback.
>
> The benchmark section includes scoped wins and the remote workloads that lost. The cache rule is “fast when proven; normal Cargo when not.”
>
> I would value review of the authority boundary more than general launch feedback: planner false negatives/positives, incomplete compiler evidence, release recovery, and split/sync ownership are the areas where adversarial real-world cases matter.
>
> Article: [URL]
> Repo: https://github.com/loadingalias/cargo-rail

## 8. Lobste.rs submission

**Title**

> Cargo-Rail 0.22: one Cargo workspace model, no second task runner

**Author comment**

> Author here. The central change is subtraction: I removed Cargo-Rail’s generalized `run` layer. Cargo/nextest/Just/CI now execute versioned package scope emitted by the planner.
>
> The post explains why affected CI, verified compiler reuse, dependency coherence, exact-SHA releases, and crate split/sync share an authority boundary without becoming a universal orchestrator. It also includes the retained benchmark losses, not only wins, and links an Apache Iggy production CI integration.
>
> I’m especially interested in criticism of the proof and fallback boundaries.

## 9. Rust Forum announcement

**Title**

> [ANN] Cargo-Rail 0.22: affected-work plans and verified compiler reuse without a task runner

**Body**

> I’ve released Cargo-Rail 0.22.0.
>
> The largest design change is the removal of the top-level `cargo rail run` command and `[run]` configuration. Cargo-Rail no longer tries to own repository command execution. Cargo, cargo-nextest, Just, and CI consume the planner’s typed package scope directly.
>
> The narrower model is:
>
> - capture one authoritative Cargo/source view for an operation;
> - derive affected scope, dependency edits, cache authority, release intent, or crate ownership from that view;
> - emit an explicit, versioned plan;
> - revalidate before mutation or an external effect;
> - fall back conservatively when evidence is incomplete.
>
> `cargo rail plan` remains the read-only entry point:
>
> ```bash
> cargo install cargo-rail --version 0.22.0 --locked
> cargo rail plan --merge-base --explain
> ```
>
> The plan emits separate build/test/bench/docs/infra/custom surfaces with stable reason codes and exact Cargo package arguments. It does not execute the selected work.
>
> Apache Iggy has merged the planner into pre-merge CI to scope Cargo and nextest work from its dependency DAG while retaining full-workspace check/Clippy coverage and a full-suite fallback. Prosody workflows use the GitHub Action to gate build, test, and infrastructure jobs.
>
> The compiler cache remains underneath ordinary Cargo. Eligible results are restored only after revalidating the compiler/sysroot, argv, source and dependency bytes, compiler-visible environment, supported native inputs, action/result binding, and exact outputs. Unsupported or incomplete cases run through normal Cargo.
>
> The retained benchmark report includes losing distributed workloads; the automatic policy keeps those classes local. I am not claiming universal speedups.
>
> The launch post is here: [URL]
>
> Repository: https://github.com/loadingalias/cargo-rail
>
> I’m looking for concrete reports from multi-crate workspaces, especially planner false negatives/positives, unclear reason chains, provider/host qualification results, and review of release or split/sync recovery semantics.

## 10. OpenAI Codex for Open Source application

The current form allows 500 characters for each of the three main narrative fields. These answers fit those limits.

### Why does this repository qualify? — 356 characters

> Cargo-Rail is an actively maintained MIT-licensed Rust workspace engine. It gives Cargo, nextest, Just, and CI one versioned affected-work contract, plus verified compiler-result reuse, dependency coherence, exact-SHA releases, and crate split/sync. Apache Iggy already uses its dependency-DAG planner in pre-merge CI; other projects use its GitHub Action.

### How will you use API credits? — 414 characters

> Use credits for repository maintenance, not Cargo-Rail runtime behavior: triage and deduplicate reports; turn failures into minimal Rust workspace fixtures; review PRs against documented invariants; maintain cross-platform and toolchain compatibility matrices; and prepare release notes and checklists. Planning, cache admission, and release decisions remain deterministic, local, auditable, and model-independent.

### Anything else? — 326 characters

> I am Cargo-Rail’s primary maintainer. v0.22 deliberately removes the generalized task runner so Cargo, nextest, Just, and CI retain execution authority. Support would fund external workspace pilots, public correctness and benchmark corpora, contributor onboarding, and maintenance of a deliberately smaller authority boundary.

### Stronger evidence to attach or link

- Cargo-Rail repository and v0.22 release.
- The launch article.
- Apache Iggy PR #3095 and its current CI action.
- Prosody’s pinned cargo-rail-action workflow.
- Benchmarking contract and retained raw evidence.
- CONTRIBUTING policy requiring reproducible performance evidence.

Do not claim the API credits will pay living expenses. OpenAI’s current programs provide product/API support. Use Sequoia for the direct living-expense case.

## 11. OpenAI Codex Open Source Fund application

The separate current fund form describes grants of up to $25,000 in API credits. Keep the story consistent with the maintainer application, but use the extra space to define a project rather than a personal subscription need.

### Brief project description

> Cargo-Rail is an MIT-licensed Rust workspace engine for multi-crate repositories. It captures Cargo’s resolved model and an exact source view once per operation, then derives versioned affected-work plans, verified compiler-result reuse, dependency-coherence edits, reviewed exact-SHA releases, and Cargo-aware crate split/sync. It does not replace Cargo, nextest, Just, Git, or CI; it removes the duplicate partial workspace models those tools are often forced to consume. Apache Iggy already uses the planner in pre-merge CI, and Prosody projects use the GitHub Action. Version 0.22 deliberately removes Cargo-Rail’s generalized task runner to keep execution authority in existing domain tools.

### How would you use API credits?

> Credits would fund open-source maintainer automation around Cargo-Rail rather than model-dependent product behavior. I would build auditable workflows to triage and cluster incoming reports, reduce failures into minimal reproducible Rust workspaces, compare proposed patches against documented planner/cache/release invariants, summarize cross-platform CI failures, maintain compatibility evidence across Cargo/Rust releases, and prepare release checklists and notes. Every generated conclusion would remain reviewable by a maintainer. Cargo-Rail’s planner, cache admission, mutation, and release decisions would remain deterministic and fully functional without OpenAI services.

### Anything else

> The project has reached the stage where external adoption creates a maintenance problem worth solving: reports are no longer only feature requests, but cases involving real Cargo graphs, CI contracts, compiler evidence, and recovery semantics. The highest-leverage use of credits is to turn those reports into reproducible evidence quickly enough that contributors can review and own subsystems. The project’s runtime should not acquire an AI dependency.

## 12. Sequoia Open Source Fellowship application

Sequoia explicitly says it values real-world adoption rather than treating stars as the only signal. Make Apache Iggy the first proof point. The fellowship is extremely selective, so the application needs a bounded 6–12 month transformation, not a list of possible features.

### One-paragraph project pitch

> Cargo-Rail is an existing MIT-licensed Rust workspace engine that removes duplicated, inconsistent models of large Cargo repositories. It gives affected CI, verified compiler reuse, dependency cleanup, exact-SHA releases, and crate split/sync one captured and revalidated workspace authority while leaving execution to Cargo, nextest, Just, Git, and CI. Apache Iggy already uses its dependency-DAG planner in pre-merge CI, and other projects use the GitHub Action. The 0.22 release removes Cargo-Rail’s generalized task runner, stabilizing a narrower boundary that is ready for broader external adoption and shared maintenance.

### Why the work matters

> Large Rust codebases commonly pay for the same repository analysis many times and still receive inconsistent answers about what changed, what must run, what can be reused, and what is safe to release. This is both a performance and correctness problem. Cargo-Rail’s opportunity is not to become another build platform; it is to make Cargo’s model reusable as an authority by the tools teams already trust. That can reduce unnecessary CI and compiler work while making every skipped job, restored artifact, manifest edit, and external release effect explainable.

### What 6–12 months would buy

> Fellowship support would let me turn a technically mature, primarily solo-maintained system into durable community infrastructure. I would focus on five outcomes: (1) run structured pilots with at least five substantial external Rust workspaces and publish the measured decisions and failures; (2) expand public correctness and performance qualification across Linux, Windows, macOS, x86-64, Arm64, and supported remote authorities; (3) commission or organize adversarial review of the compiler-cache and release-transaction proof boundaries; (4) build migration guides and fixtures from real adopters rather than synthetic examples; and (5) establish domain ownership, a public roadmap, and a maintainer succession path so the project no longer depends on one person’s context.

### Why now

> The architecture has just crossed an important threshold: 0.22 deletes the generic runner instead of expanding it. That subtraction makes the project easier to adopt incrementally, easier to review, and less likely to become a competing ecosystem. There is now external CI adoption and retained performance/correctness evidence, but the project needs focused time for pilots, qualification, documentation, and governance before maintenance load hardens around a single maintainer.

### What not to put in the application

- Do not propose building a private async runtime or networking stack.
- Do not promise to replace every Rust workspace tool.
- Do not frame the fellowship as compensation for past effort.
- Do not lead with financial pressure. State plainly that the stipend would make 6–12 months of focused work possible, then define the public outcomes.
- Do not make stars the primary adoption evidence.

## 13. AWS Cloud Credits for Open Source application

The AWS program currently asks for an OSI-approved license, active maintenance/community engagement, and a project that is not dominated by one vendor or VC-backed entity. It favors technical complements to AWS or projects important to AWS customers.

Cargo-Rail’s strongest AWS case is not generic CI spending. It is that S3 is a supported remote result authority and EC2 is the natural environment for reproducible compiler-cache/distributed-execution qualification used by Rust teams on AWS.

### Project description

> Cargo-Rail is an independently maintained, MIT-licensed Rust workspace engine. It derives affected CI scope from Cargo’s resolved dependency model, provides verified compiler-result reuse beneath ordinary Cargo, coordinates reviewed exact-SHA crate releases, and synchronizes Cargo crates across monorepo and standalone-repository boundaries. It has no hosted SaaS or proprietary control plane. Apache Iggy already uses the planner in pre-merge CI, and other open-source projects use its GitHub Action.

### How AWS credits will be used

> Credits will fund reproducible correctness, failure, and performance qualification rather than production hosting. The test matrix will use disposable EC2 x86-64 and Arm64 workers, Windows and Linux environments, S3 as an empty/warm remote result authority, and isolated distributed compiler workers. It will exercise cold/warm paths, credential failures, throttling, network interruption, object conflict/corruption handling, worker loss, capability mismatch, and local fallback. Raw accepted and rejected benchmark evidence will be retained publicly. Additional credits will support CI, artifact storage, and cross-region latency qualification.

### Why this matters to AWS customers

> Rust teams running CI and build fleets on AWS need to know when S3-backed reuse or EC2-based distributed compilation is correct and when network placement loses to local work. Cargo-Rail’s policy is explicitly not “remote everything”: results must pass native verification, and measured losing operation classes remain local. The qualification work would provide open, reproducible guidance and tooling for customers using Cargo with S3 and EC2 without requiring a Cargo-Rail-hosted service.

### Community/governance language

> Cargo-Rail is not controlled by a vendor or VC-backed company. The project is MIT-licensed, accepts public issues and pull requests, documents contribution and evidence requirements, and is actively seeking domain co-maintainers. The next phase includes explicit subsystem ownership and a public roadmap to reduce dependence on a single maintainer.

## 14. A grant-facing evidence page to add to the repository

Create a concise `docs/adoption-and-impact.md` before or immediately after launch. Grant reviewers should not have to reconstruct the story from a 3,000-word post and a large repository.

Suggested sections:

1. **What Cargo-Rail is** — three sentences.
2. **Current adopters** — Apache Iggy, Prosody, and any others who consent to listing.
3. **Measured evidence** — links to raw corpora and the exact bounded claims.
4. **Safety model** — conservative widening, verified restore, local fallback, exact-SHA effects.
5. **Maintainer activity** — release cadence, issue/PR response, supported platforms.
6. **Six-month roadmap** — external pilots, qualification, governance, contributor ownership.
7. **Funding use** — Sequoia for maintainer time, AWS for infrastructure, OpenAI for maintenance automation. Keep the runtime independent of all three.

## 15. Community-maintenance plan

A generic “contributors welcome” section is not enough for a system this broad. Make ownership concrete.

Create or label four review domains:

- `domain/planner-cargo-graph`
- `domain/compiler-proof-cache`
- `domain/release-transaction`
- `domain/split-sync-git`

For each domain, publish:

- its invariants and threat/failure model;
- the smallest fixture that exercises it;
- the commands required before review;
- beginner-safe issues versus changes requiring an experienced reviewer;
- the current maintainer and a path to co-maintainer status.

The best grant story is not “fund one person to keep doing everything.” It is “fund the transition from one-person context to inspectable, shared subsystem ownership.”

## 16. Final standard for the launch

The post should make a Rust infrastructure engineer think:

1. “This project knows what it does **not** own.”
2. “The author reports the losing cases.”
3. “I can try the planner without giving it execution authority.”
4. “A real Apache project is already using the contract.”
5. “The project has a credible path from solo maintenance to shared ownership.”

That combination is substantially more persuasive than claiming Cargo-Rail changes every Rust codebase.
