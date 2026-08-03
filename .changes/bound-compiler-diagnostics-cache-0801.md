---
"cargo-rail" = "minor"
---

Add automatic verified compiler reuse for eligible clean Cargo profiles while preserving active incremental profiles,
explicit incremental requests, and existing wrappers. Move compiler evidence into the typed, user-wide
content-addressed store and retain the bounded legacy file only for one-time import.

Content-identify each exact Cargo, rustc, rustdoc, complete sysroot, host, environment, and compiler invocation instead
of gating native reuse through a checked-in toolchain allowlist. Structurally eligible native library units can reuse
verified results across supported Rust releases and bundled named codegen backends; external backend paths and
incompletely modeled units continue through the exact compiler invocation.

Add separately scoped workspace/local cache status, preview, and cleanup commands while keeping `clean --cache` as the
combined compatibility alias. Coordinate restore, publication, garbage collection, status, and cleanup through one
validated lifecycle lock; reclaim crash staging and evict least-recently-used unleased results under the configured
byte bound. Local status and cleanup remain exact after `cargo clean` without a workspace-local pointer to user-wide
state. Keep Windows lifecycle locks buildable on stable Rust while preserving exact file identity, hard-link rejection,
and pathname replacement resistance.

Delegate the exact normal all-workspace build and distribution actions directly to unchanged Cargo when an active
profile and its target location are statically unambiguous. This preserves Cargo and wrapper ownership while avoiding
metadata, Git, tool hashing, action-key construction, and cache setup. For an eligible clean profile, prove the
workspace-library boundary with one locked, no-dependencies Cargo metadata query and enter verified compiler reuse
without capturing Git state or expanding the full action plan; ambiguous and unsupported shapes retain captured
planning.
Default text execution reports one concise native-cache decision; `--explain` retains the full stable reason and
per-unit evidence plus accounted verified-result bytes read and written. Stop synchronously flushing observational
run receipts and parent-owned compiler observations because they are not recovery authority.

Memoize the complete Linux sysroot identity only while two exact filesystem inventories agree, and bypass to a full
hash when generation evidence changes. Reuse non-Rust files reported by rustc dep-info, including `include!`,
`include_str!`, and `include_bytes!` inputs. Keep timed benchmark commands free of opt-in diagnostics and validate their
outcomes through a separate unmeasured replay. The benchmark workflow now defaults to one accepted interleaved group
and treats that complete group as publishable bounded evidence; larger sample counts are explicit for distribution or
tail claims.

Move native compiler identity from whole-workspace session state to the exact compiler unit that consumed it. The
session no longer includes the complete Cargo configuration or `Cargo.lock`; exact rustc arguments, cfg, sources,
dependency contents, observed filesystem reads, observed environment reads, and a narrow compiler-process environment
remain authoritative. Output-neutral warning, job, build-directory, target-directory, network, registry, and
unrelated lockfile changes can now reuse verified results, while unknown or unobservable behavior continues to bypass.

Preserve Cargo's exact rustc argv and current directory. Bind opaque Rust metadata to the physical source root instead
of rewriting the compiler invocation and claiming unsafe cross-checkout portability. Store reversible internal path
tokens for verified dep-info and JSON compiler streams, then late-bind the current output directory after CAS
verification while preserving Windows separators and escaping exactly. Remove the redundant pre-context lockfile hash,
warm rehashing of already captured inputs, and the cold-path dep-info root scan. Strengthen compatibility and benchmark
oracles to compare direct, disabled, cold, and warm output bytes without root-bound exclusions, and invalidate
qualification evidence when the cache execution contract changes. Restore a real MSRV-to-stable compatibility matrix
instead of testing the repository toolchain repeatedly. Also make the benchmark identity manifest valid when the
repository has no untracked files.

Decode rustc's Makefile-escaped dep-info environment values before hashing them, so Windows `OUT_DIR` and Cargo path
dependencies compare against the exact compiler process environment instead of producing false warm misses. Require
every cold cache publication to become a warm hit in the real-world fixture while preserving the exact bypass count.
Run that long fixture first and exclusively under nextest so Windows load cannot consume its bounded timeout.

Retain reviewed Rust 1.97.1 Linux and Windows v4 qualification corpora with 20 accepted lane samples, two complete
groups, no rejections, and no false hits on each host. Keep large benchmark diagnostics file-backed through `jq`
instead of passing megabyte JSON values through the process argument limit.
