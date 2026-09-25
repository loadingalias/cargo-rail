//! Inspect, validate, and explain `rail.toml` repository policy.

use crate::commands::common::TextJsonOutputFormat;
use crate::config::{self, DecodedConfig, RailConfig, schema};
use crate::error::{ConfigError, RailError, RailResult};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};

fn print_config_json<T: Serialize>(mode: &str, result: &str, exit_code: i32, payload: &T) -> RailResult<()> {
    let payload_value = serde_json::to_value(payload).map_err(|e| RailError::message(e.to_string()))?;
    let mut output = crate::output::machine_json_envelope("config", mode, result, exit_code, payload_value);
    if mode == "explain" {
        output["schema_version"] = serde_json::json!(2);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|e| RailError::message(e.to_string()))?
    );
    Ok(())
}

/// Context that marks a Cargo workspace failure rather than a policy error.
const WORKSPACE_VALIDATION: &str = "cannot validate Cargo workspace configuration";

/// Validation result for JSON output
#[derive(Serialize)]
struct ValidationResult {
    command: &'static str,
    action: &'static str,
    valid: bool,
    evidence: ValidationEvidence,
    config_path: Option<String>,
    errors: Vec<ValidationIssue>,
    warnings: Vec<ValidationIssue>,
}

/// A single validation issue
#[derive(Serialize, Clone)]
struct ValidationIssue {
    section: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
}

impl ValidationIssue {
    fn new(section: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            section: section.into(),
            message: message.into(),
            help: None,
            line: None,
            column: None,
        }
    }

    fn with_location(mut self, line: usize, column: usize) -> Self {
        self.line = Some(line);
        self.column = Some(column);
        self
    }
}

/// Strictness mode for validation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictnessMode {
    /// Explicit --strict flag
    Strict,
    /// Explicit --no-strict flag
    NoStrict,
    /// Auto-detect based on CI environment
    Auto,
}

impl StrictnessMode {
    /// Determine if we should be strict based on mode and environment
    pub fn is_strict(&self) -> bool {
        match self {
            StrictnessMode::Strict => true,
            StrictnessMode::NoStrict => false,
            StrictnessMode::Auto => is_ci_environment(),
        }
    }
}

/// Check if running in a CI environment
fn is_ci_environment() -> bool {
    std::env::var("CI").is_ok()
        || std::env::var("GITHUB_ACTIONS").is_ok()
        || std::env::var("GITLAB_CI").is_ok()
        || std::env::var("CIRCLECI").is_ok()
}

/// Result of config locate for JSON output
#[derive(Serialize)]
struct LocateResult {
    command: &'static str,
    action: &'static str,
    found: bool,
    path: Option<String>,
    search_paths: Vec<String>,
}

/// Print the path to the active configuration file
///
/// This is the equivalent of `cargo locate-project` for rail.toml.
/// Searches in order: rail.toml, .rail.toml, .cargo/rail.toml, .config/rail.toml
pub fn run_config_locate(
    workspace_root: &Path,
    config_override: Option<&Path>,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    let json = format.is_json();

    if let Some(explicit_path) = config_override {
        let path = if explicit_path.is_absolute() {
            explicit_path.to_path_buf()
        } else {
            workspace_root.join(explicit_path)
        };

        if path.exists() {
            if json {
                let result = LocateResult {
                    command: "config",
                    action: "locate",
                    found: true,
                    path: Some(path.display().to_string()),
                    search_paths: vec![],
                };
                print_config_json("locate", "success", 0, &result)?;
            } else {
                println!("{}", path.display());
            }
            return Ok(());
        } else {
            return Err(RailError::message(format!(
                "specified config file not found: {}",
                path.display()
            )));
        }
    }

    // Policy belongs to the Cargo workspace root, even when invoked from a member directory.
    let discovery_root = crate::workspace::discovery_root(workspace_root);
    let search_paths = [
        discovery_root.join("rail.toml"),
        discovery_root.join(".rail.toml"),
        discovery_root.join(".cargo").join("rail.toml"),
        discovery_root.join(".config").join("rail.toml"),
    ];

    let config_path = RailConfig::find_config_path(&discovery_root);

    if json {
        let result = LocateResult {
            command: "config",
            action: "locate",
            found: config_path.is_some(),
            path: config_path.as_ref().map(|p| p.display().to_string()),
            search_paths: search_paths.iter().map(|p| p.display().to_string()).collect(),
        };
        print_config_json(
            "locate",
            if config_path.is_some() { "success" } else { "not_found" },
            0,
            &result,
        )?;
    } else if let Some(path) = &config_path {
        println!("{}", path.display());
    } else {
        println!("no config file found");
        println!();
        println!("searched:");
        for p in &search_paths {
            println!("  {}", p.display());
        }
        println!();
        println!("hint: run 'cargo rail init' to create one");
    }

    Ok(())
}

/// Print the effective configuration with defaults merged
///
/// Shows what cargo-rail will actually use: user settings plus defaults
/// for any fields not explicitly set.
pub fn run_config_print(
    workspace_root: &Path,
    config_override: Option<&Path>,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    let json = format.is_json();

    let (source, decoded, _) = inspect_config(workspace_root, config_override)?;
    let config = decoded.config;

    if json {
        #[derive(Serialize)]
        struct PrintResult {
            command: &'static str,
            action: &'static str,
            config_path: Option<String>,
            config: RailConfig,
        }

        let result = PrintResult {
            command: "config",
            action: "print",
            config_path: source.path.as_ref().map(|path| path.display().to_string()),
            config,
        };
        print_config_json("print", "success", 0, &result)?;
    } else {
        println!("# Effective configuration ({})", source.label());
        println!("# This shows all settings including defaults for unset fields.");
        println!();

        let toml_str = toml_edit::ser::to_string_pretty(&config)
            .map_err(|e| RailError::message(format!("failed to serialize config: {}", e)))?;
        print!("{}", toml_str);
    }

    Ok(())
}

/// One effective configuration value and its provenance.
#[derive(Debug, Serialize)]
struct ExplainedField {
    path: String,
    configured: Option<serde_json::Value>,
    effective: serde_json::Value,
    default: Option<serde_json::Value>,
    source: String,
    classification: &'static str,
    why: &'static str,
}

#[derive(Serialize)]
struct ExplainResult {
    command: &'static str,
    action: &'static str,
    config_path: Option<String>,
    fields: Vec<ExplainedField>,
}

/// Explain effective configuration values, defaults, and provenance.
pub fn run_config_explain(
    workspace_root: &Path,
    config_override: Option<&Path>,
    requested_fields: &[String],
    all: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    let json = format.is_json();

    let (source, decoded, _) = inspect_config(workspace_root, config_override)?;
    let config = decoded.config;
    let configured: serde_json::Value =
        toml_edit::de::from_document(decoded.document).map_err(|error| RailError::message(error.to_string()))?;
    let mut effective = serde_json::to_value(&config).map_err(|error| RailError::message(error.to_string()))?;
    if let Some(surface_targets) = effective.pointer_mut("/surface/targets") {
        *surface_targets = serde_json::to_value(config.surface.targets.effective(&config.targets))
            .map_err(|error| RailError::message(error.to_string()))?;
    }
    // `"all"` inherits every resolution domain; show that exact set.
    if config.unify.compiler_targets.inherits_all()
        && let Some(compiler_targets) = effective.pointer_mut("/unify/compiler_targets")
    {
        let domains = if config.targets.is_empty() {
            vec!["default"]
        } else {
            config.targets.iter().map(String::as_str).collect()
        };
        *compiler_targets = serde_json::json!(domains);
    }
    let defaults =
        serde_json::to_value(RailConfig::default()).map_err(|error| RailError::message(error.to_string()))?;

    let configured = flatten_json(&configured);
    let effective = flatten_json(&effective);
    let defaults = flatten_json(&defaults);
    let paths: BTreeSet<_> = effective
        .keys()
        .chain(configured.keys())
        .filter(|path| schema::field_spec_path(path).is_some())
        .cloned()
        .collect();

    let mut fields: Vec<_> = paths
        .into_iter()
        .filter_map(|path| {
            let field_spec = schema::field_spec_path(&path)?;
            let configured_value = configured.get(&path).cloned();
            let effective_value = effective.get(&path).cloned().unwrap_or(serde_json::Value::Null);
            let source = if path == schema::ConfigPath::from_dotted("surface.targets")
                && config.surface.targets.inherits_workspace()
            {
                format!("{} (inherited from targets)", source.label())
            } else if path == schema::ConfigPath::from_dotted("unify.compiler_targets")
                && config.unify.compiler_targets.inherits_all()
            {
                let origin = if configured_value.is_some() {
                    source.label()
                } else {
                    "default".to_string()
                };
                format!("{origin} (inherited from targets)")
            } else if configured_value.is_some() {
                source.label()
            } else {
                "default".to_string()
            };
            let default = defaults.get(&path).cloned();
            Some(ExplainedField {
                path: path.to_string(),
                configured: configured_value,
                effective: effective_value,
                default,
                source,
                classification: "project_policy",
                why: field_spec.why,
            })
        })
        .collect();

    if !all && requested_fields.is_empty() {
        fields.retain(|field| field.configured.is_some() || field.source != "default");
    } else if !requested_fields.is_empty() {
        let known = fields.iter().map(|field| field.path.clone()).collect::<BTreeSet<_>>();
        let mut selected = BTreeSet::new();
        let mut unknown = Vec::new();
        for requested in requested_fields {
            if known.contains(requested) {
                selected.insert(requested.clone());
                continue;
            }
            let prefix = format!("{requested}.");
            let children = known
                .iter()
                .filter(|path| path.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>();
            if children.is_empty() {
                unknown.push(requested.clone());
            } else {
                selected.extend(children);
            }
        }
        if !unknown.is_empty() {
            let parent = unknown[0].rsplit_once('.').map_or("", |(parent, _)| parent);
            let prefix = if parent.is_empty() {
                String::new()
            } else {
                format!("{parent}.")
            };
            let valid = known
                .iter()
                .filter(|path| path.starts_with(&prefix))
                .take(12)
                .cloned()
                .collect::<Vec<_>>();
            let help = if valid.is_empty() {
                "run `cargo rail config explain --all` to list known fields".to_string()
            } else {
                format!("valid child paths include: {}", valid.join(", "))
            };
            return Err(RailError::with_help(
                format!("unknown configuration field(s): {}", unknown.join(", ")),
                help,
            ));
        }
        fields.retain(|field| selected.contains(&field.path));
    }

    let result = ExplainResult {
        command: "config",
        action: "explain",
        config_path: source.path.as_ref().map(|path| path.display().to_string()),
        fields,
    };
    if json {
        print_config_json("explain", "success", 0, &result)
    } else {
        println!("Configuration: {}", source.label());
        if !all && requested_fields.is_empty() {
            if result.fields.is_empty() {
                println!("No configured overrides.");
            } else {
                for field in &result.fields {
                    let inheritance = if field.path == "surface.targets" && config.surface.targets.inherits_workspace()
                    {
                        " (inherited from targets)"
                    } else {
                        ""
                    };
                    println!(
                        "{} = {}{}",
                        field.path,
                        display_json_value(&field.effective),
                        inheritance
                    );
                }
            }
            return Ok(());
        }

        for field in &result.fields {
            println!("\n{}", field.path);
            println!(
                "  configured: {}",
                field
                    .configured
                    .as_ref()
                    .map(display_json_value)
                    .unwrap_or_else(|| "none".to_string())
            );
            println!("  effective: {}", display_json_value(&field.effective));
            println!(
                "  default: {}",
                field
                    .default
                    .as_ref()
                    .map(display_json_value)
                    .unwrap_or_else(|| "none".to_string())
            );
            println!("  source: {}", field.source);
            println!("  classification: {}", field.classification);
            println!("  why: {}", field.why);
        }
        Ok(())
    }
}

fn flatten_json(value: &serde_json::Value) -> BTreeMap<schema::ConfigPath, serde_json::Value> {
    fn visit(
        value: &serde_json::Value,
        path: &schema::ConfigPath,
        fields: &mut BTreeMap<schema::ConfigPath, serde_json::Value>,
    ) {
        match value {
            serde_json::Value::Object(object) if !object.is_empty() => {
                for (key, value) in object {
                    visit(value, &path.child(key), fields);
                }
            }
            serde_json::Value::Array(array) if !array.is_empty() && array.iter().all(serde_json::Value::is_object) => {
                for (index, value) in array.iter().enumerate() {
                    visit(value, &path.child(index.to_string()), fields);
                }
            }
            _ if !path.is_root() => {
                fields.insert(path.clone(), value.clone());
            }
            _ => {}
        }
    }

    let mut fields = BTreeMap::new();
    visit(value, &schema::ConfigPath::root(), &mut fields);
    fields
}

fn display_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

struct ConfigSource {
    path: Option<PathBuf>,
    bytes: Vec<u8>,
}

impl ConfigSource {
    fn label(&self) -> String {
        self.path.as_ref().map_or_else(
            || "coded defaults (no configuration file)".to_owned(),
            |path| path.display().to_string(),
        )
    }
}

fn read_config_source(workspace_root: &Path, config_override: Option<&Path>) -> RailResult<ConfigSource> {
    if config_override == Some(Path::new("-")) {
        let mut bytes = Vec::new();
        std::io::stdin().lock().read_to_end(&mut bytes)?;
        return Ok(ConfigSource {
            path: Some(PathBuf::from("<stdin>")),
            bytes,
        });
    }
    let path = config_override
        .map(|path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace_root.join(path)
            }
        })
        .or_else(|| RailConfig::find_config_path(&crate::workspace::discovery_root(workspace_root)));
    read_config_path(path, config_override.is_some())
}

fn read_config_path(path: Option<PathBuf>, explicit: bool) -> RailResult<ConfigSource> {
    let bytes = path
        .as_ref()
        .map(|path| {
            std::fs::read(path).map_err(|error| {
                if explicit && error.kind() == std::io::ErrorKind::NotFound {
                    RailError::message(format!("specified config file not found: {}", path.display()))
                } else {
                    RailError::message(format!("failed to read {}: {error}", path.display()))
                }
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(ConfigSource { path, bytes })
}

enum Inspection {
    Complete(Box<DecodedConfig>, Vec<String>),
    /// Cargo selected a workspace whose discovered policy is a different file.
    Relocated(PathBuf),
}

/// Every independent violation found in one configuration source, with that source's label.
struct InspectionFailure {
    label: Option<String>,
    /// The inspected source file, when one was selected.
    path: Option<PathBuf>,
    errors: Vec<RailError>,
}

impl InspectionFailure {
    fn single(error: RailError) -> Self {
        Self {
            label: None,
            path: None,
            errors: vec![error],
        }
    }

    /// The same combined error a consuming command reports for this source.
    fn into_error(self) -> RailError {
        let error = match config::combine_errors(self.errors) {
            Err(error) => error,
            Ok(()) => RailError::message("configuration inspection failed without a cause"),
        };
        match self.label {
            Some(label) => error.context(format!("configuration {label}")),
            None => error,
        }
    }
}

fn inspect_config(
    workspace_root: &Path,
    config_override: Option<&Path>,
) -> RailResult<(ConfigSource, DecodedConfig, Vec<String>)> {
    inspect_config_issues(workspace_root, config_override).map_err(InspectionFailure::into_error)
}

fn inspect_config_issues(
    workspace_root: &Path,
    config_override: Option<&Path>,
) -> Result<(ConfigSource, DecodedConfig, Vec<String>), InspectionFailure> {
    let source = read_config_source(workspace_root, config_override).map_err(InspectionFailure::single)?;
    inspect_config_source(workspace_root, config_override, source)
}

fn inspect_config_source(
    workspace_root: &Path,
    config_override: Option<&Path>,
    source: ConfigSource,
) -> Result<(ConfigSource, DecodedConfig, Vec<String>), InspectionFailure> {
    let inspect = || -> Result<Inspection, Vec<RailError>> {
        let decoded = config::decode(&source.bytes).map_err(|error| vec![error])?;
        // Intrinsic errors remain diagnostic even if the surrounding Cargo workspace is broken.
        let policy_errors = decoded.config.policy_errors();
        if !policy_errors.is_empty() {
            return Err(policy_errors);
        }
        let metadata = match validation_evidence(workspace_root, config_override) {
            ValidationEvidence::Schema => None,
            ValidationEvidence::Workspace => Some(workspace_metadata(workspace_root).map_err(|error| vec![error])?),
        };
        // Cargo's reported root is authoritative when manifests alone could not prove it.
        if let Some(metadata) = &metadata
            && config_override.is_none()
        {
            let authoritative = RailConfig::find_config_path(metadata.workspace_root.as_std_path());
            if authoritative != source.path {
                return Ok(Inspection::Relocated(
                    metadata.workspace_root.clone().into_std_path_buf(),
                ));
            }
        }
        let warnings = if let Some(metadata) = metadata {
            let members = metadata
                .packages
                .iter()
                .filter(|package| metadata.workspace_members.contains(&package.id))
                .map(|package| package.name.to_string())
                .collect::<Vec<_>>();
            decoded
                .config
                .validate_workspace(metadata.workspace_root.as_std_path(), Some(&members))
                .map_err(|error| vec![error])?
        } else {
            decoded
                .config
                .validate_without_workspace()
                .map_err(|error| vec![error])?;
            Vec::new()
        };
        Ok(Inspection::Complete(Box::new(decoded), warnings))
    };
    match inspect().map_err(|errors| InspectionFailure {
        label: Some(source.label()),
        path: source.path.clone(),
        errors,
    })? {
        Inspection::Complete(decoded, warnings) => Ok((source, *decoded, warnings)),
        Inspection::Relocated(cargo_root) => {
            let relocated = read_config_path(RailConfig::find_config_path(&cargo_root), false)
                .map_err(InspectionFailure::single)?;
            inspect_config_source(&cargo_root, config_override, relocated)
        }
    }
}

/// Validate configuration file standalone (without WorkspaceContext)
///
/// This function can diagnose parse errors and unknown keys even when
/// the config file is broken.
pub fn run_config_validate_standalone(
    workspace_root: &Path,
    config_override: Option<&Path>,
    format: TextJsonOutputFormat,
    strictness: StrictnessMode,
) -> RailResult<()> {
    let json = format.is_json();
    let strict = strictness.is_strict();

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let config_path = match inspect_config_issues(workspace_root, config_override) {
        Ok((source, _, config_warnings)) => {
            warnings.extend(
                config_warnings
                    .into_iter()
                    .map(|warning| ValidationIssue::new("config", warning)),
            );
            source.path.map(|path| path.display().to_string())
        }
        Err(failure) => {
            // Each independent violation is its own issue, so one run reports them all.
            for error in &failure.errors {
                let mut issue = validation_issue_from_error(error);
                if issue.section != "cargo"
                    && let Some((line, column)) = extract_toml_error_location(&error.to_string())
                {
                    issue = issue.with_location(line, column);
                }
                errors.push(issue);
            }
            match failure.label {
                Some(_) => failure.path.map(|path| path.display().to_string()),
                None => config_override.map(|path| path.display().to_string()).or_else(|| {
                    RailConfig::find_config_path(&crate::workspace::discovery_root(workspace_root))
                        .map(|path| path.display().to_string())
                }),
            }
        }
    };

    let (final_errors, final_warnings) = if strict {
        let mut all_errors = errors;
        all_errors.extend(warnings);
        (all_errors, vec![])
    } else {
        (errors, warnings)
    };

    let valid = final_errors.is_empty();
    let evidence = validation_evidence(workspace_root, config_override);

    if json {
        let result = ValidationResult {
            command: "config",
            action: "validate",
            valid,
            evidence,
            config_path,
            errors: final_errors,
            warnings: final_warnings,
        };
        print_config_json(
            "validate",
            if valid { "success" } else { "failed" },
            if valid { 0 } else { 2 },
            &result,
        )?;
    } else if valid {
        println!("config: {}", config_path.as_deref().unwrap_or("coded defaults"));
        if strict && is_ci_environment() {
            println!("mode: strict (CI detected)");
        } else if strict {
            println!("mode: strict");
        }
        println!();

        if !final_warnings.is_empty() {
            eprintln!("warnings:");
            for w in &final_warnings {
                eprintln!("  [{}] {}", w.section, w.message);
            }
            eprintln!();
        }
        match evidence {
            ValidationEvidence::Workspace => println!("configuration is valid for the Cargo workspace"),
            ValidationEvidence::Schema => {
                println!("configuration is valid (schema only; no Cargo workspace was checked)")
            }
        }
    } else {
        eprintln!("config: {}", config_path.as_deref().unwrap_or("coded defaults"));
        if strict {
            eprintln!("mode: strict");
        }
        eprintln!();
        eprintln!("errors:");
        for error in &final_errors {
            let message = error.message.replace('\n', "\n    ");
            if let (Some(line), Some(column)) = (error.line, error.column) {
                eprintln!("  [{}:{}:{}] {}", error.section, line, column, message);
            } else {
                eprintln!("  [{}] {}", error.section, message);
            }
            if let Some(help) = &error.help {
                eprintln!("    help: {}", help.replace('\n', "\n          "));
            }
        }
        eprintln!();
        eprintln!("configuration has {} error(s)", final_errors.len());
    }

    if valid {
        Ok(())
    } else {
        Err(RailError::ExitWithCode { code: 2 })
    }
}

/// Extract line/column from toml_edit error message if present
fn extract_toml_error_location(err: &str) -> Option<(usize, usize)> {
    // toml_edit errors often contain "at line X column Y"
    if let Some(at_pos) = err.find("at line ") {
        let rest = err.get(at_pos..)?.strip_prefix("at line ")?;
        let parts: Vec<&str> = rest.split_whitespace().take(3).collect();
        if parts.len() >= 3
            && parts[1] == "column"
            && let (Ok(line), Ok(col)) = (parts[0].parse::<usize>(), parts[2].parse::<usize>())
        {
            return Some((line, col));
        }
    }
    None
}

fn validation_issue_from_error(error: &RailError) -> ValidationIssue {
    let mut issue = match error {
        RailError::Context { context, source } if context == WORKSPACE_VALIDATION => {
            ValidationIssue::new("cargo", source.to_string())
        }
        RailError::Context { source, .. } => return validation_issue_from_error(source),
        RailError::Config(ConfigError::InvalidField { field, reason }) => {
            ValidationIssue::new(field.split('.').next().unwrap_or("config"), reason.clone())
        }
        RailError::Config(ConfigError::InvalidValue { field, .. } | ConfigError::MissingField { field }) => {
            ValidationIssue::new(field.split('.').next().unwrap_or("config"), error.to_string())
        }
        _ => ValidationIssue::new("config", error.to_string()),
    };
    issue.help = error.help_message();
    issue
}

/// What validation can prove: policy alone, or policy bound to the Cargo workspace.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ValidationEvidence {
    /// Configuration syntax, fields, and values only; no Cargo workspace was loaded.
    Schema,
    /// Configuration checked against the workspace Cargo resolves, including its lockfile.
    Workspace,
}

fn validation_evidence(workspace_root: &Path, config_override: Option<&Path>) -> ValidationEvidence {
    let no_manifest = workspace_root
        .join("Cargo.toml")
        .symlink_metadata()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
    if config_override == Some(Path::new("-")) || no_manifest {
        ValidationEvidence::Schema
    } else {
        ValidationEvidence::Workspace
    }
}

fn workspace_metadata(workspace_root: &Path) -> RailResult<cargo_metadata::Metadata> {
    let mut command = cargo_metadata::MetadataCommand::new();
    command.current_dir(workspace_root);
    // Resolve against an existing lockfile exactly as consuming commands do; never create one.
    if crate::workspace::discovery_root(workspace_root)
        .join("Cargo.lock")
        .is_file()
    {
        command.other_options(vec!["--locked".to_string()]);
    } else {
        command.no_deps();
    }
    let metadata = crate::cargo::metadata::exec(
        &command,
        workspace_root,
        crate::cargo::metadata::CargoOutput::DiscoverFrom(workspace_root),
    )
    .map_err(|error| error.context(WORKSPACE_VALIDATION))?;
    crate::workspace::capture_metadata_paths(metadata)
}

/// One sparse-configuration migration preview or apply.
#[derive(Serialize)]
struct MigrationReport {
    config_path: Option<String>,
    /// `absent`, `unchanged`, `rewrite`, or `delete`.
    migration: &'static str,
    removed: Vec<config::migration::RemovedSetting>,
    /// Comment lines generated by `config print` that no longer describe the file.
    removed_comments: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation_plan: Option<crate::mutation::MutationPlan>,
}

/// The active configuration file and its planned sparse form.
struct PlannedMigration {
    path: PathBuf,
    migration: config::migration::ConfigMigration,
}

fn planned_migration(ctx: &crate::workspace::WorkspaceContext) -> RailResult<Option<PlannedMigration>> {
    let snapshot = ctx.snapshot()?;
    let (Some(file), Some(path)) = (snapshot.rail_config(), snapshot.rail_config_path()) else {
        return Ok(None);
    };
    let migration = config::migration::plan(file.bytes(), deletion_keeps_defaults(path))?;
    Ok(Some(PlannedMigration {
        path: path.to_path_buf(),
        migration,
    }))
}

/// Deleting the file keeps the coded defaults only when it is the one discovery candidate
/// present in its directory; otherwise deletion would expose another file.
fn deletion_keeps_defaults(path: &Path) -> bool {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|name| name.to_str())) else {
        return false;
    };
    let base = match (parent.file_name().and_then(|name| name.to_str()), name) {
        (Some(".cargo" | ".config"), "rail.toml") => parent.parent(),
        (_, "rail.toml" | ".rail.toml") => Some(parent),
        _ => None,
    };
    let Some(base) = base else {
        return false;
    };
    let present = [
        base.join("rail.toml"),
        base.join(".rail.toml"),
        base.join(".cargo").join("rail.toml"),
        base.join(".config").join("rail.toml"),
    ]
    .into_iter()
    .filter(|candidate| candidate.symlink_metadata().is_ok())
    .collect::<Vec<_>>();
    present.len() == 1
        && crate::utils::canonicalize_existing(&present[0]).ok() == crate::utils::canonicalize_existing(path).ok()
}

fn migration_mutation_plan(
    ctx: &crate::workspace::WorkspaceContext,
    planned: &PlannedMigration,
) -> RailResult<crate::mutation::MutationPlan> {
    use crate::config::migration::MigratedFile;
    use crate::mutation::{ExpectedMutation, MutationAction, MutationEffect, MutationTrace};

    let (code, effect) = match &planned.migration.file {
        MigratedFile::Delete => ("CONFIG_DELETE_DEFAULT_ONLY", MutationEffect::Delete),
        MigratedFile::Rewrite(_) | MigratedFile::Unchanged => ("CONFIG_PRUNE_DEFAULTS", MutationEffect::Write),
    };
    let authority_root = if ctx.has_git() {
        ctx.git()?.git().worktree_root.clone()
    } else {
        ctx.workspace_root().to_path_buf()
    };
    let relative = crate::utils::path_relative_to(&authority_root, &planned.path).map_err(|error| {
        RailError::message(format!(
            "configuration '{}' is outside authority root '{}': {error}",
            planned.path.display(),
            authority_root.display()
        ))
    })?;
    let relative = PathBuf::from(crate::utils::path_to_git_format(&relative));
    let content = match &planned.migration.file {
        MigratedFile::Rewrite(content) => Some(content.as_str()),
        MigratedFile::Delete | MigratedFile::Unchanged => None,
    };
    let action = MutationAction::new(code, relative.display().to_string(), None)
        .with_payload(serde_json::json!({
            "removed": planned.migration.removed,
            "removed_comments": planned.migration.removed_comments,
            "content": content,
        }))
        .with_mutations(vec![ExpectedMutation::capture(&authority_root, relative, effect)]);
    crate::mutation::build_plan(
        ctx,
        "config-migrate",
        vec![action],
        Vec::new(),
        vec![MutationTrace::new(
            "CONFIG_MIGRATION_PLANNED",
            "removed only settings whose removal leaves effective policy unchanged",
        )],
    )
}

fn migration_report(
    planned: Option<&PlannedMigration>,
    mutation_plan: Option<crate::mutation::MutationPlan>,
) -> MigrationReport {
    use crate::config::migration::MigratedFile;
    let Some(planned) = planned else {
        return MigrationReport {
            config_path: None,
            migration: "absent",
            removed: Vec::new(),
            removed_comments: Vec::new(),
            content: None,
            mutation_plan: None,
        };
    };
    let (migration, content) = match &planned.migration.file {
        MigratedFile::Unchanged => ("unchanged", None),
        MigratedFile::Rewrite(content) => ("rewrite", Some(content.clone())),
        MigratedFile::Delete => ("delete", None),
    };
    MigrationReport {
        config_path: Some(planned.path.display().to_string()),
        migration,
        removed: planned.migration.removed.clone(),
        removed_comments: planned.migration.removed_comments.clone(),
        content,
        mutation_plan,
    }
}

fn print_migration_text(report: &MigrationReport, applied: bool) {
    let Some(path) = &report.config_path else {
        println!("No cargo-rail configuration file; the coded defaults are in effect.");
        return;
    };
    if report.migration == "unchanged" {
        println!("{path}: already sparse; every setting is intentional project policy.");
        return;
    }
    let verb = if applied { "Removed" } else { "Would remove" };
    let (retired, defaults): (Vec<_>, Vec<_>) = report.removed.iter().partition(|setting| setting.removed_in.is_some());
    if !retired.is_empty() {
        println!("{path}: {verb} {} key(s) that earlier releases removed:", retired.len());
        for setting in retired {
            println!(
                "  - {} = {} (removed in Cargo-Rail {})",
                setting.path,
                setting.value,
                setting.removed_in.unwrap_or_default()
            );
        }
    }
    if !defaults.is_empty() {
        println!(
            "{path}: {verb} {} setting(s) that restate current defaults:",
            defaults.len()
        );
        for setting in defaults {
            println!("  - {} = {}", setting.path, setting.value);
        }
    }
    if !report.removed_comments.is_empty() {
        println!("  - the header comment written by `cargo rail config print`");
    }
    match (&report.content, applied) {
        (None, false) => println!(
            "Would delete {path}: every setting is a default. \
             The coded defaults then apply; `cargo rail config explain --all` lists them."
        ),
        (None, true) => println!(
            "Deleted {path}: every setting was a default. \
             The coded defaults apply; `cargo rail config explain --all` lists them."
        ),
        (Some(content), false) => println!("Resulting file:\n{content}"),
        (Some(_), true) => {}
    }
    if !applied {
        println!("Effective policy is unchanged. Apply with: cargo rail config migrate apply");
    }
}

/// Preview the sparse form of the active configuration without changing files.
pub fn run_config_migrate(
    ctx: &crate::workspace::WorkspaceContext,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    let planned = planned_migration(ctx)?;
    let pending = planned
        .as_ref()
        .is_some_and(|planned| planned.migration.file != config::migration::MigratedFile::Unchanged);
    let mutation_plan = planned
        .as_ref()
        .filter(|_| pending)
        .map(|planned| migration_mutation_plan(ctx, planned))
        .transpose()?;
    let report = migration_report(planned.as_ref(), mutation_plan);
    let exit_code = i32::from(check && pending);
    match format {
        TextJsonOutputFormat::Json => {
            print_config_json("migrate", if pending { "pending" } else { "clean" }, exit_code, &report)?
        }
        TextJsonOutputFormat::Text => print_migration_text(&report, false),
    }
    if exit_code != 0 {
        return Err(RailError::CheckHasPendingChanges);
    }
    Ok(())
}

/// Apply the previewed sparse migration after revalidating drift.
pub fn run_config_migrate_apply(
    ctx: &crate::workspace::WorkspaceContext,
    plan_path: Option<&Path>,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    use crate::config::migration::MigratedFile;
    use crate::mutation::MutationTrace;

    let Some(planned) = planned_migration(ctx)? else {
        let report = migration_report(None, None);
        return match format {
            TextJsonOutputFormat::Json => print_config_json("migrate-apply", "clean", 0, &report),
            TextJsonOutputFormat::Text => {
                print_migration_text(&report, true);
                Ok(())
            }
        };
    };
    if planned.migration.file == MigratedFile::Unchanged {
        let report = migration_report(Some(&planned), None);
        return match format {
            TextJsonOutputFormat::Json => print_config_json("migrate-apply", "clean", 0, &report),
            TextJsonOutputFormat::Text => {
                print_migration_text(&report, true);
                Ok(())
            }
        };
    }
    let expected = migration_mutation_plan(ctx, &planned)?;
    let mutation_plan = if let Some(path) = plan_path {
        let approved = crate::mutation::read_plan_file(path)?;
        if !approved.operation_id.starts_with("config-migrate-") {
            return Err(RailError::with_help(
                format!("plan '{}' is not a configuration migration plan", path.display()),
                "use the mutation_plan from 'cargo rail config migrate -f json'",
            ));
        }
        crate::mutation::validate_pre_apply_with_allowed_paths(ctx, &approved, &[path.to_path_buf()])?;
        crate::mutation::validate_requested_operation(&approved, &expected)?;
        approved
    } else {
        crate::mutation::validate_pre_apply(ctx, &expected)?;
        expected
    };
    let plan_receipt = crate::mutation::write_receipt(
        ctx.workspace_root(),
        "config-migrate",
        "plan",
        "planned",
        mutation_plan.clone(),
        vec![MutationTrace::new(
            "CONFIG_MIGRATION_PLAN_CREATED",
            "created configuration migration plan",
        )],
    )?;
    crate::progress!("receipt: {}", plan_receipt.display());
    match &planned.migration.file {
        MigratedFile::Rewrite(content) => crate::utils::write_file_atomic(&planned.path, content.as_bytes())?,
        MigratedFile::Delete => std::fs::remove_file(&planned.path)
            .map_err(|error| RailError::message(format!("deleting {}: {error}", planned.path.display())))?,
        MigratedFile::Unchanged => {}
    }
    let allowed = plan_path.map(Path::to_path_buf).into_iter().collect::<Vec<_>>();
    crate::mutation::validate_changed_paths_with_allowed_paths(ctx, &mutation_plan, &allowed)?;
    let apply_receipt = crate::mutation::write_receipt(
        ctx.workspace_root(),
        "config-migrate",
        "apply",
        "applied",
        mutation_plan.clone(),
        vec![MutationTrace::new(
            "CONFIG_MIGRATION_APPLIED",
            "wrote the sparse configuration",
        )],
    )?;
    crate::progress!("receipt: {}", apply_receipt.display());
    let report = migration_report(Some(&planned), Some(mutation_plan));
    match format {
        TextJsonOutputFormat::Json => print_config_json("migrate-apply", "applied", 0, &report),
        TextJsonOutputFormat::Text => {
            print_migration_text(&report, true);
            Ok(())
        }
    }
}
