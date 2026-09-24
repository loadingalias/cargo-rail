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
