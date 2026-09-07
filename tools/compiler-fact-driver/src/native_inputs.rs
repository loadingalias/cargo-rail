//! Native input observations from the same compiler execution as the outputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::prelude::*;

use rustc_middle::{mir::TerminatorKind, mono::MonoItem, ty::TyCtxt};
use rustc_session::config::{CrateType, LtoCli};
use rustc_session::search_paths::PathKind;
use rustc_span::source_map::{FileLoader, FilePathMapping, RealFileLoader};

use crate::native_input_protocol::{
    MAX_NATIVE_INPUT_INVOCATION_BYTES, NATIVE_INPUT_PROTOCOL_VERSION, NativeAssemblyObservation,
    NativeCodegenObservation, NativeCratePattern, NativeCrateSearch, NativeCrateSource, NativeInputInvocation,
    NativeInputObservation, native_invocation_digest,
};

pub(crate) fn configure_source_directory(
    config: &mut rustc_interface::interface::Config,
    directory: &str,
) -> std::io::Result<()> {
    let loader = RelocatedSourceLoader {
        source_directory: directory.into(),
        staging_directory: std::env::current_dir()?,
    };
    config.opts.working_dir =
        FilePathMapping::empty().to_real_filename(&rustc_span::RealFileName::empty(), &loader.source_directory);
    config.file_loader = Some(Box::new(loader));
    Ok(())
}

struct RelocatedSourceLoader {
    source_directory: PathBuf,
    staging_directory: PathBuf,
}

impl RelocatedSourceLoader {
    fn staged_path(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.source_directory)
            .map(|relative| self.staging_directory.join(relative))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

impl FileLoader for RelocatedSourceLoader {
    fn file_exists(&self, path: &Path) -> bool {
        RealFileLoader.file_exists(&self.staged_path(path))
    }

    fn read_file(&self, path: &Path) -> std::io::Result<String> {
        RealFileLoader.read_file(&self.staged_path(path))
    }

    fn read_binary_file(&self, path: &Path) -> std::io::Result<Arc<[u8]>> {
        RealFileLoader.read_binary_file(&self.staged_path(path))
    }

    fn current_directory(&self) -> std::io::Result<PathBuf> {
        Ok(self.source_directory.clone())
    }
}

/// Invalid capabilities disable observation without changing compiler behavior.
pub(crate) fn load_invocation(path: &Path, arguments: &[String]) -> Result<NativeInputInvocation, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !path.is_absolute()
        || !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_NATIVE_INPUT_INVOCATION_BYTES
    {
        return Err("native input invocation is not a bounded real file".into());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("native input invocation changed while it was read".into());
    }
    let invocation = NativeInputInvocation::decode(&bytes)?;
    let result_path = Path::new(&invocation.result_path);
    if result_path.parent() != path.parent()
        || result_path == path
        || result_path.try_exists().map_err(|error| error.to_string())?
    {
        return Err("native input result must be a new sibling of its invocation".into());
    }
    if invocation.invocation_digest
        != native_invocation_digest(arguments, &std::env::current_dir().map_err(|error| error.to_string())?)?
    {
        return Err("native input invocation does not match compiler arguments".into());
    }
    Ok(invocation)
}

pub(crate) fn collect(
    tcx: TyCtxt<'_>,
    invocation: &NativeInputInvocation,
    requests: &Arc<Mutex<DependencyRequests>>,
) -> Result<NativeInputObservation, String> {
    // Collect after monomorphization so dependencies loaded by imported generic
    // bodies are included in the final crate-source list.
    let assembly = assembly_observation(tcx);
    let current_directory = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut crates = Vec::new();
    for &crate_num in tcx.crates(()) {
        let source = tcx.used_crate_source(crate_num);
        let mut files = source
            .dylib
            .iter()
            .chain(source.rlib.iter())
            .chain(source.rmeta.iter())
            .chain(source.sdylib_interface.iter())
            .map(|path| absolute_spelling(path, &current_directory))
            .collect::<Result<Vec<_>, _>>()?;
        files.sort_unstable();
        files.dedup();
        crates.push(NativeCrateSource {
            name: tcx.crate_name(crate_num).as_str().to_owned(),
            files,
        });
    }
    crates.sort_unstable();
    crates.dedup();
    let searches = search_observations(tcx, &crates, &current_directory, requests)?;
    let observation = NativeInputObservation {
        version: NATIVE_INPUT_PROTOCOL_VERSION,
        request_identity: invocation.identity()?,
        crates,
        searches,
        assembly,
        codegen: NativeCodegenObservation::NotRun,
    };
    Ok(observation)
}

pub(crate) fn publish(invocation: &NativeInputInvocation, observation: &NativeInputObservation) -> Result<(), String> {
    let bytes = observation.encode(invocation)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&invocation.result_path)
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())
}

fn search_observations(
    tcx: TyCtxt<'_>,
    crates: &[NativeCrateSource],
    current_directory: &Path,
    requests: &Arc<Mutex<DependencyRequests>>,
) -> Result<Vec<NativeCrateSearch>, String> {
    let exact_paths = tcx
        .sess
        .opts
        .externs
        .iter()
        .filter_map(|(_, entry)| entry.files())
        .flatten()
        .map(|path| path.canonicalized().to_path_buf())
        .collect::<BTreeSet<_>>();
    let requested_prefixes = dependency_search_prefixes(tcx, current_directory, requests)?;
    let mut searches = BTreeMap::<String, (BTreeSet<NativeCratePattern>, BTreeSet<String>)>::new();
    for (filesearch, target) in [
        (tcx.sess.target_filesearch(), &tcx.sess.target),
        (tcx.sess.host_filesearch(), &tcx.sess.host),
    ] {
        for search in filesearch
            .search_paths(PathKind::All)
            .filter(|search| search.kind.matches(PathKind::Crate) || search.kind.matches(PathKind::Dependency))
        {
            let mut patterns = BTreeSet::new();
            for source in crates {
                // Exact --extern files do not consult library search paths.
                if source.files.iter().all(|file| exact_paths.contains(Path::new(file))) {
                    continue;
                }
                let prefixes = requested_prefixes
                    .get(&source.files)
                    .ok_or_else(|| "native crate search origin is unavailable".to_string())?;
                let selected_prefixes = prefixes
                    .direct
                    .iter()
                    .filter(|_| search.kind.matches(PathKind::Crate))
                    .chain(
                        prefixes
                            .dependency
                            .iter()
                            .filter(|_| search.kind.matches(PathKind::Dependency)),
                    );
                for prefix in selected_prefixes {
                    patterns.extend([
                        NativeCratePattern {
                            prefix: format!("lib{prefix}"),
                            suffix: ".rlib".into(),
                        },
                        NativeCratePattern {
                            prefix: format!("lib{prefix}"),
                            suffix: ".rmeta".into(),
                        },
                        NativeCratePattern {
                            prefix: format!("lib{prefix}"),
                            suffix: ".rs".into(),
                        },
                        NativeCratePattern {
                            prefix: format!("{}{prefix}", target.dll_prefix),
                            suffix: target.dll_suffix.to_string(),
                        },
                        NativeCratePattern {
                            prefix: format!("{}{prefix}", target.staticlib_prefix),
                            suffix: target.staticlib_suffix.to_string(),
                        },
                    ]);
                }
            }
            if patterns.is_empty() {
                continue;
            }
            let directory = absolute_spelling(&search.dir, current_directory)?;
            let (queries, files) = searches.entry(directory).or_default();
            for pattern in &patterns {
                if let Some(matches) = search.files.query(&pattern.prefix, &pattern.suffix) {
                    for (_, file) in matches {
                        let path = file.path(&search.dir);
                        files.insert(
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .ok_or_else(|| "native crate search filename is unavailable".to_string())?
                                .into(),
                        );
                    }
                }
            }
            queries.extend(patterns.iter().cloned());
        }
    }
    Ok(searches
        .into_iter()
        .map(|(directory, (patterns, files))| NativeCrateSearch {
            directory,
            patterns: patterns.into_iter().collect(),
            files: files.into_iter().collect(),
        })
        .collect())
}

struct CrateSearchPrefixes {
    direct: Option<String>,
    dependency: BTreeSet<String>,
}

/// Capture the requesting suffix from the compiler's resolution events, rather
/// than substituting the selected crate's own possibly different suffix.
fn dependency_search_prefixes(
    tcx: TyCtxt<'_>,
    current_directory: &Path,
    requests: &Arc<Mutex<DependencyRequests>>,
) -> Result<BTreeMap<Vec<String>, CrateSearchPrefixes>, String> {
    let requests = requests
        .lock()
        .map_err(|_| "native dependency requests are unavailable")?;
    if !requests.complete {
        return Err("native dependency request observation is incomplete".into());
    }
    let mut result = BTreeMap::new();
    for &crate_num in tcx.crates(()) {
        let direct = tcx.extern_crate(crate_num).is_none_or(|origin| origin.is_direct());
        let requested = requests.prefixes.get(&tcx.crate_hash(crate_num).to_string());
        if !direct && requested.is_none() {
            return Err("compiler omitted an indirect dependency resolution event".into());
        }
        let name = tcx.crate_name(crate_num).as_str().to_owned();
        let source = tcx.used_crate_source(crate_num);
        let mut files = source
            .dylib
            .iter()
            .chain(source.rlib.iter())
            .chain(source.rmeta.iter())
            .chain(source.sdylib_interface.iter())
            .map(|path| absolute_spelling(path, current_directory))
            .collect::<Result<Vec<_>, _>>()?;
        files.sort_unstable();
        files.dedup();
        // Every requested prefix must cover every selected file. Otherwise a
        // broad fallback may have selected it, so retain the broad search.
        let selected_by_every_request = source.dylib.is_none()
            && source.sdylib_interface.is_none()
            && requested.is_some_and(|prefixes| {
                prefixes.iter().all(|prefix| {
                    prefix.starts_with(&name)
                        && files.iter().all(|file| {
                            let Some(filename) = Path::new(file).file_name().and_then(|name| name.to_str()) else {
                                return false;
                            };
                            filename.starts_with(&format!("lib{prefix}"))
                        })
                })
            });
        let dependency = if selected_by_every_request {
            requested.cloned().unwrap_or_default()
        } else if requested.is_some() || !direct {
            BTreeSet::from([name.clone()])
        } else {
            BTreeSet::new()
        };
        result.insert(
            files,
            CrateSearchPrefixes {
                direct: direct.then_some(name),
                dependency,
            },
        );
    }
    Ok(result)
}

#[derive(Default)]
pub(crate) struct DependencyRequests {
    prefixes: BTreeMap<String, BTreeSet<String>>,
    complete: bool,
    retained_bytes: usize,
}

pub(crate) fn observe_dependency_requests() -> Arc<Mutex<DependencyRequests>> {
    let requests = Arc::new(Mutex::new(DependencyRequests {
        complete: true,
        ..Default::default()
    }));
    let subscriber = tracing_subscriber::registry().with(DependencyRequestLayer(requests.clone()));
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        // Preserve an existing subscriber and decline native observation.
        requests.lock().unwrap().complete = false;
    }
    requests
}

struct DependencyRequestLayer(Arc<Mutex<DependencyRequests>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for DependencyRequestLayer {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _context: tracing_subscriber::layer::Context<'_, S>) -> bool {
        metadata.target() == "rustc_metadata::creader" && metadata.level() == &tracing::Level::INFO
    }

    fn on_event(&self, event: &tracing::Event<'_>, _context: tracing_subscriber::layer::Context<'_, S>) {
        let Ok(mut requests) = self.0.lock() else {
            return;
        };
        if !requests.complete {
            return;
        }
        let mut message = DependencyMessage::default();
        event.record(&mut message);
        let Some(dependency) = message.text.strip_prefix("resolving dep `") else {
            return;
        };
        if message.overflow {
            requests.complete = false;
            return;
        }
        let parsed = (|| {
            let (parent, dependency) = dependency.split_once("`->`")?;
            let (name, dependency) = dependency.split_once("` hash: `")?;
            let (hash, dependency) = dependency.split_once("` extra filename: `")?;
            let (suffix, private) = dependency.rsplit_once("` private ")?;
            let identifier = |value: &str| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            };
            if !identifier(parent)
                || !identifier(name)
                || hash.len() != 32
                || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                || !matches!(private, "true" | "false")
                || suffix.chars().any(char::is_control)
                || suffix.contains(['/', '\\'])
                || name.len() + suffix.len() > 1024
            {
                return None;
            }
            Some((hash.to_owned(), format!("{name}{suffix}")))
        })();
        match parsed {
            Some((hash, prefix)) => {
                if requests
                    .prefixes
                    .get(&hash)
                    .is_some_and(|prefixes| prefixes.contains(&prefix))
                {
                    return;
                }
                let bytes = hash.len() + prefix.len();
                if requests.retained_bytes + bytes > 4 * 1024 * 1024 {
                    requests.complete = false;
                    return;
                }
                requests.retained_bytes += bytes;
                requests.prefixes.entry(hash).or_default().insert(prefix);
            }
            _ => requests.complete = false,
        }
    }
}

#[derive(Default)]
struct DependencyMessage {
    text: String,
    overflow: bool,
}

impl std::fmt::Write for DependencyMessage {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.text.len().saturating_add(text.len()) > 8192 {
            let mut end = 8192 - self.text.len();
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            self.text.push_str(&text[..end]);
            self.overflow = true;
            return Err(std::fmt::Error);
        }
        self.text.push_str(text);
        Ok(())
    }
}

impl tracing::field::Visit for DependencyMessage {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = std::fmt::write(self, format_args!("{value:?}"));
        }
    }
}

fn absolute_spelling(path: &Path, current_directory: &Path) -> Result<String, String> {
    let path: PathBuf = if path.is_absolute() {
        path.into()
    } else {
        current_directory.join(path)
    };
    path.into_os_string()
        .into_string()
        .map_err(|_| "native crate source path is not UTF-8".into())
}

fn assembly_observation(tcx: TyCtxt<'_>) -> NativeAssemblyObservation {
    if !tcx.sess.opts.output_types.should_codegen() {
        return NativeAssemblyObservation::NoCodegen;
    }
    for unit in tcx.collect_and_partition_mono_items(()).codegen_units {
        for item in unit.items().keys() {
            match item {
                MonoItem::GlobalAsm(_) => return NativeAssemblyObservation::Present,
                MonoItem::Fn(instance) => {
                    if tcx
                        .instance_mir(instance.def)
                        .basic_blocks
                        .iter()
                        .any(|block| matches!(block.terminator().kind, TerminatorKind::InlineAsm { .. }))
                    {
                        return NativeAssemblyObservation::Present;
                    }
                }
                MonoItem::Static(_) => {}
            }
        }
    }
    // Full-graph LTO can compile assembly from dependency bitcode that is absent
    // from this crate's monomorphized MIR. Rustc does not perform that import
    // when producing only an rlib; local ThinLTO also imports no external crate.
    let imports_dependency_bitcode = !matches!(tcx.crate_types(), [CrateType::Rlib])
        && (tcx.sess.target.requires_lto
            || tcx.sess.opts.cg.linker_plugin_lto.enabled()
            || matches!(
                tcx.sess.opts.cg.lto,
                LtoCli::Yes | LtoCli::NoParam | LtoCli::Thin | LtoCli::Fat
            ));
    if imports_dependency_bitcode {
        NativeAssemblyObservation::ImportedLtoUnobserved
    } else {
        NativeAssemblyObservation::Absent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_events_preserve_the_complete_requested_suffix() {
        let requests = Arc::new(Mutex::new(DependencyRequests {
            complete: true,
            ..Default::default()
        }));
        let subscriber = tracing_subscriber::registry().with(DependencyRequestLayer(requests.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "rustc_metadata::creader", "resolved crates: {}", "x".repeat(8193));
            tracing::info!(target: "rustc_metadata::creader",
                "resolving dep `parent`->`child` hash: `0123456789abcdef0123456789abcdef` extra filename: `-requested` private true` private false");
        });
        let requests = requests.lock().unwrap();
        assert!(requests.complete);
        assert_eq!(
            requests.prefixes,
            BTreeMap::from([(
                "0123456789abcdef0123456789abcdef".into(),
                BTreeSet::from(["child-requested` private true".into()]),
            )])
        );
    }

    #[test]
    fn oversized_dependency_event_prevents_later_partial_observation() {
        let requests = Arc::new(Mutex::new(DependencyRequests {
            complete: true,
            ..Default::default()
        }));
        let subscriber = tracing_subscriber::registry().with(DependencyRequestLayer(requests.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "rustc_metadata::creader", "resolving dep `{}`", "x".repeat(8193));
            tracing::info!(target: "rustc_metadata::creader",
                "resolving dep `parent`->`child` hash: `0123456789abcdef0123456789abcdef` extra filename: `-requested` private false");
        });
        let requests = requests.lock().unwrap();
        assert!(!requests.complete);
        assert!(requests.prefixes.is_empty());
    }
}
