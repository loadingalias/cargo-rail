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
    let output = crate::output::machine_json_envelope("config", mode, result, exit_code, payload_value);
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|e| RailError::message(e.to_string()))?
    );
    Ok(())
}

/// Validation result for JSON output
#[derive(Serialize)]
struct ValidationResult {
    command: &'static str,
    action: &'static str,
    valid: bool,
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
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<usize>,
}

impl ValidationIssue {
    fn new(section: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            section: section.into(),
            message: message.into(),
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

// Config Locate

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

    // If config path was explicitly provided, use it
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

    // Search standard locations
    let search_paths = [
        workspace_root.join("rail.toml"),
        workspace_root.join(".rail.toml"),
        workspace_root.join(".cargo").join("rail.toml"),
        workspace_root.join(".config").join("rail.toml"),
    ];

    let config_path = RailConfig::find_config_path(workspace_root);

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

// Config Print

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

    // Load config from explicit path or search
    let (source, decoded, _) = inspect_config(workspace_root, config_override)?;
    let config = decoded.config;

    if json {
        // JSON output: serialize the config struct
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
        // TOML output: serialize to TOML with a header comment
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
    compatibility: Vec<config::Compatibility>,
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
        let requested: BTreeSet<_> = requested_fields.iter().map(String::as_str).collect();
        let known: BTreeSet<_> = fields.iter().map(|field| field.path.as_str()).collect();
        let unknown: Vec<_> = requested.difference(&known).copied().collect();
        if !unknown.is_empty() {
            return Err(RailError::with_help(
                format!("unknown configuration field(s): {}", unknown.join(", ")),
                "run `cargo rail config explain --all` to list known fields",
            ));
        }
        fields.retain(|field| requested.contains(field.path.as_str()));
    }

    let result = ExplainResult {
        command: "config",
        action: "explain",
        config_path: source.path.as_ref().map(|path| path.display().to_string()),
        fields,
        compatibility: decoded.compatibility,
    };
    if json {
        print_config_json("explain", "success", 0, &result)
    } else {
        println!("Configuration: {}", source.label());
        if all {
            for fact in &result.compatibility {
                println!("{}: {}", fact.path, fact.message);
            }
        }
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
        .or_else(|| RailConfig::find_config_path(workspace_root));
    let bytes = path
        .as_ref()
        .map(|path| {
            std::fs::read(path).map_err(|error| {
                if config_override.is_some() && error.kind() == std::io::ErrorKind::NotFound {
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

fn inspect_config(
    workspace_root: &Path,
    config_override: Option<&Path>,
) -> RailResult<(ConfigSource, DecodedConfig, Vec<String>)> {
    let source = read_config_source(workspace_root, config_override)?;
    let standalone = config_override == Some(Path::new("-"));
    let mut metadata = None;
    let inspect = || -> RailResult<(DecodedConfig, Vec<String>)> {
        let decoded = if standalone {
            config::decode_without_workspace(&source.bytes)?
        } else {
            config::decode(&source.bytes, |relative| {
                if metadata.is_none() {
                    metadata = standalone_workspace_metadata(workspace_root)?;
                }
                let metadata = metadata
                    .as_ref()
                    .ok_or_else(|| config::workspace_context_required("split.paths"))?;
                config::resolve_split_member(metadata, relative)
            })?
        };
        // Intrinsic errors remain diagnostic even if the surrounding Cargo workspace is broken.
        decoded.config.validate_policy()?;
        if !standalone && metadata.is_none() {
            metadata = standalone_workspace_metadata(workspace_root)?;
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
                .validate(metadata.workspace_root.as_std_path(), Some(&members))?
        } else {
            decoded.config.validate_without_workspace()?;
            Vec::new()
        };
        Ok((decoded, warnings))
    };
    let (decoded, warnings) = inspect().map_err(|error| error.context(format!("configuration {}", source.label())))?;
    Ok((source, decoded, warnings))
}

// Config Validate

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
    let config_path = match inspect_config(workspace_root, config_override) {
        Ok((source, _, config_warnings)) => {
            warnings.extend(
                config_warnings
                    .into_iter()
                    .map(|warning| ValidationIssue::new("config", warning)),
            );
            source.path.map(|path| path.display().to_string())
        }
        Err(error) => {
            let mut issue = validation_issue_from_error(&error);
            if let Some((line, column)) = extract_toml_error_location(&error.to_string()) {
                issue = issue.with_location(line, column);
            }
            errors.push(issue);
            config_override
                .map(|path| path.display().to_string())
                .or_else(|| RailConfig::find_config_path(workspace_root).map(|path| path.display().to_string()))
        }
    };

    // In strict mode, warnings become errors
    let (final_errors, final_warnings) = if strict {
        let mut all_errors = errors;
        all_errors.extend(warnings);
        (all_errors, vec![])
    } else {
        (errors, warnings)
    };

    let valid = final_errors.is_empty();

    // Output
    if json {
        let result = ValidationResult {
            command: "config",
            action: "validate",
            valid,
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
        println!("configuration is valid");
    } else {
        eprintln!("config: {}", config_path.as_deref().unwrap_or("coded defaults"));
        if strict {
            eprintln!("mode: strict");
        }
        eprintln!();
        eprintln!("errors:");
        for error in &final_errors {
            if let (Some(line), Some(column)) = (error.line, error.column) {
                eprintln!("  [{}:{}:{}] {}", error.section, line, column, error.message);
            } else {
                eprintln!("  [{}] {}", error.section, error.message);
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
    let (field, message) = match error {
        RailError::Context { source, .. } => return validation_issue_from_error(source),
        RailError::Config(ConfigError::InvalidField { field, reason }) => (field.as_str(), reason.clone()),
        RailError::Config(ConfigError::InvalidValue { field, .. }) => (field.as_str(), error.to_string()),
        RailError::Config(ConfigError::MissingField { field }) => (field.as_str(), error.to_string()),
        _ => return ValidationIssue::new("config", error.to_string()),
    };
    ValidationIssue::new(field.split('.').next().unwrap_or("config"), message)
}

fn standalone_workspace_metadata(workspace_root: &Path) -> RailResult<Option<cargo_metadata::Metadata>> {
    match workspace_root.join("Cargo.toml").symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RailError::message(format!("cannot inspect Cargo workspace: {error}"))),
        Ok(_) => {}
    }
    let mut command = cargo_metadata::MetadataCommand::new();
    command.current_dir(workspace_root).no_deps();
    command
        .exec()
        .map(Some)
        .map_err(|error| RailError::message(format!("cannot validate Cargo workspace configuration: {error}")))
}
