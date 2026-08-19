---
"cargo-rail" = "major"
---

Added a private, versioned execution boundary for bounded compiler-only Rust operations with complete source
namespaces, exact `.rmeta`/`.rlib` dependencies, metadata-only outputs, and non-linking `lib`/`rlib` archives. A
dedicated one-shot worker accepts only canonical bounded frames, reconstructs rustc from closed typed fields, binds
requests to an exact compiler/sysroot/platform capability, executes in private staging, and returns slot-addressed,
digest-described results without receiving Cargo-visible destination paths.

The client-side decoder admits frames only into private staging after checking protocol, action, capability, lease,
slot, size, mode, digest, and trailer authority. Compiler failures retain their exact exit state and bounded diagnostic
streams without returning partial artifacts; cancellation kills and reaps rustc and returns no artifact payload.

The client independently verifies the worker's exact compiler capability, bounds the worker process or transport and
streams, owns a deadline beyond the execution lease, and collapses malformed or failed workers to a cold outcome.
Native derivation accepts only unchanged captured source and Rust dependency inputs whose typed crate type, exact
emission contract, outputs, and closed stable rustc argument set match protocol v3. The local and worker operations
share one canonical workspace-relative working-directory and path-remap contract. Generated namespaces, native or
dynamic dependencies, linking, observed environment, and unknown inputs reject before worker acquisition. A
first-seen compiler environment executes locally once before its exact selector can authorize delegation.

Verified responses enter the existing native cache through one CAS-owned private staging lease. Large metadata and
rlib artifacts move into native slots without a second copy, bounded dep-info and streams use the existing portable
binding transforms, and final live recapture precedes L1 authority. The existing restore transaction remains the only
path that can publish outputs. Tests reject source drift and post-decode artifact drift before admission.

`cache setup` can qualify either a local process proof or one explicit socket-address worker using TLS 1.3 mutual
authentication. The installed receipt privately owns the pinned worker capability, trust root, client certificate,
and locked-down client key; ordinary Cargo selects only that exact receipt. The client rejects capability drift before
sending source. The worker authenticates the client before issuing a fresh one-use lease bound to the action,
capability, client nonce, and authenticated leaf-certificate workload identity. The request, response, and audit event
retain that same identity. It executes only after L1 and remote L2 miss. Transport, protocol, and pre-effect worker
failures fall back to the exact normalized local command; verified compiler failures replay bounded diagnostics and do
not retry.

After local admission and output commit, a successful distributed result may publish through the existing L2
authority; the worker never receives provider credentials or cache mutation authority. A live three-node AWS
qualification proved ordinary-Cargo remote execution, worker-offline read-only L2 reuse on an independent machine,
and exact local fallback with identical output manifests. The exact object prefix, temporary network rule, compute,
storage, certificates, and credentials were removed after evidence collection. The single-action remote sample was
slower than local fallback, so this boundary makes no performance claim.

Automatic placement now uses bounded, expiring, source-free per-operation-class cost history keyed by the pinned
worker capability and endpoint, and delegates only when at least three local and remote observations conservatively
predict a critical-path win. Explicit qualification still samples every eligible miss. Automatic mode rejects the
process-only runtime and accepts the Linux Bubblewrap policy, whose capability identity binds the root-owned
executable bytes, version, and fixed empty-root policy. Startup
qualification compiles through that policy and proves default-deny host filesystem, network, credential environment,
PID, IPC, and UTS access. A native Linux ordinary-Cargo mTLS test completed inside the policy.

The isolated direct worker remains a dedicated single-tenant or ephemeral-machine envelope, not a sandboxed
multi-tenant scheduler. Pool scheduling and content locality remain deliberately absent.

The distributed client no longer re-derives the compiler capability from scratch on every attempt. It now selects the
same revalidating sysroot identity memo the native cache session already owns, so an eligible attempt confirms the
exact toolchain instead of rehashing the whole sysroot. The memo is still only trusted when the exact sysroot evidence
matches before and after the read, so this removes repeated work without weakening capability authority. Measured on a
622 MB macOS Arm64 sysroot, the client capability phase fell from about 232 ms to about 27 ms per attempt. Contexts
without a local cache, including the worker itself, capture once per process as before.

One mutually authenticated worker's socket address now travels inside the same machine-owned identity that carries its
trust root, certificate, key, and pinned capability, so no caller can pair one worker's address with another worker's
credentials.

Every distributed attempt now retains a source-free client phase breakdown covering capability capture, connection,
TLS handshake, capability exchange, lease, source transfer, remote execution, result transfer, and local admission,
plus transferred byte counts. The worker's audit event adds its own queue, input, compiler, and result-encode
durations. Both are counts and nanoseconds with no paths, crate names, digests, or peer identity, and neither enters
action, result, admission, or placement authority. They exist so placement decisions follow measured critical-path
evidence rather than an assumed architecture.

Protocol v3 makes input and resource authority part of the execution contract. It binds at most 16,384 inputs, 64 MiB
per input, 256 MiB total input, one CPU, 2 GiB memory, zero swap, 64 tasks, 512 MiB tmpfs, wall-time, stream, and
output limits into capability, action, request, and response identity. The worker requires delegated cgroup-v2
`cpu`, `memory`, and `pids` controllers and gives every attempt a fresh exact cgroup. Bubblewrap starts from an
empty root with no host-writable bind; the exact worker, sysroot, and system runtime are read-only. Cancellation and
cleanup use `cgroup.kill`, wait for an empty group, and reject retained attempts.

Worker qualification now falsifies the resource boundary itself: it requires kernel-observed CPU throttling, a
cgroup OOM kill, a process-limit event, and an idle delegated hierarchy afterward. A disposable Azure Linux x64 host
passed the normal sandbox compile plus all three hostile probes through the repository's `just ssh-*` qualification
and evidence-collection workflow. This safety slice did not change the retained performance result and makes no new
speed claim.

The worker now handles `SIGTERM` and `SIGINT` as a bounded drain: it closes the listener, reports active
connections, refuses new work, waits for accepted operations within the protocol-derived deadline, and exits only
after reporting an empty connection set. The systemd qualification path grants 150 seconds for the 145-second drain.
Startup rejects another live cgroup owner and removes only strictly named stale attempt cgroups.

Protocol v3 also closes dependency-bearing Rust library execution. The request carries one complete canonical source
namespace, exact digest-bound `.rmeta`/`.rlib` dependency inputs, typed compiler options, and the exact metadata or
archive output contract. Unsupported or incompletely observed inputs still bypass. A first-seen compiler environment
runs locally before its exact selector can authorize delegation, preserving environment discovery as local authority.

The final same-shape `c8i.large` qualification accepted 48/48 samples across four lanes. On its six-crate dependency
DAG, Cargo-Rail completed in 10.098 seconds p50 and 10.107 seconds worst observed, beating local Cargo by 29.57% and
29.71% and pinned distributed sccache by 28.84% and 29.52%. Cargo-Rail delegated four actions and executed two exact
local saturation fallbacks; distributed sccache delegated all six. Small, single large, and parallel-check workloads
lost, and automatic placement retained the measured small and large classes locally, so the speed claim is limited to
the qualified dependency-DAG topology.
