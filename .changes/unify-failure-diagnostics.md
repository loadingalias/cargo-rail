---
"cargo-rail" = "minor"
---

Report a failed Unify compiler-evidence view with one cause, one recovery section,
and the view's Cargo command as a reproduction.
JSON errors add `failure_class`: `build_script`, `source`, `toolchain`, `cargo_rail`, or `cargo` for compiler evidence, and `lockfile`, `manifest`, `toolchain`, or `cargo` for Cargo metadata.
JSON errors no longer contain Cargo or build-script output.
Text mode shows the last lines of a failing build script's stderr,
with inherited environment values replaced by their names,
and Cargo's complete output with `--verbose` unless a Cargo credential capability is active.
A build-script failure names the environment variables the script declares, never their values.
Unify keeps progress on stderr in JSON mode, reports Cargo file-lock waits as they happen,
and prints a `Still running:` line when a phase is silent for 30 seconds.
An interrupted compiler acquisition reports the `interrupted` class and the phase it stopped.
`--diagnostics-file` output moves to schema 16 and adds `progress_phases` with per-phase durations.
The library adds `RailError::Failure` and `FailureClass`.
