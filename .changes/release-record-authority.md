---
"cargo-rail" = "minor"
---

Prepare each selected release closure in one commit and retain portable,
identity-bound release records.
Recovery requires the original intent and sealed Cargo package bytes.
Registry reconciliation compares checksums and yank state;
uncertain uploads retain distinct attempt identities and are not repeated automatically.

Preparation pushes require the captured remote branch tip.
Signed tags are created before registry publication and retain their exact objects
for recovery without another signature.

GitHub releases now require `release.validation` workflow paths and required job names.
Validation binds the exact release commit, workflow, run, attempt, and successful jobs.
Unrelated check rollups, skipped jobs, and the publishing run itself cannot authorize a release.

Required unavailable API evidence blocks release execution.
Finish or reconcile older active release records with their originating executable before upgrading.

Bind optional native assets to exact validated producer attempts and retained digests
before publication.
Reconcile GitHub drafts and public releases against exact presentation and asset inventories.

Submit configured hosted releases as one durable request.
Explicitly dispatch validation, recover original records and Cargo archives from Git,
and continue reviewed merges under the same intent.
Remove the separate finalization command and attached-wait flow.
Promote configured tag aliases only after verified immutable publication,
using the captured prior object as the Git lease.
