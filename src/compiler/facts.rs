//! Versioned, compiler-independent typed fact fragments.
//!
//! The matched compiler driver emits this protocol, but it does not define the
//! protocol's trust boundary. The ordinary cargo-rail process authenticates the
//! exact run, view, compiler, driver, and compilation unit, then validates the
//! complete fragment before any graph consumer can observe it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read as _;
use std::path::Path;

pub(crate) use crate::compiler::fact_protocol::*;
use crate::error::{RailError, RailResult};
use crate::source::{ContentDigest, RepositoryPath};

pub(crate) const MAX_COMPILER_FACT_FRAGMENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPILER_FACT_ANNOUNCEMENT_BYTES: usize = 4 * 1024;
const MAX_FACT_ITEMS: usize = 1_000_000;
const MAX_FACT_EDGES: usize = 8_000_000;
const MAX_FACT_ENTRY_POINTS: usize = 1_000_000;
const MAX_FACT_RETENTIONS: usize = 1_000_000;
const MAX_FACT_SOURCES: usize = 1_000_000;
const MAX_FACT_STRINGS: usize = 1_000_000;
const MAX_FACT_STRING_BYTES: usize = 16 * 1024;

/// Expected authority for one authenticated compiler-message announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilerFactAnnouncementExpectation {
    run_authority: CompilerFactRunAuthority,
    producer_authority: CompilerFactProducerAuthority,
    unit_identity: String,
}

/// Canonical announcement accepted from an authenticated driver message.
pub(crate) struct ValidatedCompilerFactAnnouncement {
    announcement: CompilerFactAnnouncement,
}

/// Expected authority and completeness for one compilation unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilerFactExpectation {
    run_authority: CompilerFactRunAuthority,
    object: CompilerFactObjectExpectation,
}

/// Expected producer, compilation unit, and completeness for a reusable object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilerFactObjectExpectation {
    producer_authority: CompilerFactProducerAuthority,
    unit_identity: String,
    required_coverage: BTreeSet<CompilerFactCoverage>,
}

impl CompilerFactExpectation {
    pub(crate) fn new(
        run_authority: CompilerFactRunAuthority,
        producer_authority: CompilerFactProducerAuthority,
        unit_identity: String,
        required_coverage: BTreeSet<CompilerFactCoverage>,
    ) -> Self {
        Self {
            run_authority,
            object: CompilerFactObjectExpectation {
                producer_authority,
                unit_identity,
                required_coverage,
            },
        }
    }
}

impl CompilerFactObjectExpectation {
    pub(crate) fn new(
        producer_authority: CompilerFactProducerAuthority,
        unit_identity: String,
        required_coverage: BTreeSet<CompilerFactCoverage>,
    ) -> Self {
        Self {
            producer_authority,
            unit_identity,
            required_coverage,
        }
    }
}

impl CompilerFactAnnouncementExpectation {
    pub(crate) fn new(
        run_authority: CompilerFactRunAuthority,
        producer_authority: CompilerFactProducerAuthority,
        unit_identity: String,
    ) -> Self {
        Self {
            run_authority,
            producer_authority,
            unit_identity,
        }
    }

    #[cfg(test)]
    fn from_fragment(fragment: &CompilerFactFragment) -> Self {
        Self {
            run_authority: fragment.run_authority.clone(),
            producer_authority: fragment.object.producer_authority.clone(),
            unit_identity: fragment.object.unit.identity.clone(),
        }
    }
}

pub(crate) fn required_compiler_fact_coverage() -> BTreeSet<CompilerFactCoverage> {
    BTreeSet::from([
        CompilerFactCoverage::Definitions,
        CompilerFactCoverage::Visibility,
        CompilerFactCoverage::ExactSpans,
        CompilerFactCoverage::MacroProvenance,
        CompilerFactCoverage::BodyEdges,
        CompilerFactCoverage::InterfaceEdges,
        CompilerFactCoverage::ReexportEdges,
        CompilerFactCoverage::PrivacyEdges,
        CompilerFactCoverage::TraitDispatch,
        CompilerFactCoverage::ForeignExports,
        CompilerFactCoverage::GeneratedSources,
        CompilerFactCoverage::EntryPoints,
        CompilerFactCoverage::ConservativeRetention,
    ])
}

impl ValidatedCompilerFactAnnouncement {
    /// Decode one canonical fact announcement from a Cargo compiler message.
    ///
    /// Messages with another diagnostic code are unrelated and return `None`.
    /// Once the reserved code is present, malformed data is an operational
    /// failure rather than a diagnostic cargo-rail may silently ignore.
    pub(crate) fn from_compiler_message(
        diagnostic_code: Option<&str>,
        message: &str,
        expected: &CompilerFactAnnouncementExpectation,
    ) -> RailResult<Option<Self>> {
        if diagnostic_code != Some(COMPILER_FACT_ANNOUNCEMENT_CODE) {
            return Ok(None);
        }
        let payload = message
            .strip_prefix(COMPILER_FACT_ANNOUNCEMENT_PREFIX)
            .ok_or_else(|| RailError::message("compiler fact announcement has an incompatible message envelope"))?;
        if payload.is_empty() || payload.len() > MAX_COMPILER_FACT_ANNOUNCEMENT_BYTES {
            return Err(RailError::message(
                "compiler fact announcement is empty or exceeds its byte bound",
            ));
        }
        let announcement: CompilerFactAnnouncement = serde_json::from_str(payload)?;
        announcement.validate(expected)?;
        if serde_json::to_string(&announcement)? != payload {
            return Err(RailError::message("compiler fact announcement is not canonical JSON"));
        }
        Ok(Some(Self { announcement }))
    }

    pub(crate) fn object_identity(&self) -> &str {
        &self.announcement.object_identity
    }

    pub(crate) fn content_digest(&self) -> &str {
        &self.announcement.content_digest
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.announcement.bytes
    }

    fn sidecar_path(&self, directory: &Path) -> RailResult<std::path::PathBuf> {
        let digest = self
            .announcement
            .content_digest
            .strip_prefix("sha256:")
            .ok_or_else(|| RailError::message("compiler fact announcement content digest is invalid"))?;
        Ok(directory.join(format!("compiler-fact-fragment-sha256-{digest}.json")))
    }

    #[cfg(test)]
    fn encode_message(announcement: &CompilerFactAnnouncement) -> String {
        format!(
            "{COMPILER_FACT_ANNOUNCEMENT_PREFIX}{}",
            serde_json::to_string(announcement).expect("encode announcement")
        )
    }
}

impl CompilerFactAnnouncement {
    fn validate(&self, expected: &CompilerFactAnnouncementExpectation) -> RailResult<()> {
        if self.version != COMPILER_FACT_PROTOCOL_VERSION {
            return Err(RailError::message(
                "compiler fact announcement has an incompatible protocol version",
            ));
        }
        self.run_authority.validate()?;
        self.producer_authority.validate()?;
        validate_identity(&self.unit_identity, UNIT_IDENTITY_PREFIX, "compilation unit")?;
        validate_identity(&self.object_identity, FRAGMENT_OBJECT_IDENTITY_PREFIX, "object")?;
        validate_sha256(&self.content_digest, "compiler fact announcement content digest")?;
        if self.bytes == 0 {
            return Err(RailError::message("compiler fact announcement names an empty fragment"));
        }
        if self.bytes > u64::try_from(MAX_COMPILER_FACT_FRAGMENT_BYTES).unwrap_or(u64::MAX) {
            return Err(RailError::message(format!(
                "compiler fact announcement names a {}-byte fragment above its {}-byte bound",
                self.bytes, MAX_COMPILER_FACT_FRAGMENT_BYTES
            )));
        }
        if self.run_authority != expected.run_authority {
            return Err(RailError::message(
                "compiler fact announcement does not match its authorized run",
            ));
        }
        if self.producer_authority != expected.producer_authority {
            return Err(RailError::message(
                "compiler fact announcement does not match its authorized producer",
            ));
        }
        if self.unit_identity != expected.unit_identity {
            return Err(RailError::message(
                "compiler fact announcement does not match its authorized compilation unit",
            ));
        }
        Ok(())
    }
}

/// Load the one sidecar named by an authenticated compiler announcement.
pub(crate) fn load_announced_fragment(
    directory: &Path,
    announcement: &ValidatedCompilerFactAnnouncement,
    expected: &CompilerFactExpectation,
) -> RailResult<ValidatedCompilerFactFragment> {
    let path = announcement.sidecar_path(directory)?;
    let bytes = read_fragment_sidecar(&path)?;
    if bytes.len() as u64 != announcement.bytes() {
        return Err(RailError::message(
            "announced compiler fact sidecar does not match its compiler-message length",
        ));
    }
    let actual_digest = format!("sha256:{}", ContentDigest::sha256(&bytes));
    if actual_digest != announcement.content_digest() {
        return Err(RailError::message(
            "announced compiler fact sidecar does not match its compiler-message digest",
        ));
    }
    let fragment = ValidatedCompilerFactFragment::from_bytes(&bytes, expected)?;
    if fragment.object_identity() != announcement.object_identity() {
        return Err(RailError::message(
            "announced compiler fact sidecar does not match its object identity",
        ));
    }
    Ok(fragment)
}

/// Load one private rustdoc-child sidecar that Cargo cannot carry as a compiler message.
///
/// The caller supplies every still-authorized doctest unit. The sidecar's own
/// run, producer, unit, completeness, canonical bytes, and content-addressed
/// filename must all agree before its unit identity is returned.
pub(crate) fn load_discovered_doctest_fragment(
    path: &Path,
    expected: &BTreeMap<String, CompilerFactExpectation>,
) -> RailResult<(String, ValidatedCompilerFactFragment)> {
    let file_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| RailError::message("compiler fact sidecar name is not valid UTF-8"))?;
    let digest = file_name
        .strip_prefix("compiler-fact-fragment-sha256-")
        .and_then(|name| name.strip_suffix(".json"))
        .ok_or_else(|| RailError::message("compiler fact sidecar name is incompatible"))?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RailError::message(
            "compiler fact sidecar name has an invalid content digest",
        ));
    }
    let bytes = read_fragment_sidecar(path)?;
    if ContentDigest::sha256(&bytes).to_string() != digest {
        return Err(RailError::message(
            "compiler fact sidecar does not match its content-addressed filename",
        ));
    }
    let untrusted: CompilerFactFragment = serde_json::from_slice(&bytes)?;
    let unit_identity = untrusted.object.unit.identity;
    let expectation = expected
        .get(&unit_identity)
        .ok_or_else(|| RailError::message("compiler fact sidecar names an unauthorized compilation unit"))?;
    let fragment = ValidatedCompilerFactFragment::from_bytes(&bytes, expectation)?;
    Ok((unit_identity, fragment))
}

fn read_fragment_sidecar(path: &Path) -> RailResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RailError::message(format!(
            "failed to inspect compiler fact sidecar '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file()
        || crate::utils::is_symlink_or_reparse(&metadata)
        || metadata.len() == 0
        || metadata.len() > u64::try_from(MAX_COMPILER_FACT_FRAGMENT_BYTES).unwrap_or(u64::MAX)
    {
        return Err(RailError::message(
            "compiler fact sidecar is not an exact bounded real file",
        ));
    }
    let mut file = File::open(path)?;
    if !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "compiler fact sidecar changed before it was opened or has multiple links",
        ));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| RailError::message("compiler fact sidecar length exceeds this platform"))?;
    let mut bytes = Vec::with_capacity(capacity);
    (&mut file)
        .take(
            u64::try_from(MAX_COMPILER_FACT_FRAGMENT_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)?;
    if bytes.len() != capacity || !crate::utils::private_file_matches_path(&file, path, metadata.len())? {
        return Err(RailError::message(
            "compiler fact sidecar changed while its bytes were read",
        ));
    }
    crate::instrumentation::record_hash(bytes.len());
    crate::instrumentation::record_hashed_file_bytes_read(bytes.len());
    Ok(bytes)
}

/// Authenticated, structurally complete compiler facts.
pub(crate) struct ValidatedCompilerFactFragment {
    fragment: CompilerFactFragment,
    object_identity: String,
    bytes: u64,
    object_bytes: u64,
}

/// Canonical run-independent fact content accepted for exact reuse.
#[derive(Debug, Clone)]
pub(crate) struct ValidatedCompilerFactObject {
    object: CompilerFactObject,
    identity: String,
    bytes: u64,
}

impl ValidatedCompilerFactFragment {
    /// Decode one canonical fragment and bind it to caller-owned authority.
    pub(crate) fn from_bytes(bytes: &[u8], expected: &CompilerFactExpectation) -> RailResult<Self> {
        if bytes.len() > MAX_COMPILER_FACT_FRAGMENT_BYTES {
            return Err(RailError::message(format!(
                "compiler fact fragment exceeds its {MAX_COMPILER_FACT_FRAGMENT_BYTES}-byte bound"
            )));
        }
        let fragment: CompilerFactFragment = serde_json::from_slice(bytes)?;
        fragment.validate(expected)?;
        let canonical = serde_json::to_vec(&fragment)?;
        if canonical != bytes {
            return Err(RailError::message("compiler fact fragment is not canonical JSON"));
        }
        let (object_identity, object_bytes) = fragment.object.identity_and_bytes()?;
        Ok(Self {
            fragment,
            object_identity,
            bytes: bytes.len() as u64,
            object_bytes,
        })
    }

    pub(crate) fn object_identity(&self) -> &str {
        &self.object_identity
    }

    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn into_object(self) -> ValidatedCompilerFactObject {
        ValidatedCompilerFactObject {
            object: self.fragment.object,
            identity: self.object_identity,
            bytes: self.object_bytes,
        }
    }
}

impl ValidatedCompilerFactObject {
    /// Decode a reusable object and bind it to exact producer and unit authority.
    pub(crate) fn from_bytes(bytes: &[u8], expected: &CompilerFactObjectExpectation) -> RailResult<Self> {
        if bytes.len() > MAX_COMPILER_FACT_FRAGMENT_BYTES {
            return Err(RailError::message(format!(
                "compiler fact object exceeds its {MAX_COMPILER_FACT_FRAGMENT_BYTES}-byte bound"
            )));
        }
        let object: CompilerFactObject = serde_json::from_slice(bytes)?;
        object.validate(expected)?;
        let canonical = serde_json::to_vec(&object)?;
        if canonical != bytes {
            return Err(RailError::message("compiler fact object is not canonical JSON"));
        }
        let identity = CompilerFactObject::identity_from_bytes(bytes);
        Ok(Self {
            object,
            identity,
            bytes: bytes.len() as u64,
        })
    }

    pub(crate) fn object(&self) -> &CompilerFactObject {
        &self.object
    }

    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl CompilerFactFragment {
    fn validate(&self, expected: &CompilerFactExpectation) -> RailResult<()> {
        if self.version != COMPILER_FACT_PROTOCOL_VERSION {
            return Err(RailError::message(
                "compiler fact fragment has an incompatible protocol version",
            ));
        }
        self.run_authority.validate()?;
        if self.run_authority != expected.run_authority {
            return Err(RailError::message(
                "compiler fact fragment does not match the authorized run and analysis view",
            ));
        }
        self.object.validate(&expected.object)
    }
}

impl CompilerFactObject {
    fn validate(&self, expected: &CompilerFactObjectExpectation) -> RailResult<()> {
        if self.version != COMPILER_FACT_PROTOCOL_VERSION {
            return Err(RailError::message(
                "compiler fact object has an incompatible protocol version",
            ));
        }
        self.producer_authority.validate()?;
        if self.producer_authority != expected.producer_authority || self.unit.identity != expected.unit_identity {
            return Err(RailError::message(
                "compiler fact object does not match the authorized producer and compilation unit",
            ));
        }
        self.unit.validate()?;
        self.validate_bounds()?;
        validate_strict_order(&self.strings, "compiler fact string table")?;
        for value in &self.strings {
            validate_text(value, "compiler fact string")?;
        }
        validate_strict_order(&self.sources, "compiler fact source table")?;
        let mut source_paths = BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            if !source_paths.insert(&source.path) {
                return Err(RailError::message(
                    "compiler fact source table contains a duplicate path",
                ));
            }
        }
        validate_strict_order(&self.items, "compiler fact item table")?;
        validate_strict_order(&self.edges, "compiler fact edge table")?;
        validate_strict_order(&self.entry_points, "compiler fact entry-point table")?;
        validate_strict_order(&self.retentions, "compiler fact retention table")?;

        let items = self
            .items
            .iter()
            .map(|item| (item.id, item))
            .collect::<BTreeMap<_, _>>();
        if items.len() != self.items.len() {
            return Err(RailError::message(
                "compiler fact item table contains a duplicate compiler identity",
            ));
        }
        let mut physical = BTreeSet::new();
        for item in &self.items {
            item.validate(&items, &self.sources, &self.strings)?;
            if !physical.insert(&item.physical) {
                return Err(RailError::message(
                    "compiler fact item table contains a duplicate physical identity",
                ));
            }
        }
        for edge in &self.edges {
            if !items.contains_key(&edge.source) {
                return Err(RailError::message(
                    "compiler fact edge source is not defined by its fragment",
                ));
            }
            self.validate_edge_coverage(edge.kind)?;
        }
        for entry in &self.entry_points {
            if !items.contains_key(&entry.item) {
                return Err(RailError::message(
                    "compiler fact entry point is not defined by its fragment",
                ));
            }
        }
        for retention in &self.retentions {
            if !items.contains_key(&retention.item) {
                return Err(RailError::message(
                    "compiler fact retention root is not defined by its fragment",
                ));
            }
            if let CompilerFactRetentionReason::Other(detail) = retention.reason {
                validate_string_id(detail, &self.strings)?;
            }
        }
        self.completion.validate(self, &expected.required_coverage)?;
        Ok(())
    }

    fn identity_and_bytes(&self) -> RailResult<(String, u64)> {
        let bytes = serde_json::to_vec(self)?;
        Ok((Self::identity_from_bytes(&bytes), bytes.len() as u64))
    }

    fn identity_from_bytes(bytes: &[u8]) -> String {
        format!("{FRAGMENT_OBJECT_IDENTITY_PREFIX}{}", ContentDigest::sha256(bytes))
    }

    fn validate_bounds(&self) -> RailResult<()> {
        for (actual, maximum, description) in [
            (self.strings.len(), MAX_FACT_STRINGS, "strings"),
            (self.sources.len(), MAX_FACT_SOURCES, "sources"),
            (self.items.len(), MAX_FACT_ITEMS, "items"),
            (self.edges.len(), MAX_FACT_EDGES, "edges"),
            (self.entry_points.len(), MAX_FACT_ENTRY_POINTS, "entry points"),
            (self.retentions.len(), MAX_FACT_RETENTIONS, "retentions"),
        ] {
            if actual > maximum {
                return Err(RailError::message(format!(
                    "compiler fact fragment exceeds its {maximum}-{description} bound"
                )));
            }
        }
        Ok(())
    }

    fn validate_edge_coverage(&self, kind: CompilerFactEdgeKind) -> RailResult<()> {
        let required = match kind {
            CompilerFactEdgeKind::Body => CompilerFactCoverage::BodyEdges,
            CompilerFactEdgeKind::Interface => CompilerFactCoverage::InterfaceEdges,
            CompilerFactEdgeKind::Reexport => CompilerFactCoverage::ReexportEdges,
            CompilerFactEdgeKind::VisibilityParent | CompilerFactEdgeKind::VisibilityRequirement => {
                CompilerFactCoverage::PrivacyEdges
            }
        };
        if !self.completion.coverage.contains(&required) {
            return Err(RailError::message(
                "compiler fact edge is outside the fragment's claimed coverage",
            ));
        }
        Ok(())
    }
}

impl CompilerFactRunAuthority {
    fn validate(&self) -> RailResult<()> {
        validate_identity(&self.run_identity, RUN_IDENTITY_PREFIX, "run")?;
        validate_identity(&self.view_identity, VIEW_IDENTITY_PREFIX, "view")
    }
}

impl CompilerFactProducerAuthority {
    pub(crate) fn validate(&self) -> RailResult<()> {
        validate_identity(&self.compiler_identity, COMPILER_IDENTITY_PREFIX, "compiler")?;
        validate_identity(&self.driver_identity, DRIVER_IDENTITY_PREFIX, "driver")
    }
}

impl CompilerFactUnit {
    pub(crate) fn bind_identity(mut self) -> RailResult<Self> {
        self.identity = self.calculate_identity()?;
        Ok(self)
    }

    fn calculate_identity(&self) -> RailResult<String> {
        let bytes = serde_json::to_vec(&(
            &self.invocation_identity,
            &self.package,
            &self.cargo_target,
            &self.crate_name,
            &self.target_kind,
            self.domain,
            self.role,
            &self.platform,
            &self.features,
            &self.cfg,
        ))?;
        Ok(format!("{UNIT_IDENTITY_PREFIX}{}", ContentDigest::sha256(&bytes)))
    }

    fn validate(&self) -> RailResult<()> {
        validate_identity(&self.identity, UNIT_IDENTITY_PREFIX, "compilation unit")?;
        validate_identity(
            &self.invocation_identity,
            INVOCATION_IDENTITY_PREFIX,
            "compiler invocation",
        )?;
        if self.identity != self.calculate_identity()? {
            return Err(RailError::message(
                "compiler fact compilation-unit identity does not match its canonical fields",
            ));
        }
        self.package.validate()?;
        for (value, description) in [
            (&self.cargo_target, "compiler fact Cargo target"),
            (&self.crate_name, "compiler fact crate name"),
            (&self.platform, "compiler fact platform"),
        ] {
            validate_text(value, description)?;
        }
        if let CompilerFactTargetKind::Other(name) = &self.target_kind {
            validate_text(name, "compiler fact target kind")?;
        }
        validate_strict_order(&self.features, "compiler fact feature set")?;
        validate_strict_order(&self.cfg, "compiler fact cfg set")?;
        for feature in &self.features {
            validate_text(feature, "compiler fact feature")?;
        }
        for cfg in &self.cfg {
            validate_text(cfg, "compiler fact cfg")?;
        }
        Ok(())
    }
}

impl CompilerFactPackage {
    fn validate(&self) -> RailResult<()> {
        validate_text(&self.name, "compiler fact package name")?;
        validate_text(&self.version, "compiler fact package version")?;
        semver::Version::parse(&self.version)
            .map_err(|error| RailError::message(format!("compiler fact package version is invalid: {error}")))?;
        if let Some(source) = &self.source {
            validate_text(source, "compiler fact package source")?;
        }
        Ok(())
    }
}

impl CompilerFactSource {
    fn validate(&self) -> RailResult<()> {
        validate_sha256(&self.content_digest, "compiler fact source digest")?;
        match &self.path {
            CompilerFactSourcePath::Repository(path) => validate_repository_path(path),
            CompilerFactSourcePath::Generated(path) => validate_generated_path(path),
        }
    }
}

impl CompilerItemFact {
    fn validate(
        &self,
        items: &BTreeMap<CompilerItemId, &CompilerItemFact>,
        sources: &[CompilerFactSource],
        strings: &[String],
    ) -> RailResult<()> {
        validate_span(self.physical.span, sources)?;
        validate_string_id(self.physical.source_context, strings)?;
        validate_string_id(self.name, strings)?;
        validate_string_id(self.diagnostic_path, strings)?;
        if self
            .parent
            .is_some_and(|parent| parent == self.id || !items.contains_key(&parent))
        {
            return Err(RailError::message(
                "compiler fact item has a missing or self-referential parent",
            ));
        }
        validate_visibility(&self.written_visibility, items)?;
        validate_visibility(&self.effective_visibility, items)?;
        match (&self.written_visibility, self.visibility_span) {
            (CompilerFactVisibility::Private, None) => {}
            (CompilerFactVisibility::Private, Some(_)) | (_, None) => {
                return Err(RailError::message(
                    "compiler fact written visibility does not match its source span",
                ));
            }
            (_, Some(visibility)) => {
                validate_span(visibility, sources)?;
                if visibility.source != self.physical.span.source
                    || visibility.start < self.physical.span.start
                    || visibility.end > self.physical.span.end
                {
                    return Err(RailError::message(format!(
                        "compiler fact visibility span {}..{} is outside declaration {}..{} for '{}'",
                        visibility.start,
                        visibility.end,
                        self.physical.span.start,
                        self.physical.span.end,
                        strings[self.diagnostic_path.0 as usize]
                    )));
                }
            }
        }
        match &self.macro_provenance {
            CompilerFactMacroProvenance::Written => {
                if !matches!(
                    sources[self.physical.span.source as usize].path,
                    CompilerFactSourcePath::Repository(_)
                ) {
                    return Err(RailError::message(
                        "source-written compiler fact item is outside the repository source root",
                    ));
                }
            }
            CompilerFactMacroProvenance::Expansion(call_site) => {
                if let Some(call_site) = call_site {
                    validate_span(*call_site, sources)?;
                }
            }
            CompilerFactMacroProvenance::Generated => {
                if !matches!(
                    sources[self.physical.span.source as usize].path,
                    CompilerFactSourcePath::Generated(_)
                ) {
                    return Err(RailError::message(
                        "generated compiler fact item does not name a generated source",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl CompilerFactCompletion {
    fn validate(
        &self,
        fragment: &CompilerFactObject,
        required_coverage: &BTreeSet<CompilerFactCoverage>,
    ) -> RailResult<()> {
        if !self.complete || self.coverage.is_empty() || !required_coverage.is_subset(&self.coverage) {
            return Err(RailError::message(
                "compiler fact fragment does not prove the required complete coverage",
            ));
        }
        let actual = [
            fragment.strings.len(),
            fragment.sources.len(),
            fragment.items.len(),
            fragment.edges.len(),
            fragment.entry_points.len(),
            fragment.retentions.len(),
        ];
        let declared = [
            self.strings,
            self.sources,
            self.items,
            self.edges,
            self.entry_points,
            self.retentions,
        ];
        if actual
            .into_iter()
            .zip(declared)
            .any(|(actual, declared)| u64::try_from(actual).ok() != Some(declared))
        {
            return Err(RailError::message(
                "compiler fact completion counts do not match the fragment",
            ));
        }
        if !fragment.items.is_empty()
            && ![
                CompilerFactCoverage::Definitions,
                CompilerFactCoverage::Visibility,
                CompilerFactCoverage::ExactSpans,
                CompilerFactCoverage::MacroProvenance,
            ]
            .into_iter()
            .all(|coverage| self.coverage.contains(&coverage))
        {
            return Err(RailError::message(
                "compiler fact definitions lack required structural coverage",
            ));
        }
        if !fragment.retentions.is_empty() && !self.coverage.contains(&CompilerFactCoverage::ConservativeRetention) {
            return Err(RailError::message(
                "compiler fact retention roots are outside the fragment's claimed coverage",
            ));
        }
        if !fragment.entry_points.is_empty() && !self.coverage.contains(&CompilerFactCoverage::EntryPoints) {
            return Err(RailError::message(
                "compiler fact entry points are outside the fragment's claimed coverage",
            ));
        }
        if fragment
            .sources
            .iter()
            .any(|source| matches!(source.path, CompilerFactSourcePath::Generated(_)))
            && !self.coverage.contains(&CompilerFactCoverage::GeneratedSources)
        {
            return Err(RailError::message(
                "generated compiler fact sources are outside the fragment's claimed coverage",
            ));
        }
        Ok(())
    }
}

fn validate_span(span: CompilerFactSpan, sources: &[CompilerFactSource]) -> RailResult<()> {
    let Some(source) = usize::try_from(span.source).ok().and_then(|source| sources.get(source)) else {
        return Err(RailError::message("compiler fact span names a missing source"));
    };
    let is_nonempty = span.start < span.end;
    let is_within_source = span.end <= source.bytes;
    if !is_nonempty || !is_within_source {
        return Err(RailError::message(
            "compiler fact span is empty or outside its exact source bytes",
        ));
    }
    Ok(())
}

fn validate_visibility(
    visibility: &CompilerFactVisibility,
    items: &BTreeMap<CompilerItemId, &CompilerItemFact>,
) -> RailResult<()> {
    let CompilerFactVisibility::Restricted(scope) = visibility else {
        return Ok(());
    };
    let Some(scope) = items.get(scope) else {
        return Err(RailError::message(
            "compiler fact restricted visibility names a missing scope",
        ));
    };
    if scope.physical.kind != CompilerFactItemKind::Module {
        return Err(RailError::message(
            "compiler fact restricted visibility scope is not a module",
        ));
    }
    Ok(())
}

fn validate_string_id(value: CompilerFactStringId, strings: &[String]) -> RailResult<()> {
    if usize::try_from(value.0).map_or(true, |index| index >= strings.len()) {
        return Err(RailError::message("compiler fact names a missing interned string"));
    }
    Ok(())
}

fn validate_repository_path(value: &str) -> RailResult<()> {
    if value.contains(['\\', '\0']) {
        return Err(RailError::message("compiler fact repository path is not canonical"));
    }
    let path = RepositoryPath::new(Path::new(value))?;
    if path.as_str() != value {
        return Err(RailError::message("compiler fact repository path is not canonical"));
    }
    Ok(())
}

fn validate_generated_path(value: &str) -> RailResult<()> {
    validate_text(value, "compiler fact generated path")?;
    if !is_protocol_absolute_path(value)
        || value
            .split(['/', '\\'])
            .any(|component| component == "." || component == "..")
    {
        return Err(RailError::message(
            "compiler fact generated path must be absolute and canonical",
        ));
    }
    Ok(())
}

fn is_protocol_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("\\\\")
        || bytes.get(1) == Some(&b':')
            && bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && matches!(bytes.get(2), Some(b'/' | b'\\'))
}

fn validate_identity(value: &str, prefix: &str, description: &str) -> RailResult<()> {
    let Some(digest) = value.strip_prefix(prefix) else {
        return Err(RailError::message(format!(
            "compiler fact {description} identity has an incompatible format"
        )));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RailError::message(format!(
            "compiler fact {description} identity has an invalid SHA-256 digest"
        )));
    }
    Ok(())
}

fn validate_sha256(value: &str, description: &str) -> RailResult<()> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(RailError::message(format!("{description} has an incompatible format")));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(RailError::message(format!("{description} has an invalid digest")));
    }
    Ok(())
}

fn validate_text(value: &str, description: &str) -> RailResult<()> {
    if value.is_empty()
        || value.len() > MAX_FACT_STRING_BYTES
        || value
            .chars()
            .any(|character| character == '\0' || character.is_control())
    {
        return Err(RailError::message(format!(
            "{description} is empty, oversized, or contains control bytes"
        )));
    }
    Ok(())
}

fn validate_strict_order<T: Ord>(values: &[T], description: &str) -> RailResult<()> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(RailError::message(format!(
            "{description} is not strictly ordered and unique"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(prefix: &str, byte: char) -> String {
        format!("{prefix}{}", byte.to_string().repeat(64))
    }

    fn run_authority() -> CompilerFactRunAuthority {
        CompilerFactRunAuthority {
            run_identity: identity(RUN_IDENTITY_PREFIX, '1'),
            view_identity: identity(VIEW_IDENTITY_PREFIX, '2'),
        }
    }

    fn producer_authority() -> CompilerFactProducerAuthority {
        CompilerFactProducerAuthority {
            compiler_identity: identity(COMPILER_IDENTITY_PREFIX, '3'),
            driver_identity: identity(DRIVER_IDENTITY_PREFIX, '4'),
        }
    }

    fn coverage() -> BTreeSet<CompilerFactCoverage> {
        BTreeSet::from([
            CompilerFactCoverage::Definitions,
            CompilerFactCoverage::Visibility,
            CompilerFactCoverage::ExactSpans,
            CompilerFactCoverage::MacroProvenance,
            CompilerFactCoverage::BodyEdges,
            CompilerFactCoverage::InterfaceEdges,
            CompilerFactCoverage::ReexportEdges,
            CompilerFactCoverage::PrivacyEdges,
            CompilerFactCoverage::TraitDispatch,
            CompilerFactCoverage::ForeignExports,
            CompilerFactCoverage::EntryPoints,
            CompilerFactCoverage::ConservativeRetention,
        ])
    }

    fn item_id(value: u64) -> CompilerItemId {
        CompilerItemId([0, value])
    }

    fn module() -> CompilerItemFact {
        CompilerItemFact {
            id: item_id(1),
            physical: CompilerFactPhysicalIdentity {
                span: CompilerFactSpan {
                    source: 0,
                    start: 0,
                    end: 7,
                },
                source_context: CompilerFactStringId(1),
                namespace: CompilerFactNamespace::Type,
                kind: CompilerFactItemKind::Module,
                ordinal: 0,
            },
            name: CompilerFactStringId(0),
            diagnostic_path: CompilerFactStringId(1),
            parent: None,
            written_visibility: CompilerFactVisibility::Public,
            visibility_span: Some(CompilerFactSpan {
                source: 0,
                start: 0,
                end: 3,
            }),
            effective_visibility: CompilerFactVisibility::Public,
            macro_provenance: CompilerFactMacroProvenance::Written,
        }
    }

    fn function() -> CompilerItemFact {
        CompilerItemFact {
            id: item_id(2),
            physical: CompilerFactPhysicalIdentity {
                span: CompilerFactSpan {
                    source: 0,
                    start: 8,
                    end: 22,
                },
                source_context: CompilerFactStringId(1),
                namespace: CompilerFactNamespace::Value,
                kind: CompilerFactItemKind::Function,
                ordinal: 0,
            },
            name: CompilerFactStringId(2),
            diagnostic_path: CompilerFactStringId(1),
            parent: Some(item_id(1)),
            written_visibility: CompilerFactVisibility::Restricted(item_id(1)),
            visibility_span: Some(CompilerFactSpan {
                source: 0,
                start: 8,
                end: 18,
            }),
            effective_visibility: CompilerFactVisibility::Crate,
            macro_provenance: CompilerFactMacroProvenance::Written,
        }
    }

    fn fragment() -> CompilerFactFragment {
        let coverage = coverage();
        let unit = CompilerFactUnit {
            identity: String::new(),
            invocation_identity: identity(INVOCATION_IDENTITY_PREFIX, '4'),
            package: CompilerFactPackage {
                name: "fixture".to_string(),
                version: "0.1.0".to_string(),
                source: None,
            },
            cargo_target: "fixture".to_string(),
            crate_name: "fixture".to_string(),
            target_kind: CompilerFactTargetKind::Library,
            domain: CompilerFactDomain::Production,
            role: CompilerFactRole::Target,
            platform: "aarch64-apple-darwin".to_string(),
            features: vec!["default".to_string()],
            cfg: vec!["target_arch=\"aarch64\"".to_string()],
        }
        .bind_identity()
        .expect("unit identity");
        CompilerFactFragment {
            version: COMPILER_FACT_PROTOCOL_VERSION,
            run_authority: run_authority(),
            object: CompilerFactObject {
                version: COMPILER_FACT_PROTOCOL_VERSION,
                producer_authority: producer_authority(),
                unit,
                strings: vec![
                    "crate".to_string(),
                    "fixture|fixture|src/lib.rs::crate".to_string(),
                    "run".to_string(),
                ],
                sources: vec![CompilerFactSource {
                    path: CompilerFactSourcePath::Repository("src/lib.rs".to_string()),
                    content_digest: format!("sha256:{}", "6".repeat(64)),
                    bytes: 22,
                }],
                items: vec![module(), function()],
                edges: vec![CompilerFactEdge {
                    source: item_id(2),
                    target: item_id(99),
                    kind: CompilerFactEdgeKind::Body,
                }],
                entry_points: vec![CompilerFactEntryPoint {
                    item: item_id(2),
                    kind: CompilerFactEntryPointKind::Main,
                }],
                retentions: vec![CompilerFactRetention {
                    item: item_id(2),
                    reason: CompilerFactRetentionReason::UnresolvedTraitDispatch,
                }],
                completion: CompilerFactCompletion {
                    complete: true,
                    coverage,
                    strings: 3,
                    sources: 1,
                    items: 2,
                    edges: 1,
                    entry_points: 1,
                    retentions: 1,
                },
            },
        }
    }

    fn expectation(fragment: &CompilerFactFragment) -> CompilerFactExpectation {
        CompilerFactExpectation::new(
            fragment.run_authority.clone(),
            fragment.object.producer_authority.clone(),
            fragment.object.unit.identity.clone(),
            coverage(),
        )
    }

    fn decode(fragment: &CompilerFactFragment) -> RailResult<ValidatedCompilerFactFragment> {
        let encoded = serde_json::to_vec(fragment).expect("encode fragment");
        ValidatedCompilerFactFragment::from_bytes(&encoded, &expectation(fragment))
    }

    fn decode_object(fragment: &CompilerFactFragment) -> RailResult<ValidatedCompilerFactObject> {
        let encoded = serde_json::to_vec(&fragment.object).expect("encode object");
        ValidatedCompilerFactObject::from_bytes(&encoded, &expectation(fragment).object)
    }

    fn announcement(fragment: &CompilerFactFragment) -> (CompilerFactAnnouncement, Vec<u8>) {
        let bytes = serde_json::to_vec(fragment).expect("encode fragment");
        (
            CompilerFactAnnouncement {
                version: COMPILER_FACT_PROTOCOL_VERSION,
                run_authority: fragment.run_authority.clone(),
                producer_authority: fragment.object.producer_authority.clone(),
                unit_identity: fragment.object.unit.identity.clone(),
                object_identity: fragment.object.identity_and_bytes().expect("object identity").0,
                content_digest: format!("sha256:{}", ContentDigest::sha256(&bytes)),
                bytes: bytes.len() as u64,
            },
            bytes,
        )
    }

    #[test]
    fn canonical_fragment_binds_authority_and_has_stable_identity() {
        let fragment = fragment();
        let first = decode(&fragment).expect("valid fragment");
        let second = decode(&fragment).expect("valid fragment");
        let object = decode_object(&fragment).expect("valid reusable object");
        assert_eq!(first.fragment, fragment);
        assert_eq!(object.object(), &fragment.object);
        assert_eq!(first.object_identity(), second.object_identity());
        assert!(first.object_identity().starts_with(FRAGMENT_OBJECT_IDENTITY_PREFIX));
        assert_eq!(first.object_identity(), object.identity());
    }

    #[test]
    fn reusable_object_identity_excludes_one_shot_run_authority() {
        let first_fragment = fragment();
        let first = decode(&first_fragment).expect("first valid fragment");
        let mut second_fragment = first_fragment.clone();
        second_fragment.run_authority.run_identity = identity(RUN_IDENTITY_PREFIX, 'a');
        let second = decode(&second_fragment).expect("second valid fragment");

        assert_ne!(
            ContentDigest::sha256(&serde_json::to_vec(&first_fragment).expect("first fragment")),
            ContentDigest::sha256(&serde_json::to_vec(&second_fragment).expect("second fragment"))
        );
        assert_eq!(first.object_identity(), second.object_identity());
    }

    #[test]
    fn fragment_rejects_wrong_authority_unit_version_and_coverage() {
        let fragment = fragment();
        let encoded = serde_json::to_vec(&fragment).expect("encode fragment");
        let mut wrong_authority = expectation(&fragment);
        wrong_authority.run_authority.run_identity = identity(RUN_IDENTITY_PREFIX, 'a');
        assert!(ValidatedCompilerFactFragment::from_bytes(&encoded, &wrong_authority).is_err());

        let mut wrong_unit = expectation(&fragment);
        wrong_unit.object.unit_identity = identity(UNIT_IDENTITY_PREFIX, 'b');
        assert!(ValidatedCompilerFactFragment::from_bytes(&encoded, &wrong_unit).is_err());

        let mut wrong_producer = expectation(&fragment);
        wrong_producer.object.producer_authority.driver_identity = identity(DRIVER_IDENTITY_PREFIX, 'c');
        assert!(ValidatedCompilerFactFragment::from_bytes(&encoded, &wrong_producer).is_err());

        let mut wrong_version = fragment.clone();
        wrong_version.version += 1;
        assert!(decode(&wrong_version).is_err());

        let mut incomplete = fragment;
        incomplete
            .object
            .completion
            .coverage
            .remove(&CompilerFactCoverage::TraitDispatch);
        assert!(decode(&incomplete).is_err());
    }

    #[test]
    fn fragment_rejects_noncanonical_or_duplicate_tables() {
        let fragment = fragment();
        let mut encoded = serde_json::to_vec(&fragment).expect("encode fragment");
        encoded.push(b'\n');
        assert!(ValidatedCompilerFactFragment::from_bytes(&encoded, &expectation(&fragment)).is_err());

        let mut strings = fragment.clone();
        strings.object.strings.swap(0, 1);
        assert!(decode(&strings).is_err());

        let mut items = fragment.clone();
        items.object.items.swap(0, 1);
        assert!(decode(&items).is_err());

        let mut duplicate_source = fragment;
        let source = duplicate_source.object.sources[0].clone();
        duplicate_source.object.sources.push(source);
        duplicate_source.object.completion.sources += 1;
        assert!(decode(&duplicate_source).is_err());
    }

    #[test]
    fn fragment_rejects_unbounded_or_unknown_json() {
        let oversized = vec![b' '; MAX_COMPILER_FACT_FRAGMENT_BYTES + 1];
        assert!(ValidatedCompilerFactFragment::from_bytes(&oversized, &expectation(&fragment())).is_err());
        ValidatedCompilerFactObject::from_bytes(&oversized, &expectation(&fragment()).object).unwrap_err();

        let encoded = serde_json::to_string(&fragment()).expect("encode fragment");
        let unknown = encoded.replacen('{', "{\"unknown\":true,", 1);
        assert!(ValidatedCompilerFactFragment::from_bytes(unknown.as_bytes(), &expectation(&fragment())).is_err());

        let fragment = fragment();
        let mut object = serde_json::to_vec(&fragment.object).expect("encode object");
        object.push(b'\n');
        ValidatedCompilerFactObject::from_bytes(&object, &expectation(&fragment).object).unwrap_err();
    }

    #[test]
    fn fragment_rejects_unsafe_paths_and_invalid_source_provenance() {
        let mut traversal = fragment();
        traversal.object.sources[0].path = CompilerFactSourcePath::Repository("../src/lib.rs".to_string());
        assert!(decode(&traversal).is_err());

        let mut generated = fragment();
        generated.object.sources[0].path = CompilerFactSourcePath::Generated("relative/generated.rs".to_string());
        generated.object.items[0].macro_provenance = CompilerFactMacroProvenance::Generated;
        generated.object.items[1].macro_provenance = CompilerFactMacroProvenance::Generated;
        generated
            .object
            .completion
            .coverage
            .insert(CompilerFactCoverage::GeneratedSources);
        assert!(decode(&generated).is_err());

        let mut mismatched = fragment();
        mismatched.object.items[0].macro_provenance = CompilerFactMacroProvenance::Generated;
        assert!(decode(&mismatched).is_err());
    }

    #[test]
    fn fragment_rejects_dangling_structure_and_false_completion_counts() {
        let mut parent = fragment();
        parent.object.items[1].parent = Some(item_id(404));
        assert!(decode(&parent).is_err());

        let mut scope = fragment();
        scope.object.items[1].written_visibility = CompilerFactVisibility::Restricted(item_id(2));
        assert!(decode(&scope).is_err());

        let mut string = fragment();
        string.object.items[1].diagnostic_path = CompilerFactStringId(99);
        assert!(decode(&string).is_err());

        let mut span = fragment();
        span.object.items[1].physical.span.end = span.object.items[1].physical.span.start;
        assert!(decode(&span).is_err());

        let mut outside_source = fragment();
        outside_source.object.items[1].physical.span.end = outside_source.object.sources[0].bytes + 1;
        assert!(decode(&outside_source).is_err());

        let mut counts = fragment();
        counts.object.completion.items += 1;
        assert!(decode(&counts).is_err());
    }

    #[test]
    fn fragment_allows_cross_fragment_edge_targets() {
        let fragment = fragment();
        assert!(!fragment.object.items.iter().any(|item| item.id == item_id(99)));
        decode(&fragment).unwrap();
    }

    #[test]
    fn announcement_reserves_one_canonical_authenticated_compiler_message() {
        let fragment = fragment();
        let (announcement, _) = announcement(&fragment);
        let expected = CompilerFactAnnouncementExpectation::from_fragment(&fragment);
        let message = ValidatedCompilerFactAnnouncement::encode_message(&announcement);
        let validated = ValidatedCompilerFactAnnouncement::from_compiler_message(
            Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
            &message,
            &expected,
        )
        .expect("valid announcement")
        .expect("reserved message");
        assert_eq!(validated.announcement.unit_identity, fragment.object.unit.identity);
        assert_eq!(validated.object_identity(), announcement.object_identity);
        assert_eq!(validated.content_digest(), announcement.content_digest);
        assert_eq!(validated.bytes(), announcement.bytes);

        assert!(
            ValidatedCompilerFactAnnouncement::from_compiler_message(Some("ordinary_diagnostic"), &message, &expected)
                .expect("unrelated diagnostic")
                .is_none()
        );
    }

    #[test]
    fn announcement_rejects_malformed_noncanonical_or_wrong_authority() {
        let fragment = fragment();
        let (announcement, _) = announcement(&fragment);
        let expected = CompilerFactAnnouncementExpectation::from_fragment(&fragment);
        let message = ValidatedCompilerFactAnnouncement::encode_message(&announcement);

        assert!(
            ValidatedCompilerFactAnnouncement::from_compiler_message(
                Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
                "wrong-envelope",
                &expected,
            )
            .is_err()
        );
        let noncanonical = format!("{message} ");
        assert!(
            ValidatedCompilerFactAnnouncement::from_compiler_message(
                Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
                &noncanonical,
                &expected,
            )
            .is_err()
        );
        let oversized = format!(
            "{COMPILER_FACT_ANNOUNCEMENT_PREFIX}{}",
            "x".repeat(MAX_COMPILER_FACT_ANNOUNCEMENT_BYTES + 1)
        );
        assert!(
            ValidatedCompilerFactAnnouncement::from_compiler_message(
                Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
                &oversized,
                &expected,
            )
            .is_err()
        );

        let mut wrong = expected;
        wrong.unit_identity = identity(UNIT_IDENTITY_PREFIX, 'f');
        assert!(
            ValidatedCompilerFactAnnouncement::from_compiler_message(
                Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
                &message,
                &wrong,
            )
            .is_err()
        );
    }

    #[test]
    fn announced_sidecar_is_selected_by_digest_and_revalidated_completely() {
        let directory = tempfile::tempdir().expect("fact sidecar directory");
        let fragment = fragment();
        let expected_fragment = expectation(&fragment);
        let expected_announcement = CompilerFactAnnouncementExpectation::from_fragment(&fragment);
        let (announcement, bytes) = announcement(&fragment);
        let message = ValidatedCompilerFactAnnouncement::encode_message(&announcement);
        let validated = ValidatedCompilerFactAnnouncement::from_compiler_message(
            Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
            &message,
            &expected_announcement,
        )
        .expect("valid announcement")
        .expect("reserved message");
        let path = validated.sidecar_path(directory.path()).expect("derived sidecar path");
        fs::write(&path, &bytes).expect("write announced sidecar");
        fs::write(directory.path().join("forged-unannounced.json"), b"forged").expect("write unannounced sidecar");

        let loaded = load_announced_fragment(directory.path(), &validated, &expected_fragment).expect("valid sidecar");
        assert_eq!(loaded.fragment, fragment);
        assert_eq!(loaded.object_identity(), announcement.object_identity);

        let mut tampered = bytes;
        let last = tampered.last_mut().expect("nonempty fragment");
        *last = if *last == b'}' { b']' } else { b'}' };
        fs::write(&path, tampered).expect("tamper sidecar");
        assert!(load_announced_fragment(directory.path(), &validated, &expected_fragment).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn announced_sidecar_rejects_symlinks_and_multiple_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("fact sidecar directory");
        let fragment = fragment();
        let expected_fragment = expectation(&fragment);
        let expected_announcement = CompilerFactAnnouncementExpectation::from_fragment(&fragment);
        let (announcement, bytes) = announcement(&fragment);
        let message = ValidatedCompilerFactAnnouncement::encode_message(&announcement);
        let validated = ValidatedCompilerFactAnnouncement::from_compiler_message(
            Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
            &message,
            &expected_announcement,
        )
        .expect("valid announcement")
        .expect("reserved message");
        let path = validated.sidecar_path(directory.path()).expect("derived sidecar path");
        let target = directory.path().join("target.json");
        fs::write(&target, &bytes).expect("write target");
        symlink(&target, &path).expect("create sidecar symlink");
        assert!(load_announced_fragment(directory.path(), &validated, &expected_fragment).is_err());
        fs::remove_file(&path).expect("remove symlink");

        fs::hard_link(&target, &path).expect("create sidecar hard link");
        assert!(load_announced_fragment(directory.path(), &validated, &expected_fragment).is_err());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the separately manufactured exact-toolchain companion"]
    fn manufactured_driver_fragment_passes_the_stable_admission_boundary() {
        use std::os::unix::fs::symlink;
        use std::process::Command;

        let driver = std::env::var_os("CARGO_RAIL_TEST_FACT_DRIVER")
            .map(std::path::PathBuf::from)
            .expect("CARGO_RAIL_TEST_FACT_DRIVER");
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("repository root");
        let temporary = tempfile::tempdir().expect("temporary driver tree");
        let bin = temporary.path().join("bin");
        let output = temporary.path().join("facts");
        fs::create_dir_all(&bin).expect("driver bin directory");
        fs::create_dir_all(&output).expect("fact directory");
        let rustc_output = Command::new("rustup")
            .args(["which", "rustc"])
            .output()
            .expect("locate rustc");
        assert!(rustc_output.status.success());
        let rustc = std::path::PathBuf::from(String::from_utf8(rustc_output.stdout).expect("UTF-8 rustc path").trim());
        let sysroot_output = Command::new(&rustc)
            .args(["--print", "sysroot"])
            .output()
            .expect("rustc sysroot");
        assert!(sysroot_output.status.success());
        let sysroot = std::path::PathBuf::from(String::from_utf8(sysroot_output.stdout).expect("UTF-8 sysroot").trim());
        symlink(sysroot.join("lib"), temporary.path().join("lib")).expect("toolchain library link");
        let staged = bin.join("cargo-rail-fact-driver");
        fs::copy(driver, &staged).expect("stage exact driver");

        let run = run_authority();
        let producer = producer_authority();
        let mut required_coverage = coverage();
        required_coverage.insert(CompilerFactCoverage::GeneratedSources);
        let unit = CompilerFactUnit {
            identity: String::new(),
            invocation_identity: identity(INVOCATION_IDENTITY_PREFIX, '5'),
            package: CompilerFactPackage {
                name: "fact-probe".to_string(),
                version: "0.0.0".to_string(),
                source: None,
            },
            cargo_target: "fact-probe".to_string(),
            crate_name: "fact_probe".to_string(),
            target_kind: CompilerFactTargetKind::Library,
            domain: CompilerFactDomain::Production,
            role: CompilerFactRole::Target,
            platform: std::env::consts::ARCH.to_string(),
            features: Vec::new(),
            cfg: Vec::new(),
        }
        .bind_identity()
        .expect("unit identity");
        let unit_identity = unit.identity.clone();
        let invocation = CompilerFactInvocation {
            version: COMPILER_FACT_PROTOCOL_VERSION,
            observation_directory: output.to_string_lossy().into_owned(),
            source_root: root.to_string_lossy().into_owned(),
            generated_roots: vec![temporary.path().join("cargo-target").to_string_lossy().into_owned()],
            run_authority: run.clone(),
            producer_authority: producer.clone(),
            unit,
            required_coverage: required_coverage.clone(),
        };
        let capability = temporary.path().join("invocation.json");
        fs::write(&capability, serde_json::to_vec(&invocation).expect("encode invocation")).expect("write invocation");
        let cargo = Command::new("cargo")
            .current_dir(root.join("tools/compiler-fact-driver/tests/fixtures/workspace"))
            .args(["check", "--locked", "--message-format=json"])
            .env("CARGO_TARGET_DIR", temporary.path().join("cargo-target"))
            .env("RUSTC_WORKSPACE_WRAPPER", &staged)
            .env(COMPILER_FACT_INVOCATION_ENV, &capability)
            .output()
            .expect("run Cargo through exact driver");
        assert!(cargo.status.success(), "{}", String::from_utf8_lossy(&cargo.stderr));
        let message = String::from_utf8(cargo.stdout)
            .expect("UTF-8 Cargo messages")
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|event| {
                event["reason"] == "compiler-message"
                    && event["message"]["code"]["code"] == COMPILER_FACT_ANNOUNCEMENT_CODE
            })
            .expect("Cargo fact announcement");
        let expectation = CompilerFactAnnouncementExpectation {
            run_authority: run.clone(),
            producer_authority: producer.clone(),
            unit_identity: unit_identity.clone(),
        };
        let announcement = ValidatedCompilerFactAnnouncement::from_compiler_message(
            Some(COMPILER_FACT_ANNOUNCEMENT_CODE),
            message["message"]["message"].as_str().expect("announcement message"),
            &expectation,
        )
        .expect("valid announcement")
        .expect("reserved announcement");
        let expected_fragment = CompilerFactExpectation::new(run, producer, unit_identity, required_coverage);
        let fragment =
            load_announced_fragment(&output, &announcement, &expected_fragment).expect("admitted driver fragment");
        assert!(!fragment.fragment.object.items.is_empty());
        assert!(
            fragment
                .fragment
                .object
                .edges
                .iter()
                .any(|edge| edge.kind == CompilerFactEdgeKind::Body)
        );
    }
}
