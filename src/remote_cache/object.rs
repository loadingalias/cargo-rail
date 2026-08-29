//! Provider-neutral native compiler-result object protocol.

use std::fs::File;
#[cfg(test)]
use std::io::Read as _;
use std::io::{BufReader, Seek as _, Write as _};

use serde::{Deserialize, Serialize};

use super::{
    RemoteCacheMode, RemoteCacheSelection, RemoteProtocolMarkerState, RemoteStoreError, RemoteStoreResult, azure, s3,
};
use crate::compiler::native_cache::pack::NativeAssociation;

pub(super) const OBJECT_NAMESPACE: &str = "native-v5";
pub(super) const PROTOCOL_MARKER: &[u8] = b"cargo-rail-native-cache-v5\n";
pub(super) const ENTRY_MAGIC: &[u8; 8] = b"CRNENTR1";
pub(super) const ENTRY_VERSION: u16 = 1;
pub(super) const ENTRY_PRELUDE_BYTES: u64 = 8 + 2 + 4;
pub(super) const ENTRY_PRELUDE_LEN: usize = 8 + 2 + 4;
// Remote entries optimize for the many consumers of one compiler miss. On
// representative native-result packs, level 9 is the libzstd strategy knee:
// materially smaller and faster to decode than levels 1-8 without the sharply
// rising encode cost of levels 10+.
const COMPRESSION_LEVEL: i32 = 9;
pub(super) const MAX_METADATA_BYTES: u64 = 1024 * 1024;
pub(super) const MAX_ENTRY_BYTES: u64 = crate::compiler::native_cache::pack::MAX_PACK_BYTES + MAX_METADATA_BYTES;
pub(super) const MAX_CONDITIONAL_ATTEMPTS: usize = 16;
pub(super) const MAX_ETAG_BYTES: usize = 128;
pub(super) const STREAM_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TransferMetrics {
    pub(crate) request_attempts: u64,
    pub(crate) payload_bytes_read: u64,
    pub(crate) payload_bytes_written: u64,
    pub(crate) service_elapsed_ns: u64,
}

pub(crate) enum Lookup {
    Miss,
    Conflict,
    Unique {
        environment_names: Vec<String>,
        action_key: String,
        result_key: String,
        body: EntryBody,
        bytes: u64,
        compressed_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Publication {
    Unique,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EntryIdentity {
    pub(super) environment_names: Vec<String>,
    pub(super) action_key: String,
    pub(super) result_key: String,
    pub(super) pack_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EntryRecord {
    pub(super) version: u32,
    pub(super) base_action_key: String,
    pub(super) state: EntryState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum EntryState {
    Unique {
        identity: EntryIdentity,
        payload_length: u64,
    },
    Conflict {
        first: EntryIdentity,
        second: EntryIdentity,
    },
}

pub(super) struct StoredEntry {
    pub(super) record: EntryRecord,
    pub(super) body: Option<EntryBody>,
    pub(super) etag: String,
}

pub(crate) struct EntryBody {
    state: EntryBodyState,
    remaining: u64,
    compressed_bytes: u64,
}

enum EntryBodyState {
    Source(CompressedSource),
    Decoder(Box<zstd::stream::read::Decoder<'static, BufReader<CompressedSource>>>),
    Done,
}

struct CompressedSource {
    source: Box<dyn std::io::Read + Send>,
    remaining: u64,
}

impl std::io::Read for CompressedSource {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() || self.remaining == 0 {
            return Ok(0);
        }
        let maximum = usize::try_from(self.remaining.min(output.len() as u64)).unwrap_or(output.len());
        let read = self.source.read(&mut output[..maximum])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "remote entry stream ended before its declared length",
            ));
        }
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

impl std::io::Read for EntryBody {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if matches!(self.state, EntryBodyState::Source(_)) {
            let EntryBodyState::Source(source) = std::mem::replace(&mut self.state, EntryBodyState::Done) else {
                return Err(std::io::Error::other("remote entry source disappeared"));
            };
            self.state = EntryBodyState::Decoder(Box::new(
                zstd::stream::read::Decoder::with_buffer(BufReader::with_capacity(STREAM_BUFFER_BYTES, source))
                    .map_err(|_| std::io::Error::other("remote entry compressed payload is malformed"))?,
            ));
        }
        let EntryBodyState::Decoder(decoder) = &mut self.state else {
            return Ok(0);
        };
        if self.remaining == 0 {
            let mut trailing = [0_u8; 1];
            if decoder.read(&mut trailing)? != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "remote entry decompressed beyond its pack bound",
                ));
            }
            let EntryBodyState::Decoder(decoder) = std::mem::replace(&mut self.state, EntryBodyState::Done) else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "remote entry decoder disappeared",
                ));
            };
            let source = decoder.finish();
            if !source.buffer().is_empty() || source.get_ref().remaining != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "remote entry compressed payload ended before its declared length",
                ));
            }
            return Ok(0);
        }
        let maximum = usize::try_from(self.remaining.min(output.len() as u64)).unwrap_or(output.len());
        let read = decoder.read(&mut output[..maximum])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "remote entry decompressed to less than its pack length",
            ));
        }
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

impl EntryBody {
    pub(super) fn new(source: Box<dyn std::io::Read + Send>, compressed_bytes: u64, pack_length: u64) -> Self {
        Self {
            state: EntryBodyState::Source(CompressedSource {
                source,
                remaining: compressed_bytes,
            }),
            remaining: pack_length,
            compressed_bytes,
        }
    }

    pub(crate) const fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }

    pub(crate) fn copy_compressed_to<W: std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<u64> {
        let EntryBodyState::Source(mut source) = std::mem::replace(&mut self.state, EntryBodyState::Done) else {
            return Err(std::io::Error::other(
                "remote entry compressed source was already consumed",
            ));
        };
        let copied = std::io::copy(&mut source, writer)?;
        if copied != self.compressed_bytes || source.remaining != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "remote entry compressed payload has an inconsistent length",
            ));
        }
        Ok(copied)
    }
}

pub(super) enum PutCondition {
    Absent,
    Match(String),
}

pub(super) enum PutOutcome {
    Written,
    PreconditionFailed,
}

enum Backend {
    Azure(Box<azure::AzureBackend>),
    S3(s3::S3Backend),
}

impl Backend {
    fn connect(selection: &RemoteCacheSelection) -> RemoteStoreResult<Self> {
        if selection.authority.is_azure_blob() {
            azure::connect(selection).map(Box::new).map(Self::Azure)
        } else if selection.authority.supports_s3_transport() {
            s3::connect(selection).map(Self::S3)
        } else {
            Err(RemoteStoreError::configuration(
                "selected remote provider is not qualified for direct transport",
            ))
        }
    }

    fn metrics(&self) -> TransferMetrics {
        match self {
            Self::Azure(store) => store.metrics(),
            Self::S3(store) => store.metrics(),
        }
    }

    fn take_metrics(&self) -> TransferMetrics {
        match self {
            Self::Azure(store) => store.take_metrics(),
            Self::S3(store) => store.take_metrics(),
        }
    }

    fn get_marker(&self, key: &str) -> RemoteStoreResult<Option<Vec<u8>>> {
        match self {
            Self::Azure(store) => store.get_marker(key),
            Self::S3(store) => store.get_marker(key),
        }
    }

    fn get_entry(&self, key: &str, base_action_key: &str) -> RemoteStoreResult<Option<StoredEntry>> {
        match self {
            Self::Azure(store) => store.get_entry(key, base_action_key),
            Self::S3(store) => store.get_entry(key, base_action_key),
        }
    }

    fn put_bytes(&self, key: &str, body: &[u8], condition: PutCondition) -> RemoteStoreResult<PutOutcome> {
        match self {
            Self::Azure(store) => store.put_bytes(key, body, condition),
            Self::S3(store) => store.put_bytes(key, body, condition),
        }
    }

    fn put_file(&self, key: &str, body: File, bytes: u64, condition: PutCondition) -> RemoteStoreResult<PutOutcome> {
        match self {
            Self::Azure(store) => store.put_file(key, body, bytes, condition),
            Self::S3(store) => store.put_file(key, body, bytes, condition),
        }
    }
}

/// One provider transport bound to the single object protocol.
pub(super) struct ObjectStore {
    backend: Backend,
    prefix: String,
    mode: RemoteCacheMode,
}

pub(super) fn connect(selection: &RemoteCacheSelection) -> RemoteStoreResult<ObjectStore> {
    connect_with_marker(selection).map(|(store, _)| store)
}

pub(super) fn probe(selection: &RemoteCacheSelection) -> RemoteStoreResult<RemoteProtocolMarkerState> {
    connect_with_marker(selection).map(|(_, marker)| marker)
}

fn connect_with_marker(
    selection: &RemoteCacheSelection,
) -> RemoteStoreResult<(ObjectStore, RemoteProtocolMarkerState)> {
    let store = ObjectStore {
        backend: Backend::connect(selection)?,
        prefix: selection.authority.prefix().to_string(),
        mode: selection.mode(),
    };
    let marker = store.ensure_protocol_marker()?;
    Ok((store, marker))
}

impl ObjectStore {
    pub(super) fn metrics(&self) -> TransferMetrics {
        self.backend.metrics()
    }

    pub(super) fn take_metrics(&self) -> TransferMetrics {
        self.backend.take_metrics()
    }

    fn can_write(&self) -> bool {
        self.mode == RemoteCacheMode::ReadWrite
    }

    fn marker_key(&self) -> String {
        self.object_suffix("protocol")
    }

    fn entry_key(&self, identity: &str) -> RemoteStoreResult<String> {
        crate::compiler::native_cache::validate_base_action_key(identity)
            .map_err(|_| RemoteStoreError::integrity("remote cache entry identity is invalid"))?;
        let digest = identity
            .rsplit_once('-')
            .map(|(_, digest)| digest)
            .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| RemoteStoreError::integrity("remote cache object identity has no digest"))?;
        let shard = digest
            .get(..2)
            .ok_or_else(|| RemoteStoreError::integrity("remote cache object identity digest is too short"))?;
        Ok(self.object_suffix(&format!("entries/{shard}/{identity}")))
    }

    fn object_suffix(&self, suffix: &str) -> String {
        if self.prefix.is_empty() {
            format!("{OBJECT_NAMESPACE}/{suffix}")
        } else {
            format!("{}/{OBJECT_NAMESPACE}/{suffix}", self.prefix)
        }
    }

    fn ensure_protocol_marker(&self) -> RemoteStoreResult<RemoteProtocolMarkerState> {
        let key = self.marker_key();
        match self.backend.get_marker(&key)? {
            Some(bytes) if bytes == PROTOCOL_MARKER => return Ok(RemoteProtocolMarkerState::Existing),
            Some(_) => {
                return Err(RemoteStoreError::integrity(
                    "remote cache protocol marker is incompatible",
                ));
            }
            None if !self.can_write() => {
                return Err(RemoteStoreError::configuration(
                    "remote cache protocol marker is unavailable",
                ));
            }
            None => {}
        }
        let _ = self.backend.put_bytes(&key, PROTOCOL_MARKER, PutCondition::Absent)?;
        match self.backend.get_marker(&key)? {
            Some(bytes) if bytes == PROTOCOL_MARKER => Ok(RemoteProtocolMarkerState::Initialized),
            _ => Err(RemoteStoreError::integrity(
                "remote cache protocol marker did not converge",
            )),
        }
    }

    pub(super) fn lookup(&self, base_action_key: &str) -> RemoteStoreResult<Lookup> {
        let Some(entry) = self
            .backend
            .get_entry(&self.entry_key(base_action_key)?, base_action_key)?
        else {
            return Ok(Lookup::Miss);
        };
        match entry.record.state {
            EntryState::Conflict { .. } => Ok(Lookup::Conflict),
            EntryState::Unique { identity, .. } => {
                let body = entry
                    .body
                    .ok_or_else(|| RemoteStoreError::integrity("remote unique entry has no payload"))?;
                let compressed_bytes = body.compressed_bytes();
                Ok(Lookup::Unique {
                    environment_names: identity.environment_names,
                    action_key: identity.action_key,
                    result_key: identity.result_key,
                    body,
                    bytes: identity.pack_length,
                    compressed_bytes,
                })
            }
        }
    }

    pub(super) fn publish(
        &self,
        association: &NativeAssociation,
        base_action_key: &str,
        environment_names: &[String],
        pack: File,
    ) -> RemoteStoreResult<Publication> {
        if !self.can_write() {
            return Err(RemoteStoreError::configuration(
                "selected remote cache mode does not permit publication",
            ));
        }
        super::validate_environment_names(environment_names)?;
        let requested = EntryIdentity {
            environment_names: environment_names.to_vec(),
            action_key: association.action_key().to_string(),
            result_key: association.result_key().to_string(),
            pack_length: association.pack_length(),
        };
        validate_entry_identity(&requested)?;
        let (mut unique_body, unique_bytes) = encode_unique_entry(base_action_key, &requested, pack)?;
        let key = self.entry_key(base_action_key)?;
        for _ in 0..MAX_CONDITIONAL_ATTEMPTS {
            let Some(existing) = self.backend.get_entry(&key, base_action_key)? else {
                unique_body.rewind().map_err(io_unavailable)?;
                if matches!(
                    self.backend.put_file(
                        &key,
                        unique_body.try_clone().map_err(io_unavailable)?,
                        unique_bytes,
                        PutCondition::Absent,
                    )?,
                    PutOutcome::Written
                ) {
                    return Ok(Publication::Unique);
                }
                continue;
            };
            match existing.record.state {
                EntryState::Conflict { .. } => return Ok(Publication::Conflict),
                EntryState::Unique { identity, .. } if identity == requested => {
                    let mut body = existing
                        .body
                        .ok_or_else(|| RemoteStoreError::integrity("remote unique entry has no payload"))?;
                    std::io::copy(&mut body, &mut std::io::sink())
                        .map_err(|_| RemoteStoreError::integrity("remote unique entry payload is malformed"))?;
                    return Ok(Publication::Unique);
                }
                EntryState::Unique { identity, .. } => {
                    let (first, second) = canonical_entry_pair(identity, requested.clone())?;
                    let conflict = EntryRecord {
                        version: 1,
                        base_action_key: base_action_key.to_string(),
                        state: EntryState::Conflict { first, second },
                    };
                    let (body, bytes) = encode_entry(&conflict, None)?;
                    if matches!(
                        self.backend
                            .put_file(&key, body, bytes, PutCondition::Match(existing.etag))?,
                        PutOutcome::Written
                    ) {
                        return Ok(Publication::Conflict);
                    }
                }
            }
        }
        Err(RemoteStoreError::unavailable(
            "remote entry publication remained contended",
        ))
    }
}

fn canonical_entry_pair(
    first: EntryIdentity,
    second: EntryIdentity,
) -> RemoteStoreResult<(EntryIdentity, EntryIdentity)> {
    validate_entry_identity(&first)?;
    validate_entry_identity(&second)?;
    if first == second {
        return Err(RemoteStoreError::integrity("remote conflict repeats one entry"));
    }
    Ok(if first < second {
        (first, second)
    } else {
        (second, first)
    })
}

fn validate_entry_identity(identity: &EntryIdentity) -> RemoteStoreResult<()> {
    super::validate_environment_names(&identity.environment_names)?;
    crate::compiler::native_cache::validate_action_key(&identity.action_key)
        .map_err(|_| RemoteStoreError::integrity("remote entry action identity is invalid"))?;
    crate::compiler::native_cache::validate_result_key(&identity.result_key)
        .map_err(|_| RemoteStoreError::integrity("remote entry result identity is invalid"))?;
    if identity.pack_length == 0 || identity.pack_length > crate::compiler::native_cache::pack::MAX_PACK_BYTES {
        return Err(RemoteStoreError::integrity("remote entry pack length is invalid"));
    }
    Ok(())
}

fn validate_entry_record(record: &EntryRecord, base_action_key: &str) -> RemoteStoreResult<()> {
    if record.version != 1 || record.base_action_key != base_action_key {
        return Err(RemoteStoreError::integrity(
            "remote entry does not match its object key",
        ));
    }
    crate::compiler::native_cache::validate_base_action_key(base_action_key)
        .map_err(|_| RemoteStoreError::integrity("remote entry base action identity is invalid"))?;
    match &record.state {
        EntryState::Unique {
            identity,
            payload_length,
        } => {
            validate_entry_identity(identity)?;
            if *payload_length == 0 || *payload_length > MAX_ENTRY_BYTES {
                return Err(RemoteStoreError::integrity("remote entry payload length is invalid"));
            }
        }
        EntryState::Conflict { first, second } => {
            let canonical = canonical_entry_pair(first.clone(), second.clone())?;
            if canonical != (first.clone(), second.clone()) {
                return Err(RemoteStoreError::integrity("remote entry conflict is not canonical"));
            }
        }
    }
    Ok(())
}

fn encode_unique_entry(
    base_action_key: &str,
    identity: &EntryIdentity,
    mut pack: File,
) -> RemoteStoreResult<(File, u64)> {
    let metadata = pack.metadata().map_err(io_unavailable)?;
    if !metadata.is_file() || metadata.len() != identity.pack_length {
        return Err(RemoteStoreError::integrity(
            "remote publication pack does not match its verified association",
        ));
    }
    pack.rewind().map_err(io_unavailable)?;
    let mut compressed = tempfile::tempfile().map_err(io_unavailable)?;
    {
        let mut encoder =
            zstd::stream::write::Encoder::new(&mut compressed, COMPRESSION_LEVEL).map_err(io_unavailable)?;
        let copied = std::io::copy(&mut pack, &mut encoder).map_err(io_unavailable)?;
        if copied != identity.pack_length {
            return Err(RemoteStoreError::integrity(
                "remote publication pack changed while compressed",
            ));
        }
        encoder.finish().map_err(io_unavailable)?;
    }
    compressed.flush().map_err(io_unavailable)?;
    let payload_length = compressed.metadata().map_err(io_unavailable)?.len();
    compressed.rewind().map_err(io_unavailable)?;
    let record = EntryRecord {
        version: 1,
        base_action_key: base_action_key.to_string(),
        state: EntryState::Unique {
            identity: identity.clone(),
            payload_length,
        },
    };
    encode_entry(&record, Some(&mut compressed))
}

fn encode_entry(record: &EntryRecord, mut payload: Option<&mut File>) -> RemoteStoreResult<(File, u64)> {
    validate_entry_record(record, &record.base_action_key)?;
    let header = encode_canonical(record, "entry")?;
    let header_length = u32::try_from(header.len())
        .map_err(|_| RemoteStoreError::integrity("remote entry header length is invalid"))?;
    let expected_payload = match &record.state {
        EntryState::Unique { payload_length, .. } => *payload_length,
        EntryState::Conflict { .. } => 0,
    };
    let mut body = tempfile::tempfile().map_err(io_unavailable)?;
    body.write_all(ENTRY_MAGIC).map_err(io_unavailable)?;
    body.write_all(&ENTRY_VERSION.to_le_bytes()).map_err(io_unavailable)?;
    body.write_all(&header_length.to_le_bytes()).map_err(io_unavailable)?;
    body.write_all(&header).map_err(io_unavailable)?;
    let copied = match payload.as_mut() {
        Some(payload) => std::io::copy(payload, &mut body).map_err(io_unavailable)?,
        None => 0,
    };
    if copied != expected_payload {
        return Err(RemoteStoreError::integrity(
            "remote entry payload changed while encoded",
        ));
    }
    body.flush().map_err(io_unavailable)?;
    let bytes = body.metadata().map_err(io_unavailable)?.len();
    if bytes > MAX_ENTRY_BYTES {
        return Err(RemoteStoreError::integrity("remote entry exceeds its byte bound"));
    }
    body.rewind().map_err(io_unavailable)?;
    Ok((body, bytes))
}

fn encode_canonical<T: Serialize>(value: &T, label: &str) -> RemoteStoreResult<Vec<u8>> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| RemoteStoreError::integrity(format!("remote {label} encoding failed")))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(RemoteStoreError::integrity(format!(
            "remote {label} exceeds its byte bound"
        )));
    }
    Ok(bytes)
}

fn decode_canonical<T>(bytes: &[u8], label: &str) -> RemoteStoreResult<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let value = serde_json::from_slice::<T>(bytes)
        .map_err(|_| RemoteStoreError::integrity(format!("remote {label} is malformed")))?;
    if encode_canonical(&value, label)? != bytes {
        return Err(RemoteStoreError::integrity(format!(
            "remote {label} is not canonically encoded"
        )));
    }
    Ok(value)
}

pub(super) fn decode_entry_prelude(prelude: &[u8; ENTRY_PRELUDE_LEN]) -> RemoteStoreResult<usize> {
    if &prelude[..8] != ENTRY_MAGIC || u16::from_le_bytes([prelude[8], prelude[9]]) != ENTRY_VERSION {
        return Err(RemoteStoreError::integrity("remote entry prelude is incompatible"));
    }
    let header_length = u32::from_le_bytes(
        prelude[10..14]
            .try_into()
            .map_err(|_| RemoteStoreError::integrity("remote entry header length is malformed"))?,
    );
    if header_length == 0 || u64::from(header_length) > MAX_METADATA_BYTES {
        return Err(RemoteStoreError::integrity(
            "remote entry header exceeds its byte bound",
        ));
    }
    usize::try_from(header_length)
        .map_err(|_| RemoteStoreError::integrity("remote entry header length is out of range"))
}

pub(super) fn decode_entry_record(header: &[u8], base_action_key: &str) -> RemoteStoreResult<EntryRecord> {
    let record = decode_canonical::<EntryRecord>(header, "entry")?;
    validate_entry_record(&record, base_action_key)?;
    Ok(record)
}

pub(super) fn exact_length(value: Option<i64>, maximum: u64, label: &str) -> RemoteStoreResult<u64> {
    let value = value
        .ok_or_else(|| RemoteStoreError::integrity(format!("remote {label} has no exact length")))
        .and_then(|bytes| {
            u64::try_from(bytes).map_err(|_| RemoteStoreError::integrity(format!("remote {label} length is invalid")))
        })?;
    if value > maximum {
        return Err(RemoteStoreError::integrity(format!(
            "remote {label} exceeds its byte bound"
        )));
    }
    Ok(value)
}

pub(super) fn parse_etag(value: Option<&str>) -> RemoteStoreResult<String> {
    let value = value.ok_or_else(|| RemoteStoreError::integrity("remote response has no ETag"))?;
    if value.is_empty()
        || value.len() > MAX_ETAG_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(RemoteStoreError::integrity("remote response ETag is invalid"));
    }
    Ok(value.to_string())
}

fn io_unavailable(_error: std::io::Error) -> RemoteStoreError {
    RemoteStoreError::unavailable("remote cache local streaming failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(value: u8) -> String {
        format!("{}{value:064x}", crate::compiler::native_cache::BASE_ACTION_KEY_PREFIX)
    }

    fn action(value: u8) -> String {
        format!("{}{value:064x}", crate::compiler::native_cache::ACTION_KEY_PREFIX)
    }

    fn result(value: u8) -> String {
        format!("{}{value:064x}", crate::compiler::native_cache::RESULT_KEY_PREFIX)
    }

    fn identity(value: u8) -> EntryIdentity {
        EntryIdentity {
            environment_names: vec![format!("CARGO_RAIL_TEST_{value}")],
            action_key: action(value),
            result_key: result(value),
            pack_length: u64::from(value) + 1,
        }
    }

    fn decode_entry_header(reader: &mut File, base_action_key: &str) -> RemoteStoreResult<EntryRecord> {
        let mut prelude = [0_u8; ENTRY_PRELUDE_LEN];
        reader
            .read_exact(&mut prelude)
            .map_err(|_| RemoteStoreError::integrity("remote entry prelude is truncated"))?;
        let header_length = decode_entry_prelude(&prelude)?;
        let mut header = vec![0_u8; header_length];
        reader
            .read_exact(&mut header)
            .map_err(|_| RemoteStoreError::integrity("remote entry header is truncated"))?;
        decode_entry_record(&header, base_action_key)
    }

    #[test]
    fn entry_conflicts_are_canonical_and_terminal() {
        let first = identity(1);
        let second = identity(2);
        let (left, right) = canonical_entry_pair(second.clone(), first.clone()).expect("pair");
        assert_eq!((left, right), (first, second));
    }

    #[test]
    fn unique_entry_round_trips_one_compressed_pack() {
        let base_action_key = base(1);
        let payload = vec![b'a'; 128 * 1024];
        let mut pack = tempfile::tempfile().expect("pack");
        pack.write_all(&payload).expect("write pack");
        pack.rewind().expect("rewind pack");
        let mut entry_identity = identity(1);
        entry_identity.pack_length = payload.len() as u64;

        let (mut encoded, encoded_bytes) =
            encode_unique_entry(&base_action_key, &entry_identity, pack).expect("encode entry");
        assert!(encoded_bytes < payload.len() as u64);
        let record = decode_entry_header(&mut encoded, &base_action_key).expect("decode header");
        let header_bytes = encoded.stream_position().expect("position");
        assert_eq!(
            record.state,
            EntryState::Unique {
                identity: entry_identity,
                payload_length: encoded_bytes - header_bytes,
            }
        );
        let mut compressed = Vec::new();
        encoded.read_to_end(&mut compressed).expect("read compressed pack");
        let mut body = EntryBody::new(
            Box::new(std::io::Cursor::new(compressed.clone())),
            compressed.len() as u64,
            payload.len() as u64,
        );
        let mut decoded = Vec::new();
        body.read_to_end(&mut decoded).expect("decode pack");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn malformed_compressed_payload_is_rejected_as_integrity_failure() {
        let payload = b"not a zstd frame".to_vec();
        let mut body = EntryBody::new(
            Box::new(std::io::Cursor::new(payload.clone())),
            payload.len() as u64,
            128,
        );
        let error = std::io::copy(&mut body, &mut std::io::sink()).expect_err("malformed payload must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
    }

    #[test]
    fn conflict_entry_has_no_payload_and_preserves_the_canonical_pair() {
        let first = identity(1);
        let second = identity(2);
        let record = EntryRecord {
            version: 1,
            base_action_key: base(2),
            state: EntryState::Conflict { first, second },
        };
        let (mut encoded, bytes) = encode_entry(&record, None).expect("encode conflict");
        assert_eq!(bytes, encoded.metadata().expect("metadata").len());
        assert_eq!(
            decode_entry_header(&mut encoded, &base(2)).expect("decode conflict"),
            record
        );
        assert_eq!(encoded.stream_position().expect("position"), bytes);
    }
}
