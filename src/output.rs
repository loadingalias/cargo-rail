//! Centralized output control for CLI commands.
//!
//! Provides consistent, ergonomic output handling with quiet mode support.
//!
//! # Categories
//!
//! **Critical** (always shown):
//! - [`error!`] - Error messages: `error: something went wrong`
//! - [`warn!`] - Warnings: `warning: configuration needs attention`
//! - [`help!`] - Help hints: `help: try --force`
//!
//! **Informational** (suppressed with `--quiet`):
//! - [`status!`] - Progress messages (no prefix)
//! - [`note!`] - Notes: `note: config found at /path`

use std::io::IsTerminal as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

// Global State

static INVOCATION_OUTPUT: OnceLock<InvocationOutput> = OnceLock::new();

/// Transport selected once for the complete invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputProtocol {
    /// Human-readable stdout plus diagnostics on stderr.
    Text,
    /// Exactly one complete JSON value on stdout, with diagnostics suppressed.
    ///
    /// A long-running command may explicitly retain progress on stderr; stdout still
    /// carries exactly one JSON value.
    Json,
    /// Command-owned raw stdout stream with failures reported on stderr.
    Raw,
}

/// Immutable process-output context established immediately after Clap parsing.
#[derive(Debug)]
pub struct InvocationOutput {
    protocol: OutputProtocol,
    quiet: bool,
    progress: bool,
    verbose: bool,
    color: bool,
    stdout_terminal: bool,
    stderr_terminal: bool,
}

impl InvocationOutput {
    /// Capture transport and terminal state once for this process.
    pub fn capture(quiet: bool, verbose: bool, json: bool) -> Self {
        Self::capture_protocol(
            quiet,
            verbose,
            if json {
                OutputProtocol::Json
            } else {
                OutputProtocol::Text
            },
        )
    }

    /// Capture one explicitly selected transport and terminal state.
    #[doc(hidden)]
    pub fn capture_protocol(quiet: bool, verbose: bool, protocol: OutputProtocol) -> Self {
        Self::capture_protocol_with_progress(quiet, verbose, protocol, false)
    }

    /// Capture one transport while retaining explicitly redirected progress.
    #[doc(hidden)]
    pub fn capture_protocol_with_progress(
        quiet: bool,
        verbose: bool,
        protocol: OutputProtocol,
        redirected_progress: bool,
    ) -> Self {
        let stdout_terminal = std::io::stdout().is_terminal();
        let stderr_terminal = std::io::stderr().is_terminal();
        let raw_or_json = protocol != OutputProtocol::Text;
        Self {
            protocol,
            quiet: quiet || raw_or_json,
            progress: !quiet && (!raw_or_json || redirected_progress),
            verbose: verbose && !raw_or_json,
            color: !raw_or_json && stderr_terminal && std::env::var_os("NO_COLOR").is_none(),
            stdout_terminal,
            stderr_terminal,
        }
    }

    /// Selected transport protocol.
    pub const fn protocol(&self) -> OutputProtocol {
        self.protocol
    }

    /// Whether stdout was a terminal at invocation start.
    pub const fn stdout_is_terminal(&self) -> bool {
        self.stdout_terminal
    }

    /// Whether stderr was a terminal at invocation start.
    pub const fn stderr_is_terminal(&self) -> bool {
        self.stderr_terminal
    }

    /// Whether bounded operational detail was requested.
    pub const fn verbose(&self) -> bool {
        self.verbose
    }

    /// Whether diagnostic color is permitted for this invocation.
    pub const fn color_enabled(&self) -> bool {
        self.color
    }

    /// Whether operational progress may be written to stderr.
    pub const fn progress_enabled(&self) -> bool {
        self.progress
    }
}

// Phase heartbeat

/// Longest interval without progress output while a phase runs.
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_secs(30);

/// What the current phase is doing, as the heartbeat reports it.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// Cargo-Rail computes in its own process.
    Analysis,
    /// Cargo-Rail waits for a Cargo subprocess.
    Cargo,
}

impl Activity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Analysis => "analysis",
            Self::Cargo => "cargo",
        }
    }
}

#[derive(Debug, Clone)]
struct Phase {
    label: String,
    activity: Activity,
    started: Instant,
    lock_wait: Option<String>,
}

/// The command's current phase plus nested phases that concurrent threads entered.
#[derive(Debug)]
struct Phases {
    base: Option<Phase>,
    nested: Vec<(u64, Phase)>,
    next_id: u64,
}

impl Phases {
    fn current(&mut self) -> Option<&mut Phase> {
        match self.nested.last_mut() {
            Some((_, phase)) => Some(phase),
            None => self.base.as_mut(),
        }
    }
}

static PHASES: Mutex<Phases> = Mutex::new(Phases {
    base: None,
    nested: Vec::new(),
    next_id: 0,
});
static PROGRESS_EPOCH: OnceLock<Instant> = OnceLock::new();
static LAST_PROGRESS_MILLIS: AtomicU64 = AtomicU64::new(0);

fn progress_epoch() -> Instant {
    *PROGRESS_EPOCH.get_or_init(Instant::now)
}

fn phases() -> std::sync::MutexGuard<'static, Phases> {
    PHASES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Record that a progress line was written now.
#[doc(hidden)]
pub fn record_progress() {
    let elapsed = u64::try_from(progress_epoch().elapsed().as_millis()).unwrap_or(u64::MAX);
    LAST_PROGRESS_MILLIS.store(elapsed, Ordering::Relaxed);
}

fn quiet_for() -> Duration {
    let last = Duration::from_millis(LAST_PROGRESS_MILLIS.load(Ordering::Relaxed));
    progress_epoch().elapsed().saturating_sub(last)
}

impl Phase {
    fn record(&self) {
        crate::instrumentation::record_progress_phase(
            &self.label,
            self.activity.as_str(),
            self.lock_wait.is_some(),
            self.started.elapsed(),
        );
    }
}

fn new_phase(activity: Activity, label: String) -> Phase {
    Phase {
        label,
        activity,
        started: Instant::now(),
        lock_wait: None,
    }
}

/// Print `label` as progress and make it the command's current phase.
#[doc(hidden)]
pub fn begin_phase(activity: Activity, label: String) {
    crate::status!("{label}");
    let label = label.trim().trim_end_matches("...").to_string();
    let previous = phases().base.replace(new_phase(activity, label));
    if let Some(previous) = previous {
        previous.record();
    }
}

/// End the command's current phase and record its duration.
#[doc(hidden)]
pub fn end_phase() {
    let phase = phases().base.take();
    if let Some(phase) = phase {
        phase.record();
    }
}

/// The command's current phase, without nested phases.
pub(crate) fn command_phase() -> Option<String> {
    phases().base.as_ref().map(|phase| phase.label.clone())
}

/// A nested phase that ends when dropped.
#[must_use = "the nested phase ends when the guard is dropped"]
pub(crate) struct NestedPhase {
    id: u64,
}

/// Enter a nested phase without printing a line; the heartbeat names it if it runs long.
pub(crate) fn nested_phase(activity: Activity, label: impl Into<String>) -> NestedPhase {
    let mut phases = phases();
    let id = phases.next_id;
    phases.next_id += 1;
    phases.nested.push((id, new_phase(activity, label.into())));
    NestedPhase { id }
}

impl Drop for NestedPhase {
    fn drop(&mut self) {
        let ended = {
            let mut phases = phases();
            let index = phases.nested.iter().position(|(id, _)| *id == self.id);
            index.map(|index| phases.nested.remove(index).1)
        };
        if let Some(phase) = ended {
            phase.record();
        }
    }
}

/// Report that Cargo is blocked on a file lock during the current phase.
pub(crate) fn report_lock_wait(subject: &str, lock: &str) {
    crate::status!("  {subject}: Cargo is waiting for a file lock on {lock}");
    if let Some(phase) = phases().current() {
        phase.lock_wait = Some(lock.to_string());
    }
}

/// The lock that a Cargo stderr line reports waiting for.
pub(crate) fn cargo_lock_wait(line: &str) -> Option<&str> {
    let lock = line.trim().strip_prefix("Blocking waiting for file lock")?.trim();
    Some(lock.strip_prefix("on ").unwrap_or(lock)).filter(|lock| !lock.is_empty())
}

fn heartbeat_line(phase: &Phase) -> String {
    let activity = match (&phase.lock_wait, phase.activity) {
        (Some(lock), _) => format!("waiting for Cargo's file lock on {lock}"),
        (None, Activity::Cargo) => "waiting for a Cargo subprocess".to_string(),
        (None, Activity::Analysis) => "in-process analysis".to_string(),
    };
    format!(
        "  Still running: {} ({activity}; {}s in this phase)",
        phase.label,
        phase.started.elapsed().as_secs()
    )
}

/// Background reporter that fills every quiet interval of a running phase with one line.
#[derive(Debug)]
#[must_use = "the heartbeat stops when dropped"]
pub struct Heartbeat {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    /// Start the heartbeat when progress output is enabled.
    #[doc(hidden)]
    pub fn start() -> Self {
        if !progress_enabled() {
            return Self {
                stop: None,
                thread: None,
            };
        }
        Self::start_with(PROGRESS_INTERVAL, |line| crate::status!("{line}"))
    }

    fn start_with(interval: Duration, emit: impl Fn(String) + Send + 'static) -> Self {
        record_progress();
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("cargo-rail-heartbeat".to_string())
            .spawn(move || {
                loop {
                    let wait = interval.saturating_sub(quiet_for()).max(Duration::from_millis(1));
                    match stopped.recv_timeout(wait) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    if quiet_for() < interval {
                        continue;
                    }
                    let line = phases().current().map(|phase| heartbeat_line(phase));
                    if let Some(line) = line {
                        emit(line);
                    }
                    record_progress();
                }
            })
            .ok();
        Self {
            stop: thread.as_ref().map(|_| stop),
            thread,
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

/// Stable schema version for machine-readable command output envelopes.
pub const MACHINE_OUTPUT_SCHEMA_VERSION: u32 = 1;

/// Install the immutable output context. Call exactly once at startup.
#[doc(hidden)]
pub fn init(output: InvocationOutput) {
    INVOCATION_OUTPUT
        .set(output)
        .expect("invocation output must be initialized exactly once");
}

fn invocation() -> &'static InvocationOutput {
    INVOCATION_OUTPUT.get_or_init(|| InvocationOutput::capture(false, false, false))
}

/// Check if quiet mode is enabled.
pub fn is_quiet() -> bool {
    invocation().quiet
}

/// Check if JSON mode is enabled.
pub fn is_json_mode() -> bool {
    invocation().protocol == OutputProtocol::Json
}

/// Check whether bounded operational detail was requested.
pub fn is_verbose() -> bool {
    invocation().verbose()
}

/// Check whether terminal-aware diagnostic color is permitted.
pub fn color_enabled() -> bool {
    invocation().color_enabled()
}

/// Check whether operational progress may be written to stderr.
#[doc(hidden)]
pub fn progress_enabled() -> bool {
    invocation().progress_enabled()
}

/// Write one human or machine stdout fragment without panicking on a closed pipe.
#[doc(hidden)]
pub fn write_stdout(arguments: std::fmt::Arguments<'_>, newline: bool) {
    let mut stdout = std::io::stdout().lock();
    let result = stdout
        .write_fmt(arguments)
        .and_then(|()| if newline { stdout.write_all(b"\n") } else { Ok(()) });
    if let Err(error) = result {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        if !is_json_mode()
            && let Err(_stderr_error) = writeln!(std::io::stderr().lock(), "error: failed writing stdout: {error}")
        {
        }
        std::process::exit(1);
    }
}

/// Build a stable machine-readable JSON envelope.
///
/// The returned object always contains:
/// - `schema_version`
/// - `command`
/// - `mode`
/// - `result`
/// - `exit_code`
///
/// If `payload` is an object, its keys are merged into the top-level envelope
/// without overriding existing standard keys. Non-object payloads are stored in
/// `payload`.
pub fn machine_json_envelope(
    command: &str,
    mode: &str,
    result: &str,
    exit_code: i32,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "schema_version".to_string(),
        serde_json::Value::Number(serde_json::Number::from(MACHINE_OUTPUT_SCHEMA_VERSION)),
    );
    out.insert("command".to_string(), serde_json::Value::String(command.to_string()));
    out.insert("mode".to_string(), serde_json::Value::String(mode.to_string()));
    out.insert("result".to_string(), serde_json::Value::String(result.to_string()));
    out.insert(
        "exit_code".to_string(),
        serde_json::Value::Number(serde_json::Number::from(exit_code)),
    );

    match payload {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if !out.contains_key(&key) {
                    out.insert(key, value);
                }
            }
        }
        other => {
            out.insert("payload".to_string(), other);
        }
    }

    serde_json::Value::Object(out)
}

// Critical Output (always shown)

/// Print an error message to stderr.
///
/// Always shown, even in quiet mode. Adds `error: ` prefix.
///
/// ```no_run
/// # fn main() {
/// cargo_rail::error!("failed to read file");
/// // Output: error: failed to read file
/// # }
/// ```
#[macro_export]
macro_rules! error {
  ($($arg:tt)*) => {
    if !$crate::output::is_json_mode() {
      eprintln!("error: {}", format_args!($($arg)*))
    }
  };
}

/// Print a warning message to stderr.
///
/// Always shown, even in quiet mode. Adds `warning: ` prefix.
///
/// ```no_run
/// # fn main() {
/// cargo_rail::warn!("configuration needs attention");
/// // Output: warning: configuration needs attention
/// # }
/// ```
#[macro_export]
macro_rules! warn {
  ($($arg:tt)*) => {
    if !$crate::output::is_json_mode() {
      eprintln!("warning: {}", format_args!($($arg)*))
    }
  };
}

/// Print a help hint to stderr.
///
/// Always shown, even in quiet mode. Adds `help: ` prefix.
/// Typically used after an error to suggest a fix.
///
/// ```no_run
/// # fn main() {
/// cargo_rail::error!("missing required argument");
/// cargo_rail::help!("run with --help for usage");
/// // Output:
/// // error: missing required argument
/// // help: run with --help for usage
/// # }
/// ```
#[macro_export]
macro_rules! help {
  ($($arg:tt)*) => {
    if !$crate::output::is_json_mode() {
      eprintln!("help: {}", format_args!($($arg)*))
    }
  };
}

/// Print a status/progress message to stderr.
///
/// Suppressed in quiet mode. No prefix added.
/// Use for transient progress info like "analyzing...", "writing files...".
///
/// ```no_run
/// # fn main() {
/// # let crates = vec![1, 2, 3];
/// cargo_rail::status!("analyzing {} crates...", crates.len());
/// // Output: analyzing 3 crates...
/// # }
/// ```
#[macro_export]
macro_rules! status {
  ($($arg:tt)*) => {
    if $crate::output::progress_enabled() {
      eprintln!($($arg)*);
      $crate::output::record_progress();
    }
  };
}

/// Print a progress line and make it the current phase that the heartbeat reports.
///
/// The first argument is the phase's [`Activity`](crate::output::Activity).
#[doc(hidden)]
#[macro_export]
macro_rules! phase {
  ($activity:expr, $($arg:tt)*) => {
    $crate::output::begin_phase($activity, format!($($arg)*))
  };
}

/// Print a note to stderr.
///
/// Suppressed in quiet mode. Adds `note: ` prefix.
/// Use for non-critical informational messages.
///
/// ```no_run
/// # fn main() {
/// # let path = std::path::Path::new("/project/.config/rail.toml");
/// cargo_rail::note!("existing config found at {}", path.display());
/// // Output: note: existing config found at /project/.config/rail.toml
/// # }
/// ```
#[macro_export]
macro_rules! note {
  ($($arg:tt)*) => {
    if !$crate::output::is_quiet() {
      eprintln!("note: {}", format_args!($($arg)*))
    }
  };
}

/// Alias for [`status!`]. Use whichever reads better in context.
#[macro_export]
macro_rules! progress {
  ($($arg:tt)*) => {
    $crate::status!($($arg)*)
  };
}

/// Print bounded operational detail only when `--verbose` is active.
#[macro_export]
macro_rules! verbose_progress {
  ($($arg:tt)*) => {
    if $crate::output::is_verbose() {
      $crate::status!($($arg)*)
    }
  };
}

#[cfg(test)]
mod tests {
    use super::{Activity, Heartbeat, InvocationOutput, OutputProtocol};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn heartbeat_fills_each_quiet_interval_and_names_the_current_activity() {
        // Long enough that a delayed wakeup on a loaded host stays inside one interval.
        const INTERVAL: Duration = Duration::from_millis(100);
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&lines);
        let taken = || std::mem::take(&mut *lines.lock().unwrap());
        let heartbeat = Heartbeat::start_with(INTERVAL, move |line| sink.lock().unwrap().push(line));

        crate::phase!(Activity::Analysis, "Detecting unused dependencies...");
        std::thread::sleep(INTERVAL * 3);
        let analysis = taken();
        assert!(analysis.len() >= 2, "each quiet interval needs one line: {analysis:?}");
        assert!(
            analysis
                .iter()
                .all(|line| line.contains("Detecting unused dependencies (in-process analysis;")),
            "{analysis:?}"
        );

        {
            let _metadata = super::nested_phase(Activity::Cargo, "running cargo metadata");
            std::thread::sleep(INTERVAL * 2);
            super::report_lock_wait("cargo metadata", "package cache");
            std::thread::sleep(INTERVAL * 2);
        }
        let nested = taken();
        assert!(
            nested
                .iter()
                .any(|line| line.contains("running cargo metadata (waiting for a Cargo subprocess;")),
            "{nested:?}"
        );
        assert!(
            nested
                .iter()
                .any(|line| line.contains("running cargo metadata (waiting for Cargo's file lock on package cache;")),
            "{nested:?}"
        );

        // Other progress output resets the quiet interval. A heartbeat may already have fired for the
        // preceding quiet phase, so resume progress before discarding it.
        super::record_progress();
        drop(taken());
        for _ in 0..6 {
            super::record_progress();
            std::thread::sleep(INTERVAL / 4);
        }
        assert!(
            taken().is_empty(),
            "progress within the interval must suppress the heartbeat"
        );

        std::thread::sleep(INTERVAL * 2);
        assert!(
            taken()
                .iter()
                .all(|line| line.contains("Detecting unused dependencies (in-process analysis;")),
            "the enclosing phase resumes when the nested phase ends"
        );
        drop(heartbeat);
        std::thread::sleep(INTERVAL * 2);
        assert!(taken().is_empty(), "no line after the heartbeat stops");
    }

    #[test]
    fn raw_protocol_suppresses_advisory_output_without_becoming_json() {
        let output = InvocationOutput::capture_protocol(false, true, OutputProtocol::Raw);

        assert_eq!(output.protocol(), OutputProtocol::Raw);
        assert!(output.quiet, "raw streams must suppress progress and advisory output");
        assert!(!output.verbose(), "raw streams must not enable text detail");
        assert!(!output.color_enabled(), "raw streams must remain byte-stable");
        assert!(!output.progress_enabled(), "raw streams must suppress progress");
    }

    #[test]
    fn redirected_json_can_retain_progress_without_enabling_advisories() {
        let output = InvocationOutput::capture_protocol_with_progress(false, true, OutputProtocol::Json, true);

        assert_eq!(output.protocol(), OutputProtocol::Json);
        assert!(output.quiet, "JSON must continue suppressing advisory output");
        assert!(
            output.progress_enabled(),
            "redirected JSON must retain operational progress"
        );
        assert!(!output.verbose(), "JSON must not enable text detail");
        assert!(!output.color_enabled(), "JSON diagnostics must remain byte-stable");

        let quiet = InvocationOutput::capture_protocol_with_progress(true, false, OutputProtocol::Json, true);
        assert!(!quiet.progress_enabled(), "--quiet must suppress redirected progress");
    }
}
