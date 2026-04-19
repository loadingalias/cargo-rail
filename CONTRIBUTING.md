# Contributing to cargo-rail

## Local Setup

Required tools:

- Rust stable
- `just`
- `cargo-nextest`
- `cargo-deny`

Typical local workflow:

```bash
just check
just test
```

Full-workspace verification:

```bash
just check-all
just test-all
```

Regenerate command docs when CLI help changes:

```bash
just gen-docs
```

## Expectations

- Keep changes focused.
- Update docs when behavior changes.
- Add or update tests when behavior changes.
- Run `just check && just test` before opening a PR.

## Pull Requests

- Use a clear title and summary.
- Call out user-visible behavior changes.
- Link related issues when applicable.

## Security

Do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md).
