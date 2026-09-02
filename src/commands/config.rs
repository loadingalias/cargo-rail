//! Inspect, validate, explain, and migrate `rail.toml` repository policy.

use crate::commands::common::TextJsonOutputFormat;
use crate::config::{RailConfig, schema, v0_25};
use crate::error::{ConfigError, RailError, RailResult};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Component, Path, PathBuf};

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
    let (mut config, config_path) = load_config_with_path(workspace_root, config_override)?;
    let effective_surface_targets = config.surface.targets.effective(&config.targets);
    config.surface.targets = crate::config::SurfaceTargetSelection::Explicit(effective_surface_targets);

    if json {
        // JSON output: serialize the config struct
        #[derive(Serialize)]
        struct PrintResult {
            command: &'static str,
            action: &'static str,
            config_path: String,
            config: RailConfig,
        }

        let result = PrintResult {
            command: "config",
            action: "print",
            config_path: config_path.display().to_string(),
            config,
        };
        print_config_json("print", "success", 0, &result)?;
    } else {
        // TOML output: serialize to TOML with a header comment
        println!("# Effective configuration (loaded from {})", config_path.display());
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
    config_path: String,
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

    let config_path = resolve_config_path(workspace_root, config_override)?;
    let (config, bytes) = RailConfig::load_path_with_bytes(&config_path)?;
    let content = std::str::from_utf8(&bytes)
        .map_err(|error| RailError::message(format!("{} is not valid UTF-8: {error}", config_path.display())))?;
    let configured: serde_json::Value = toml_edit::de::from_str(content)
        .map_err(|error| RailError::message(format!("failed to parse {}: {error}", config_path.display())))?;
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
                format!("{} (inherited from targets)", config_path.display())
            } else if configured_value.is_some() {
                config_path.display().to_string()
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
        config_path: config_path.display().to_string(),
        fields,
    };
    if json {
        print_config_json("explain", "success", 0, &result)
    } else {
        if !all && requested_fields.is_empty() {
            if result.fields.is_empty() {
                println!("No configured overrides.");
            } else {
                for field in &result.fields {
                    println!(
                        "{} = {} ({})",
                        field.path,
                        display_json_value(&field.effective),
                        field.source
                    );
                }
            }
            return Ok(());
        }

        println!("Configuration: {}", config_path.display());
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

/// Load config from explicit path or search, returning both config and path
fn load_config_with_path(workspace_root: &Path, config_override: Option<&Path>) -> RailResult<(RailConfig, PathBuf)> {
    let config_path = resolve_config_path(workspace_root, config_override)?;
    let (config, _) = RailConfig::load_path_with_bytes(&config_path)?;
    let workspace_members = standalone_workspace_members(workspace_root)?;
    config.validate(workspace_root, workspace_members.as_deref())?;
    Ok((config, config_path))
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

    let mut errors: Vec<ValidationIssue> = Vec::new();
    let mut warnings: Vec<ValidationIssue> = Vec::new();

    // Find and read one configuration source. `-` is an explicit, read-only
    // stdin protocol so canonical output can be validated without a temp file.
    let (config_path, content) = match read_validation_source(workspace_root, config_override) {
        Ok(source) => source,
        Err(err) => {
            let is_default_lookup_miss = config_override.is_none();
            if json {
                let result = ValidationResult {
                    command: "config",
                    action: "validate",
                    valid: false,
                    config_path: None,
                    errors: vec![ValidationIssue::new(
                        "config",
                        if is_default_lookup_miss {
                            "no configuration file found".to_string()
                        } else {
                            err.to_string()
                        },
                    )],
                    warnings: vec![],
                };
                print_config_json("validate", "failed", 2, &result)?;
                return Err(RailError::ExitWithCode { code: 2 });
            }
            if is_default_lookup_miss {
                return Err(RailError::with_help(
                    "no configuration file found",
                    "run 'cargo rail init' to create one",
                ));
            }
            return Err(err);
        }
    };

    // Check for parse errors with line/column info
    let raw_doc: Result<toml_edit::DocumentMut, _> = content.parse();
    if let Err(parse_err) = &raw_doc {
        let err_str = parse_err.to_string();
        // Try to extract line/column from toml_edit error
        let issue = if let Some((line, col)) = extract_toml_error_location(&err_str) {
            ValidationIssue::new("syntax", format!("TOML parse error: {}", err_str)).with_location(line, col)
        } else {
            ValidationIssue::new("syntax", format!("TOML parse error: {}", err_str))
        };
        errors.push(issue);
    }

    // Check for unknown keys (only if parsing succeeded)
    if let Ok(doc) = &raw_doc {
        check_unknown_keys(doc, &mut errors);
    }

    // Try to load and validate semantically
    let parsed_config = RailConfig::parse_bytes(content.as_bytes())
        .map_err(|error| RailError::message(format!("failed to parse {}: {error}", config_path.display())));
    match parsed_config {
        Ok(config) => {
            match standalone_workspace_members(workspace_root)
                .and_then(|members| config.validate(workspace_root, members.as_deref()))
            {
                Ok(config_warnings) => warnings.extend(
                    config_warnings
                        .into_iter()
                        .map(|warning| ValidationIssue::new("config", warning)),
                ),
                Err(error) => errors.push(validation_issue_from_error(&error)),
            }
        }
        Err(err) => {
            // Only add if we didn't already catch it in raw parsing
            if errors.is_empty() {
                errors.push(ValidationIssue::new("config", format!("failed to load: {}", err)));
            }
        }
    }

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
            config_path: Some(config_path.display().to_string()),
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
        println!("config: {}", config_path.display());
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
        eprintln!("config: {}", config_path.display());
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

fn read_validation_source(workspace_root: &Path, config_override: Option<&Path>) -> RailResult<(PathBuf, String)> {
    if config_override == Some(Path::new("-")) {
        let mut content = String::new();
        std::io::stdin().lock().read_to_string(&mut content)?;
        return Ok((PathBuf::from("<stdin>"), content));
    }
    let path = resolve_config_path(workspace_root, config_override)?;
    let content = std::fs::read_to_string(&path)
        .map_err(|error| RailError::message(format!("failed to read {}: {error}", path.display())))?;
    Ok((path, content))
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

/// Check for unknown keys in the TOML document
fn check_unknown_keys(doc: &toml_edit::DocumentMut, warnings: &mut Vec<ValidationIssue>) {
    for path in schema::document_paths(doc) {
        if !schema::is_known_config_path(&path) {
            warnings.push(ValidationIssue::new(
                path.first().unwrap_or("config"),
                format!("unknown configuration key '{path}'"),
            ));
        }
    }
}

fn validation_issue_from_error(error: &RailError) -> ValidationIssue {
    let (field, message) = match error {
        RailError::Config(ConfigError::InvalidField { field, reason }) => (field.as_str(), reason.clone()),
        RailError::Config(ConfigError::InvalidValue { field, .. }) => (field.as_str(), error.to_string()),
        RailError::Config(ConfigError::MissingField { field }) => (field.as_str(), error.to_string()),
        _ => return ValidationIssue::new("config", error.to_string()),
    };
    ValidationIssue::new(field.split('.').next().unwrap_or("config"), message)
}

fn standalone_workspace_members(workspace_root: &Path) -> RailResult<Option<Vec<String>>> {
    if !workspace_root.join("Cargo.toml").is_file() {
        return Ok(None);
    }
    let mut command = cargo_metadata::MetadataCommand::new();
    command.current_dir(workspace_root).no_deps();
    let metadata = match command.exec() {
        Ok(metadata) => metadata,
        Err(_) => return Ok(None),
    };
    let member_ids = metadata.workspace_members.iter().collect::<BTreeSet<_>>();
    Ok(Some(
        metadata
            .packages
            .iter()
            .filter(|package| member_ids.contains(&package.id))
            .map(|package| package.name.to_string())
            .collect(),
    ))
}

// Config Migrate

#[derive(Serialize)]
struct ConfigMigrateResult {
    command: &'static str,
    action: &'static str,
    config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_config: Option<String>,
    changes: Vec<v0_25::MigrationChange>,
    has_changes: bool,
}

#[derive(Default)]
struct MigrationApplyResult {
    previous_config: Option<PathBuf>,
}

trait MigrationPrimitiveSeam {
    fn after_split_revalidation(&mut self, _config_path: &Path) -> RailResult<()> {
        Ok(())
    }

    fn after_temporary_prepared(&mut self, _config_path: &Path, _artifact_path: &Path) -> RailResult<()> {
        Ok(())
    }

    fn after_final_revalidation(&mut self, _config_path: &Path, _artifact_path: &Path) -> RailResult<()> {
        Ok(())
    }

    fn after_publication(&mut self, _config_path: &Path, _artifact_path: &Path) -> RailResult<()> {
        Ok(())
    }

    fn before_cleanup(&mut self, _config_path: &Path, _artifact_path: &Path) -> RailResult<()> {
        Ok(())
    }
}

struct NoMigrationPrimitiveSeam;

impl MigrationPrimitiveSeam for NoMigrationPrimitiveSeam {}

#[cfg(test)]
type MigrationPathFault<'a> = Box<dyn FnOnce(&Path, &Path) -> RailResult<()> + 'a>;
#[cfg(test)]
type MigrationPathValidation<'a> = Box<dyn FnOnce(&Path) -> RailResult<()> + 'a>;

#[cfg(test)]
#[derive(Default)]
struct TestMigrationPrimitiveSeam<'a> {
    after_split_revalidation: Option<MigrationPathValidation<'a>>,
    after_temporary_prepared: Option<MigrationPathFault<'a>>,
    after_final_revalidation: Option<MigrationPathFault<'a>>,
    after_publication: Option<MigrationPathFault<'a>>,
    before_cleanup: Option<MigrationPathFault<'a>>,
}

#[cfg(test)]
impl MigrationPrimitiveSeam for TestMigrationPrimitiveSeam<'_> {
    fn after_split_revalidation(&mut self, config_path: &Path) -> RailResult<()> {
        self.after_split_revalidation
            .take()
            .map_or(Ok(()), |fault| fault(config_path))
    }

    fn after_temporary_prepared(&mut self, config_path: &Path, artifact_path: &Path) -> RailResult<()> {
        self.after_temporary_prepared
            .take()
            .map_or(Ok(()), |fault| fault(config_path, artifact_path))
    }

    fn after_final_revalidation(&mut self, config_path: &Path, artifact_path: &Path) -> RailResult<()> {
        self.after_final_revalidation
            .take()
            .map_or(Ok(()), |fault| fault(config_path, artifact_path))
    }

    fn after_publication(&mut self, config_path: &Path, artifact_path: &Path) -> RailResult<()> {
        self.after_publication
            .take()
            .map_or(Ok(()), |fault| fault(config_path, artifact_path))
    }

    fn before_cleanup(&mut self, config_path: &Path, artifact_path: &Path) -> RailResult<()> {
        self.before_cleanup
            .take()
            .map_or(Ok(()), |fault| fault(config_path, artifact_path))
    }
}

/// Check or apply the exact v0.25.0-to-current semantic migration.
///
/// Coded defaults are never materialized. TOML data outside the named
/// migration fields remains owned by the input document.
pub fn run_config_migrate(
    workspace_root: &Path,
    config_override: Option<&Path>,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    run_config_migrate_with_seam(
        workspace_root,
        config_override,
        check,
        format,
        &mut NoMigrationPrimitiveSeam,
    )
}

fn run_config_migrate_with_seam<S: MigrationPrimitiveSeam>(
    workspace_root: &Path,
    config_override: Option<&Path>,
    check: bool,
    format: TextJsonOutputFormat,
    seam: &mut S,
) -> RailResult<()> {
    let config_path = resolve_config_path(workspace_root, config_override)?;
    let config_path = validate_migration_destination(workspace_root, &config_path)?;
    let original = read_stable_regular_file(&config_path, "configuration migration input")?;
    let mut split_manifests = SplitManifestAuthority::capture(workspace_root)?;
    let normalized = v0_25::normalize(&original, |package_root| split_manifests.read_and_retain(package_root))?;
    let workspace_members = migration_workspace_members(workspace_root)?;
    normalized.config.validate(workspace_root, Some(&workspace_members))?;
    let has_changes = !normalized.changes.is_empty();

    if check {
        emit_migration_result(&config_path, None, normalized.changes, has_changes, true, format)?;
        if has_changes {
            return Err(RailError::CheckHasPendingChanges);
        }
        return Ok(());
    }

    let mut apply_result = MigrationApplyResult::default();
    if has_changes {
        split_manifests.revalidate()?;
        seam.after_split_revalidation(&config_path)?;
        let mut destination = MigrationDestination::capture(workspace_root, &config_path, &original)?;
        apply_result = destination.replace_if_unchanged(
            &original,
            &normalized.bytes,
            || revalidate_migration_context(workspace_root, &workspace_members, &normalized.config, &split_manifests),
            seam,
        )?;
    }

    emit_migration_result(
        &config_path,
        apply_result.previous_config.as_deref(),
        normalized.changes,
        has_changes,
        false,
        format,
    )
}

fn normalized_split_member_path(package_root: &Path) -> RailResult<(PathBuf, Vec<OsString>)> {
    let mut normalized = PathBuf::new();
    let mut components = Vec::new();
    for component in package_root.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => {
                normalized.push(component);
                components.push(component.to_os_string());
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RailError::message(format!(
                    "legacy split member must be a workspace-relative path without parent traversal: {}",
                    package_root.display()
                )));
            }
        }
    }
    Ok((normalized, components))
}

fn split_manifest_changed(package_root: &Path) -> RailError {
    RailError::with_help(
        format!(
            "legacy split member manifest changed while migration was being prepared: {}",
            package_root.join("Cargo.toml").display()
        ),
        "inspect the concurrent edit, then run `cargo rail config migrate` again",
    )
}

#[cfg(unix)]
struct RetainedSplitManifest {
    package_root: PathBuf,
    components: Vec<OsString>,
    directory: std::fs::File,
    manifest: std::fs::File,
    expected_len: u64,
    expected_bytes: Vec<u8>,
}

#[cfg(unix)]
struct SplitManifestAuthority {
    root_path: PathBuf,
    root: std::fs::File,
    manifests: BTreeMap<PathBuf, RetainedSplitManifest>,
}

#[cfg(unix)]
impl SplitManifestAuthority {
    fn capture(workspace_root: &Path) -> RailResult<Self> {
        use rustix::fs::{Mode, OFlags};

        let root_path = crate::utils::canonicalize_existing(workspace_root).map_err(|error| {
            RailError::message(format!(
                "failed to resolve workspace root {}: {error}",
                workspace_root.display()
            ))
        })?;
        let root = rustix::fs::open(
            &root_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(|error| {
            RailError::message(format!(
                "failed to retain configuration migration workspace root '{}': {error}",
                root_path.display()
            ))
        })?;
        if !unix_path_matches_opened_directory(&root, &root_path)? {
            return Err(RailError::message(
                "workspace root changed while configuration migration was being prepared",
            ));
        }
        Ok(Self {
            root_path,
            root,
            manifests: BTreeMap::new(),
        })
    }

    fn read_and_retain(&mut self, package_root: &Path) -> RailResult<Vec<u8>> {
        let (package_root, components) = normalized_split_member_path(package_root)?;
        if let Some(retained) = self.manifests.get(&package_root) {
            self.revalidate_one(retained)?;
            return Ok(retained.expected_bytes.clone());
        }

        let directory = unix_open_relative_directory(&self.root, &components, &package_root)?;
        let mut manifest = unix_open_relative_manifest(&directory, &package_root)?;
        let metadata = manifest.metadata()?;
        if !metadata.is_file() {
            return Err(RailError::message(format!(
                "legacy split member manifest is not a regular file: {}",
                package_root.join("Cargo.toml").display()
            )));
        }
        let expected_len = metadata.len();
        let expected_bytes = read_opened_migration_input(
            &mut manifest,
            expected_len,
            &self.root_path.join(&package_root).join("Cargo.toml"),
        )?;
        if !unix_relative_path_matches_opened_file(
            &directory,
            std::ffi::OsStr::new("Cargo.toml"),
            &manifest,
            expected_len,
        )? {
            return Err(split_manifest_changed(&package_root));
        }
        self.manifests.insert(
            package_root.clone(),
            RetainedSplitManifest {
                package_root,
                components,
                directory,
                manifest,
                expected_len,
                expected_bytes: expected_bytes.clone(),
            },
        );
        Ok(expected_bytes)
    }

    fn revalidate(&self) -> RailResult<()> {
        if !unix_path_matches_opened_directory(&self.root, &self.root_path)? {
            return Err(RailError::message(
                "workspace root changed while configuration migration was being prepared",
            ));
        }
        for retained in self.manifests.values() {
            self.revalidate_one(retained)?;
        }
        Ok(())
    }

    fn revalidate_one(&self, retained: &RetainedSplitManifest) -> RailResult<()> {
        use std::os::unix::fs::MetadataExt as _;

        let current_directory = unix_open_relative_directory(&self.root, &retained.components, &retained.package_root)?;
        let retained_directory = retained.directory.metadata()?;
        let current_directory_metadata = current_directory.metadata()?;
        if retained_directory.dev() != current_directory_metadata.dev()
            || retained_directory.ino() != current_directory_metadata.ino()
        {
            return Err(split_manifest_changed(&retained.package_root));
        }
        if !unix_relative_path_matches_opened_file(
            &current_directory,
            std::ffi::OsStr::new("Cargo.toml"),
            &retained.manifest,
            retained.expected_len,
        )? {
            return Err(split_manifest_changed(&retained.package_root));
        }
        let mut manifest = retained.manifest.try_clone()?;
        let live = read_opened_migration_input(
            &mut manifest,
            retained.expected_len,
            &self.root_path.join(&retained.package_root).join("Cargo.toml"),
        )?;
        if live != retained.expected_bytes
            || !unix_relative_path_matches_opened_file(
                &current_directory,
                std::ffi::OsStr::new("Cargo.toml"),
                &retained.manifest,
                retained.expected_len,
            )?
        {
            return Err(split_manifest_changed(&retained.package_root));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn unix_open_relative_directory(
    root: &std::fs::File,
    components: &[OsString],
    package_root: &Path,
) -> RailResult<std::fs::File> {
    use rustix::fs::{Mode, OFlags};

    let mut directory = root.try_clone()?;
    for component in components {
        directory = rustix::fs::openat(
            &directory,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(|error| {
            RailError::message(format!(
                "cannot retain legacy split member directory '{}': {error}",
                package_root.display()
            ))
        })?;
    }
    Ok(directory)
}

#[cfg(unix)]
fn unix_open_relative_manifest(directory: &std::fs::File, package_root: &Path) -> RailResult<std::fs::File> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::openat(
        directory,
        "Cargo.toml",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(std::fs::File::from)
    .map_err(|error| {
        RailError::message(format!(
            "legacy split member manifest is not a regular, non-symlink file or could not be retained '{}': {error}",
            package_root.join("Cargo.toml").display()
        ))
    })
}

#[cfg(windows)]
struct RetainedWindowsPath {
    path: PathBuf,
    handle: std::fs::File,
    observation: crate::windows_fs::FileObservation,
}

#[cfg(windows)]
struct RetainedSplitManifest {
    package_root: PathBuf,
    _directories: Vec<RetainedWindowsPath>,
    manifest_path: PathBuf,
    manifest: std::fs::File,
    observation: crate::windows_fs::FileObservation,
    expected_bytes: Vec<u8>,
}

#[cfg(windows)]
struct SplitManifestAuthority {
    root_path: PathBuf,
    root: RetainedWindowsPath,
    manifests: BTreeMap<PathBuf, RetainedSplitManifest>,
}

#[cfg(windows)]
impl SplitManifestAuthority {
    fn capture(workspace_root: &Path) -> RailResult<Self> {
        let root_path = crate::utils::canonicalize_existing(workspace_root).map_err(|error| {
            RailError::message(format!(
                "failed to resolve workspace root {}: {error}",
                workspace_root.display()
            ))
        })?;
        let root = retain_windows_path(&root_path, true)?;
        Ok(Self {
            root_path,
            root,
            manifests: BTreeMap::new(),
        })
    }

    fn read_and_retain(&mut self, package_root: &Path) -> RailResult<Vec<u8>> {
        let (package_root, components) = normalized_split_member_path(package_root)?;
        if let Some(retained) = self.manifests.get(&package_root) {
            self.revalidate_one(retained)?;
            return Ok(retained.expected_bytes.clone());
        }
        let mut current = self.root_path.clone();
        let mut directories = Vec::with_capacity(components.len());
        for component in components {
            current.push(component);
            directories.push(retain_windows_path(&current, true)?);
        }
        let manifest_path = current.join("Cargo.toml");
        let mut manifest = crate::windows_fs::open_for_execution_guard(&manifest_path).map_err(|error| {
            RailError::message(format!(
                "failed to retain legacy split member manifest '{}': {error}",
                manifest_path.display()
            ))
        })?;
        let observation = crate::windows_fs::observe_file(&manifest)?;
        crate::windows_fs::prove_local_ntfs(&manifest, observation.volume_serial_number)?;
        if !manifest.metadata()?.is_file() {
            return Err(RailError::message(format!(
                "legacy split member manifest is not a regular file: {}",
                manifest_path.display()
            )));
        }
        let expected_bytes = read_opened_migration_input(&mut manifest, observation.size, &manifest_path)?;
        let retained = RetainedSplitManifest {
            package_root: package_root.clone(),
            _directories: directories,
            manifest_path,
            manifest,
            observation,
            expected_bytes: expected_bytes.clone(),
        };
        self.revalidate_one(&retained)?;
        self.manifests.insert(package_root, retained);
        Ok(expected_bytes)
    }

    fn revalidate(&self) -> RailResult<()> {
        if !windows_retained_path_matches(&self.root)? {
            return Err(RailError::message(
                "workspace root changed while configuration migration was being prepared",
            ));
        }
        for retained in self.manifests.values() {
            self.revalidate_one(retained)?;
        }
        Ok(())
    }

    fn revalidate_one(&self, retained: &RetainedSplitManifest) -> RailResult<()> {
        if retained
            ._directories
            .iter()
            .any(|directory| !windows_retained_path_matches(directory).unwrap_or(false))
            || !windows_path_matches_observation(&retained.manifest_path, retained.observation)?
        {
            return Err(split_manifest_changed(&retained.package_root));
        }
        let mut manifest = retained.manifest.try_clone()?;
        let live = read_opened_migration_input(&mut manifest, retained.observation.size, &retained.manifest_path)?;
        if live != retained.expected_bytes
            || crate::windows_fs::observe_file(&retained.manifest)? != retained.observation
        {
            return Err(split_manifest_changed(&retained.package_root));
        }
        Ok(())
    }
}

#[cfg(windows)]
fn retain_windows_path(path: &Path, directory: bool) -> RailResult<RetainedWindowsPath> {
    let handle = crate::windows_fs::open_for_mutable_directory_guard(path).map_err(|error| {
        RailError::message(format!(
            "failed to retain migration input '{}': {error}",
            path.display()
        ))
    })?;
    if handle.metadata()?.is_dir() != directory {
        return Err(RailError::message(format!(
            "configuration migration input has the wrong file type: {}",
            path.display()
        )));
    }
    let observation = crate::windows_fs::observe_file(&handle)?;
    crate::windows_fs::prove_local_ntfs(&handle, observation.volume_serial_number)?;
    Ok(RetainedWindowsPath {
        path: path.to_path_buf(),
        handle,
        observation,
    })
}

#[cfg(windows)]
fn windows_path_matches_observation(path: &Path, expected: crate::windows_fs::FileObservation) -> RailResult<bool> {
    let named = match crate::windows_fs::open_for_observation(path) {
        Ok(named) => named,
        Err(_) => return Ok(false),
    };
    let actual = crate::windows_fs::observe_file(&named)?;
    Ok(actual.volume_serial_number == expected.volume_serial_number && actual.file_id == expected.file_id)
}

#[cfg(windows)]
fn windows_retained_path_matches(retained: &RetainedWindowsPath) -> RailResult<bool> {
    let live = crate::windows_fs::observe_file(&retained.handle)?;
    if live.volume_serial_number != retained.observation.volume_serial_number
        || live.file_id != retained.observation.file_id
    {
        return Ok(false);
    }
    windows_path_matches_observation(&retained.path, retained.observation)
}

#[cfg(not(any(unix, windows)))]
struct SplitManifestAuthority;

#[cfg(not(any(unix, windows)))]
impl SplitManifestAuthority {
    fn capture(_workspace_root: &Path) -> RailResult<Self> {
        Err(RailError::message(
            "configuration migration requires retained workspace filesystem authority on this platform",
        ))
    }

    fn read_and_retain(&mut self, _package_root: &Path) -> RailResult<Vec<u8>> {
        Err(RailError::message(
            "configuration migration requires retained workspace filesystem authority on this platform",
        ))
    }

    fn revalidate(&self) -> RailResult<()> {
        Ok(())
    }
}

fn migration_workspace_members(workspace_root: &Path) -> RailResult<Vec<String>> {
    let manifest = workspace_root.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(RailError::message(format!(
            "configuration migration requires an authoritative Cargo workspace manifest: {}",
            manifest.display()
        )));
    }
    let mut command = cargo_metadata::MetadataCommand::new();
    command.current_dir(workspace_root).no_deps();
    let metadata = command.exec().map_err(|error| {
        RailError::message(format!(
            "failed to resolve authoritative Cargo workspace membership for configuration migration: {error}"
        ))
    })?;
    let member_ids = metadata.workspace_members.iter().collect::<BTreeSet<_>>();
    let mut members = metadata
        .packages
        .iter()
        .filter(|package| member_ids.contains(&package.id))
        .map(|package| package.name.to_string())
        .collect::<Vec<_>>();
    members.sort_unstable();
    members.dedup();
    Ok(members)
}

fn revalidate_migration_context(
    workspace_root: &Path,
    expected_members: &[String],
    normalized_config: &RailConfig,
    split_manifests: &SplitManifestAuthority,
) -> RailResult<()> {
    split_manifests.revalidate()?;
    let live_members = migration_workspace_members(workspace_root)?;
    if live_members != expected_members {
        return Err(RailError::with_help(
            "Cargo workspace membership changed while configuration migration was being prepared",
            "inspect the concurrent workspace edit, then run `cargo rail config migrate` again",
        ));
    }
    normalized_config.validate(workspace_root, Some(&live_members))?;
    Ok(())
}

fn migration_input_changed(config_path: &Path) -> RailError {
    RailError::with_help(
        format!(
            "configuration changed while migration was being prepared: {}",
            config_path.display()
        ),
        "inspect the concurrent edit, then run `cargo rail config migrate` again",
    )
}

fn migration_destination_changed(config_path: &Path, subject: &str) -> RailError {
    RailError::with_help(
        format!(
            "configuration {subject} changed while migration was being prepared: {}",
            config_path.display()
        ),
        "inspect the concurrent replacement, then run `cargo rail config migrate` again",
    )
}

#[cfg(any(unix, windows))]
fn read_opened_migration_input(
    opened: &mut std::fs::File,
    expected_len: u64,
    config_path: &Path,
) -> RailResult<Vec<u8>> {
    opened.rewind().map_err(|error| {
        RailError::message(format!(
            "failed to rewind configuration migration input '{}': {error}",
            config_path.display()
        ))
    })?;

    #[cfg(windows)]
    let before = crate::windows_fs::observe_file(opened)?;
    #[cfg(unix)]
    let before = crate::utils::stable_open_file_generation(opened);

    let capacity = usize::try_from(expected_len)
        .map_err(|_| RailError::message(format!("configuration '{}' is too large", config_path.display())))?;
    let limit = expected_len
        .checked_add(1)
        .ok_or_else(|| RailError::message(format!("configuration '{}' is too large", config_path.display())))?;
    let mut bytes = Vec::with_capacity(capacity);
    opened.take(limit).read_to_end(&mut bytes).map_err(|error| {
        RailError::message(format!(
            "failed to read configuration migration input '{}': {error}",
            config_path.display()
        ))
    })?;

    #[cfg(windows)]
    let after = crate::windows_fs::observe_file(opened)?;
    #[cfg(unix)]
    let after = crate::utils::stable_open_file_generation(opened);
    if before != after || bytes.len() as u64 != expected_len {
        return Err(migration_input_changed(config_path));
    }
    Ok(bytes)
}

#[cfg(unix)]
struct MigrationDestination {
    config_path: PathBuf,
    parent_path: PathBuf,
    file_name: OsString,
    parent: std::fs::File,
    input: std::fs::File,
    expected_len: u64,
    security_metadata: UnixSecurityMetadata,
}

#[cfg(unix)]
impl MigrationDestination {
    fn capture(workspace_root: &Path, config_path: &Path, original: &[u8]) -> RailResult<Self> {
        use rustix::fs::{Mode, OFlags};

        let revalidated = validate_migration_destination(workspace_root, config_path)?;
        if revalidated != config_path {
            return Err(migration_destination_changed(config_path, "destination"));
        }
        let parent_path = config_path
            .parent()
            .ok_or_else(|| RailError::message("configuration migration destination has no parent directory"))?
            .to_path_buf();
        let file_name = config_path
            .file_name()
            .ok_or_else(|| RailError::message("configuration migration destination has no file name"))?
            .to_os_string();

        let parent = rustix::fs::open(
            &parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(|error| {
            RailError::message(format!(
                "failed to retain configuration parent directory '{}': {error}",
                parent_path.display()
            ))
        })?;
        if !parent.metadata()?.is_dir() || !unix_path_matches_opened_directory(&parent, &parent_path)? {
            return Err(migration_destination_changed(config_path, "destination parent"));
        }

        let mut input = rustix::fs::openat(
            &parent,
            &file_name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(std::fs::File::from)
        .map_err(|error| {
            RailError::message(format!(
                "failed to retain configuration migration input '{}': {error}",
                config_path.display()
            ))
        })?;
        let input_metadata = input.metadata()?;
        if !input_metadata.is_file() {
            return Err(migration_destination_changed(config_path, "destination"));
        }
        let expected_len = input_metadata.len();
        let security_metadata = UnixSecurityMetadata::capture(&input, config_path)?;
        let live = read_opened_migration_input(&mut input, expected_len, config_path)?;
        if live != original {
            return Err(migration_input_changed(config_path));
        }
        if !unix_relative_path_matches_opened_file(&parent, &file_name, &input, expected_len)? {
            return Err(migration_destination_changed(config_path, "destination"));
        }

        Ok(Self {
            config_path: config_path.to_path_buf(),
            parent_path,
            file_name,
            parent,
            input,
            expected_len,
            security_metadata,
        })
    }

    fn replace_if_unchanged<F, S>(
        &mut self,
        original: &[u8],
        replacement: &[u8],
        mut validate_inputs: F,
        seam: &mut S,
    ) -> RailResult<MigrationApplyResult>
    where
        F: FnMut() -> RailResult<()>,
        S: MigrationPrimitiveSeam,
    {
        let (temporary_name, mut temporary) = create_relative_migration_temp(&self.parent, &self.config_path)?;
        let result = (|| {
            #[cfg(target_os = "macos")]
            std::fs::copy(unix_descriptor_path(&self.input), unix_descriptor_path(&temporary)).map_err(|error| {
                RailError::message(format!(
                    "failed to copy configuration ACL and security metadata into the retained replacement '{}': {error}",
                    self.config_path.display()
                ))
            })?;
            temporary.set_len(0).map_err(|error| {
                RailError::message(format!(
                    "failed to reset copied configuration replacement '{}': {error}",
                    self.config_path.display()
                ))
            })?;
            temporary.rewind().map_err(|error| {
                RailError::message(format!(
                    "failed to rewind copied configuration replacement '{}': {error}",
                    self.config_path.display()
                ))
            })?;
            temporary.write_all(replacement).map_err(|error| {
                RailError::message(format!(
                    "failed to write atomic configuration replacement '{}': {error}",
                    self.config_path.display()
                ))
            })?;
            self.security_metadata.apply_and_verify(&temporary, &self.config_path)?;
            temporary.sync_all().map_err(|error| {
                RailError::message(format!(
                    "failed to prepare atomic configuration replacement '{}': {error}",
                    self.config_path.display()
                ))
            })?;

            let temporary_path = unix_opened_path(&temporary)?;
            seam.after_temporary_prepared(&self.config_path, &temporary_path)?;
            validate_inputs()?;
            self.revalidate(original)?;
            seam.after_final_revalidation(&self.config_path, &temporary_path)?;
            unix_exchange_relative(&self.parent, &temporary_name, &self.file_name, &self.config_path)?;
            let previous_path = unix_opened_path(&self.input)?;
            seam.after_publication(&self.config_path, &previous_path)?;

            let published_validation = self.validate_owned_replacement(&mut temporary, replacement, &self.file_name);
            let mut published_changed = published_validation.is_err();
            let commit_validation = published_validation.and_then(|()| {
                if !unix_path_matches_opened_directory(&self.parent, &self.parent_path)? {
                    return Err(migration_destination_changed(&self.config_path, "destination parent"));
                }
                self.validate_original_at(&temporary_name, original)?;
                validate_inputs()
            });
            if let Err(validation_error) = commit_validation {
                if self
                    .validate_owned_replacement(&mut temporary, replacement, &self.file_name)
                    .is_err()
                {
                    published_changed = true;
                }
                if let Err(rollback_error) =
                    unix_exchange_relative(&self.parent, &temporary_name, &self.file_name, &self.config_path)
                {
                    return Err(RailError::with_help(
                        format!(
                            "configuration migration detected concurrent drift but could not restore the destination: {rollback_error}"
                        ),
                        "inspect the preserved configuration paths reported below before retrying",
                    ));
                }
                rustix::fs::fsync(&self.parent).map_err(|error| {
                    RailError::message(format!(
                        "failed to persist configuration migration rollback in '{}': {error}",
                        self.parent_path.display()
                    ))
                })?;
                if published_changed
                    || self
                        .validate_owned_replacement(&mut temporary, replacement, &temporary_name)
                        .is_err()
                {
                    return Err(RailError::with_help(
                        format!("configuration migration rolled back after concurrent drift: {validation_error}"),
                        "inspect the preserved configuration paths reported below before retrying",
                    ));
                }
                return Err(validation_error);
            }

            let previous_path = unix_opened_path(&self.input)?;
            seam.before_cleanup(&self.config_path, &previous_path)?;
            self.validate_owned_replacement(&mut temporary, replacement, &self.file_name)?;
            self.validate_original_handle(original)?;
            validate_inputs()?;
            rustix::fs::fsync(&self.parent).map_err(|error| {
                RailError::message(format!(
                    "failed to persist the configuration parent directory '{}': {error}",
                    self.parent_path.display()
                ))
            })?;
            Ok(MigrationApplyResult {
                previous_config: Some(unix_opened_path(&self.input)?),
            })
        })();

        result.map_err(|error| {
            let original_path = unix_opened_path(&self.input)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|path_error| format!("<retained original path unavailable: {path_error}>"));
            let artifact_path = unix_opened_path(&temporary)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|path_error| format!("<retained migration artifact path unavailable: {path_error}>"));
            error.context(format!(
                "preserved configuration paths: original={original_path}; migration_artifact={artifact_path}"
            ))
        })
    }

    fn revalidate(&mut self, original: &[u8]) -> RailResult<()> {
        if !unix_path_matches_opened_directory(&self.parent, &self.parent_path)? {
            return Err(migration_destination_changed(&self.config_path, "destination parent"));
        }
        let live = read_opened_migration_input(&mut self.input, self.expected_len, &self.config_path)?;
        if live != original {
            return Err(migration_input_changed(&self.config_path));
        }
        if !unix_relative_path_matches_opened_file(&self.parent, &self.file_name, &self.input, self.expected_len)? {
            return Err(migration_destination_changed(&self.config_path, "destination"));
        }
        Ok(())
    }

    fn validate_original_at(&mut self, name: &std::ffi::OsStr, original: &[u8]) -> RailResult<()> {
        if !unix_relative_path_matches_opened_file(&self.parent, name, &self.input, self.expected_len)? {
            return Err(migration_destination_changed(&self.config_path, "destination"));
        }
        let live = read_opened_migration_input(&mut self.input, self.expected_len, &self.config_path)?;
        if live != original {
            return Err(migration_input_changed(&self.config_path));
        }
        let live_security_metadata = UnixSecurityMetadata::capture(&self.input, &self.config_path)?;
        if live_security_metadata != self.security_metadata {
            return Err(migration_destination_changed(&self.config_path, "security metadata"));
        }
        Ok(())
    }

    fn validate_original_handle(&mut self, original: &[u8]) -> RailResult<()> {
        let live = read_opened_migration_input(&mut self.input, self.expected_len, &self.config_path)?;
        if live != original {
            return Err(migration_input_changed(&self.config_path));
        }
        let live_security_metadata = UnixSecurityMetadata::capture(&self.input, &self.config_path)?;
        if live_security_metadata != self.security_metadata {
            return Err(migration_destination_changed(&self.config_path, "security metadata"));
        }
        Ok(())
    }

    fn validate_owned_replacement(
        &self,
        temporary: &mut std::fs::File,
        replacement: &[u8],
        name: &std::ffi::OsStr,
    ) -> RailResult<()> {
        let expected_len = u64::try_from(replacement.len())
            .map_err(|_| RailError::message("configuration replacement length exceeds u64"))?;
        if !unix_relative_path_matches_opened_file(&self.parent, name, temporary, expected_len)? {
            return Err(migration_destination_changed(
                &self.config_path,
                "published destination",
            ));
        }
        let live = read_opened_migration_input(temporary, expected_len, &self.config_path)?;
        if live != replacement {
            return Err(migration_destination_changed(
                &self.config_path,
                "published destination bytes",
            ));
        }
        let security_metadata = UnixSecurityMetadata::capture(temporary, &self.config_path)?;
        if security_metadata != self.security_metadata {
            return Err(migration_destination_changed(
                &self.config_path,
                "published destination security metadata",
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct UnixSecurityMetadata {
    uid: u32,
    gid: u32,
    mode: u32,
    xattrs: BTreeMap<OsString, Vec<u8>>,
    #[cfg(target_os = "macos")]
    acl: Vec<exacl::AclEntry>,
    flags: u32,
}

#[cfg(unix)]
impl UnixSecurityMetadata {
    fn capture(file: &std::fs::File, config_path: &Path) -> RailResult<Self> {
        use std::os::unix::fs::MetadataExt as _;

        let before = crate::utils::stable_open_file_generation(file).ok_or_else(|| {
            RailError::message(format!(
                "cannot prove stable security metadata for configuration '{}'; migration was not applied",
                config_path.display()
            ))
        })?;
        let metadata = file.metadata()?;
        let xattrs = unix_xattrs(file).map_err(|error| {
            RailError::message(format!(
                "failed to capture configuration extended attributes '{}': {error}",
                config_path.display()
            ))
        })?;
        #[cfg(target_os = "macos")]
        let acl = unix_acl_snapshot(file, config_path)?;
        #[cfg(target_os = "linux")]
        let flags = rustix::fs::ioctl_getflags(file)
            .map_err(|error| {
                RailError::message(format!(
                    "failed to capture configuration inode flags '{}': {error}",
                    config_path.display()
                ))
            })?
            .bits();
        #[cfg(target_os = "macos")]
        let flags = {
            use std::os::macos::fs::MetadataExt as _;
            metadata.st_flags()
        };
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
        let flags = 0_u32;
        let after = crate::utils::stable_open_file_generation(file).ok_or_else(|| {
            RailError::message(format!(
                "cannot prove stable security metadata for configuration '{}'; migration was not applied",
                config_path.display()
            ))
        })?;
        if before != after {
            return Err(migration_input_changed(config_path));
        }
        if flags != 0 {
            return Err(RailError::with_help(
                format!(
                    "configuration has inode flags that cannot be preserved safely by migration: {}",
                    config_path.display()
                ),
                "remove the flags explicitly, migrate, then restore only the flags still required",
            ));
        }
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
        return Err(RailError::message(format!(
            "exact ACL preservation for configuration migration is unavailable on this Unix platform: {}",
            config_path.display()
        )));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Ok(Self {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode(),
            xattrs,
            #[cfg(target_os = "macos")]
            acl,
            flags,
        })
    }

    fn apply_and_verify(&self, file: &std::fs::File, config_path: &Path) -> RailResult<()> {
        use rustix::fs::Mode;
        use rustix::process::{Gid, Uid};
        use std::os::unix::fs::MetadataExt as _;

        let current = file.metadata()?;
        if current.uid() != self.uid || current.gid() != self.gid {
            rustix::fs::fchown(file, Some(Uid::from_raw(self.uid)), Some(Gid::from_raw(self.gid))).map_err(
                |error| {
                    RailError::message(format!(
                        "failed to preserve configuration ownership '{}': {error}",
                        config_path.display()
                    ))
                },
            )?;
        }
        #[cfg(target_os = "macos")]
        let raw_mode = u16::try_from(self.mode & 0o7777)
            .map_err(|_| RailError::message("configuration mode exceeds the macOS mode width"))?;
        #[cfg(not(target_os = "macos"))]
        let raw_mode = self.mode & 0o7777;
        if current.mode() & 0o7777 != self.mode & 0o7777 {
            rustix::fs::fchmod(file, Mode::from_raw_mode(raw_mode)).map_err(|error| {
                RailError::message(format!(
                    "failed to preserve configuration mode '{}': {error}",
                    config_path.display()
                ))
            })?;
        }

        let current_xattrs = unix_xattrs(file).map_err(|error| {
            RailError::message(format!(
                "failed to inspect replacement extended attributes '{}': {error}",
                config_path.display()
            ))
        })?;
        for name in current_xattrs.keys().filter(|name| !self.xattrs.contains_key(*name)) {
            rustix::fs::fremovexattr(file, name).map_err(|error| {
                RailError::message(format!(
                    "failed to remove an inherited replacement extended attribute '{}': {error}",
                    config_path.display()
                ))
            })?;
        }
        for (name, value) in &self.xattrs {
            rustix::fs::fsetxattr(file, name, value, rustix::fs::XattrFlags::empty()).map_err(|error| {
                RailError::message(format!(
                    "failed to preserve configuration extended attribute '{}': {error}",
                    config_path.display()
                ))
            })?;
        }
        #[cfg(target_os = "macos")]
        if unix_acl_snapshot(file, config_path)? != self.acl {
            return Err(RailError::message(format!(
                "replacement access control list does not exactly match configuration '{}'; migration was not applied",
                config_path.display()
            )));
        }
        let captured = Self::capture(file, config_path)?;
        if &captured != self {
            return Err(RailError::message(format!(
                "replacement security metadata does not exactly match configuration '{}'; migration was not applied",
                config_path.display()
            )));
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn unix_acl_snapshot(file: &std::fs::File, config_path: &Path) -> RailResult<Vec<exacl::AclEntry>> {
    let path = unix_acl_path(file)?;
    let before = crate::utils::stable_open_file_generation(file)
        .ok_or_else(|| RailError::message("configuration ACL generation evidence is unavailable"))?;
    if !unix_following_path_matches_opened_file(&path, file)? {
        return Err(migration_destination_changed(config_path, "ACL authority"));
    }
    let acl = exacl::getfacl(&path, None).map_err(|error| {
        RailError::message(format!(
            "failed to capture configuration access control list '{}': {error}",
            config_path.display()
        ))
    })?;
    let after = crate::utils::stable_open_file_generation(file)
        .ok_or_else(|| RailError::message("configuration ACL generation evidence is unavailable"))?;
    if before != after || !unix_following_path_matches_opened_file(&path, file)? || unix_acl_path(file)? != path {
        return Err(migration_destination_changed(config_path, "ACL authority"));
    }
    Ok(acl)
}

#[cfg(target_os = "macos")]
fn unix_acl_path(file: &std::fs::File) -> RailResult<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;

    let path = rustix::fs::getpath(file).map_err(|error| {
        RailError::message(format!(
            "failed to resolve retained configuration ACL authority: {error}"
        ))
    })?;
    Ok(PathBuf::from(OsString::from_vec(path.to_bytes().to_vec())))
}

#[cfg(target_os = "macos")]
fn unix_following_path_matches_opened_file(path: &Path, file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let path_metadata = std::fs::metadata(path)?;
    let file_metadata = file.metadata()?;
    Ok(path_metadata.is_file()
        && file_metadata.is_file()
        && path_metadata.dev() == file_metadata.dev()
        && path_metadata.ino() == file_metadata.ino()
        && path_metadata.len() == file_metadata.len())
}

#[cfg(target_os = "macos")]
fn unix_descriptor_path(file: &std::fs::File) -> PathBuf {
    use std::os::fd::AsRawFd as _;

    Path::new("/dev/fd").join(file.as_raw_fd().to_string())
}

#[cfg(unix)]
fn unix_opened_path(file: &std::fs::File) -> RailResult<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;

        std::fs::read_link(Path::new("/proc/self/fd").join(file.as_raw_fd().to_string()))
            .map_err(|error| RailError::message(format!("failed to resolve retained configuration path: {error}")))
    }
    #[cfg(target_vendor = "apple")]
    {
        use std::os::unix::ffi::OsStringExt as _;

        let path = rustix::fs::getpath(file)
            .map_err(|error| RailError::message(format!("failed to resolve retained configuration path: {error}")))?;
        Ok(PathBuf::from(OsString::from_vec(path.to_bytes().to_vec())))
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    {
        let _ = file;
        Err(RailError::message(
            "retained configuration path resolution is unavailable on this Unix platform",
        ))
    }
}

#[cfg(unix)]
fn unix_xattrs(file: &std::fs::File) -> std::io::Result<BTreeMap<OsString, Vec<u8>>> {
    use std::os::unix::ffi::OsStringExt as _;

    let mut empty = [0_u8; 0];
    let required = rustix::fs::flistxattr(file, &mut empty)?;
    let mut names = vec![0_u8; required];
    let read = rustix::fs::flistxattr(file, &mut names)?;
    names.truncate(read);
    if !names.is_empty() && names.last() != Some(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "extended attribute list was not NUL-terminated",
        ));
    }
    let mut attributes = BTreeMap::new();
    for raw_name in names.split(|byte| *byte == 0).filter(|name| !name.is_empty()) {
        let name = OsString::from_vec(raw_name.to_vec());
        let mut empty = [0_u8; 0];
        let required = rustix::fs::fgetxattr(file, &name, &mut empty)?;
        let mut value = vec![0_u8; required];
        let read = rustix::fs::fgetxattr(file, &name, &mut value)?;
        value.truncate(read);
        attributes.insert(name, value);
    }
    Ok(attributes)
}

#[cfg(all(unix, any(target_os = "linux", target_vendor = "apple")))]
fn unix_exchange_relative(
    parent: &std::fs::File,
    first: &std::ffi::OsStr,
    second: &std::ffi::OsStr,
    config_path: &Path,
) -> RailResult<()> {
    rustix::fs::renameat_with(parent, first, parent, second, rustix::fs::RenameFlags::EXCHANGE).map_err(|error| {
        RailError::message(format!(
            "failed to conditionally exchange configuration '{}': {error}",
            config_path.display()
        ))
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_vendor = "apple"))))]
fn unix_exchange_relative(
    _parent: &std::fs::File,
    _first: &std::ffi::OsStr,
    _second: &std::ffi::OsStr,
    config_path: &Path,
) -> RailResult<()> {
    Err(RailError::message(format!(
        "configuration migration requires atomic exchange rename support on this Unix platform: {}",
        config_path.display()
    )))
}

#[cfg(unix)]
fn unix_path_matches_opened_directory(opened: &std::fs::File, path: &Path) -> std::io::Result<bool> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt as _;

    let current = match rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(current) => std::fs::File::from(current),
        Err(_) => return Ok(false),
    };
    let opened = opened.metadata()?;
    let current = current.metadata()?;
    Ok(opened.is_dir() && current.is_dir() && opened.dev() == current.dev() && opened.ino() == current.ino())
}

#[cfg(unix)]
fn unix_relative_path_matches_opened_file(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    opened: &std::fs::File,
    expected_len: u64,
) -> std::io::Result<bool> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt as _;

    let current = match rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(current) => std::fs::File::from(current),
        Err(_) => return Ok(false),
    };
    let opened = opened.metadata()?;
    let current = current.metadata()?;
    Ok(opened.is_file()
        && current.is_file()
        && opened.len() == expected_len
        && current.len() == expected_len
        && opened.dev() == current.dev()
        && opened.ino() == current.ino())
}

#[cfg(unix)]
fn create_relative_migration_temp(parent: &std::fs::File, config_path: &Path) -> RailResult<(OsString, std::fs::File)> {
    use rustix::fs::{Mode, OFlags};

    for _ in 0..64 {
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).map_err(|error| {
            RailError::message(format!(
                "failed to generate an unguessable configuration temporary name: {error}"
            ))
        })?;
        let nonce = entropy.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let name = OsString::from(format!(".cargo-rail-config-migrate-{nonce}.tmp"));
        match rustix::fs::openat(
            parent,
            &name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(file) => return Ok((name, std::fs::File::from(file))),
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => {
                return Err(RailError::message(format!(
                    "failed to create retained-parent temporary file for '{}': {error}",
                    config_path.display()
                )));
            }
        }
    }
    Err(RailError::message(format!(
        "failed to create an exclusive temporary file for '{}' after 64 attempts",
        config_path.display()
    )))
}

#[cfg(windows)]
struct MigrationDestination {
    config_path: PathBuf,
    parent_path: PathBuf,
    parent: std::fs::File,
    parent_volume_serial_number: u64,
    parent_file_id: u64,
    input: Option<std::fs::File>,
    input_observation: crate::windows_fs::FileObservation,
    expected_len: u64,
}

#[cfg(windows)]
struct WindowsReplacementGuard {
    temporary_path: PathBuf,
    backup_path: PathBuf,
    temporary_observation: crate::windows_fs::FileObservation,
}

#[cfg(windows)]
struct WindowsPublishedState {
    published: std::fs::File,
    published_observation: crate::windows_fs::FileObservation,
    published_bytes: Vec<u8>,
    backup: std::fs::File,
    backup_observation: crate::windows_fs::FileObservation,
    backup_bytes: Vec<u8>,
}

#[cfg(windows)]
impl MigrationDestination {
    fn capture(workspace_root: &Path, config_path: &Path, original: &[u8]) -> RailResult<Self> {
        let revalidated = validate_migration_destination(workspace_root, config_path)?;
        if revalidated != config_path {
            return Err(migration_destination_changed(config_path, "destination"));
        }
        let parent_path = config_path
            .parent()
            .ok_or_else(|| RailError::message("configuration migration destination has no parent directory"))?
            .to_path_buf();
        let parent = crate::windows_fs::open_for_mutable_directory_guard(&parent_path).map_err(|error| {
            RailError::message(format!(
                "failed to retain configuration parent directory '{}': {error}",
                parent_path.display()
            ))
        })?;
        if !parent.metadata()?.is_dir() {
            return Err(migration_destination_changed(config_path, "destination parent"));
        }
        let parent_observation = crate::windows_fs::observe_file(&parent)?;
        crate::windows_fs::prove_local_ntfs(&parent, parent_observation.volume_serial_number)?;

        let mut input = crate::windows_fs::open_for_stable_byte_observation(config_path).map_err(|error| {
            RailError::message(format!(
                "failed to retain configuration migration input '{}': {error}",
                config_path.display()
            ))
        })?;
        let input_observation = crate::windows_fs::observe_file(&input)?;
        crate::windows_fs::prove_local_ntfs(&input, input_observation.volume_serial_number)?;
        let input_metadata = input.metadata()?;
        if !input_metadata.is_file() {
            return Err(migration_destination_changed(config_path, "destination"));
        }
        let expected_len = input_observation.size;
        let live = read_opened_migration_input(&mut input, expected_len, config_path)?;
        if live != original {
            return Err(migration_input_changed(config_path));
        }
        if !crate::utils::opened_file_matches_path(&input, config_path, expected_len)? {
            return Err(migration_destination_changed(config_path, "destination"));
        }
        let input_observation = crate::windows_fs::observe_file(&input)?;

        let destination = Self {
            config_path: config_path.to_path_buf(),
            parent_path,
            parent,
            parent_volume_serial_number: parent_observation.volume_serial_number,
            parent_file_id: parent_observation.file_id,
            input: Some(input),
            input_observation,
            expected_len,
        };
        if !destination.parent_path_matches_retained()? {
            return Err(migration_destination_changed(config_path, "destination parent"));
        }
        Ok(destination)
    }

    fn replace_if_unchanged<F, S>(
        &mut self,
        original: &[u8],
        replacement: &[u8],
        mut validate_inputs: F,
        seam: &mut S,
    ) -> RailResult<MigrationApplyResult>
    where
        F: FnMut() -> RailResult<()>,
        S: MigrationPrimitiveSeam,
    {
        let (temporary_path, mut temporary) = create_windows_migration_temp(&self.config_path)?;
        let preparation = (|| {
            temporary
                .write_all(replacement)
                .and_then(|()| temporary.sync_all())
                .map_err(|error| {
                    RailError::message(format!(
                        "failed to prepare atomic configuration replacement '{}': {error}",
                        self.config_path.display()
                    ))
                })?;
            seam.after_temporary_prepared(&self.config_path, &temporary_path)?;
            validate_inputs()?;
            self.revalidate(original)?;
            seam.after_final_revalidation(&self.config_path, &temporary_path)?;

            let temporary_observation = crate::windows_fs::observe_file(&temporary)?;
            crate::windows_fs::prove_local_ntfs(&temporary, temporary_observation.volume_serial_number)?;
            if temporary_observation.volume_serial_number != self.input_observation.volume_serial_number {
                return Err(RailError::message(format!(
                    "configuration replacement is not on the destination volume: {}",
                    self.config_path.display()
                )));
            }
            Ok(temporary_observation)
        })();
        let temporary_observation = match preparation {
            Ok(observation) => observation,
            Err(error) => {
                return Err(error.context(format!(
                    "migration replacement preserved at {}",
                    temporary_path.display()
                )));
            }
        };
        drop(temporary);
        let backup_path = random_windows_neighbor_path(&self.config_path, "backup")?;
        match crate::windows_fs::replace_file_with_backup(&self.config_path, &temporary_path, &backup_path) {
            Ok(()) => {
                let guard = WindowsReplacementGuard {
                    temporary_path,
                    backup_path,
                    temporary_observation,
                };
                self.finalize_windows_replacement(guard, original, replacement, &mut validate_inputs, seam)
            }
            Err(error) => Err(RailError::with_help(
                format!(
                    "failed to replace configuration while preserving Windows security metadata '{}': {error}",
                    self.config_path.display()
                ),
                format!(
                    "inspect the preserved replacement at {} and any prior destination at {} before retrying",
                    temporary_path.display(),
                    backup_path.display()
                ),
            )),
        }
    }

    fn finalize_windows_replacement<F, S>(
        &mut self,
        guard: WindowsReplacementGuard,
        original: &[u8],
        replacement: &[u8],
        validate_inputs: &mut F,
        seam: &mut S,
    ) -> RailResult<MigrationApplyResult>
    where
        F: FnMut() -> RailResult<()>,
        S: MigrationPrimitiveSeam,
    {
        let captured = self.capture_windows_published_state(&guard, original, replacement, validate_inputs, seam);
        let mut state = match captured {
            Ok(state) => state,
            Err(error) => {
                return self.rollback_windows_replacement(guard, original, replacement, error);
            }
        };

        let backup_path_before_cleanup = match crate::windows_fs::opened_path(&state.backup) {
            Ok(path) => path,
            Err(error) => {
                drop(state);
                return self.rollback_windows_replacement(guard, original, replacement, error.into());
            }
        };
        if let Err(error) = seam.before_cleanup(&self.config_path, &backup_path_before_cleanup) {
            drop(state);
            return self.rollback_windows_replacement(guard, original, replacement, error);
        }

        let final_validation = (|| {
            if crate::windows_fs::observe_file(&state.published)? != state.published_observation
                || !crate::utils::opened_file_matches_path(
                    &state.published,
                    &self.config_path,
                    state.published_observation.size,
                )?
                || read_opened_migration_input(
                    &mut state.published,
                    state.published_observation.size,
                    &self.config_path,
                )? != state.published_bytes
            {
                return Err(migration_destination_changed(
                    &self.config_path,
                    "published destination",
                ));
            }
            if crate::windows_fs::observe_file(&state.backup)? != state.backup_observation
                || read_opened_migration_input(&mut state.backup, state.backup_observation.size, &guard.backup_path)?
                    != state.backup_bytes
            {
                return Err(migration_destination_changed(
                    &self.config_path,
                    "previous configuration",
                ));
            }
            let retained_input = self
                .input
                .as_ref()
                .ok_or_else(|| RailError::message("configuration input authority is unavailable"))?;
            if crate::windows_fs::observe_file(retained_input)? != self.input_observation {
                return Err(migration_destination_changed(
                    &self.config_path,
                    "previous configuration metadata",
                ));
            }
            validate_inputs()
        })();
        if let Err(error) = final_validation {
            drop(state);
            return self.rollback_windows_replacement(guard, original, replacement, error);
        }

        let backup_path = match crate::windows_fs::opened_path(&state.backup) {
            Ok(path) => path,
            Err(error) => {
                drop(state);
                return self.rollback_windows_replacement(
                    guard,
                    original,
                    replacement,
                    RailError::message(format!(
                        "the retained previous configuration path could not be resolved after final validation: {error}"
                    )),
                );
            }
        };
        drop(state.published);
        drop(self.input.take());
        match crate::windows_fs::delete_file_by_handle(state.backup) {
            Ok(()) => Ok(MigrationApplyResult::default()),
            Err(error) => Err(RailError::with_help(
                format!("configuration was replaced but its exact previous version could not be removed: {error}"),
                format!(
                    "the previous configuration remains at {}; inspect it before retrying",
                    backup_path.display()
                ),
            )),
        }
    }

    fn capture_windows_published_state<F, S>(
        &mut self,
        guard: &WindowsReplacementGuard,
        original: &[u8],
        replacement: &[u8],
        validate_inputs: &mut F,
        seam: &mut S,
    ) -> RailResult<WindowsPublishedState>
    where
        F: FnMut() -> RailResult<()>,
        S: MigrationPrimitiveSeam,
    {
        seam.after_publication(&self.config_path, &guard.backup_path)?;
        let replacement_len = u64::try_from(replacement.len())
            .map_err(|_| RailError::message("configuration replacement length exceeds u64"))?;
        let mut published =
            crate::windows_fs::open_for_stable_byte_observation(&self.config_path).map_err(|error| {
                RailError::message(format!(
                    "failed to retain the published configuration for commit validation '{}': {error}",
                    self.config_path.display()
                ))
            })?;
        let published_bytes = read_opened_migration_input(&mut published, replacement_len, &self.config_path)?;
        let published_observation = crate::windows_fs::observe_file(&published)?;
        if published_observation.volume_serial_number != guard.temporary_observation.volume_serial_number
            || published_observation.file_id != guard.temporary_observation.file_id
            || published_bytes != replacement
            || !crate::utils::opened_file_matches_path(&published, &self.config_path, published_observation.size)?
        {
            return Err(migration_destination_changed(
                &self.config_path,
                "published destination",
            ));
        }

        let mut backup =
            crate::windows_fs::open_for_stable_byte_observation_and_delete(&guard.backup_path).map_err(|error| {
                RailError::message(format!(
                    "failed to retain the Windows configuration backup '{}': {error}",
                    guard.backup_path.display()
                ))
            })?;
        let backup_observation = crate::windows_fs::observe_file(&backup)?;
        let backup_bytes = read_opened_migration_input(&mut backup, backup_observation.size, &guard.backup_path)?;
        if backup_observation != self.input_observation
            || backup_bytes != original
            || backup_observation.size != self.expected_len
            || crate::windows_fs::observe_file(&backup)? != backup_observation
            || !crate::utils::opened_file_matches_path(&backup, &guard.backup_path, backup_observation.size)?
        {
            return Err(migration_destination_changed(
                &self.config_path,
                "previous configuration",
            ));
        }
        let retained_input = self
            .input
            .as_mut()
            .ok_or_else(|| RailError::message("configuration input authority is unavailable"))?;
        if crate::windows_fs::observe_file(retained_input)? != self.input_observation
            || !crate::utils::opened_file_matches_path(retained_input, &guard.backup_path, self.expected_len)?
            || read_opened_migration_input(retained_input, self.expected_len, &guard.backup_path)? != original
        {
            return Err(migration_destination_changed(
                &self.config_path,
                "previous configuration",
            ));
        }
        if !self.parent_path_matches_retained()? {
            return Err(migration_destination_changed(&self.config_path, "destination parent"));
        }
        validate_inputs()?;
        Ok(WindowsPublishedState {
            published,
            published_observation,
            published_bytes,
            backup,
            backup_observation,
            backup_bytes,
        })
    }

    fn rollback_windows_replacement(
        &mut self,
        guard: WindowsReplacementGuard,
        original: &[u8],
        replacement: &[u8],
        validation_error: RailError,
    ) -> RailResult<MigrationApplyResult> {
        let validation_message = validation_error.to_string();
        let readiness = (|| {
            let replacement_len = u64::try_from(replacement.len())
                .map_err(|_| RailError::message("configuration replacement length exceeds u64"))?;
            let mut published = crate::windows_fs::open_for_stable_byte_observation(&self.config_path)?;
            let published_observation = crate::windows_fs::observe_file(&published)?;
            if published_observation.volume_serial_number != guard.temporary_observation.volume_serial_number
                || published_observation.file_id != guard.temporary_observation.file_id
                || read_opened_migration_input(&mut published, replacement_len, &self.config_path)? != replacement
                || crate::windows_fs::observe_file(&published)? != published_observation
                || !crate::utils::opened_file_matches_path(&published, &self.config_path, replacement_len)?
            {
                return Err(migration_destination_changed(
                    &self.config_path,
                    "published destination",
                ));
            }

            let retained_input = self
                .input
                .as_mut()
                .ok_or_else(|| RailError::message("configuration input authority is unavailable"))?;
            let backup_path = crate::windows_fs::opened_path(retained_input)?;
            if crate::windows_fs::observe_file(retained_input)? != self.input_observation
                || !crate::utils::opened_file_matches_path(retained_input, &backup_path, self.expected_len)?
                || read_opened_migration_input(retained_input, self.expected_len, &backup_path)? != original
                || crate::windows_fs::observe_file(retained_input)? != self.input_observation
            {
                return Err(migration_destination_changed(
                    &self.config_path,
                    "previous configuration",
                ));
            }
            Ok(backup_path)
        })();
        let backup_path = match readiness {
            Ok(path) => path,
            Err(rollback_barrier) => {
                let retained_backup = self
                    .input
                    .as_ref()
                    .and_then(|file| crate::windows_fs::opened_path(file).ok())
                    .unwrap_or_else(|| guard.backup_path.clone());
                return Err(RailError::with_help(
                    format!(
                        "configuration migration failed validation ({validation_message}) and could not safely roll back: {rollback_barrier}"
                    ),
                    format!(
                        "inspect the published configuration at {} and the previous configuration at {}; the prepared replacement path was {}",
                        self.config_path.display(),
                        retained_backup.display(),
                        guard.temporary_path.display()
                    ),
                ));
            }
        };

        drop(self.input.take());
        let rollback_path = match random_windows_neighbor_path(&self.config_path, "rollback") {
            Ok(path) => path,
            Err(error) => {
                return Err(RailError::with_help(
                    format!(
                        "configuration migration failed validation ({validation_message}) and could not reserve rollback recovery: {error}"
                    ),
                    format!(
                        "inspect the published configuration at {} and the previous configuration at {}",
                        self.config_path.display(),
                        backup_path.display()
                    ),
                ));
            }
        };
        match crate::windows_fs::replace_file_with_backup(&self.config_path, &backup_path, &rollback_path) {
            Ok(()) => self.finalize_windows_rollback(guard, original, replacement, rollback_path, validation_error),
            Err(error) => Err(RailError::with_help(
                format!(
                    "configuration migration failed validation ({validation_message}) and rollback failed: {error}"
                ),
                format!(
                    "inspect the published configuration at {}, the previous configuration at {}, and any rollback artifact at {}",
                    self.config_path.display(),
                    backup_path.display(),
                    rollback_path.display()
                ),
            )),
        }
    }

    fn finalize_windows_rollback(
        &self,
        guard: WindowsReplacementGuard,
        original: &[u8],
        replacement: &[u8],
        rollback_path: PathBuf,
        validation_error: RailError,
    ) -> RailResult<MigrationApplyResult> {
        let validation_message = validation_error.to_string();
        let validation = (|| {
            let mut restored = crate::windows_fs::open_for_stable_byte_observation(&self.config_path)?;
            if crate::windows_fs::observe_file(&restored)? != self.input_observation
                || read_opened_migration_input(&mut restored, self.expected_len, &self.config_path)? != original
                || crate::windows_fs::observe_file(&restored)? != self.input_observation
                || !crate::utils::opened_file_matches_path(&restored, &self.config_path, self.expected_len)?
            {
                return Err(migration_destination_changed(&self.config_path, "restored destination"));
            }

            let mut rolled_aside = crate::windows_fs::open_for_stable_byte_observation_and_delete(&rollback_path)?;
            let rolled_observation = crate::windows_fs::observe_file(&rolled_aside)?;
            if rolled_observation.volume_serial_number != guard.temporary_observation.volume_serial_number
                || rolled_observation.file_id != guard.temporary_observation.file_id
                || read_opened_migration_input(&mut rolled_aside, rolled_observation.size, &rollback_path)?
                    != replacement
                || crate::windows_fs::observe_file(&rolled_aside)? != rolled_observation
                || !crate::utils::opened_file_matches_path(&rolled_aside, &rollback_path, rolled_observation.size)?
            {
                return Err(migration_destination_changed(&self.config_path, "rollback artifact"));
            }
            Ok(rolled_aside)
        })();
        let rolled_aside = match validation {
            Ok(file) => file,
            Err(error) => {
                return Err(RailError::with_help(
                    format!(
                        "configuration migration rolled back after validation failed ({validation_message}), but rollback verification failed: {error}"
                    ),
                    format!(
                        "inspect the restored configuration at {} and the preserved rollback artifact at {}",
                        self.config_path.display(),
                        rollback_path.display()
                    ),
                ));
            }
        };
        match crate::windows_fs::delete_file_by_handle(rolled_aside) {
            Ok(()) => Err(validation_error),
            Err(error) => Err(RailError::with_help(
                format!(
                    "configuration migration rolled back after validation failed ({validation_message}), but its exact rollback artifact could not be removed: {error}"
                ),
                format!(
                    "the restored configuration is at {}; inspect the rollback artifact at {}",
                    self.config_path.display(),
                    rollback_path.display()
                ),
            )),
        }
    }

    fn revalidate(&mut self, original: &[u8]) -> RailResult<()> {
        if !self.parent_path_matches_retained()? {
            return Err(migration_destination_changed(&self.config_path, "destination parent"));
        }
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| RailError::message("configuration input authority is unavailable"))?;
        let live = read_opened_migration_input(input, self.expected_len, &self.config_path)?;
        if live != original {
            return Err(migration_input_changed(&self.config_path));
        }
        if !crate::utils::opened_file_matches_path(input, &self.config_path, self.expected_len)? {
            return Err(migration_destination_changed(&self.config_path, "destination"));
        }
        let live_observation = crate::windows_fs::observe_file(input)?;
        if live_observation != self.input_observation {
            return Err(migration_destination_changed(&self.config_path, "destination metadata"));
        }
        Ok(())
    }

    fn parent_path_matches_retained(&self) -> RailResult<bool> {
        let retained = crate::windows_fs::observe_file(&self.parent)?;
        crate::windows_fs::prove_local_ntfs(&self.parent, retained.volume_serial_number)?;
        if retained.volume_serial_number != self.parent_volume_serial_number || retained.file_id != self.parent_file_id
        {
            return Ok(false);
        }
        let named = match crate::windows_fs::open_for_observation(&self.parent_path) {
            Ok(named) => named,
            Err(_) => return Ok(false),
        };
        if !named.metadata()?.is_dir() {
            return Ok(false);
        }
        let named = crate::windows_fs::observe_file(&named)?;
        Ok(named.volume_serial_number == self.parent_volume_serial_number && named.file_id == self.parent_file_id)
    }
}

#[cfg(windows)]
fn create_windows_migration_temp(config_path: &Path) -> RailResult<(PathBuf, std::fs::File)> {
    for _ in 0..64 {
        let candidate = random_windows_neighbor_path(config_path, "replacement")?;
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(RailError::message(format!(
                    "failed to create retained-parent replacement '{}': {error}",
                    candidate.display()
                )));
            }
        }
    }
    Err(RailError::message(format!(
        "failed to create an exclusive Windows replacement for '{}' after 64 attempts",
        config_path.display()
    )))
}

#[cfg(windows)]
fn random_windows_neighbor_path(config_path: &Path, purpose: &str) -> RailResult<PathBuf> {
    let file_name = config_path
        .file_name()
        .ok_or_else(|| RailError::message("configuration migration destination has no file name"))?
        .to_string_lossy();
    for _ in 0..64 {
        let mut entropy = [0_u8; 16];
        getrandom::fill(&mut entropy).map_err(|error| {
            RailError::message(format!("failed to generate a private Windows migration path: {error}"))
        })?;
        let nonce = entropy.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let candidate = config_path.with_file_name(format!(".{file_name}.cargo-rail-{purpose}-{nonce}"));
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(RailError::message(format!(
        "failed to reserve a private Windows {purpose} path for configuration migration"
    )))
}

#[cfg(not(any(unix, windows)))]
struct MigrationDestination;

#[cfg(not(any(unix, windows)))]
impl MigrationDestination {
    fn capture(_workspace_root: &Path, config_path: &Path, _original: &[u8]) -> RailResult<Self> {
        Err(RailError::message(format!(
            "configuration migration apply requires retained directory operations on this platform: {}",
            config_path.display()
        )))
    }

    fn replace_if_unchanged<F, S>(
        &mut self,
        _original: &[u8],
        _replacement: &[u8],
        _validate_inputs: F,
        _seam: &mut S,
    ) -> RailResult<MigrationApplyResult>
    where
        F: FnMut() -> RailResult<()>,
        S: MigrationPrimitiveSeam,
    {
        Err(RailError::message(
            "configuration migration apply requires retained directory operations on this platform",
        ))
    }
}

fn validate_migration_destination(workspace_root: &Path, config_path: &Path) -> RailResult<PathBuf> {
    let metadata = std::fs::symlink_metadata(config_path).map_err(|error| {
        RailError::message(format!(
            "failed to inspect configuration destination {}: {error}",
            config_path.display()
        ))
    })?;
    if !metadata.file_type().is_file() || crate::utils::is_symlink_or_reparse(&metadata) {
        return Err(RailError::message(format!(
            "configuration migration requires a regular, non-symlink file: {}",
            config_path.display()
        )));
    }
    let workspace_root = crate::utils::canonicalize_existing(workspace_root).map_err(|error| {
        RailError::message(format!(
            "failed to resolve workspace root {}: {error}",
            workspace_root.display()
        ))
    })?;
    let config_path = crate::utils::canonicalize_existing(config_path).map_err(|error| {
        RailError::message(format!(
            "failed to resolve configuration destination {}: {error}",
            config_path.display()
        ))
    })?;
    if !config_path.starts_with(&workspace_root) {
        return Err(RailError::message(format!(
            "configuration migration destination is outside the workspace: {}",
            config_path.display()
        )));
    }
    Ok(config_path)
}

fn read_stable_regular_file(path: &Path, description: &str) -> RailResult<Vec<u8>> {
    let named_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| RailError::message(format!("failed to inspect {}: {error}", path.display())))?;
    if !named_metadata.is_file() || crate::utils::is_symlink_or_reparse(&named_metadata) {
        return Err(RailError::message(format!(
            "{description} is not a regular, non-symlink file: {}",
            path.display()
        )));
    }

    #[cfg(windows)]
    let mut opened = crate::windows_fs::open_for_stable_byte_observation(path).map_err(|error| {
        RailError::message(format!(
            "failed to open {description} '{}' without following reparse points: {error}",
            path.display()
        ))
    })?;
    #[cfg(not(windows))]
    let mut opened = std::fs::File::open(path)
        .map_err(|error| RailError::message(format!("failed to open {description} '{}': {error}", path.display())))?;

    let expected_len = named_metadata.len();
    if !crate::utils::opened_file_matches_path(&opened, path, expected_len)? {
        return Err(RailError::message(format!(
            "{description} changed before it could be read: {}",
            path.display()
        )));
    }

    #[cfg(windows)]
    let before = {
        let observation = crate::windows_fs::observe_file(&opened)?;
        crate::windows_fs::prove_local_ntfs(&opened, observation.volume_serial_number)?;
        observation
    };
    #[cfg(not(windows))]
    let before = crate::utils::stable_open_file_generation(&opened);

    let capacity = usize::try_from(expected_len)
        .map_err(|_| RailError::message(format!("{description} '{}' is too large", path.display())))?;
    let limit = expected_len
        .checked_add(1)
        .ok_or_else(|| RailError::message(format!("{description} '{}' is too large", path.display())))?;
    let mut bytes = Vec::with_capacity(capacity);
    (&mut opened).take(limit).read_to_end(&mut bytes)?;

    #[cfg(windows)]
    let after = crate::windows_fs::observe_file(&opened)?;
    #[cfg(not(windows))]
    let after = crate::utils::stable_open_file_generation(&opened);
    if before != after
        || bytes.len() as u64 != expected_len
        || !crate::utils::opened_file_matches_path(&opened, path, expected_len)?
    {
        return Err(RailError::message(format!(
            "{description} changed while it was read: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

fn emit_migration_result(
    config_path: &Path,
    previous_config: Option<&Path>,
    changes: Vec<v0_25::MigrationChange>,
    has_changes: bool,
    check: bool,
    format: TextJsonOutputFormat,
) -> RailResult<()> {
    if format.is_json() {
        let result = ConfigMigrateResult {
            command: "config",
            action: "migrate",
            config_path: config_path.display().to_string(),
            previous_config: previous_config.map(|path| path.display().to_string()),
            changes,
            has_changes,
        };
        return print_config_json(
            "migrate",
            if check && has_changes {
                "pending_changes"
            } else if has_changes {
                "applied"
            } else {
                "success"
            },
            i32::from(check && has_changes),
            &result,
        );
    }

    if changes.is_empty() {
        println!("Configuration is current.");
        if crate::output::is_verbose() {
            println!("Config: {}", config_path.display());
        }
        return Ok(());
    }

    println!(
        "{} {} semantic configuration change(s) in {}:",
        if check { "Pending:" } else { "Applied" },
        changes.len(),
        config_path.display()
    );
    for change in changes {
        if let Some(replacement) = change.replacement {
            println!("  replace {} with {}", change.path, replacement);
        } else {
            println!("  remove {}", change.path);
        }
        if crate::output::is_verbose() {
            println!("    {}", change.message);
        }
    }
    if let Some(previous_config) = previous_config {
        println!("Previous configuration: {}", previous_config.display());
    }
    if check {
        println!("Next: cargo rail config migrate");
    }
    Ok(())
}

fn resolve_config_path(workspace_root: &Path, config_override: Option<&Path>) -> RailResult<PathBuf> {
    if let Some(explicit_path) = config_override {
        let path = if explicit_path.is_absolute() {
            explicit_path.to_path_buf()
        } else {
            workspace_root.join(explicit_path)
        };
        if path.exists() {
            return Ok(path);
        }
        return Err(RailError::message(format!(
            "specified config file not found: {}",
            path.display()
        )));
    }

    RailConfig::find_config_path(workspace_root).ok_or_else(|| {
        RailError::with_help(
            "no rail.toml found".to_string(),
            "run 'cargo rail init' first to create a configuration file".to_string(),
        )
    })
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    const ORIGINAL: &[u8] = b"[release]\nrequire_clean = true\n";
    const CONCURRENT: &[u8] =
        b"# concurrent configuration replacement\n[release]\ntag_format = \"concurrent-{version}\"\n";

    struct TestWorkspace {
        _directory: tempfile::TempDir,
        root: PathBuf,
        config: PathBuf,
        #[cfg(unix)]
        member_manifest: PathBuf,
    }

    impl TestWorkspace {
        fn new(config: &[u8]) -> RailResult<Self> {
            let directory = tempfile::tempdir()?;
            let root = directory.path().to_path_buf();
            std::fs::create_dir_all(root.join(".config"))?;
            std::fs::create_dir_all(root.join("crates/member/src"))?;
            std::fs::write(
                root.join("Cargo.toml"),
                "[workspace]\nmembers = [\"crates/member\"]\nexclude = [\"host\"]\nresolver = \"2\"\n",
            )?;
            let member_manifest = root.join("crates/member/Cargo.toml");
            std::fs::write(
                &member_manifest,
                "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            )?;
            std::fs::write(root.join("crates/member/src/lib.rs"), "")?;
            let config_path = root.join(".config/rail.toml");
            std::fs::write(&config_path, config)?;
            Ok(Self {
                _directory: directory,
                root,
                config: config_path,
                #[cfg(unix)]
                member_manifest,
            })
        }

        fn migrate(&self, seam: &mut TestMigrationPrimitiveSeam<'_>) -> RailResult<()> {
            run_config_migrate_with_seam(&self.root, None, false, TextJsonOutputFormat::Text, seam)
        }

        #[cfg(unix)]
        fn unix_artifacts(&self) -> RailResult<Vec<PathBuf>> {
            let mut artifacts = std::fs::read_dir(self.config.parent().expect("configuration has a parent"))?
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".cargo-rail-config-migrate-")
                })
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            artifacts.sort();
            Ok(artifacts)
        }
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions provide the failure diagnostics"
    )]
    fn primitive_seam_preserves_split_drift_and_parent_substitution() -> RailResult<()> {
        let split_config = b"[crates.bundle.split]\nremote = \"../bundle\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \"crates/member\" }]\n";
        let workspace = TestWorkspace::new(split_config)?;
        let manifest = workspace.member_manifest.clone();
        let concurrent_manifest = b"[package]\nname = \"concurrent-member\"\nversion = \"9.9.9\"\nedition = \"2024\"\n";
        let mut seam = TestMigrationPrimitiveSeam {
            after_temporary_prepared: Some(Box::new(move |_, _| {
                std::fs::write(&manifest, concurrent_manifest)?;
                Ok(())
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        let error = workspace
            .migrate(&mut seam)
            .expect_err("split drift must abort migration");
        assert!(error.to_string().contains("split member manifest changed"));
        assert_eq!(std::fs::read(&workspace.config)?, split_config);
        assert_eq!(std::fs::read(&workspace.member_manifest)?, concurrent_manifest);
        assert_eq!(
            workspace.unix_artifacts()?.len(),
            1,
            "prepared artifact must be preserved"
        );

        let parent_workspace = TestWorkspace::new(ORIGINAL)?;
        let displaced_parent = parent_workspace.root.join(".config.cargo-rail-test-displaced");
        let mut seam = TestMigrationPrimitiveSeam {
            after_temporary_prepared: Some(Box::new({
                let displaced_parent = displaced_parent.clone();
                move |config_path, _| {
                    let parent = config_path.parent().expect("configuration has a parent");
                    std::fs::rename(parent, &displaced_parent)?;
                    std::fs::create_dir(parent)?;
                    std::fs::write(config_path, CONCURRENT)?;
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        parent_workspace
            .migrate(&mut seam)
            .expect_err("retained parent substitution must abort migration");
        assert_eq!(std::fs::read(&parent_workspace.config)?, CONCURRENT);
        assert_eq!(std::fs::read(displaced_parent.join("rail.toml"))?, ORIGINAL);
        assert_eq!(
            std::fs::read_dir(displaced_parent)?
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cargo-rail-config-migrate-"))
                .count(),
            1,
            "prepared artifact in the retained directory must be preserved"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions provide the failure diagnostics"
    )]
    fn primitive_seam_preserves_final_destination_and_split_authority_drifts() -> RailResult<()> {
        let destination_workspace = TestWorkspace::new(ORIGINAL)?;
        let displaced_destination = destination_workspace.config.with_extension("toml.before-final");
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new({
                let displaced_destination = displaced_destination.clone();
                move |config_path, _| {
                    std::fs::rename(config_path, &displaced_destination)?;
                    std::fs::write(config_path, CONCURRENT)?;
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        destination_workspace
            .migrate(&mut seam)
            .expect_err("a destination replacement after final validation must abort migration");
        assert_eq!(std::fs::read(&destination_workspace.config)?, CONCURRENT);
        assert_eq!(std::fs::read(&displaced_destination)?, ORIGINAL);
        assert_eq!(
            destination_workspace.unix_artifacts()?.len(),
            1,
            "the prepared migration must be preserved"
        );

        let split_config = b"[crates.bundle.split]\nremote = \"../bundle\"\nbranch = \"main\"\nmode = \"single\"\npaths = [{ crate = \"crates/member\" }]\n";
        let manifest_workspace = TestWorkspace::new(split_config)?;
        let manifest = manifest_workspace.member_manifest.clone();
        let concurrent_manifest = b"[package]\nname = \"concurrent-member\"\nversion = \"9.9.9\"\nedition = \"2024\"\n";
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new(move |_, _| {
                std::fs::write(&manifest, concurrent_manifest)?;
                Ok(())
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        manifest_workspace
            .migrate(&mut seam)
            .expect_err("split manifest drift after final validation must roll back migration");
        assert_eq!(std::fs::read(&manifest_workspace.config)?, split_config);
        assert_eq!(std::fs::read(&manifest_workspace.member_manifest)?, concurrent_manifest);
        assert_eq!(manifest_workspace.unix_artifacts()?.len(), 1);

        let ancestor_workspace = TestWorkspace::new(split_config)?;
        let crates = ancestor_workspace.root.join("crates");
        let displaced_crates = ancestor_workspace.root.join("crates.before-final");
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new({
                let crates = crates.clone();
                let displaced_crates = displaced_crates.clone();
                move |_, _| {
                    std::fs::rename(&crates, &displaced_crates)?;
                    std::fs::create_dir_all(crates.join("member"))?;
                    std::fs::write(crates.join("member/Cargo.toml"), concurrent_manifest)?;
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        ancestor_workspace
            .migrate(&mut seam)
            .expect_err("a split-member ancestor replacement must roll back migration");
        assert_eq!(std::fs::read(&ancestor_workspace.config)?, split_config);
        assert_eq!(std::fs::read(crates.join("member/Cargo.toml"))?, concurrent_manifest);
        assert!(displaced_crates.join("member/Cargo.toml").is_file());
        assert_eq!(ancestor_workspace.unix_artifacts()?.len(), 1);

        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata_workspace = TestWorkspace::new(ORIGINAL)?;
        let initial_mode = std::fs::metadata(&metadata_workspace.config)?.mode() & 0o7777;
        let changed_mode = if initial_mode == 0o600 { 0o640 } else { 0o600 };
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new(move |config_path, _| {
                std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(changed_mode))?;
                Ok(())
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        metadata_workspace
            .migrate(&mut seam)
            .expect_err("security metadata drift after final validation must roll back migration");
        assert_eq!(std::fs::read(&metadata_workspace.config)?, ORIGINAL);
        assert_eq!(
            std::fs::metadata(&metadata_workspace.config)?.mode() & 0o7777,
            changed_mode,
            "the concurrent metadata edit must survive rollback"
        );
        assert_eq!(metadata_workspace.unix_artifacts()?.len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions provide the failure diagnostics"
    )]
    fn primitive_seam_revalidates_transitive_host_after_publication() -> RailResult<()> {
        let config = b"[unify]\ntransitive_pinning = { host = \"host\" }\n[release]\nrequire_clean = true\n";
        let workspace = TestWorkspace::new(config)?;
        std::fs::create_dir_all(workspace.root.join("host/src"))?;
        let host_manifest = workspace.root.join("host/Cargo.toml");
        let displaced_manifest = workspace.root.join("host/Cargo.toml.concurrent");
        std::fs::write(
            &host_manifest,
            "[package]\nname = \"host\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        std::fs::write(workspace.root.join("host/src/lib.rs"), "")?;
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new({
                let displaced_manifest = displaced_manifest.clone();
                move |_, _| {
                    std::fs::rename(&host_manifest, &displaced_manifest)?;
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        let error = workspace
            .migrate(&mut seam)
            .expect_err("transitive host drift must fail post-publication validation");
        assert!(error.to_string().contains("unify.transitive_pinning.host"));
        assert_eq!(std::fs::read(&workspace.config)?, config);
        assert!(displaced_manifest.is_file());
        assert_eq!(
            workspace.unix_artifacts()?.len(),
            1,
            "rolled-back output must be preserved"
        );

        let membership_workspace = TestWorkspace::new(ORIGINAL)?;
        std::fs::create_dir_all(membership_workspace.root.join("crates/other/src"))?;
        std::fs::write(
            membership_workspace.root.join("crates/other/Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )?;
        std::fs::write(membership_workspace.root.join("crates/other/src/lib.rs"), "")?;
        std::fs::write(
            membership_workspace.root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/member\"]\nexclude = [\"host\", \"crates/other\"]\nresolver = \"2\"\n",
        )?;
        let workspace_manifest = membership_workspace.root.join("Cargo.toml");
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new(move |_, _| {
                std::fs::write(
                    &workspace_manifest,
                    "[workspace]\nmembers = [\"crates/member\", \"crates/other\"]\nexclude = [\"host\"]\nresolver = \"2\"\n",
                )?;
                Ok(())
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        let error = membership_workspace
            .migrate(&mut seam)
            .expect_err("workspace membership drift must roll back migration");
        assert!(error.to_string().contains("workspace membership changed"));
        assert_eq!(std::fs::read(&membership_workspace.config)?, ORIGINAL);
        assert_eq!(membership_workspace.unix_artifacts()?.len(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions provide the failure diagnostics"
    )]
    fn primitive_seam_preserves_post_publication_replacement() -> RailResult<()> {
        let workspace = TestWorkspace::new(ORIGINAL)?;
        let displaced_published = workspace
            .config
            .with_file_name("rail.toml.published-before-replacement");
        let mut seam = TestMigrationPrimitiveSeam {
            after_publication: Some(Box::new({
                let displaced_published = displaced_published.clone();
                move |config_path, _| {
                    std::fs::rename(config_path, &displaced_published)?;
                    std::fs::write(config_path, CONCURRENT)?;
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        workspace
            .migrate(&mut seam)
            .expect_err("post-publication replacement must not report success");
        assert_eq!(std::fs::read(&workspace.config)?, ORIGINAL);
        assert!(
            !std::fs::read(&displaced_published)?.is_empty(),
            "published migration output must remain preserved"
        );
        let artifacts = workspace.unix_artifacts()?;
        assert_eq!(artifacts.len(), 1);
        assert_eq!(std::fs::read(&artifacts[0])?, CONCURRENT);

        let bytes_workspace = TestWorkspace::new(ORIGINAL)?;
        let after_publication = b"# concurrent edit of published bytes\n";
        let mut seam = TestMigrationPrimitiveSeam {
            after_publication: Some(Box::new(move |config_path, _| {
                std::fs::write(config_path, after_publication)?;
                Ok(())
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        bytes_workspace
            .migrate(&mut seam)
            .expect_err("published byte drift must roll back without deletion");
        assert_eq!(std::fs::read(&bytes_workspace.config)?, ORIGINAL);
        let artifacts = bytes_workspace.unix_artifacts()?;
        assert_eq!(artifacts.len(), 1);
        assert_eq!(std::fs::read(&artifacts[0])?, after_publication);

        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let metadata_workspace = TestWorkspace::new(ORIGINAL)?;
        let initial_mode = std::fs::metadata(&metadata_workspace.config)?.mode() & 0o7777;
        let changed_mode = if initial_mode == 0o600 { 0o640 } else { 0o600 };
        let mut seam = TestMigrationPrimitiveSeam {
            after_publication: Some(Box::new(move |config_path, _| {
                std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(changed_mode))?;
                Ok(())
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        metadata_workspace
            .migrate(&mut seam)
            .expect_err("published metadata drift must roll back without deletion");
        assert_eq!(std::fs::read(&metadata_workspace.config)?, ORIGINAL);
        let artifacts = metadata_workspace.unix_artifacts()?;
        assert_eq!(artifacts.len(), 1);
        assert_eq!(std::fs::metadata(&artifacts[0])?.mode() & 0o7777, changed_mode);
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn primitive_seam_preserves_windows_destination_replacements() {
        let final_workspace = TestWorkspace::new(ORIGINAL).expect("final-revalidation workspace");
        let displaced_original = final_workspace.config.with_extension("toml.before-final");
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new({
                let displaced_original = displaced_original.clone();
                move |config_path, _| {
                    std::fs::rename(config_path, &displaced_original)?;
                    std::fs::write(config_path, CONCURRENT)?;
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        final_workspace
            .migrate(&mut seam)
            .expect_err("a destination replacement after final validation must not report success");
        assert_eq!(
            std::fs::read(&final_workspace.config).expect("restored final-revalidation destination"),
            ORIGINAL
        );
        assert!(!displaced_original.exists());
        let backups = std::fs::read_dir(final_workspace.config.parent().expect("configuration has a parent"))
            .expect("final-revalidation artifacts")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".cargo-rail-backup-"))
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read(backups[0].path()).expect("preserved final-revalidation replacement"),
            CONCURRENT
        );

        let published_workspace = TestWorkspace::new(ORIGINAL).expect("post-publication workspace");
        let displaced_published = published_workspace.config.with_extension("toml.after-publication");
        let mut seam = TestMigrationPrimitiveSeam {
            after_publication: Some(Box::new({
                let displaced_published = displaced_published.clone();
                move |config_path, _| {
                    std::fs::rename(config_path, &displaced_published)?;
                    std::fs::write(config_path, CONCURRENT)?;
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        published_workspace
            .migrate(&mut seam)
            .expect_err("a post-publication replacement must not report success");
        assert_eq!(
            std::fs::read(&published_workspace.config).expect("preserved post-publication destination"),
            CONCURRENT
        );
        assert!(
            !std::fs::read(&displaced_published)
                .expect("displaced published configuration")
                .is_empty()
        );
        let backups = std::fs::read_dir(published_workspace.config.parent().expect("configuration has a parent"))
            .expect("post-publication artifacts")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".cargo-rail-backup-"))
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read(backups[0].path()).expect("preserved post-publication predecessor"),
            ORIGINAL
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions provide the failure diagnostics"
    )]
    fn primitive_seam_cleanup_substitution_never_deletes_concurrent_bytes() -> RailResult<()> {
        let workspace = TestWorkspace::new(ORIGINAL)?;
        let moved_previous = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let concurrent_path = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let mut seam = TestMigrationPrimitiveSeam {
            before_cleanup: Some(Box::new({
                let moved_previous = std::rc::Rc::clone(&moved_previous);
                let concurrent_path = std::rc::Rc::clone(&concurrent_path);
                move |_, artifact_path| {
                    let moved = artifact_path.with_file_name("cargo-rail-test-moved-previous.toml");
                    std::fs::rename(artifact_path, &moved)?;
                    std::fs::write(artifact_path, CONCURRENT)?;
                    *moved_previous.borrow_mut() = Some(moved);
                    *concurrent_path.borrow_mut() = Some(artifact_path.to_path_buf());
                    Ok(())
                }
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        workspace.migrate(&mut seam)?;
        let moved_previous = moved_previous
            .borrow()
            .clone()
            .expect("cleanup seam records moved previous path");
        let concurrent_path = concurrent_path
            .borrow()
            .clone()
            .expect("cleanup seam records concurrent path");
        assert_eq!(std::fs::read(&concurrent_path)?, CONCURRENT);
        #[cfg(unix)]
        assert_eq!(std::fs::read(&moved_previous)?, ORIGINAL);
        #[cfg(windows)]
        assert!(
            !moved_previous.exists(),
            "handle-bound disposition must delete only the moved previous file"
        );
        assert!(!std::fs::read_to_string(&workspace.config)?.contains("require_clean"));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions provide the failure diagnostics"
    )]
    fn primitive_seam_rolls_back_acl_drift_without_destroying_it() -> RailResult<()> {
        let workspace = TestWorkspace::new(ORIGINAL)?;
        let baseline_acl = exacl::getfacl(&workspace.config, None)
            .map_err(|error| RailError::message(format!("failed to read test ACL: {error}")))?;
        let mut seam = TestMigrationPrimitiveSeam {
            after_final_revalidation: Some(Box::new(move |config_path, _| {
                let uid = rustix::process::getuid().as_raw();
                let principal = if uid == 0 { "1" } else { "0" };
                let mut acl = exacl::getfacl(config_path, None)
                    .map_err(|error| RailError::message(format!("failed to read test ACL: {error}")))?;
                acl.push(exacl::AclEntry::allow_user(principal, exacl::Perm::READ, None));
                exacl::setfacl(&[config_path], &acl, None)
                    .map_err(|error| RailError::message(format!("failed to edit test ACL: {error}")))?;
                Ok(())
            })),
            ..TestMigrationPrimitiveSeam::default()
        };
        workspace.migrate(&mut seam).expect_err("ACL drift must roll back");
        assert_eq!(std::fs::read(&workspace.config)?, ORIGINAL);
        assert_ne!(
            exacl::getfacl(&workspace.config, None)
                .map_err(|error| RailError::message(format!("failed to read test ACL: {error}")))?,
            baseline_acl
        );
        Ok(())
    }
}
