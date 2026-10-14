# Security Policy

## Supported releases

Security fixes are for the latest Cargo-Rail release.
Report a vulnerability even if you found it in an older release.
Include every version or commit that you tested.

## Report a vulnerability privately

Send the report through [GitHub's private vulnerability form](https://github.com/loadingalias/cargo-rail/security/advisories/new).
Do not put vulnerability details in a public issue, discussion, pull request, or commit.

Include this information:

- the affected Cargo-Rail version or commit;
- the operating system, target, Rust toolchain, and installation method, when they apply;
- the security impact and the access that an attacker needs;
- the smallest reliable reproduction or proof of concept; and
- known mitigations or a suggested fix, if you have them.

Remove credentials, private source, and unrelated environment values from attached logs.

For a cache or installation report, include the related hit, miss, bypass, or verification failure.
Also state whether the issue involves local storage, a remote provider, or a distributed worker.

For a release report, include the Cargo-Rail version, the source commit,
the workflow run and attempt, and the `cargo rail release --format json` result.
State the step that failed: preflight, release commit, push, assets, registry, GitHub release,
or alias.
Before you share logs, remove credentials and private release content from them.

If GitHub Security Advisories are not available, send the same information by email to `thealiaslab@gmail.com`.
Keep the report private until the maintainers release a fix
or agree with you on a disclosure timeline.
