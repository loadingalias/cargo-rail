//! Input bytes reported by the selected COFF linker through LLD_REPRODUCE.

use super::*;

pub(crate) const ADAPTER_ENV: &str = "CARGO_RAIL_COFF_LINK_ADAPTER";
pub(crate) const DRIVER_ENV: &str = "CARGO_RAIL_COFF_LINK_DRIVER";
pub(crate) const ARCHIVE_ENV: &str = "CARGO_RAIL_COFF_LINK_ARCHIVE";
pub(crate) const EVIDENCE_ENV: &str = "CARGO_RAIL_COFF_LINK_EVIDENCE";
pub(super) const ARCHIVE_FILE: &str = "coff-linker-inputs.tar";
pub(super) const EVIDENCE_FILE: &str = "coff-linker-driver-inputs.json";
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverEvidence {
    version: u32,
    completed: bool,
    current_directory: String,
    driver: LinkFileWitness,
    driver_generation: Option<String>,
    arguments: Vec<String>,
    response_files: Vec<LinkResponseFileWitness>,
    driver_probe: LinkDriverProbe,
    runtime_files: Vec<LinkFileWitness>,
}

pub(super) fn selected_driver(arguments: &[String], current_directory: &Path) -> RailResult<(String, PathBuf)> {
    let mut selection = None;
    let mut flavor = None;
    let mut index = 0;
    while index < arguments.len() {
        if let Some(option) = short_option_value(&arguments[index], arguments.get(index + 1).map(String::as_str), "-C")
        {
            if let Some(value) = option.strip_prefix("linker=") {
                selection = Some(value.to_string());
            }
            if let Some(value) = option.strip_prefix("linker-flavor=") {
                flavor = Some(value);
            }
        }
        index += if arguments[index] == "-C" { 2 } else { 1 };
    }
    let selection =
        selection.ok_or_else(|| RailError::message("COFF observation requires an explicit lld-link selection"))?;
    let path = crate::executable::resolve_executable_selection(OsStr::new(&selection), current_directory)?;
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if !matches!(name, "lld-link" | "lld-link.exe" | "rust-lld" | "rust-lld.exe")
        || flavor.is_some_and(|flavor| !matches!(flavor, "lld-link" | "msvc-lld"))
        || ["LLD_REPRODUCE", "LINK", "_LINK_"]
            .iter()
            .any(|name| std::env::var_os(name).is_some())
    {
        return Err(RailError::message("selected COFF linker observation is unavailable"));
    }
    Ok((selection, path))
}

pub(super) fn prepare(command: &mut Command, observation: &RawCompilerInvocation, directory: &Path) {
    let prepared = (|| -> RailResult<()> {
        let (_, driver) = selected_driver(&observation.compiler_arguments, &std::env::current_dir()?)?;
        let adapter = std::env::current_exe()?;
        let archive = directory.join(ARCHIVE_FILE);
        let evidence = directory.join(EVIDENCE_FILE);
        if !adapter.is_absolute() || archive.try_exists()? || evidence.try_exists()? {
            return Err(RailError::message("COFF observation paths are unavailable"));
        }
        command
            .arg(format!("-Clinker={}", adapter.display()))
            .arg("-Clinker-flavor=lld-link")
            .env(ADAPTER_ENV, "1")
            .env(DRIVER_ENV, driver)
            .env(ARCHIVE_ENV, archive)
            .env(EVIDENCE_ENV, evidence);
        Ok(())
    })();
    if let Err(error) = prepared {
        report_native_action_diagnostic("COFF linker setup", &error);
    }
}

fn evidence_paths() -> RailResult<(PathBuf, PathBuf)> {
    let archive = std::env::var_os(ARCHIVE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| RailError::message("COFF archive binding is unavailable"))?;
    let evidence = std::env::var_os(EVIDENCE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| RailError::message("COFF driver binding is unavailable"))?;
    let parent = evidence
        .parent()
        .ok_or_else(|| RailError::message("COFF evidence has no parent"))?;
    if !parent.is_absolute()
        || archive.parent() != Some(parent)
        || archive.file_name() != Some(OsStr::new(ARCHIVE_FILE))
        || evidence.file_name() != Some(OsStr::new(EVIDENCE_FILE))
    {
        return Err(RailError::message("COFF evidence escaped its private binding"));
    }
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message("COFF evidence directory is not one real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(RailError::message("COFF evidence directory permits unowned writes"));
        }
    }
    Ok((archive, evidence))
}

fn read_evidence(path: &Path) -> RailResult<DriverEvidence> {
    let evidence: DriverEvidence = serde_json::from_slice(&read_bounded(path, MAX_ELF_LINK_DEPENDENCY_BYTES)?)?;
    if evidence.version != 1 || !Path::new(&evidence.current_directory).is_absolute() {
        return Err(RailError::message("COFF driver evidence is invalid"));
    }
    validate_link_file(&evidence.driver)?;
    validate_link_driver_probe(&evidence.driver_probe)?;
    if evidence.runtime_files.len() > MAX_LINK_INPUTS {
        return Err(RailError::message("COFF runtime closure exceeds its bound"));
    }
    for file in &evidence.runtime_files {
        validate_link_file(file)?;
    }
    validate_link_response_evidence(&evidence.response_files, &evidence.arguments)?;
    Ok(evidence)
}

pub(crate) fn configure(command: &mut Command, arguments: &[OsString]) -> bool {
    let configured = (|| -> RailResult<()> {
        if std::env::var_os("LLD_REPRODUCE").is_some() {
            return Err(RailError::message("caller owns LLD_REPRODUCE"));
        }
        let (archive, evidence_path) = evidence_paths()?;
        if !link_adapter_file_is_empty(&evidence_path) {
            let previous = read_evidence(&evidence_path)?;
            if previous.completed {
                return Err(RailError::message("COFF linker attempt already completed"));
            }
        } else if !link_adapter_file_is_empty(&archive) {
            return Err(RailError::message("COFF archive has no recognized attempt"));
        }
        for path in [&archive, &evidence_path, &evidence_path.with_extension("rsp")] {
            match fs::symlink_metadata(path) {
                Ok(_) => overwrite_private_command_file(path, b"")?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => write_private_command_file(path, b"")?,
                Err(error) => return Err(error.into()),
            }
        }
        let current_directory = std::env::current_dir()?;
        let captured = capture_arguments(arguments, &current_directory)?;
        validate_arguments(&captured.arguments, &current_directory)?;
        let driver = std::env::var_os(DRIVER_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| RailError::message("COFF driver selection is unavailable"))?;
        let (driver, driver_generation) = capture_link_file(
            &driver,
            Instant::now(),
            &mut NativeCaptureBudget::new(LINK_CAPTURE_LIMITS),
        )?;
        let version_arguments = if Path::new(&driver.path)
            .file_stem()
            .is_some_and(|name| name == "rust-lld")
        {
            vec!["-flavor".to_string(), "link".to_string(), "--version".to_string()]
        } else {
            vec!["--version".to_string()]
        };
        let (driver_probe, runtime_files) =
            capture_link_runtime_probe(Path::new(&driver.path), &version_arguments, &current_directory)?;
        let execution = if captured.response_files.is_empty() {
            captured.arguments.iter().map(OsString::from).collect::<Vec<_>>()
        } else {
            let response = captured
                .arguments
                .iter()
                .map(|argument| quote_response_argument(argument))
                .collect::<Vec<_>>()
                .join("\n");
            write_link_adapter_file(&evidence_path.with_extension("rsp"), response.as_bytes())?;
            vec![OsString::from(format!(
                "@{}",
                evidence_path.with_extension("rsp").display()
            ))]
        };
        let evidence = DriverEvidence {
            version: 1,
            completed: false,
            current_directory: current_directory.to_string_lossy().into_owned(),
            driver,
            driver_generation,
            arguments: captured.arguments,
            response_files: captured.response_files,
            driver_probe,
            runtime_files,
        };
        write_link_adapter_file(&evidence_path, &serde_json::to_vec(&evidence)?)?;
        command.args(execution).env("LLD_REPRODUCE", archive);
        Ok(())
    })();
    if let Err(error) = &configured {
        report_native_action_diagnostic("COFF linker adapter", error);
    }
    configured.is_ok()
}

pub(crate) fn finalize() -> bool {
    let finalized = (|| -> RailResult<()> {
        let (archive, evidence_path) = evidence_paths()?;
        let mut evidence = read_evidence(&evidence_path)?;
        if evidence.driver_generation != linker_generation_identity(Path::new(&evidence.driver.canonical_path)) {
            return Err(RailError::message("COFF driver changed during execution"));
        }
        let metadata = fs::symlink_metadata(archive)?;
        if !metadata.is_file() || crate::utils::is_symlink_or_reparse(&metadata) || metadata.len() > MAX_ARCHIVE_BYTES {
            return Err(RailError::message("COFF reproduction archive is unavailable"));
        }
        evidence.completed = true;
        overwrite_private_command_file(&evidence_path, &serde_json::to_vec(&evidence)?)
    })();
    if let Err(error) = &finalized {
        report_native_action_diagnostic("COFF linker finalization", error);
    }
    finalized.is_ok()
}

fn validate_arguments(arguments: &[String], current_directory: &Path) -> RailResult<()> {
    for (index, argument) in arguments.iter().enumerate() {
        if argument == "-flavor" && arguments.get(index + 1).is_some_and(|argument| argument == "link")
            || argument == "link" && index != 0 && arguments[index - 1] == "-flavor"
        {
            continue;
        }
        if argument.starts_with('@') || argument.as_bytes().contains(&0) {
            return Err(RailError::message("COFF linker argument is not captured"));
        }
        if argument
            .strip_prefix('/')
            .is_some_and(|argument| argument.contains('/'))
            && !argument.contains(':')
            && absolute_link_argument_path(argument, current_directory).is_file()
        {
            continue;
        }
        let Some(option) = argument.strip_prefix('/').or_else(|| argument.strip_prefix('-')) else {
            continue;
        };
        let name = option.split(':').next().unwrap_or_default().to_ascii_lowercase();
        // Every admitted file option is reproduced by LLD. Persistent state and
        // side-output switches require their own output binding before admission.
        if !matches!(
            name.as_str(),
            "out"
                | "pdb"
                | "pdbaltpath"
                | "implib"
                | "libpath"
                | "defaultlib"
                | "nodefaultlib"
                | "machine"
                | "subsystem"
                | "entry"
                | "dll"
                | "noentry"
                | "debug"
                | "nologo"
                | "nxcompat"
                | "dynamicbase"
                | "highentropyva"
                | "largeaddressaware"
                | "opt"
                | "incremental"
                | "natvis"
                | "def"
                | "wholearchive"
                | "export"
                | "include"
                | "alternatename"
                | "errorlimit"
                | "brepro"
                | "timestamp"
                | "threads"
                | "stack"
                | "heap"
                | "guard"
                | "safeseh"
                | "allowbind"
                | "allowisolation"
                | "release"
                | "fixed"
                | "delayload"
                | "ignore"
                | "merge"
                | "section"
                | "align"
                | "filealign"
                | "version"
                | "osversion"
                | "manifest"
                | "manifestuac"
                | "manifestdependency"
                | "functionpadmin"
                | "cetcompat"
                | "verbose"
                | "failifmismatch"
                | "wx"
        ) {
            return Err(RailError::message(format!(
                "COFF linker option '/{name}' has no observed input/output contract"
            )));
        }
        if name == "incremental" && !option.eq_ignore_ascii_case("incremental:no")
            || name == "manifest" && !option.eq_ignore_ascii_case("manifest:no")
        {
            return Err(RailError::message(
                "COFF linker persistent or manifest output is not owned",
            ));
        }
    }
    Ok(())
}

fn capture_arguments(arguments: &[OsString], current_directory: &Path) -> RailResult<CapturedLinkArguments> {
    let mut pending = arguments
        .iter()
        .map(|argument| {
            argument
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| RailError::message("COFF linker argument is not UTF-8"))
        })
        .collect::<RailResult<Vec<_>>>()?;
    pending.reverse();
    let mut arguments = Vec::new();
    let mut responses = BTreeMap::new();
    let mut expanded = 0usize;
    let mut bytes_read = 0usize;
    while let Some(argument) = pending.pop() {
        if let Some(path) = argument.strip_prefix('@') {
            expanded += 1;
            if path.is_empty() || expanded > MAX_LINK_RESPONSE_FILES {
                return Err(RailError::message("COFF response expansion exceeds its bound"));
            }
            let path = absolute_link_argument_path(path, current_directory);
            let bytes = read_bounded(&path, MAX_ELF_LINK_DEPENDENCY_BYTES)?;
            bytes_read = bytes_read.saturating_add(bytes.len());
            if bytes_read > MAX_ELF_LINK_DEPENDENCY_BYTES {
                return Err(RailError::message("COFF response bytes exceed their bound"));
            }
            let (file, _) = capture_link_file(
                &path,
                Instant::now(),
                &mut NativeCaptureBudget::new(LINK_CAPTURE_LIMITS),
            )?;
            if file.content_digest != digest(&bytes)
                || responses
                    .get(&file.path)
                    .is_some_and(|previous: &LinkResponseFileWitness| previous.file != file)
            {
                return Err(RailError::message("COFF response file changed during capture"));
            }
            let private_parent = private_rustc_response_parent(&file)?;
            responses.insert(file.path.clone(), LinkResponseFileWitness { file, private_parent });
            pending.extend(parse_response(&bytes)?.into_iter().rev());
        } else {
            arguments.push(argument);
        }
        if arguments.len().saturating_add(pending.len()) > MAX_LINK_INPUTS {
            return Err(RailError::message("COFF linker arguments exceed their bound"));
        }
    }
    Ok(CapturedLinkArguments {
        arguments,
        response_files: responses.into_values().collect(),
    })
}

fn parse_response(bytes: &[u8]) -> RailResult<Vec<String>> {
    let text = if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        let (pairs, remainder) = bytes.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(RailError::message("COFF UTF-16 response is truncated"));
        }
        String::from_utf16(&pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect::<Vec<_>>())
            .map_err(|_| RailError::message("COFF response is not valid UTF-16"))?
    } else {
        std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
            .map_err(|_| RailError::message("COFF response is not valid UTF-8"))?
            .to_string()
    };
    if text.contains('\0') {
        return Err(RailError::message("COFF response contains NUL"));
    }
    let mut chars = text.chars().peekable();
    let mut arguments = Vec::new();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut argument = String::new();
        let mut quoted = false;
        while let Some(&character) = chars.peek() {
            if !quoted && character.is_ascii_whitespace() {
                break;
            }
            chars.next();
            if character == '\\' {
                let mut count = 1;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    count += 1;
                }
                if chars.peek() == Some(&'"') {
                    argument.extend(std::iter::repeat_n('\\', count / 2));
                    if count % 2 == 1 {
                        chars.next();
                        argument.push('"');
                    }
                } else {
                    argument.extend(std::iter::repeat_n('\\', count));
                }
            } else if character == '"' {
                if quoted && chars.peek() == Some(&'"') {
                    chars.next();
                    argument.push('"');
                } else {
                    quoted = !quoted;
                }
            } else {
                argument.push(character);
            }
        }
        if quoted {
            return Err(RailError::message("COFF response has an unterminated quote"));
        }
        arguments.push(argument);
        if arguments.len() > MAX_LINK_INPUTS {
            return Err(RailError::message("COFF response exceeds its argument bound"));
        }
    }
    Ok(arguments)
}

fn quote_response_argument(argument: &str) -> String {
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        quoted.extend(std::iter::repeat_n(
            '\\',
            if character == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        quoted.push(character);
    }
    quoted.extend(std::iter::repeat_n('\\', slashes * 2));
    quoted.push('"');
    quoted
}

#[derive(Debug)]
struct ReproducedFile {
    bytes: u64,
    content_digest: String,
}

struct Reproduction {
    files: BTreeMap<PathBuf, ReproducedFile>,
    arguments: Vec<String>,
}

/// Decode LLD's bounded USTAR/PAX stream in place. Archive names never become
/// filesystem writes; each ordinary member is the bytes LLD actually consumed.
fn read_reproduction(path: &Path) -> RailResult<Reproduction> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_ARCHIVE_BYTES || metadata.len() % 512 != 0 {
        return Err(RailError::message("COFF reproduction exceeds its archive bounds"));
    }
    let mut consumed = 0u64;
    let mut files = BTreeMap::new();
    let mut arguments = None;
    let mut prefix = None::<String>;
    let mut extended_path = None;
    let mut header = [0u8; 512];
    let mut buffer = [0u8; 64 * 1024];
    loop {
        file.read_exact(&mut header)?;
        consumed = consumed.saturating_add(512);
        if header.iter().all(|byte| *byte == 0) {
            file.read_exact(&mut header)?;
            consumed = consumed.saturating_add(512);
            if header.iter().any(|byte| *byte != 0) || consumed != metadata.len() || extended_path.is_some() {
                return Err(RailError::message("COFF reproduction has an invalid terminator"));
            }
            break;
        }
        let checksum = octal(&header[148..156])?;
        let actual_checksum = header
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    u64::from(b' ')
                } else {
                    u64::from(*byte)
                }
            })
            .sum::<u64>();
        if checksum != actual_checksum
            || &header[257..263] != b"ustar\0"
            || &header[263..265] != b"00"
            || !matches!(header[156], 0 | b'0' | b'x')
        {
            return Err(RailError::message(
                "COFF reproduction has an unsupported archive header",
            ));
        }
        let size = octal(&header[124..136])?;
        let padded = size
            .checked_add(511)
            .map(|size| size / 512 * 512)
            .ok_or_else(|| RailError::message("COFF archive length overflow"))?;
        if consumed.saturating_add(padded).saturating_add(1024) > metadata.len() {
            return Err(RailError::message("COFF archive member exceeds its byte bound"));
        }
        let name = tar_string(&header[..100])?;
        let directory = tar_string(&header[345..500])?;
        let name = if directory.is_empty() {
            name
        } else {
            format!("{directory}/{name}")
        };
        let name = extended_path.take().unwrap_or(name);
        let mut hash = Sha256::new();
        let collect = header[156] == b'x' || name.ends_with("/response.txt") || name.ends_with("/version.txt");
        if collect && size > MAX_ELF_LINK_DEPENDENCY_BYTES as u64 {
            return Err(RailError::message("COFF archive metadata exceeds its byte bound"));
        }
        let mut bytes = Vec::new();
        let mut remaining = size;
        while remaining != 0 {
            let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            file.read_exact(&mut buffer[..count])?;
            hash.update(&buffer[..count]);
            if collect {
                bytes.extend_from_slice(&buffer[..count]);
            }
            remaining -= count as u64;
        }
        let padding =
            usize::try_from(padded - size).map_err(|_| RailError::message("COFF archive padding exceeds its bound"))?;
        file.read_exact(&mut buffer[..padding])?;
        if buffer[..padding].iter().any(|byte| *byte != 0) {
            return Err(RailError::message("COFF archive padding is invalid"));
        }
        consumed = consumed.saturating_add(padded);
        if header[156] == b'x' {
            if extended_path.is_some() {
                return Err(RailError::message("COFF archive repeats an extended path"));
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| RailError::message("COFF PAX path is not UTF-8"))?;
            let (length, path) = text
                .split_once(" path=")
                .ok_or_else(|| RailError::message("COFF PAX record is unsupported"))?;
            let path = path
                .strip_suffix('\n')
                .ok_or_else(|| RailError::message("COFF PAX record length is invalid"))?;
            if length.parse::<usize>().ok() != Some(bytes.len()) || path.contains('\n') {
                return Err(RailError::message("COFF PAX record length is invalid"));
            }
            extended_path = Some(path.to_string());
            continue;
        }
        let (root, relative) = name
            .split_once('/')
            .ok_or_else(|| RailError::message("COFF archive member has no root"))?;
        if root.is_empty() || prefix.as_ref().is_some_and(|prefix| prefix != root) {
            return Err(RailError::message("COFF archive roots disagree"));
        }
        prefix.get_or_insert_with(|| root.to_string());
        if relative == "response.txt" {
            if arguments.replace(parse_response(&bytes)?).is_some() {
                return Err(RailError::message("COFF archive repeats its response"));
            }
        } else if relative != "version.txt" {
            let path = reproduced_path(relative)?;
            let content_digest = format!("sha256:{}", ContentDigest::from_sha256_bytes(hash.finalize()));
            if files
                .insert(
                    path,
                    ReproducedFile {
                        bytes: size,
                        content_digest,
                    },
                )
                .is_some()
                || files.len() > MAX_LINK_INPUTS
            {
                return Err(RailError::message("COFF archive file set is invalid"));
            }
        }
    }
    if files.is_empty() {
        return Err(RailError::message("COFF archive contains no linker inputs"));
    }
    Ok(Reproduction {
        files,
        arguments: arguments.ok_or_else(|| RailError::message("COFF archive has no response"))?,
    })
}

fn tar_string(bytes: &[u8]) -> RailResult<String> {
    let length = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    if bytes[length..].iter().any(|byte| *byte != 0) {
        return Err(RailError::message("COFF archive header string has trailing bytes"));
    }
    Ok(std::str::from_utf8(&bytes[..length])
        .map_err(|_| RailError::message("COFF archive name is not UTF-8"))?
        .to_string())
}

fn octal(bytes: &[u8]) -> RailResult<u64> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| RailError::message("COFF archive number is invalid"))?
        .trim_matches(['\0', ' ']);
    u64::from_str_radix(text, 8).map_err(|_| RailError::message("COFF archive number is not octal"))
}

fn reproduced_path(relative: &str) -> RailResult<PathBuf> {
    if relative.is_empty() || relative.split('/').any(|part| matches!(part, "" | "." | "..")) || relative.contains('\0')
    {
        return Err(RailError::message("COFF archive path is invalid"));
    }
    #[cfg(not(windows))]
    let path = PathBuf::from("/").join(relative);
    #[cfg(windows)]
    let path = {
        let (drive, tail) = relative
            .split_once('/')
            .ok_or_else(|| RailError::message("COFF archive drive is unavailable"))?;
        if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
            return Err(RailError::message("COFF archive path is not a drive path"));
        }
        PathBuf::from(format!("{drive}:/{tail}"))
    };
    Ok(path)
}

pub(super) fn capture(
    observation: &RawCompilerInvocation,
    output_paths: &NativeOutputPaths,
    archive: &Path,
    evidence_path: &Path,
    installation_authority: Option<&str>,
) -> RailResult<(CoffLinkerWitness, Option<LinkerGenerationWitness>, u64)> {
    let evidence = read_evidence(evidence_path)?;
    if !evidence.completed {
        return Err(RailError::message("COFF linker attempt did not complete successfully"));
    }
    let current_directory = Path::new(&evidence.current_directory);
    let (driver_selection, driver_path) = selected_driver(&observation.compiler_arguments, current_directory)?;
    if driver_path != Path::new(&evidence.driver.path) {
        return Err(RailError::message("COFF driver selection changed"));
    }
    let linked = output_paths
        .artifacts
        .iter()
        .filter(|output| output.role.requires_linker())
        .collect::<Vec<_>>();
    let [linked] = linked.as_slice() else {
        return Err(RailError::message("COFF evidence requires one linked output"));
    };
    let linked_path = crate::utils::canonicalize_existing(&linked.path)?;
    let linked_parent = linked_path
        .parent()
        .ok_or_else(|| RailError::message("COFF linked output has no parent"))?;
    let selected_output = evidence
        .arguments
        .iter()
        .filter_map(|argument| option_value(argument, "out"))
        .next_back()
        .ok_or_else(|| RailError::message("COFF evidence has no selected output"))?;
    if crate::utils::canonicalize_existing(&absolute_link_argument_path(selected_output, current_directory))?
        != linked_path
    {
        return Err(RailError::message("COFF evidence does not bind the exact output"));
    }
    let reproduction = read_reproduction(archive)?;
    validate_arguments(&reproduction.arguments, current_directory)?;
    for argument in &reproduction.arguments {
        for option in ["def", "natvis", "wholearchive"] {
            if let Some(path) = option_value(argument, option)
                && !reproduction.files.contains_key(&reproduced_path(path)?)
            {
                return Err(RailError::message("COFF file option has no consumed-byte evidence"));
            }
        }
    }
    let response_inputs = persistent_link_response_inputs(&evidence.response_files, observation, linked_parent);
    let direct_inputs = evidence
        .arguments
        .iter()
        .map(|argument| absolute_link_argument_path(argument, current_directory))
        .collect::<BTreeSet<_>>();
    let mut found_paths = response_inputs.keys().cloned().collect::<BTreeSet<_>>();
    found_paths.extend(evidence.runtime_files.iter().map(|file| PathBuf::from(&file.path)));
    let mut expected_files = BTreeMap::new();
    let mut endogenous_objects = 0u32;
    let mut dependency_archives = BTreeSet::new();
    let mut dependency_by_file = BTreeMap::new();
    for (name, artifact) in &observation.dependency_artifacts {
        let filename = observation_path_basename(&artifact.path)
            .ok_or_else(|| RailError::message("COFF dependency has no filename"))?;
        if dependency_by_file.insert(filename, (name, artifact)).is_some() {
            return Err(RailError::message("COFF dependency filenames are ambiguous"));
        }
    }
    for (path, expected) in reproduction.files {
        let filename = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let parent = path.parent();
        let temporary_input = parent
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            .is_some_and(is_rustc_temporary_name)
            && parent
                .and_then(Path::parent)
                .and_then(|parent| crate::utils::canonicalize_existing(parent).ok())
                .as_deref()
                == Some(linked_parent);
        let codegen_object = filename.ends_with(".rcgu.o")
            && direct_inputs.contains(&path)
            && parent
                .and_then(|parent| crate::utils::canonicalize_existing(parent).ok())
                .as_deref()
                == Some(linked_parent);
        if temporary_input || codegen_object {
            endogenous_objects = endogenous_objects
                .checked_add(1)
                .ok_or_else(|| RailError::message("COFF generated input count overflow"))?;
            continue;
        }
        if path.extension() == Some(OsStr::new("rlib"))
            && let Some((name, artifact)) = dependency_by_file.get(filename)
        {
            if expected.content_digest != artifact.content_digest {
                return Err(RailError::message(
                    "COFF dependency archive differs from the compiler observation",
                ));
            }
            dependency_archives.insert((*name).clone());
            continue;
        }
        found_paths.insert(path.clone());
        expected_files.insert(path, expected);
    }
    if endogenous_objects == 0 {
        return Err(RailError::message("COFF archive has no certified rustc object"));
    }
    let mut search_directories = BTreeSet::from([current_directory.to_path_buf()]);
    for argument in &reproduction.arguments {
        if let Some(path) = option_value(argument, "libpath") {
            search_directories.insert(reproduced_path(path)?);
        }
    }
    let mut missing = BTreeSet::new();
    let mut directories = Vec::new();
    let search_started = Instant::now();
    for directory in search_directories {
        if search_started.elapsed() > MAX_SOURCE_CAPTURE_TIME {
            return Err(RailError::message(
                "COFF library search capture exceeds its elapsed bound",
            ));
        }
        match capture_search_directory(&directory) {
            Ok(directory) => directories.push(directory),
            Err(error)
                if fs::symlink_metadata(&directory)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                let _ = error;
                missing.insert(directory.to_string_lossy().into_owned());
            }
            Err(error) => return Err(error),
        }
    }
    directories.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    let started = Instant::now();
    let mut budget = NativeCaptureBudget::new(LINK_CAPTURE_LIMITS);
    let (driver, driver_generation) = capture_link_file(&driver_path, started, &mut budget)?;
    if driver != evidence.driver || driver_generation != evidence.driver_generation {
        return Err(RailError::message("COFF driver changed before publication"));
    }
    let mut found = Vec::new();
    for path in found_paths {
        let (file, generation) = capture_link_file(&path, started, &mut budget)?;
        if expected_files
            .get(&path)
            .is_some_and(|expected| expected.bytes != file.bytes || expected.content_digest != file.content_digest)
            || response_inputs.get(&path).is_some_and(|expected| *expected != file)
            || evidence
                .runtime_files
                .iter()
                .any(|expected| expected.path == file.path && *expected != file)
        {
            return Err(RailError::message("COFF linker input changed after it was consumed"));
        }
        found.push((file, generation));
    }
    found.sort_unstable_by(|left, right| left.0.path.cmp(&right.0.path));
    let (found, found_generations): (Vec<_>, Vec<_>) = found.into_iter().unzip();
    let witness = FileLinkerWitness {
        version: 3,
        driver_selection,
        linker: driver.clone(),
        driver,
        driver_probe: evidence.driver_probe,
        found,
        missing: missing.into_iter().collect(),
        endogenous_objects,
        dependency_archives: dependency_archives.into_iter().collect(),
    };
    validate_file_linker_witness(&witness)?;
    let generations = installation_authority.and_then(|authority| {
        Some(LinkerGenerationWitness {
            version: 1,
            installation_authority: installation_authority_identity(authority),
            driver: driver_generation.clone()?,
            linker: driver_generation?,
            found: found_generations.into_iter().collect::<Option<Vec<_>>>()?,
        })
    });
    Ok((
        CoffLinkerWitness {
            files: witness,
            search_directories: directories,
        },
        generations,
        budget.bytes_hashed,
    ))
}

fn option_value<'a>(argument: &'a str, expected: &str) -> Option<&'a str> {
    let option = argument.strip_prefix('/').or_else(|| argument.strip_prefix('-'))?;
    let (name, value) = option.split_once(':')?;
    name.eq_ignore_ascii_case(expected).then_some(value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CoffLinkerWitness {
    pub(super) files: FileLinkerWitness,
    search_directories: Vec<SearchDirectory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchDirectory {
    path: String,
    canonical_path: String,
    entries_digest: String,
}

fn capture_search_directory(path: &Path) -> RailResult<SearchDirectory> {
    let canonical = crate::utils::canonicalize_existing(path)?;
    let before = native_metadata_guard(&canonical, &fs::metadata(&canonical)?)?;
    let mut names = Vec::new();
    let mut bytes = 0usize;
    for entry in fs::read_dir(&canonical)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| RailError::message("COFF library directory has a non-UTF-8 entry"))?;
        bytes = bytes.saturating_add(name.len());
        names.push(name);
        if names.len() > MAX_LINK_INPUTS || bytes > MAX_LINK_PATH_BYTES {
            return Err(RailError::message("COFF library directory exceeds its bounds"));
        }
    }
    names.sort_unstable();
    let entries_digest = digest(&serde_json::to_vec(&names)?);
    if before != native_metadata_guard(&canonical, &fs::metadata(&canonical)?)?
        || crate::utils::canonicalize_existing(path)? != canonical
    {
        return Err(RailError::message("COFF library directory changed during capture"));
    }
    Ok(SearchDirectory {
        path: path.to_string_lossy().into_owned(),
        canonical_path: canonical.to_string_lossy().into_owned(),
        entries_digest,
    })
}

pub(super) fn validate(witness: &CoffLinkerWitness) -> RailResult<()> {
    validate_file_linker_witness(&witness.files)?;
    if witness.files.driver != witness.files.linker || witness.search_directories.len() > MAX_LINK_INPUTS {
        return Err(RailError::message("COFF linker witness is invalid"));
    }
    let mut previous = None::<&str>;
    for directory in &witness.search_directories {
        if !Path::new(&directory.path).is_absolute()
            || !Path::new(&directory.canonical_path).is_absolute()
            || directory.path.len() > MAX_DYNAMIC_REPOSITORY_PATH_BYTES
            || directory.path.contains('\0')
            || previous.is_some_and(|previous| previous >= directory.path.as_str())
        {
            return Err(RailError::message("COFF search directories are invalid"));
        }
        validate_sha256(&directory.entries_digest)?;
        previous = Some(&directory.path);
    }
    Ok(())
}

pub(super) fn revalidate(
    witness: &CoffLinkerWitness,
    generations: Option<&LinkerGenerationWitness>,
    authority: Option<&str>,
) -> RailResult<u64> {
    validate(witness)?;
    for directory in &witness.search_directories {
        if capture_search_directory(Path::new(&directory.path))? != *directory {
            return Err(RailError::message("COFF library search directory changed"));
        }
    }
    revalidate_file_linker_witness(&witness.files, generations, authority)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_response_preserves_quoted_paths_and_literal_backslashes() {
        let response = br#"/OUT:"C:\build directory\fixture.dll" "C:\objects\unit one.obj" /PDBALTPATH:"%_PDB%" "embedded\"quote" "trailing\\""#;
        assert_eq!(
            parse_response(response).expect("Windows response"),
            [
                "/OUT:C:\\build directory\\fixture.dll",
                "C:\\objects\\unit one.obj",
                "/PDBALTPATH:%_PDB%",
                "embedded\"quote",
                "trailing\\",
            ]
        );
        assert_eq!(
            quote_response_argument("C:\\path with space\\"),
            "\"C:\\path with space\\\\\""
        );
        parse_response(b"\"unterminated").expect_err("unterminated Windows response quoting must fail");
        parse_response(&[0xff, 0xfe, 0x41]).expect_err("odd-byte UTF-16 response must fail");
    }

    #[test]
    fn case_variant_library_appearance_changes_the_search_identity() {
        let directory = tempfile::tempdir().expect("search directory");
        fs::write(directory.path().join("kernel32.lib"), b"selected").expect("selected library");
        let before = capture_search_directory(directory.path()).expect("search capture");
        fs::write(directory.path().join("USERENV.LIB"), b"new candidate").expect("case variant candidate");
        let after = capture_search_directory(directory.path()).expect("search recapture");
        assert_ne!(
            before.entries_digest, after.entries_digest,
            "case-folded lookup must see a new candidate"
        );
    }
}
