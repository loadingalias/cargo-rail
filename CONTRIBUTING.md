# Contributing to cargo-rail

Thanks for contributing.

## Development Setup

Prerequisites:

- Rust toolchain (stable; MSRV is enforced in CI)
- `just`
- `cargo-nextest`
- `cargo-deny`
- `cargo-audit`

```bash
just build
just check
just test
```

For full-workspace verification:

```bash
just check-all
just test-all
```

## Change Requirements

- Keep changes focused and atomic.
- Update docs when behavior changes.
- Add or update tests for behavior changes.
- Run `just check-all && just test` before opening a PR.

## Pull Requests

- Use a clear title and summary.
- Include rationale and expected behavior changes.
- Link related issues when applicable.

## Security

Please do not open public issues for vulnerabilities. See [SECURITY.md](SECURITY.md).
