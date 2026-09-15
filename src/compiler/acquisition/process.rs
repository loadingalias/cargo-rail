//! Owned process trees for one Cargo acquisition view.

use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicBool};

const GRACEFUL_TERMINATION: Duration = Duration::from_secs(2);
const TERMINATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessTermination {
    pub(crate) forced: bool,
    pub(crate) elapsed: Duration,
}

#[derive(Debug)]
pub(crate) struct BoundedProcessOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Run one owned process tree with fixed output and elapsed-time bounds.
pub(crate) fn run_bounded_process(
    command: &mut Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
) -> io::Result<BoundedProcessOutput> {
    let mut process = ProcessTree::spawn(command)?;
    let stdout = process.take_stdout()?;
    let stderr = process.take_stderr()?;
    let stdout_reader = thread::Builder::new()
        .name("cargo-rail-bounded-stdout".to_string())
        .spawn(move || read_bounded_stream(stdout, stdout_limit, false))?;
    let stderr_reader = match thread::Builder::new()
        .name("cargo-rail-bounded-stderr".to_string())
        .spawn(move || read_bounded_stream(stderr, stderr_limit, true))
    {
        Ok(reader) => reader,
        Err(error) => {
            drop(process.terminate());
            drop(stdout_reader.join());
            return Err(error);
        }
    };
    let deadline = Instant::now() + timeout;
    let status = loop {
        match process.try_wait() {
            Ok(Some(status)) => break process.finish(status),
            Ok(None) => {}
            Err(error) => {
                drop(process.terminate());
                break Err(error);
            }
        }
        if process.cancellation_requested() {
            process.terminate()?;
            break Err(io::Error::new(io::ErrorKind::Interrupted, "process cancelled"));
        }
        if Instant::now() >= deadline {
            process.terminate()?;
            break Err(io::Error::new(io::ErrorKind::TimedOut, "process exceeded its deadline"));
        }
        thread::sleep(TERMINATION_POLL_INTERVAL);
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("bounded stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("bounded stderr reader panicked"))??;
    Ok(BoundedProcessOutput {
        status: status?,
        stdout,
        stderr,
    })
}

fn read_bounded_stream(mut reader: impl io::Read, limit: usize, retain_tail: bool) -> io::Result<Vec<u8>> {
    let mut retained = Vec::with_capacity(limit.min(16 * 1024));
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        if limit == 0 {
            continue;
        }
        if retain_tail {
            if read >= limit {
                retained.clear();
                retained.extend_from_slice(&chunk[read - limit..read]);
            } else {
                let overflow = retained.len().saturating_add(read).saturating_sub(limit);
                if overflow > 0 {
                    retained.drain(..overflow);
                }
                retained.extend_from_slice(&chunk[..read]);
            }
        } else if retained.len() < limit {
            let remaining = limit - retained.len();
            retained.extend_from_slice(&chunk[..read.min(remaining)]);
        }
    }
    Ok(retained)
}

/// One direct Cargo child and every descendant it creates.
#[derive(Debug)]
pub(crate) struct ProcessTree {
    child: Child,
    #[cfg(unix)]
    process_group: rustix::process::Pid,
    #[cfg(windows)]
    job: crate::windows_fs::ProcessJob,
    resolved: bool,
}

impl ProcessTree {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;

            let _ = cancellation_flag()?;
            command.process_group(0);
            let child = command.spawn()?;
            let process_id = i32::try_from(child.id())
                .map_err(|_| io::Error::other("Cargo process identifier exceeds the Unix pid range"))?;
            let process_group = rustix::process::Pid::from_raw(process_id).ok_or_else(|| {
                io::Error::other("Cargo returned a process identifier that cannot name a Unix process group")
            })?;
            Ok(Self {
                child,
                process_group,
                resolved: false,
            })
        }

        #[cfg(windows)]
        {
            let (child, job) = crate::windows_fs::spawn_in_process_job(command)?;
            Ok(Self {
                child,
                job,
                resolved: false,
            })
        }

        #[cfg(not(any(unix, windows)))]
        {
            let child = command.spawn()?;
            Ok(Self { child, resolved: false })
        }
    }

    pub(crate) fn take_stdout(&mut self) -> io::Result<ChildStdout> {
        self.child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("Cargo compiler acquisition has no stdout pipe"))
    }

    pub(crate) fn take_stderr(&mut self) -> io::Result<ChildStderr> {
        self.child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("Cargo compiler acquisition has no stderr pipe"))
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub(crate) fn cancellation_requested(&self) -> bool {
        #[cfg(unix)]
        {
            CANCELLATION
                .get()
                .is_some_and(|cancellation| cancellation.load(std::sync::atomic::Ordering::SeqCst))
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Resolve a normally exited direct child after its process tree drains.
    pub(crate) fn finish(&mut self, status: ExitStatus) -> io::Result<ExitStatus> {
        if self.wait_tree_empty(GRACEFUL_TERMINATION)? {
            self.resolved = true;
            return Ok(status);
        }

        let termination = self.terminate()?;
        Err(io::Error::other(format!(
            "Cargo exited while descendant processes remained; the process tree was terminated{} after {:.3}s",
            if termination.forced { " forcibly" } else { "" },
            termination.elapsed.as_secs_f64()
        )))
    }

    /// Terminate every live member, reap the direct child, and prove the tree empty.
    pub(crate) fn terminate(&mut self) -> io::Result<ProcessTermination> {
        let started = Instant::now();
        let forced;

        #[cfg(unix)]
        {
            let mut required_force = false;
            signal_group(self.process_group, rustix::process::Signal::TERM).map_err(|error| {
                io::Error::new(error.kind(), format!("sending SIGTERM to Cargo process group: {error}"))
            })?;
            if !self.wait_tree_empty(GRACEFUL_TERMINATION)? {
                required_force = true;
                signal_group(self.process_group, rustix::process::Signal::KILL).map_err(|error| {
                    io::Error::new(error.kind(), format!("sending SIGKILL to Cargo process group: {error}"))
                })?;
                if !self.wait_tree_empty(GRACEFUL_TERMINATION)? {
                    return Err(io::Error::other(
                        "Cargo process tree remained live after forced termination",
                    ));
                }
            }
            forced = required_force;
        }

        #[cfg(windows)]
        {
            forced = true;
            self.job.terminate(1)?;
            if !self.wait_tree_empty(GRACEFUL_TERMINATION)? {
                return Err(io::Error::other(
                    "Cargo process tree remained live after forced termination",
                ));
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            forced = true;
            self.child.kill()?;
            if !self.wait_tree_empty(GRACEFUL_TERMINATION)? {
                return Err(io::Error::other("Cargo process remained live after forced termination"));
            }
        }

        self.resolved = true;
        let termination = ProcessTermination {
            forced,
            elapsed: started.elapsed(),
        };
        crate::instrumentation::record_compiler_acquisition_process_termination(
            termination.forced,
            termination.elapsed,
        );
        Ok(termination)
    }

    fn wait_tree_empty(&mut self, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            let direct_child_reaped = self.child.try_wait()?.is_some();
            if direct_child_reaped && self.tree_is_empty()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(TERMINATION_POLL_INTERVAL);
        }
    }

    fn tree_is_empty(&self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            match rustix::process::test_kill_process_group(self.process_group) {
                Ok(()) => Ok(false),
                Err(rustix::io::Errno::SRCH) => Ok(true),
                // macOS may transiently report EPERM while a SIGKILLed group
                // is leaving the process table. It is not proof of absence;
                // keep waiting and fail the bounded termination if it persists.
                Err(rustix::io::Errno::PERM) => Ok(false),
                Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error())),
            }
        }
        #[cfg(windows)]
        {
            self.job.is_empty()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(true)
        }
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        #[cfg(unix)]
        drop(signal_group(self.process_group, rustix::process::Signal::KILL));
        #[cfg(windows)]
        drop(self.job.terminate(1));
        #[cfg(not(any(unix, windows)))]
        drop(self.child.kill());
        drop(self.wait_tree_empty(GRACEFUL_TERMINATION));
    }
}

#[cfg(unix)]
static CANCELLATION: OnceLock<Arc<AtomicBool>> = OnceLock::new();
#[cfg(unix)]
static CANCELLATION_INIT: Mutex<()> = Mutex::new(());

#[cfg(unix)]
fn cancellation_flag() -> io::Result<&'static Arc<AtomicBool>> {
    if let Some(cancellation) = CANCELLATION.get() {
        return Ok(cancellation);
    }
    let _guard = CANCELLATION_INIT
        .lock()
        .map_err(|_| io::Error::other("compiler acquisition cancellation initialization was poisoned"))?;
    if let Some(cancellation) = CANCELLATION.get() {
        return Ok(cancellation);
    }
    let cancellation = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register_conditional_default(signal, Arc::clone(&cancellation))?;
        signal_hook::flag::register(signal, Arc::clone(&cancellation))?;
    }
    CANCELLATION
        .set(cancellation)
        .map_err(|_| io::Error::other("compiler acquisition cancellation initialized twice"))?;
    CANCELLATION
        .get()
        .ok_or_else(|| io::Error::other("compiler acquisition cancellation disappeared"))
}

#[cfg(unix)]
fn signal_group(group: rustix::process::Pid, signal: rustix::process::Signal) -> io::Result<()> {
    match rustix::process::kill_process_group(group, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error())),
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessTree;
    #[cfg(unix)]
    use super::run_bounded_process;
    use std::process::Command;

    #[test]
    fn normally_exited_process_tree_is_empty() {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/c", "exit", "0"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        };
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn process tree");
        let status = loop {
            if let Some(status) = tree.try_wait().expect("wait process tree") {
                break status;
            }
            std::thread::yield_now();
        };
        assert!(tree.finish(status).expect("finish process tree").success());
    }

    #[test]
    fn normally_exited_process_tree_allows_short_lived_descendants_to_drain() {
        #[cfg(windows)]
        let directory = tempfile::tempdir().expect("Windows process-tree fixture");
        #[cfg(windows)]
        let mut command = {
            let script = directory.path().join("draining-descendant.cmd");
            std::fs::write(
                &script,
                "@echo off\r\nstart \"\" /b ping -n 2 127.0.0.1 >nul\r\nexit /b 0\r\n",
            )
            .expect("Windows draining-descendant script");
            Command::new(script)
        };
        #[cfg(unix)]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "(sleep 1) &"]);
            command
        };
        #[cfg(not(any(unix, windows)))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        };

        let mut tree = ProcessTree::spawn(&mut command).expect("spawn draining process tree");
        let status = loop {
            if let Some(status) = tree.try_wait().expect("wait for direct process") {
                break status;
            }
            std::thread::yield_now();
        };
        assert!(tree.finish(status).expect("drain process tree").success());
    }

    #[cfg(unix)]
    #[test]
    fn termination_reaches_descendants_that_ignore_graceful_shutdown() {
        let directory = tempfile::tempdir().expect("process-tree fixture");
        let marker = directory.path().join("descendant-started");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("trap '' TERM; (trap '' TERM; : > \"$1\"; while :; do sleep 1; done) & wait")
            .arg("cargo-rail-process-tree-test")
            .arg(&marker);
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn descendant tree");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(marker.is_file(), "descendant did not start");
        let termination = tree.terminate().expect("terminate descendant tree");
        assert!(termination.forced, "TERM-ignoring descendant did not require SIGKILL");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_runner_retains_stdout_prefix_and_stderr_tail() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 0123456789; printf abcdefghij >&2"]);
        let output =
            run_bounded_process(&mut command, std::time::Duration::from_secs(5), 4, 4).expect("bounded process");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"0123");
        assert_eq!(output.stderr, b"ghij");
    }

    #[cfg(windows)]
    #[test]
    fn windows_job_termination_reaches_spawned_descendants() {
        let directory = tempfile::tempdir().expect("Windows process-tree fixture");
        let marker = directory.path().join("descendant-started");
        let script = directory.path().join("descendant.cmd");
        std::fs::write(
            &script,
            format!(
                "@echo off\r\necho ready>\"{}\"\r\nstart \"\" /b ping -t 127.0.0.1 >nul\r\nping -t 127.0.0.1 >nul\r\n",
                marker.display()
            ),
        )
        .expect("Windows descendant script");
        let mut command = Command::new(&script);
        let mut tree = ProcessTree::spawn(&mut command).expect("spawn Windows descendant tree");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(marker.is_file(), "Windows descendant did not start");
        let termination = tree.terminate().expect("terminate Windows descendant tree");
        assert!(termination.forced, "Windows Job Object termination must be explicit");
    }
}
