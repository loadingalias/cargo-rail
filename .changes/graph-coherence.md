---
"cargo-rail" = "major"
---

Bound dependency coherence to one captured workspace graph. Root and member manifests, inherited workspace
dependencies, source feature evidence, conservative documentation references, and MSRV policy now come from the same
snapshot. Existing inherited declarations participate in unused-dependency proof without producing no-op edits, and
renamed dependencies retain their exact Cargo alias and package identity through planning and application.

Captured `[workspace.package]` policy now produces explicit inheritance decisions. Unify rewrites only member values
that are semantically equal and safe to inherit, reports missing and divergent declarations without changing them, and
retains version- and workspace-relative path fields for their owning release or path policy. JSON, explanations,
Markdown reports, proof certificates, mutation traces, previews, receipts, and deterministic apply order share the same
decision set.

Added root-independent, versioned feature/target coverage views with direct Cargo and nextest argument arrays. Removed
the former target-load result that was presented as validation despite proving only that already-required metadata
existed. Each target now carries only feature selections whose captured cfg predicates can apply to that target.
Dependency rust-version metadata is now reported as an unprobed lower-bound candidate and cannot silently
lower a higher declared workspace MSRV. The report separately records whether the captured compiler satisfies that
candidate and states that no candidate-compiler build authorized a lowering. Apply reconstructs every captured
feature/target view as an exact Cargo resolution, restores all manifests if any view fails, and records the verified
view identities in its receipt and machine output. Cargo lockfile refresh is an explicit planned mutation with backup,
fingerprint, rollback, and undo coverage. Manifest, report, and receipt writes are atomic, reports are deterministic,
and any write or verification failure restores the complete authorized file set. Post-apply validation retains
fingerprint-bound starting changes while rejecting newly unplanned paths; commands such as release that prohibit
unrelated starting dirt retain their stricter boundary. Public workspace-hack replacement claims were removed until
generated-hack detection, exact removal, and end-to-end parity evidence exist.
