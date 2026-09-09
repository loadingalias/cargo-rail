//! Exact executable-byte identities with explicit runtime limitations.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use rscrypto::Sha256;
use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};
use crate::source::ContentDigest;

const EXECUTABLE_IDENTITY_VERSION: u32 = 1;
const MAX_INTERPRETER_DEPTH: usize = 4;
const MAX_RUNTIME_PROBE_BYTES: usize = 1024 * 1024;
const MAX_RUNTIME_IMAGES: usize = 4096;

/// Loader selection observed during one execution. A probe does not cover
/// libraries opened only by a later compilation or link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutableRuntimeSelection {
    files: Vec<PathBuf>,
    platform_images: Vec<String>,
    search_files: Vec<PathBuf>,
    missing_files: Vec<PathBuf>,
}

impl ExecutableRuntimeSelection {
    pub(crate) fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub(crate) fn inputs(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.iter().chain(&self.search_files)
    }

    pub(crate) fn missing_files(&self) -> &[PathBuf] {
        &self.missing_files
    }

    pub(crate) fn validate(&self) -> RailResult<()> {
        if self.files.is_empty()
            || self
                .files
                .len()
                .saturating_add(self.platform_images.len())
                .saturating_add(self.search_files.len())
                .saturating_add(self.missing_files.len())
                > MAX_RUNTIME_IMAGES
            || !self.files.windows(2).all(|pair| pair[0] < pair[1])
            || !self.platform_images.windows(2).all(|pair| pair[0] < pair[1])
            || !self.search_files.windows(2).all(|pair| pair[0] < pair[1])
            || !self.missing_files.windows(2).all(|pair| pair[0] < pair[1])
            || self.inputs().any(|path| self.missing_files.binary_search(path).is_ok())
            || self.inputs().chain(&self.missing_files).any(|path| {
                !path.is_absolute()
                    || path.as_os_str().as_encoded_bytes().len() > 4096
                    || path.as_os_str().as_encoded_bytes().contains(&0)
            })
            || self
                .platform_images
                .iter()
                .any(|image| image.len() > 4133 || image.as_bytes().contains(&0))
        {
            return Err(RailError::message("executable runtime selection is invalid"));
        }
        Ok(())
    }
}

/// Observe the loader's selected files while leaving the selected program's
/// information output separate. Callers must bind the returned file bytes.
/// Protected shared-cache images stay in the existing host-platform runtime
/// boundary; their loader identifiers describe selection, not byte identity.
pub(crate) fn observe_executable_runtime(
    program: &Path,
    arguments: &[OsString],
    current_dir: &Path,
) -> RailResult<(Output, ExecutableRuntimeSelection)> {
    if cfg!(target_os = "linux") {
        return observe_glibc_executable_runtime(program, arguments, current_dir);
    }
    if !cfg!(target_os = "macos") {
        return Err(RailError::message(
            "executable runtime observation is unavailable on this host",
        ));
    }
    // Apple's system ld is a developer-tool trampoline. Resolve the same
    // selected implementation before requesting loader diagnostics, which
    // the protected trampoline strips from its environment.
    let implementation = if program == Path::new("/usr/bin/ld") {
        let selected = Command::new("/usr/bin/xcrun").args(["--find", "ld"]).output()?;
        if !selected.status.success() || selected.stdout.len() > MAX_RUNTIME_PROBE_BYTES {
            return Err(RailError::message("developer linker implementation is unavailable"));
        }
        let path = std::str::from_utf8(&selected.stdout)
            .map_err(|_| RailError::message("developer linker implementation is not UTF-8"))?
            .trim();
        if !Path::new(path).is_absolute() || path == "/usr/bin/ld" {
            return Err(RailError::message("developer linker implementation is invalid"));
        }
        PathBuf::from(path)
    } else {
        program.to_path_buf()
    };
    let mut output = Command::new(&implementation)
        .args(arguments)
        .current_dir(current_dir)
        .env("DYLD_PRINT_LIBRARIES", "1")
        .env_remove("DYLD_PRINT_TO_FILE")
        .output()?;
    if !output.status.success()
        || output.stdout.len() > MAX_RUNTIME_PROBE_BYTES
        || output.stderr.len() > MAX_RUNTIME_PROBE_BYTES
    {
        return Err(RailError::message("executable runtime information probe failed"));
    }
    let (mut selection, stderr) = parse_macos_runtime_selection(&output.stderr)?;
    let canonical_program = crate::utils::canonicalize_existing(&implementation)?;
    if !selection.files.iter().any(|path| path == &canonical_program) {
        return Err(RailError::message(
            "executable runtime probe did not observe its selected program",
        ));
    }
    if implementation != program {
        selection.files.push(crate::utils::canonicalize_existing(program)?);
        selection.files.sort_unstable();
        selection.files.dedup();
    }
    output.stderr = stderr;
    Ok((output, selection))
}

fn observe_glibc_executable_runtime(
    program: &Path,
    arguments: &[OsString],
    current_dir: &Path,
) -> RailResult<(Output, ExecutableRuntimeSelection)> {
    if ["LD_AUDIT", "LD_PROFILE"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        return Err(RailError::message(
            "ELF runtime audit or profile observation is unavailable",
        ));
    }
    let trace = Command::new(program)
        .args(arguments)
        .current_dir(current_dir)
        .env("LD_TRACE_LOADED_OBJECTS", "1")
        .output()?;
    if !trace.status.success() || !trace.stderr.is_empty() || trace.stdout.len() > MAX_RUNTIME_PROBE_BYTES {
        return Err(RailError::message(
            "ELF loader did not expose its selected runtime files",
        ));
    }
    let selection = parse_glibc_runtime_selection(program, &trace.stdout)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(current_dir)
        .env_remove("LD_TRACE_LOADED_OBJECTS");
    let (output, runtime) = execute_glibc_runtime(command, selection)?;
    if !output.status.success()
        || output.stdout.len() > MAX_RUNTIME_PROBE_BYTES
        || output.stderr.len() > MAX_RUNTIME_PROBE_BYTES
    {
        return Err(RailError::message("executable runtime information probe failed"));
    }
    Ok((output, runtime?.selection))
}

/// Process entry programs and loader images observed during one command,
/// including completed dynamic children. Exec replacements belong to selection's
/// file closure. This does not observe arbitrary file I/O or static children.
#[derive(Debug)]
pub(crate) struct ExecutableRuntimeExecution {
    pub(crate) selection: ExecutableRuntimeSelection,
    /// Canonical root program first, followed by one entry per observed child.
    pub(crate) programs: Vec<PathBuf>,
}

/// Execute once with inherited stdin and captured output. Observation failure
/// cannot replace the command's status or diagnostics. `startup` must come from
/// a separate information probe; tracing the actual argv as a startup probe
/// could execute a static program twice.
pub(crate) fn execute_glibc_runtime(
    mut command: Command,
    startup: ExecutableRuntimeSelection,
) -> std::io::Result<(Output, RailResult<ExecutableRuntimeExecution>)> {
    let program = PathBuf::from(command.get_program());
    let prepared = (|| -> RailResult<tempfile::TempDir> {
        if !cfg!(target_os = "linux") {
            return Err(RailError::message(
                "glibc execution observation is unavailable on this host",
            ));
        }
        for name in [
            "LD_AUDIT",
            "LD_PROFILE",
            "LD_DEBUG",
            "LD_DEBUG_OUTPUT",
            "LD_TRACE_LOADED_OBJECTS",
        ] {
            let value = command
                .get_envs()
                .find_map(|(key, value)| (key == name).then(|| value.map(OsStr::to_os_string)))
                .unwrap_or_else(|| std::env::var_os(name));
            if value.is_some_and(|value| !value.is_empty()) {
                return Err(RailError::message(
                    "explicit ELF loader diagnostics prevent execution observation",
                ));
            }
        }
        startup.validate()?;
        if !startup.files.contains(&crate::utils::canonicalize_existing(&program)?) {
            return Err(RailError::message(
                "ELF startup selection does not bind the selected program",
            ));
        }
        let directory = tempfile::tempdir()?;
        command
            .env("LD_DEBUG", "files,libs")
            .env("LD_DEBUG_OUTPUT", directory.path().join("loader"));
        Ok(directory)
    })();
    let child = command
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let process = child.id();
    let output = child.wait_with_output()?;
    let runtime = prepared.and_then(|directory| collect_glibc_execution(&program, process, &directory, startup));
    Ok((output, runtime))
}

fn collect_glibc_execution(
    program: &Path,
    process: u32,
    directory: &tempfile::TempDir,
    selection: ExecutableRuntimeSelection,
) -> RailResult<ExecutableRuntimeExecution> {
    let mut paths = fs::read_dir(directory.path())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_unstable();
    let expected = directory.path().join(format!("loader.{process}"));
    if !paths.contains(&expected) || paths.len() > MAX_RUNTIME_IMAGES {
        return Err(RailError::message(
            "ELF runtime probe has no bounded root process trace",
        ));
    }
    let mut remaining = MAX_RUNTIME_PROBE_BYTES;
    let mut files = BTreeSet::new();
    let mut search_files = BTreeSet::new();
    let mut missing_files = BTreeSet::new();
    let mut programs = vec![crate::utils::canonicalize_existing(program)?];
    for path in &paths {
        let pid = path
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.strip_prefix("loader."))
            .and_then(|pid| pid.parse::<u32>().ok())
            .filter(|pid| *pid > 0 && *pid <= i32::MAX.cast_unsigned())
            .ok_or_else(|| RailError::message("ELF runtime trace has an invalid process identity"))?;
        if path.file_name().and_then(OsStr::to_str) != Some(format!("loader.{pid}").as_str()) {
            return Err(RailError::message(
                "ELF runtime trace process identity is not canonical",
            ));
        }
        if pid != process {
            require_finished_runtime_process(pid)?;
        }
        let generation = crate::utils::stable_file_generation(path)
            .ok_or_else(|| RailError::message("ELF runtime trace is not a stable real file"))?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.len() > remaining as u64 {
            return Err(RailError::message(
                "ELF runtime execution traces exceed their byte bound",
            ));
        }
        let mut bytes = Vec::new();
        File::open(path)?.take(remaining as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != metadata.len()
            || bytes.len() > remaining
            || crate::utils::stable_file_generation(path).as_ref() != Some(&generation)
        {
            return Err(RailError::message(
                "ELF runtime execution trace changed or exceeded its bound",
            ));
        }
        remaining -= bytes.len();
        let text =
            std::str::from_utf8(&bytes).map_err(|_| RailError::message("ELF child runtime trace is not UTF-8"))?;
        let mut records = BTreeMap::<u32, String>::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let (identity, _) = line
                .trim_start_matches(' ')
                .split_once(":\t")
                .ok_or_else(|| RailError::message("ELF runtime trace has no process identity"))?;
            let identity = identity
                .parse::<u32>()
                .ok()
                .filter(|pid| *pid > 0 && *pid <= i32::MAX.cast_unsigned())
                .ok_or_else(|| RailError::message("ELF runtime trace has an invalid process identity"))?;
            let record = records.entry(identity).or_default();
            record.push_str(line);
            record.push('\n');
            if records.len() > MAX_RUNTIME_IMAGES {
                return Err(RailError::message("ELF runtime process selection exceeds its bound"));
            }
        }
        let owner = records
            .remove(&pid)
            .ok_or_else(|| RailError::message("ELF runtime trace has no owning process"))?;
        let child;
        let selected_program = if pid == process {
            program
        } else {
            let prefix = format!("{pid}:\tinitialize program: ");
            child = owner
                .lines()
                .find_map(|line| line.trim_start_matches(' ').strip_prefix(&prefix))
                .filter(|path| Path::new(path).is_absolute())
                .map(PathBuf::from)
                .ok_or_else(|| RailError::message("ELF runtime child has no observed absolute program"))?;
            programs.push(crate::utils::canonicalize_existing(&child)?);
            &child
        };
        let startup = if pid == process {
            selection.clone()
        } else {
            ExecutableRuntimeSelection {
                files: vec![crate::utils::canonicalize_existing(selected_program)?],
                platform_images: Vec::new(),
                search_files: Vec::new(),
                missing_files: Vec::new(),
            }
        };
        let selected = parse_glibc_execution_selection(selected_program, pid, owner.as_bytes(), startup)?;
        for (forked, records) in records {
            require_finished_runtime_process(forked)?;
            let forked =
                parse_glibc_process_selection(selected_program, forked, records.as_bytes(), selected.clone(), true)?;
            files.extend(forked.files);
            search_files.extend(forked.search_files);
            missing_files.extend(forked.missing_files);
        }
        files.extend(selected.files);
        search_files.extend(selected.search_files);
        missing_files.extend(selected.missing_files);
        if files.len() > MAX_RUNTIME_IMAGES {
            return Err(RailError::message("ELF runtime image selection exceeds its bound"));
        }
    }
    let mut final_paths = fs::read_dir(directory.path())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    final_paths.sort_unstable();
    if final_paths != paths {
        return Err(RailError::message("ELF runtime process traces changed during capture"));
    }
    let selection = ExecutableRuntimeSelection {
        files: files.into_iter().collect(),
        platform_images: selection.platform_images,
        search_files: search_files.into_iter().collect(),
        missing_files: missing_files.into_iter().collect(),
    };
    selection.validate()?;
    let execution = ExecutableRuntimeExecution { selection, programs };
    if execution.programs.first() != Some(&crate::utils::canonicalize_existing(program)?)
        || execution
            .programs
            .iter()
            .any(|program| !execution.selection.files.contains(program))
    {
        return Err(RailError::message(
            "ELF program selection changed during runtime capture",
        ));
    }
    Ok(execution)
}

fn require_finished_runtime_process(process: u32) -> RailResult<()> {
    let path = format!("/proc/{process}/stat");
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = String::new();
    file.by_ref().take(4097).read_to_string(&mut bytes)?;
    if bytes.len() <= 4096
        && bytes
            .rsplit_once(") ")
            .is_some_and(|(_, fields)| matches!(fields.split_ascii_whitespace().next(), Some("Z" | "X")))
    {
        return Ok(());
    }
    Err(RailError::message(
        "ELF runtime probe left a live or unverifiable child",
    ))
}

fn parse_glibc_runtime_selection(program: &Path, bytes: &[u8]) -> RailResult<ExecutableRuntimeSelection> {
    let text = std::str::from_utf8(bytes).map_err(|_| RailError::message("ELF loader selection is not UTF-8"))?;
    let mut files = BTreeSet::from([crate::utils::canonicalize_existing(program)?]);
    let mut platform_images = BTreeSet::new();
    let mut loader = false;
    for line in text.lines() {
        let record = line
            .strip_prefix('\t')
            .ok_or_else(|| RailError::message("ELF loader record is invalid"))?;
        let (selection, address) = record
            .rsplit_once(" (0x")
            .ok_or_else(|| RailError::message("ELF loader address is unavailable"))?;
        if !address
            .strip_suffix(')')
            .is_some_and(|address| !address.is_empty() && address.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(RailError::message("ELF loader address is invalid"));
        }
        let path = if let Some((_, path)) = selection.split_once(" => ") {
            path
        } else if matches!(selection, "linux-vdso.so.1" | "linux-gate.so.1") {
            platform_images.insert(selection.to_string());
            continue;
        } else {
            loader = true;
            selection
        };
        if !Path::new(path).is_absolute() || path.len() > 4096 || path.as_bytes().contains(&0) {
            return Err(RailError::message("ELF loader selected an unresolved runtime file"));
        }
        files.insert(crate::utils::canonicalize_existing(Path::new(path))?);
        if files.len().saturating_add(platform_images.len()) > MAX_RUNTIME_IMAGES {
            return Err(RailError::message("ELF runtime selection exceeds its bound"));
        }
    }
    if !loader || files.len() < 2 {
        return Err(RailError::message("ELF loader implementation was not observed"));
    }
    let selection = ExecutableRuntimeSelection {
        files: files.into_iter().collect(),
        platform_images: platform_images.into_iter().collect(),
        search_files: Vec::new(),
        missing_files: Vec::new(),
    };
    selection.validate()?;
    Ok(selection)
}

fn parse_glibc_execution_selection(
    program: &Path,
    process: u32,
    bytes: &[u8],
    startup: ExecutableRuntimeSelection,
) -> RailResult<ExecutableRuntimeSelection> {
    let invalid = || RailError::message("ELF runtime exec transition is incomplete or unsupported");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let prefix = format!("{process}:\tinitialize program: ");
    let images = text
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            line.trim_start_matches(' ')
                .strip_prefix(&prefix)
                .map(|path| (index, Path::new(path)))
        })
        .collect::<Vec<_>>();
    if images.len() <= 1 {
        return parse_glibc_process_selection(program, process, bytes, startup, false);
    }
    if images.len() > MAX_RUNTIME_IMAGES || images[0].1 != program {
        return Err(invalid());
    }
    let lines = text.lines().collect::<Vec<_>>();
    let file_prefix = format!("{process}:\tfile=");
    let mut boundaries = vec![0];
    for pair in images.windows(2) {
        let [(previous_index, previous), (index, next)] = pair else {
            return Err(invalid());
        };
        if !next.is_absolute() {
            return Err(invalid());
        }
        let transfer = format!("{process}:\ttransferring control: {}", previous.display());
        let transferred = lines[*previous_index..*index]
            .iter()
            .position(|line| line.trim_start_matches(' ') == transfer)
            .map(|offset| previous_index + offset + 1)
            .ok_or_else(invalid)?;
        // A new dynamic image starts with its own loader dependency requests,
        // before its initialization record. Do not merge its maps with the old
        // image: that would let an earlier initialization certify a later map.
        let request = format!(" [0];  needed by {} [0]", next.display());
        let boundary = lines[transferred..*index]
            .iter()
            .position(|line| {
                line.trim_start_matches(' ')
                    .strip_prefix(&file_prefix)
                    .is_some_and(|record| record.ends_with(&request))
            })
            .map(|offset| transferred + offset)
            .ok_or_else(invalid)?;
        boundaries.push(boundary);
    }
    boundaries.push(lines.len());
    let mut combined = ExecutableRuntimeSelection {
        files: Vec::new(),
        platform_images: startup.platform_images.clone(),
        search_files: Vec::new(),
        missing_files: Vec::new(),
    };
    for (index, (_, image)) in images.iter().enumerate() {
        let initial = if index == 0 {
            startup.clone()
        } else {
            ExecutableRuntimeSelection {
                files: vec![crate::utils::canonicalize_existing(image)?],
                platform_images: Vec::new(),
                search_files: Vec::new(),
                missing_files: Vec::new(),
            }
        };
        let frame = lines[boundaries[index]..boundaries[index + 1]].join("\n");
        let selected = parse_glibc_process_selection(image, process, frame.as_bytes(), initial, false)?;
        combined.files.extend(selected.files);
        combined.search_files.extend(selected.search_files);
        combined.missing_files.extend(selected.missing_files);
    }
    for paths in [
        &mut combined.files,
        &mut combined.search_files,
        &mut combined.missing_files,
    ] {
        paths.sort_unstable();
        paths.dedup();
    }
    combined.validate()?;
    Ok(combined)
}

fn parse_glibc_process_selection(
    program: &Path,
    process: u32,
    bytes: &[u8],
    startup: ExecutableRuntimeSelection,
    forked: bool,
) -> RailResult<ExecutableRuntimeSelection> {
    let invalid = || RailError::message("ELF runtime execution trace is incomplete or unsupported");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let prefix = format!("{process}:\t");
    let mut files = if forked {
        startup.files.iter().cloned().collect()
    } else {
        BTreeSet::from([crate::utils::canonicalize_existing(program)?])
    };
    let mut initialized = BTreeSet::new();
    if forked {
        for path in &files {
            initialized.insert(path.as_os_str().to_os_string());
            initialized.insert(path.file_name().ok_or_else(invalid)?.to_os_string());
        }
    }
    let mut search_paths = BTreeSet::new();
    let mut searched = BTreeSet::new();
    let mut searching = None::<String>;
    let mut mappings = BTreeSet::new();
    let mut dynamic_requests = Vec::new();
    let mut started = forked;
    let mut transferred = forked;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let record = line.trim_start_matches(' ').strip_prefix(&prefix).ok_or_else(invalid)?;
        if let Some(path) = record.strip_prefix("calling init: ") {
            let path = Path::new(path);
            if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > 4096 {
                return Err(invalid());
            }
            initialized.insert(path.as_os_str().to_os_string());
            initialized.insert(path.file_name().ok_or_else(invalid)?.to_os_string());
            files.insert(crate::utils::canonicalize_existing(path)?);
        } else if let Some(path) = record.strip_prefix("initialize program: ") {
            if started || Path::new(path) != program {
                return Err(invalid());
            }
            started = true;
        } else if let Some(path) = record.strip_prefix("transferring control: ") {
            if !started || transferred || Path::new(path) != program {
                return Err(invalid());
            }
            transferred = true;
        } else if let Some(record) = record.strip_prefix("file=") {
            let (name, event) = record.split_once(" [0];  ").ok_or_else(invalid)?;
            if name.is_empty() || name.len() > 4096 {
                return Err(invalid());
            }
            if event == "generating link map" {
                if !mappings.insert(OsString::from(name)) {
                    return Err(invalid());
                }
            } else if let Some(requester) = event.strip_prefix("dynamically loaded by ") {
                if !requester.ends_with(" [0]") || initialized.contains(OsStr::new(name)) {
                    return Err(invalid());
                }
                dynamic_requests.push(OsString::from(name));
            } else if event != "destroying link map"
                && !event
                    .strip_prefix("needed by ")
                    .is_some_and(|value| value.ends_with(" [0]"))
            {
                return Err(invalid());
            }
        } else if let Some(name) = record
            .strip_prefix("find library=")
            .and_then(|value| value.strip_suffix(" [0]; searching"))
        {
            if name.is_empty() || name.len() > 4096 {
                return Err(invalid());
            }
            searching = Some(name.to_string());
        } else if let Some(path) = record.strip_prefix("  trying file=") {
            let path = PathBuf::from(path);
            let name = searching.as_ref().ok_or_else(invalid)?;
            if !path.is_absolute() {
                return Err(invalid());
            }
            searched.insert(OsString::from(name));
            search_paths.insert(path);
        } else if let Some(path) = record.strip_prefix(" search cache=") {
            if !Path::new(path).is_absolute() {
                return Err(invalid());
            }
            search_paths.insert(PathBuf::from(path));
        } else if record.starts_with(" search path=") {
        } else if let Some(opened) = record.strip_prefix("opening file=") {
            let (path, count) = opened.split_once(" [0]; direct_opencount=").ok_or_else(invalid)?;
            let path = Path::new(path);
            if !path.is_absolute()
                || !initialized.contains(path.as_os_str())
                || count.parse::<u32>().ok().is_none_or(|count| count == 0)
            {
                return Err(invalid());
            }
            // Constructors can nest dlopen calls. Only a matching successful
            // open closes a request; an already initialized basename does not.
            if let Some(requested) = dynamic_requests.pop()
                && requested != path.as_os_str()
                && Some(requested.as_os_str()) != path.file_name()
            {
                return Err(invalid());
            }
        } else if record.is_empty()
            || record.starts_with("  dynamic: ")
            || record.starts_with("    entry: ")
            || record
                .strip_prefix("calling fini: ")
                .is_some_and(|value| value.ends_with(" [0]"))
            || record
                .strip_prefix("calling preinit: ")
                .is_some_and(|value| Path::new(value) == program)
            || record
                .strip_prefix("closing file=")
                .is_some_and(|value| value.contains(" [0]; direct_opencount="))
        {
        } else {
            return Err(invalid());
        }
        if files.len() > MAX_RUNTIME_IMAGES
            || mappings.len() > MAX_RUNTIME_IMAGES
            || dynamic_requests.len() > MAX_RUNTIME_IMAGES
            || search_paths.len() > MAX_RUNTIME_IMAGES
        {
            return Err(invalid());
        }
    }
    // A startup listing never executes dlopen. Every mapped library must also
    // reach the loader's initialization boundary, including libraries with no
    // constructor, before its selected file can enter this execution witness.
    // Failed requests require their actual search attempts and remain distinct
    // from maps whose initialization or successful-open records are missing.
    for request in &dynamic_requests {
        if mappings
            .iter()
            .any(|mapping| mapping == request || Path::new(mapping).file_name() == Some(request.as_os_str()))
        {
            return Err(invalid());
        }
        if Path::new(request).is_absolute() {
            search_paths.insert(PathBuf::from(request));
        } else if !searched.contains(request) {
            return Err(invalid());
        }
    }
    if !started
        || !transferred
        || (!forked && mappings.is_empty())
        || !mappings.is_subset(&initialized)
        || !startup.files.iter().all(|path| files.contains(path))
    {
        return Err(invalid());
    }
    let mut search_files = Vec::new();
    let mut missing_files = Vec::new();
    for path in search_paths {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => search_files.push(path),
            Err(error) if error.kind() == ErrorKind::NotFound => missing_files.push(path),
            Ok(_) => return Err(invalid()),
            Err(error) => return Err(error.into()),
        }
    }
    let selection = ExecutableRuntimeSelection {
        files: files.into_iter().collect(),
        platform_images: startup.platform_images,
        search_files,
        missing_files,
    };
    selection.validate()?;
    Ok(selection)
}

fn parse_macos_runtime_selection(bytes: &[u8]) -> RailResult<(ExecutableRuntimeSelection, Vec<u8>)> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| RailError::message("executable runtime probe output is not UTF-8"))?;
    let mut files = BTreeSet::new();
    let mut platform_images = BTreeSet::new();
    let mut image_names = BTreeSet::new();
    let mut stderr = Vec::new();
    for line in text.split_inclusive('\n') {
        let Some(record) = line.strip_prefix("dyld[") else {
            stderr.extend_from_slice(line.as_bytes());
            continue;
        };
        let (_, record) = record
            .split_once("]: ")
            .filter(|(pid, _)| !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or_else(|| RailError::message("executable runtime loader record is invalid"))?;
        if let Some(name) = record
            .trim_end_matches('\n')
            .strip_prefix("move loaded to delayed: ")
            .or_else(|| record.trim_end_matches('\n').strip_prefix("move delayed to loaded: "))
        {
            if !image_names.contains(name) {
                return Err(RailError::message("loader delayed an unobserved runtime image"));
            }
            continue;
        }
        let record = record
            .strip_prefix('<')
            .ok_or_else(|| RailError::message("executable runtime image record is invalid"))?;
        let (identifier, path) = record
            .trim_end_matches('\n')
            .split_once("> ")
            .ok_or_else(|| RailError::message("executable runtime image record is invalid"))?;
        if identifier.len() != 36
            || identifier.bytes().enumerate().any(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte != b'-'
                } else {
                    !byte.is_ascii_hexdigit()
                }
            })
            || path.is_empty()
            || path.len() > 4096
            || path.as_bytes().contains(&0)
            || !Path::new(path).is_absolute()
        {
            return Err(RailError::message("executable runtime image selection is invalid"));
        }
        match fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {
                files.insert(crate::utils::canonicalize_existing(Path::new(path))?);
            }
            Err(error)
                if error.kind() == ErrorKind::NotFound
                    && (Path::new(path).starts_with("/usr/lib") || Path::new(path).starts_with("/System/Library")) =>
            {
                platform_images.insert(format!("{identifier} {path}"));
            }
            _ => {
                return Err(RailError::message("executable runtime selected an unverifiable image"));
            }
        }
        if let Some(name) = Path::new(path).file_name().and_then(OsStr::to_str) {
            image_names.insert(name.to_string());
        }
        if files.len().saturating_add(platform_images.len()) > MAX_RUNTIME_IMAGES {
            return Err(RailError::message(
                "executable runtime image selection exceeds its bound",
            ));
        }
    }
    if files.is_empty() {
        return Err(RailError::message("executable runtime probe exposed no file images"));
    }
    Ok((
        ExecutableRuntimeSelection {
            files: files.into_iter().collect(),
            platform_images: platform_images.into_iter().collect(),
            search_files: Vec::new(),
            missing_files: Vec::new(),
        },
        stderr,
    ))
}

/// Exact bytes selected for one process executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecutableIdentity {
    version: u32,
    selection: String,
    resolved_path: String,
    content_digest: String,
    executable: bool,
    interpreter: Option<Box<Self>>,
    limitations: BTreeSet<String>,
}

/// Executables captured once for one immutable workspace snapshot.
#[derive(Debug, Clone)]
pub(crate) struct ToolchainExecutableIdentities {
    cargo: ExecutableIdentity,
    rustc: ExecutableIdentity,
    rustdoc: Option<ExecutableIdentity>,
    rustc_wrapper: Option<ExecutableIdentity>,
    rustc_workspace_wrapper: Option<ExecutableIdentity>,
    cargo_implementation: Option<ExecutableIdentity>,
    rustc_implementation: Option<ExecutableIdentity>,
    rustdoc_implementation: Option<ExecutableIdentity>,
    limitations: BTreeSet<String>,
}

impl ExecutableIdentity {
    /// Resolve and digest the executable that `Command` would select.
    pub(crate) fn capture(selection: &OsStr, current_dir: &Path, source_root: &Path) -> RailResult<Self> {
        capture_at_depth(selection, current_dir, source_root, 0)
    }

    /// Stable bytes suitable for a larger framed identity.
    pub(crate) fn identity_bytes(&self) -> RailResult<Vec<u8>> {
        serde_json::to_vec(self).map_err(Into::into)
    }

    /// Fail-closed limitations that prevent this executable from proving an exact process identity.
    pub(crate) fn limitations(&self) -> impl Iterator<Item = &str> {
        self.limitations.iter().map(String::as_str)
    }

    pub(crate) fn content_digest(&self) -> &str {
        &self.content_digest
    }

    pub(crate) fn same_resolved_file(&self, other: &Self) -> bool {
        self.resolved_path == other.resolved_path && self.content_digest == other.content_digest
    }

    /// Recover the selected command spelling for a separately revalidated capture.
    /// The final symlink and basename remain intact for multicall executables.
    pub(crate) fn selected_program(&self, source_root: &Path) -> RailResult<OsString> {
        let invalid = || RailError::message("captured executable selection is invalid");
        if self.version != EXECUTABLE_IDENTITY_VERSION || !source_root.is_absolute() {
            return Err(invalid());
        }
        let (kind, spelling) = self.selection.split_once(':').ok_or_else(invalid)?;
        let path = Path::new(spelling);
        if spelling.is_empty()
            || spelling.contains('\0')
            || path.file_name().is_none()
            || path.components().any(|component| component == Component::ParentDir)
        {
            return Err(invalid());
        }
        let selected = match kind {
            "path"
                if matches!(path.components().next(), Some(Component::Normal(_))) && path.components().count() == 1 =>
            {
                path.to_path_buf()
            }
            "host" if path.is_absolute() => path.to_path_buf(),
            "repository" if path.is_relative() && !path.has_root() => {
                crate::source::RepositoryPath::new(path)?;
                source_root.join(path)
            }
            _ => return Err(invalid()),
        };
        if !resolve_program(selected.as_os_str(), source_root)?.is_file() {
            return Err(RailError::message(
                "captured executable selection no longer resolves to a file",
            ));
        }
        Ok(selected.into_os_string())
    }
}

/// Resolve the selected command path without following its final symlink.
///
/// Preserving the selected path matters for multicall executables such as the
/// Rustup proxies: the basename controls which tool the executable dispatches.
pub(crate) fn resolve_executable_selection(selection: &OsStr, current_dir: &Path) -> RailResult<PathBuf> {
    resolve_program(selection, current_dir)
}

/// Resolve the canonical executable bytes behind one command selection.
/// Callers must preserve the selected path separately when its basename can
/// affect process behavior.
pub(crate) fn resolve_executable_path(selection: &OsStr, current_dir: &Path) -> RailResult<PathBuf> {
    crate::utils::canonicalize_existing(&resolve_executable_selection(selection, current_dir)?).map_err(Into::into)
}

impl ToolchainExecutableIdentities {
    pub(crate) fn capture(
        toolchain: &crate::cargo::ToolchainIdentity,
        current_dir: &Path,
        source_root: &Path,
    ) -> RailResult<Self> {
        let mut captured = std::collections::BTreeMap::<(PathBuf, std::ffi::OsString), ExecutableIdentity>::new();
        let mut capture = |program: &OsStr| -> RailResult<ExecutableIdentity> {
            let selected = resolve_program(program, current_dir)?;
            selected.canonicalize().map_err(|error| {
                RailError::message(format!(
                    "failed to resolve executable '{}': {error}",
                    program.to_string_lossy()
                ))
            })?;
            // The exact argv[0] selection is behavior. Multicall programs such
            // as rustup dispatch by basename, and arbitrary executables may
            // distinguish different spellings of the same selected path.
            let key = (selected, program.to_os_string());
            if let Some(identity) = captured.get(&key) {
                return Ok(identity.clone());
            }
            let identity = ExecutableIdentity::capture(program, current_dir, source_root)?;
            captured.insert(key, identity.clone());
            Ok(identity)
        };
        let cargo = capture(toolchain.cargo_program())?;
        let rustc = capture(toolchain.rustc_program())?;
        // rustdoc is part of the selected Rust distribution even when the current
        // action compiles rather than documents. Native-cache keys bind the
        // complete Cargo/rustc/rustdoc trio.
        let rustdoc = Some(capture(toolchain.rustdoc_program())?);
        let rustc_wrapper = toolchain.rustc_wrapper_program().map(&mut capture).transpose()?;
        let rustc_workspace_wrapper = toolchain
            .rustc_workspace_wrapper_program()
            .map(&mut capture)
            .transpose()?;
        let mut limitations = BTreeSet::new();
        let mut implementation =
            |name: &'static str| match capture(sysroot_program(toolchain.rustc_sysroot(), name).as_os_str()) {
                Ok(identity) => Some(identity),
                Err(_) => {
                    limitations.insert(format!("{name}_implementation_executable_unavailable"));
                    None
                }
            };
        let cargo_implementation = implementation("cargo");
        let rustc_implementation = implementation("rustc");
        let rustdoc_implementation = Some(implementation("rustdoc")).flatten();
        for (role, executable) in [
            ("cargo", Some(&cargo)),
            ("rustc", Some(&rustc)),
            ("rustdoc", rustdoc.as_ref()),
            ("rustc_wrapper", rustc_wrapper.as_ref()),
            ("rustc_workspace_wrapper", rustc_workspace_wrapper.as_ref()),
            ("cargo_implementation", cargo_implementation.as_ref()),
            ("rustc_implementation", rustc_implementation.as_ref()),
            ("rustdoc_implementation", rustdoc_implementation.as_ref()),
        ] {
            if let Some(executable) = executable {
                limitations.extend(
                    executable
                        .limitations()
                        .map(|limitation| format!("{role}_{limitation}")),
                );
            }
        }
        Ok(Self {
            cargo,
            rustc,
            rustdoc,
            rustc_wrapper,
            rustc_workspace_wrapper,
            cargo_implementation,
            rustc_implementation,
            rustdoc_implementation,
            limitations,
        })
    }

    pub(crate) fn rustc(&self) -> &ExecutableIdentity {
        &self.rustc
    }

    pub(crate) fn rustdoc(&self) -> Option<&ExecutableIdentity> {
        self.rustdoc.as_ref()
    }

    pub(crate) fn rustc_wrapper(&self) -> Option<&ExecutableIdentity> {
        self.rustc_wrapper.as_ref()
    }

    pub(crate) fn rustc_workspace_wrapper(&self) -> Option<&ExecutableIdentity> {
        self.rustc_workspace_wrapper.as_ref()
    }

    pub(crate) fn rustc_implementation(&self) -> Option<&ExecutableIdentity> {
        self.rustc_implementation.as_ref()
    }

    pub(crate) fn limitations(&self) -> impl Iterator<Item = &str> {
        self.limitations.iter().map(String::as_str)
    }

    pub(crate) fn identity_bytes(&self) -> RailResult<Vec<u8>> {
        serde_json::to_vec(&(
            &self.cargo,
            &self.rustc,
            &self.rustdoc,
            &self.rustc_wrapper,
            &self.rustc_workspace_wrapper,
            &self.cargo_implementation,
            &self.rustc_implementation,
            &self.rustdoc_implementation,
            &self.limitations,
        ))
        .map_err(Into::into)
    }
}

pub(crate) fn sysroot_program(sysroot: &Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    let name = format!("{name}.exe");
    sysroot.join("bin").join(name)
}

fn capture_at_depth(
    selection: &OsStr,
    current_dir: &Path,
    source_root: &Path,
    depth: usize,
) -> RailResult<ExecutableIdentity> {
    if depth > MAX_INTERPRETER_DEPTH {
        return Err(RailError::message(format!(
            "executable interpreter chain exceeds {MAX_INTERPRETER_DEPTH} entries"
        )));
    }
    let selection_text = selection
        .to_str()
        .ok_or_else(|| RailError::message("executable selection is not valid UTF-8"))?;
    if selection_text.is_empty() {
        return Err(RailError::message("executable selection is empty"));
    }
    let resolved = resolve_program(selection, current_dir)?;
    let canonical = resolved.canonicalize().map_err(|error| {
        RailError::message(format!(
            "failed to resolve executable '{}': {error}",
            resolved.display()
        ))
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        RailError::message(format!(
            "failed to inspect executable '{}': {error}",
            canonical.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(RailError::message(format!(
            "selected executable '{}' is not a regular file",
            canonical.display()
        )));
    }
    let (content_digest, shebang) = digest_executable(&canonical)?;
    let executable = is_executable(&metadata);
    let mut limitations = BTreeSet::new();
    let interpreter = if let Some(shebang) = shebang {
        limitations.insert("script_runtime_inputs_unavailable".to_string());
        match shebang_interpreter(&shebang)? {
            Some(interpreter) if is_env_interpreter(&interpreter) => {
                limitations.insert("env_shebang_resolution_unavailable".to_string());
                None
            }
            Some(interpreter) => Some(Box::new(capture_at_depth(
                OsStr::new(&interpreter),
                canonical.parent().unwrap_or(current_dir),
                source_root,
                depth + 1,
            )?)),
            None => {
                limitations.insert("script_interpreter_identity_unavailable".to_string());
                None
            }
        }
    } else {
        limitations.insert("dynamic_executable_inputs_unavailable".to_string());
        None
    };
    if let Some(interpreter) = &interpreter {
        limitations.extend(interpreter.limitations.iter().cloned());
    }

    Ok(ExecutableIdentity {
        version: EXECUTABLE_IDENTITY_VERSION,
        selection: portable_selection(selection_text, current_dir, source_root),
        resolved_path: portable_path(&canonical, source_root),
        content_digest: format!("sha256:{content_digest}"),
        executable,
        interpreter,
        limitations,
    })
}

fn digest_executable(path: &Path) -> RailResult<(ContentDigest, Option<Vec<u8>>)> {
    let mut file = File::open(path)
        .map_err(|error| RailError::message(format!("failed to read executable '{}': {error}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut prefix = [0_u8; 2];
    let mut prefix_len = 0;
    while prefix_len < prefix.len() {
        let read = read_executable_chunk(&mut file, &mut prefix[prefix_len..], path)?;
        if read == 0 {
            break;
        }
        prefix_len += read;
    }
    hasher.update(&prefix[..prefix_len]);
    let mut bytes_read = prefix_len;
    let mut shebang = (prefix_len == prefix.len() && prefix == *b"#!").then(Vec::new);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = read_executable_chunk(&mut file, &mut buffer, path)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes_read = bytes_read.saturating_add(read);
        if let Some(line) = &mut shebang
            && !line.ends_with(b"\n")
        {
            let end = buffer[..read]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(read, |newline| newline + 1);
            line.extend_from_slice(&buffer[..end]);
        }
    }
    crate::instrumentation::record_hash(bytes_read);
    crate::instrumentation::record_hashed_file_bytes_read(bytes_read);
    Ok((ContentDigest::from_sha256_bytes(hasher.finalize()), shebang))
}

fn read_executable_chunk(file: &mut File, buffer: &mut [u8], path: &Path) -> RailResult<usize> {
    loop {
        match file.read(buffer) {
            Ok(read) => return Ok(read),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(RailError::message(format!(
                    "failed to read executable '{}': {error}",
                    path.display()
                )));
            }
        }
    }
}

pub(crate) fn resolve_program(selection: &OsStr, current_dir: &Path) -> RailResult<PathBuf> {
    let selected = Path::new(selection);
    if selected.is_absolute() || selected.components().count() > 1 {
        return Ok(if selected.is_absolute() {
            selected.to_path_buf()
        } else {
            current_dir.join(selected)
        });
    }

    let path = std::env::var_os("PATH").ok_or_else(|| {
        RailError::message(format!(
            "cannot resolve executable '{}' because PATH is absent",
            selected.display()
        ))
    })?;
    for directory in std::env::split_paths(&path) {
        for candidate in program_candidates(&directory, selection) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(RailError::message(format!(
        "executable '{}' was not found in PATH",
        selected.display()
    )))
}

#[cfg(not(windows))]
fn program_candidates(directory: &Path, selection: &OsStr) -> Vec<PathBuf> {
    vec![directory.join(selection)]
}

#[cfg(windows)]
fn program_candidates(directory: &Path, selection: &OsStr) -> Vec<PathBuf> {
    let selected = Path::new(selection);
    if selected.extension().is_some() {
        return vec![directory.join(selected)];
    }
    let extensions = std::env::var_os("PATHEXT").unwrap_or_else(|| std::ffi::OsString::from(".COM;.EXE;.BAT;.CMD"));
    extensions
        .to_string_lossy()
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| directory.join(format!("{}{}", selected.to_string_lossy(), extension)))
        .collect()
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

fn shebang_interpreter(shebang: &[u8]) -> RailResult<Option<String>> {
    let line = shebang.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let line = std::str::from_utf8(line)
        .map_err(|_| RailError::message("script shebang is not valid UTF-8"))?
        .trim();
    Ok(line.split_ascii_whitespace().next().map(str::to_string))
}

fn is_env_interpreter(interpreter: &str) -> bool {
    Path::new(interpreter).file_name().is_some_and(|name| name == "env")
}

fn portable_selection(selection: &str, current_dir: &Path, source_root: &Path) -> String {
    let path = Path::new(selection);
    if path.is_absolute() {
        portable_path(path, source_root)
    } else if path.components().count() > 1 {
        portable_path(&current_dir.join(path), source_root)
    } else {
        format!("path:{selection}")
    }
}

fn portable_path(path: &Path, source_root: &Path) -> String {
    let repository_relative = path.strip_prefix(source_root).ok().or_else(|| {
        let canonical_root = source_root.canonicalize().ok()?;
        path.strip_prefix(canonical_root).ok()
    });
    repository_relative
        .map(|relative| format!("repository:{}", crate::utils::path_to_git_format(relative)))
        .unwrap_or_else(|| format!("host:{}", crate::utils::path_to_git_format(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn runtime_probe_observes_dlopen_and_constructor_loaded_libraries() {
        let root = tempfile::tempdir().expect("runtime fixture");
        let leaf = root.path().join("libleaf.so");
        let plugin = root.path().join("libplugin.so");
        let program = root.path().join("probe");
        let launcher = root.path().join("launcher");
        fs::write(root.path().join("leaf.c"), "int leaf(void) { return 7; }\n").expect("leaf source");
        fs::write(root.path().join("plugin.c"), format!(
            "#include <dlfcn.h>\n#include <stdlib.h>\n__attribute__((constructor)) static void initialize(void) {{ if (!dlopen({}, RTLD_NOW)) abort(); }}\n",
            serde_json::to_string(leaf.to_str().expect("leaf path")).expect("C path")
        )).expect("plugin source");
        fs::write(root.path().join("main.c"), format!(
            "#include <dlfcn.h>\n#include <stdio.h>\nint main(int argc, char **argv) {{ if (!dlopen({}, RTLD_NOW)) return 1; if (argc == 2 && dlopen(argv[1], RTLD_NOW)) return 2; puts(\"ordinary output\"); fputs(\"ordinary diagnostic\\n\", stderr); return 0; }}\n",
            serde_json::to_string(plugin.to_str().expect("plugin path")).expect("C path")
        )).expect("program source");
        fs::write(root.path().join("launcher.c"), format!(
            "#include <unistd.h>\n#include <sys/wait.h>\nint main(void) {{ int status; pid_t child = fork(); if (child < 0) return 1; if (child == 0) {{ execl({path}, {path}, (char *)0); _exit(1); }} if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return 1; return WEXITSTATUS(status); }}\n",
            path = serde_json::to_string(program.to_str().expect("program path")).expect("C path")
        )).expect("launcher source");
        for (source, output, shared) in [
            ("leaf.c", &leaf, true),
            ("plugin.c", &plugin, true),
            ("main.c", &program, false),
            ("launcher.c", &launcher, false),
        ] {
            let mut command = Command::new("cc");
            if shared {
                command.args(["-shared", "-fPIC"]);
            }
            let output = command
                .arg(root.path().join(source))
                .arg("-o")
                .arg(output)
                .arg("-ldl")
                .output()
                .expect("C compiler");
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }
        let missing_name = OsString::from("libleaf.so");
        let ordinary = Command::new(&program)
            .arg(&missing_name)
            .output()
            .expect("failed lookup");
        assert!(ordinary.status.success(), "the bare name must remain unresolved");
        assert_eq!(ordinary.stdout, b"ordinary output\n");
        assert_eq!(ordinary.stderr, b"ordinary diagnostic\n");
        observe_executable_runtime(&program, &[missing_name], root.path())
            .expect_err("a loaded basename cannot prove a later failed lookup succeeded");
        for executable in [&program, &launcher] {
            let ordinary = Command::new(executable).output().expect("ordinary program");
            assert!(ordinary.status.success());
            let (observed, selection) =
                observe_executable_runtime(executable, &[], root.path()).expect("runtime execution");
            assert_eq!(observed.status.code(), ordinary.status.code());
            assert_eq!(observed.stdout, ordinary.stdout);
            assert_eq!(observed.stderr, ordinary.stderr);
            for path in [executable, &program, &plugin, &leaf] {
                assert!(
                    selection
                        .files()
                        .contains(&crate::utils::canonicalize_existing(path).expect("selected file")),
                    "missing {} in {selection:?}",
                    path.display()
                );
            }
        }
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn runtime_probe_binds_failed_library_searches_in_a_completed_fork() {
        let root = tempfile::tempdir().expect("fork runtime fixture");
        let program = root.path().join("forker");
        let source = root.path().join("forker.c");
        let library = root.path().join("librail_optional.so");
        fs::write(
            &source,
            r#"#include <dlfcn.h>
#include <unistd.h>
#include <sys/wait.h>
int main(void) {
    pid_t child = fork();
    if (child < 0) return 91;
    if (!child) {
        char result = dlopen("librail_optional.so", RTLD_NOW) ? '1' : '0';
        _exit(write(1, &result, 1) == 1 ? 0 : 92);
    }
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return 93;
    return WEXITSTATUS(status);
}
"#,
        )
        .expect("fork source");
        let compiled = Command::new("cc")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .arg(format!("-Wl,-rpath,{}", root.path().display()))
            .arg("-ldl")
            .output()
            .expect("C compiler");
        assert!(compiled.status.success(), "{compiled:?}");
        let run = |expected: &[u8]| {
            let ordinary = Command::new(&program).output().expect("ordinary fork");
            assert!(ordinary.status.success(), "{ordinary:?}");
            assert_eq!(ordinary.stdout, expected);
            assert!(ordinary.stderr.is_empty());
            let (observed, selected) =
                observe_executable_runtime(&program, &[], root.path()).expect("fork observation");
            assert_eq!(observed.status.code(), ordinary.status.code());
            assert_eq!(observed.stdout, ordinary.stdout);
            assert_eq!(observed.stderr, ordinary.stderr);
            selected
        };
        let absent = run(b"0");
        assert!(
            absent.missing_files.contains(&library),
            "the attempted absent candidate must be bound"
        );
        assert!(!absent.files.contains(&library));
        fs::write(root.path().join("optional.c"), "int optional(void) { return 7; }\n").expect("library source");
        let compiled = Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(root.path().join("optional.c"))
            .arg("-o")
            .arg(&library)
            .output()
            .expect("shared library compiler");
        assert!(compiled.status.success(), "{compiled:?}");
        let present = run(b"1");
        assert!(!present.missing_files.contains(&library));
        assert!(
            present
                .files
                .contains(&crate::utils::canonicalize_existing(&library).expect("selected library"))
        );
        assert_ne!(
            present, absent,
            "a newly loadable candidate invalidates the runtime selection"
        );
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn runtime_execution_preserves_failure_after_incomplete_observation() {
        let root = tempfile::tempdir().expect("execution fixture");
        let source = root.path().join("failure.c");
        let program = root.path().join("failure");
        let marker = root.path().join("executions");
        fs::write(
            &source,
            r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--version") == 0) { puts("fixture version"); return 0; }
    if (argc != 3) return 91;
    FILE *marker = fopen(argv[2], "a");
    if (!marker) return 92;
    fputs("executed once\n", marker);
    fclose(marker);
    if (strcmp(argv[1], "--erase-trace") == 0 && getenv("LD_DEBUG_OUTPUT")) {
        char path[4096];
        snprintf(path, sizeof(path), "%s.%ld", getenv("LD_DEBUG_OUTPUT"), (long)getpid());
        unlink(path);
    }
    puts("ordinary stdout");
    fputs("ordinary stderr\n", stderr);
    return 23;
}
"#,
        )
        .expect("native source");
        let compiled = Command::new("cc")
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .output()
            .expect("C compiler");
        assert!(compiled.status.success(), "{compiled:?}");
        let (_, startup) = observe_executable_runtime(&program, &[OsString::from("--version")], root.path())
            .expect("fixture startup runtime");
        for mode in ["--explicit-loader-output", "--erase-trace"] {
            let command = || {
                let mut command = Command::new(&program);
                command.arg(mode).arg(&marker);
                if mode == "--explicit-loader-output" {
                    command.env("LD_DEBUG_OUTPUT", root.path().join("user-loader-output"));
                }
                command
            };
            fs::write(&marker, b"").expect("reset ordinary marker");
            let ordinary = command().output().expect("ordinary execution");
            assert_eq!(ordinary.status.code(), Some(23));
            assert_eq!(ordinary.stdout, b"ordinary stdout\n");
            assert_eq!(ordinary.stderr, b"ordinary stderr\n");
            assert_eq!(fs::read(&marker).expect("ordinary marker"), b"executed once\n");
            fs::write(&marker, b"").expect("reset observed marker");
            let (observed, runtime) = execute_glibc_runtime(command(), startup.clone()).expect("observed execution");
            assert!(runtime.is_err(), "{mode}: incomplete observation must reject");
            assert_eq!(observed.status.code(), ordinary.status.code());
            assert_eq!(observed.stdout, ordinary.stdout);
            assert_eq!(observed.stderr, ordinary.stderr);
            assert_eq!(fs::read(&marker).expect("observed marker"), b"executed once\n");
        }
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    #[ignore = "requires GCC, collect2 and liblto_plugin; run the GCC runtime contract explicitly"]
    fn runtime_probe_observes_collect2_child_and_gcc_linker_plugin() {
        let root = tempfile::tempdir().expect("GCC runtime fixture");
        let gcc = resolve_executable_selection(OsStr::new("gcc"), root.path()).expect("GCC");
        let selected = |argument: &str| {
            let output = Command::new(&gcc).arg(argument).output().expect("GCC selection");
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).expect("GCC path").trim().to_string()
        };
        let collect2 = resolve_executable_selection(OsStr::new(&selected("-print-prog-name=collect2")), root.path())
            .expect("collect2");
        let linker =
            resolve_executable_selection(OsStr::new(&selected("-print-prog-name=ld")), root.path()).expect("linker");
        let plugin = PathBuf::from(selected("-print-file-name=liblto_plugin.so"));
        assert!(
            plugin.is_absolute() && plugin.is_file(),
            "GCC plugin prerequisite: {}",
            plugin.display()
        );
        for (program, arguments, required) in [
            (&collect2, vec![OsString::from("--version")], &linker),
            (
                &linker,
                vec![
                    OsString::from("-plugin"),
                    plugin.as_os_str().to_os_string(),
                    OsString::from("--version"),
                ],
                &plugin,
            ),
        ] {
            let ordinary = Command::new(program).args(&arguments).output().expect("ordinary tool");
            assert!(ordinary.status.success(), "{ordinary:?}");
            let (observed, selection) =
                observe_executable_runtime(program, &arguments, root.path()).expect("GCC runtime observation");
            assert_eq!(observed.status.code(), ordinary.status.code());
            assert_eq!(observed.stdout, ordinary.stdout);
            assert_eq!(observed.stderr, ordinary.stderr);
            assert!(
                selection
                    .files()
                    .contains(&crate::utils::canonicalize_existing(required).expect("required runtime")),
                "{selection:?}"
            );
        }
        use std::os::unix::fs::PermissionsExt as _;

        let source = root.path().join("native.c");
        let object = root.path().join("native.o");
        let library = root.path().join("libnative.so");
        fs::write(&source, "unsigned runtime_value(void) { return 7; }\n").expect("C source");
        let compiled = Command::new(&gcc)
            .args(["-fPIC", "-c"])
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .output()
            .expect("compile C object");
        assert!(compiled.status.success(), "{compiled:?}");
        let (_, startup) =
            observe_executable_runtime(&gcc, &[OsString::from("--version")], root.path()).expect("GCC startup runtime");
        for missing in [false, true] {
            let command = || {
                let mut command = Command::new(&gcc);
                command
                    .current_dir(root.path())
                    .args(["-shared", "-nodefaultlibs"])
                    .arg(&object)
                    .arg("-o")
                    .arg(&library);
                if missing {
                    command.arg("-Wl,-l:__cargo_rail_runtime_missing__");
                }
                command
            };
            let ordinary = command().output().expect("ordinary GCC link");
            assert_eq!(ordinary.status.success(), !missing, "{ordinary:?}");
            let expected = if missing {
                assert!(!library.exists());
                assert!(String::from_utf8_lossy(&ordinary.stderr).contains("__cargo_rail_runtime_missing__"));
                None
            } else {
                let bytes = fs::read(&library).expect("ordinary shared library");
                let mode = fs::metadata(&library).expect("ordinary mode").permissions().mode();
                fs::remove_file(&library).expect("remove ordinary output");
                Some((bytes, mode))
            };
            let (observed, runtime) = execute_glibc_runtime(command(), startup.clone()).expect("execute GCC link");
            assert_eq!(observed.status.code(), ordinary.status.code());
            assert_eq!(observed.stdout, ordinary.stdout);
            assert_eq!(observed.stderr, ordinary.stderr);
            let mut runtime = runtime.expect("actual GCC link runtime");
            runtime.programs.sort_unstable();
            let mut programs = [&gcc, &collect2, &linker]
                .map(|path| crate::utils::canonicalize_existing(path).expect("selected program"));
            programs.sort_unstable();
            assert_eq!(runtime.programs, programs, "actual GCC/collect2/linker executions");
            assert!(
                runtime
                    .selection
                    .files()
                    .contains(&fs::canonicalize(&plugin).expect("plugin path"))
            );
            if let Some((bytes, mode)) = expected {
                assert_eq!(fs::read(&library).expect("observed library"), bytes);
                assert_eq!(
                    fs::metadata(&library).expect("observed mode").permissions().mode(),
                    mode
                );
            } else {
                assert!(!library.exists(), "failed linking must not leave a library");
            }
        }
    }

    #[test]
    fn runtime_execution_rejects_uninitialized_maps_and_ambiguous_processes() {
        let root = tempfile::tempdir().expect("runtime records");
        let program = root.path().join("program");
        let library = root.path().join("library.so");
        fs::write(&program, b"program").expect("program");
        fs::write(&library, b"library").expect("library");
        let mut files = vec![
            crate::utils::canonicalize_existing(&program).expect("program"),
            crate::utils::canonicalize_existing(&library).expect("library"),
        ];
        files.sort_unstable();
        let startup = ExecutableRuntimeSelection {
            files,
            platform_images: Vec::new(),
            search_files: Vec::new(),
            missing_files: Vec::new(),
        };
        let trace = format!(
            "42:\tfile=library.so [0];  generating link map\n42:\tcalling init: {}\n42:\tinitialize program: {}\n42:\ttransferring control: {}\n",
            library.display(),
            program.display(),
            program.display()
        );
        parse_glibc_execution_selection(&program, 42, trace.as_bytes(), startup.clone()).expect("complete execution");
        let requested = format!(
            "42:\tfile=library.so [0];  dynamically loaded by {} [0]\n{trace}",
            program.display()
        );
        let opened = format!(
            "{requested}42:\topening file={} [0]; direct_opencount=1\n",
            library.display()
        );
        parse_glibc_execution_selection(&program, 42, opened.as_bytes(), startup.clone())
            .expect("completed dynamic load");
        for changed in [
            requested,
            opened.replace("direct_opencount=1", "direct_opencount=0"),
            trace.replace(&format!("42:\tcalling init: {}\n", library.display()), ""),
            trace.replace("42:", "43:"),
            trace.replace(" [0];", " [1];"),
            format!("{trace}42:\tfile=library.so [0];  generating link map\n"),
            format!("{trace}42:\tinitialize program: {}\n", program.display()),
            format!("{trace}42:\tfile=unobserved.so [0];  generating link map\n"),
            format!(
                "{trace}42:\tfile=missing.so [0];  dynamically loaded by {} [0]\n",
                program.display()
            ),
        ] {
            parse_glibc_execution_selection(&program, 42, changed.as_bytes(), startup.clone())
                .expect_err("incomplete or unsupported execution");
        }
    }

    #[test]
    fn runtime_exec_images_require_independent_loader_initialization() {
        let root = tempfile::tempdir().expect("runtime records");
        let program = root.path().join("wrapper");
        let replacement = root.path().join("implementation");
        let library = root.path().join("library.so");
        for path in [&program, &replacement, &library] {
            fs::write(path, b"image").expect("runtime image");
        }
        let startup = ExecutableRuntimeSelection {
            files: vec![crate::utils::canonicalize_existing(&program).expect("wrapper")],
            platform_images: Vec::new(),
            search_files: Vec::new(),
            missing_files: Vec::new(),
        };
        let wrapper = format!(
            "42:\tfile=library.so [0];  generating link map\n42:\tcalling init: {}\n42:\tinitialize program: {}\n42:\ttransferring control: {}\n",
            library.display(),
            program.display(),
            program.display()
        );
        let implementation = format!(
            "42:\tfile=library.so [0];  needed by {} [0]\n42:\tfile=library.so [0];  generating link map\n42:\tcalling init: {}\n42:\tinitialize program: {}\n42:\ttransferring control: {}\n",
            replacement.display(),
            library.display(),
            replacement.display(),
            replacement.display()
        );
        let trace = format!("{wrapper}{implementation}");
        let selected = parse_glibc_execution_selection(&program, 42, trace.as_bytes(), startup.clone())
            .expect("complete exec transition");
        let mut expected = [&program, &replacement, &library]
            .map(|path| crate::utils::canonicalize_existing(path).expect("canonical image"));
        expected.sort_unstable();
        assert_eq!(selected.files(), expected);
        let missing_init = implementation.replace(&format!("42:\tcalling init: {}\n", library.display()), "");
        parse_glibc_execution_selection(
            &program,
            42,
            format!("{wrapper}{missing_init}").as_bytes(),
            startup.clone(),
        )
        .expect_err("wrapper initialization cannot certify the replacement's library map");
        let missing_transfer =
            implementation.replace(&format!("42:\ttransferring control: {}\n", replacement.display()), "");
        parse_glibc_execution_selection(&program, 42, format!("{wrapper}{missing_transfer}").as_bytes(), startup)
            .expect_err("replacement must reach execution");
    }

    #[test]
    fn runtime_loader_transitions_require_an_already_observed_image() {
        let directory = tempfile::tempdir().expect("runtime fixture");
        let file = directory.path().join("selected-runtime.dylib");
        fs::write(&file, b"runtime file").expect("runtime file");
        let record = format!("dyld[123]: <00000000-0000-0000-0000-000000000001> {}\n", file.display());
        let trace = format!(
            "{record}dyld[123]: move loaded to delayed: selected-runtime.dylib\ndyld[123]: move delayed to loaded: selected-runtime.dylib\nordinary tool diagnostic\n"
        );
        let (selection, stderr) = parse_macos_runtime_selection(trace.as_bytes()).expect("known loader transitions");
        assert_eq!(
            selection.files(),
            [crate::utils::canonicalize_existing(&file).expect("canonical runtime")]
        );
        assert_eq!(stderr, b"ordinary tool diagnostic\n");
        assert_eq!(
            parse_macos_runtime_selection(
                format!("{record}dyld[123]: move loaded to delayed: unobserved.dylib\n").as_bytes()
            )
            .expect_err("transition cannot invent a loaded image")
            .to_string(),
            "loader delayed an unobserved runtime image"
        );
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn runtime_loader_revalidates_an_earlier_library_search_candidate() {
        let root = tempfile::tempdir().expect("runtime lookup fixture");
        let earlier = root.path().join("earlier");
        let later = root.path().join("later");
        fs::create_dir(&earlier).expect("earlier search directory");
        fs::create_dir(&later).expect("later search directory");
        fs::write(
            root.path().join("library.c"),
            b"int fixture_value(void) { return 0; }\n",
        )
        .expect("library source");
        fs::write(
            root.path().join("main.c"),
            b"int fixture_value(void); int main(void) { return fixture_value(); }\n",
        )
        .expect("program source");
        let library = later.join("libfixture.so");
        let program = root.path().join("probe");
        let linked = Command::new("cc")
            .current_dir(root.path())
            .args(["-shared", "-fPIC", "library.c", "-o"])
            .arg(&library)
            .output()
            .expect("compile runtime fixture");
        assert!(linked.status.success(), "runtime fixture library failed: {linked:?}");
        let linked = Command::new("cc")
            .current_dir(root.path())
            .arg("main.c")
            .arg("-L")
            .arg(&later)
            .arg("-lfixture")
            .arg(format!("-Wl,-rpath,{}:{}", earlier.display(), later.display()))
            .arg("-o")
            .arg(&program)
            .output()
            .expect("compile runtime program");
        assert!(linked.status.success(), "runtime fixture program failed: {linked:?}");
        let (_, before) = observe_executable_runtime(&program, &[OsString::from("--version")], root.path())
            .expect("initial runtime selection");
        assert!(
            before
                .files()
                .contains(&crate::utils::canonicalize_existing(&library).expect("selected library"))
        );
        let new_library = earlier.join("libfixture.so");
        fs::copy(&library, &new_library).expect("same-byte earlier candidate");
        let (_, after) = observe_executable_runtime(&program, &[OsString::from("--version")], root.path())
            .expect("changed runtime selection");
        assert!(
            after
                .files()
                .contains(&crate::utils::canonicalize_existing(&new_library).expect("new selected library"))
        );
        assert!(
            !after
                .files()
                .contains(&crate::utils::canonicalize_existing(&library).expect("old library remains"))
        );
        assert_ne!(
            before, after,
            "same bytes at an earlier lookup path must change runtime evidence"
        );
    }

    #[test]
    fn selected_program_preserves_captured_path_relative_and_absolute_selections() {
        let directory = tempfile::tempdir().expect("source root");
        let external = tempfile::tempdir().expect("host root");
        fs::create_dir(directory.path().join("tools")).expect("tool directory");
        let repository_program = directory.path().join("tools/rustc");
        let host_program = external.path().join("rustc");
        fs::write(&repository_program, b"repository compiler").expect("repository compiler");
        fs::write(&host_program, b"host compiler").expect("host compiler");
        for (selection, expected) in [
            (OsString::from("rustc"), OsString::from("rustc")),
            (
                OsString::from("tools/rustc"),
                repository_program.clone().into_os_string(),
            ),
            (
                repository_program.clone().into_os_string(),
                repository_program.into_os_string(),
            ),
            (host_program.clone().into_os_string(), host_program.into_os_string()),
        ] {
            let captured =
                ExecutableIdentity::capture(&selection, directory.path(), directory.path()).expect("capture selection");
            assert_eq!(
                Path::new(&captured.selected_program(directory.path()).expect("recover selection")),
                Path::new(&expected)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn selected_program_keeps_the_multicall_symlink_basename() {
        let directory = tempfile::tempdir().expect("source root");
        let implementation = directory.path().join("rustup");
        let selected = directory.path().join("rustc");
        fs::write(&implementation, b"multicall compiler").expect("compiler implementation");
        std::os::unix::fs::symlink(&implementation, &selected).expect("compiler selection");
        let captured = ExecutableIdentity::capture(selected.as_os_str(), directory.path(), directory.path())
            .expect("capture compiler");

        assert_eq!(
            captured.selected_program(directory.path()).expect("recover compiler"),
            selected
        );
        assert_ne!(
            captured.selected_program(directory.path()).expect("recover compiler"),
            implementation
        );
    }

    #[test]
    fn selected_program_rejects_malformed_authority_and_missing_files() {
        let directory = tempfile::tempdir().expect("source root");
        let selected = directory.path().join("rustc");
        fs::write(&selected, b"compiler").expect("compiler");
        let captured = ExecutableIdentity::capture(selected.as_os_str(), directory.path(), directory.path())
            .expect("capture compiler");
        for selection in [
            "rustc",
            "unknown:rustc",
            "path:",
            "path:tools/rustc",
            "path:../rustc",
            "host:rustc",
            "repository:/rustc",
            "repository:../rustc",
            "repository:tools/../../rustc",
            "repository:rustc\0",
        ] {
            let mut invalid = captured.clone();
            invalid.selection = selection.to_string();
            assert!(
                invalid.selected_program(directory.path()).is_err(),
                "accepted {selection:?}"
            );
        }
        let mut invalid = captured.clone();
        invalid.version = EXECUTABLE_IDENTITY_VERSION + 1;
        invalid
            .selected_program(directory.path())
            .expect_err("unknown identity version");
        captured
            .selected_program(Path::new("relative-root"))
            .expect_err("relative source root");
        fs::remove_file(selected).expect("remove compiler");
        captured
            .selected_program(directory.path())
            .expect_err("missing compiler");
    }

    #[test]
    fn executable_identity_uses_logical_repository_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let program = directory.path().join("tool");
        fs::write(&program, b"native-tool").expect("write tool");
        let identity = ExecutableIdentity::capture(&program.into_os_string(), directory.path(), directory.path())
            .expect("capture executable");
        let serialized = String::from_utf8(identity.identity_bytes().expect("identity bytes")).expect("utf8 JSON");

        assert!(serialized.contains("repository:tool"));
        assert!(!serialized.contains(&directory.path().to_string_lossy().to_string()));
    }

    #[test]
    fn executable_identity_changes_with_exact_bytes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let program = directory.path().join("tool");
        fs::write(&program, b"first").expect("write tool");
        let first = ExecutableIdentity::capture(program.as_os_str(), directory.path(), directory.path())
            .expect("first identity");
        fs::write(&program, b"second").expect("mutate tool");
        let second = ExecutableIdentity::capture(program.as_os_str(), directory.path(), directory.path())
            .expect("second identity");

        assert_ne!(first, second);
    }

    #[test]
    fn executable_digest_streams_exact_bytes_and_retains_only_the_shebang() {
        let directory = tempfile::tempdir().expect("tempdir");
        let program = directory.path().join("tool");
        for (bytes, expected_digest, expected_shebang) in [
            (
                b"#".to_vec(),
                "334359b90efed75da5f0ada1d5e6b256f4a6bd0aee7eb39c0f90182a021ffc8b",
                None,
            ),
            (
                b"#!/bin/sh\npayload\xff".to_vec(),
                "86b8ff2539328098b7ddcfee07768fe37e41ad84df6d334056cd3e5dbf2c6e9b",
                Some(b"/bin/sh\n".to_vec()),
            ),
            (
                vec![0x5a; 256 * 1024 + 1],
                "b0630119a35d42df473390bebf77bc16837f0590058d3be38b538bd5dc5e3415",
                None,
            ),
        ] {
            fs::write(&program, &bytes).expect("write tool");
            let (digest, shebang) = digest_executable(&program).expect("digest executable");
            assert_eq!(digest.to_string(), expected_digest);
            assert_eq!(ContentDigest::sha256(&bytes).to_string(), expected_digest);
            assert_eq!(shebang, expected_shebang);
        }
    }
}
