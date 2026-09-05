//! Strict parsing and normalization for remote compiler-cache URLs.

use std::net::IpAddr;

use rscrypto::Sha256;

use super::{RemoteStoreError, RemoteStoreResult};
use crate::compiler::native_cache::RemoteAuthorityId;
use crate::source::ContentDigest;

pub(super) const MAX_URL_BYTES: usize = 4 * 1024;
const MAX_REGION_BYTES: usize = 64;
const MAX_BUCKET_BYTES: usize = 255;
const MAX_PREFIX_BYTES: usize = 2 * 1024;
const AUTHORITY_DOMAIN: &[u8] = b"cargo-rail-remote-authority-v2\0";
const PROTOCOL: &str = "native-v6";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    AwsS3,
    AzureBlob,
    CloudflareR2,
    S3Compatible,
}

struct RemoteCacheUrlParts {
    provider: Provider,
    normalized_url: String,
    endpoint: String,
    bucket: String,
    prefix: String,
    region: String,
    expected_owner: Option<String>,
    addressing: &'static str,
}

impl Provider {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AwsS3 => "aws-s3",
            Self::AzureBlob => "azure-blob",
            Self::CloudflareR2 => "cloudflare-r2",
            Self::S3Compatible => "s3-compatible",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct RemoteCacheAuthority {
    provider: Provider,
    normalized_url: String,
    endpoint: String,
    bucket: String,
    prefix: String,
    region: String,
    expected_owner: Option<String>,
    addressing: &'static str,
    identity: RemoteAuthorityId,
}

impl RemoteCacheAuthority {
    pub(super) fn parse(value: &str) -> RemoteStoreResult<Self> {
        validate_url_bytes(value)?;
        let (scheme, remainder) = value
            .split_once("://")
            .ok_or_else(|| RemoteStoreError::configuration("remote cache URL has no supported scheme"))?;
        if scheme != scheme.to_ascii_lowercase() {
            return Err(RemoteStoreError::configuration(
                "remote cache URL scheme must be lowercase",
            ));
        }
        let (location, query) = split_query(remainder)?;
        let (authority, path) = location
            .split_once('/')
            .map_or((location, ""), |(authority, path)| (authority, path));
        if authority.is_empty() || authority.contains('@') {
            return Err(RemoteStoreError::configuration(
                "remote cache URL authority is empty or contains user information",
            ));
        }
        match scheme {
            "s3" => Self::parse_aws(authority, path, query),
            "azure" => Self::parse_azure(authority, path, query),
            "r2" => Self::parse_r2(authority, path, query),
            "s3+http" => Self::parse_loopback_fixture(authority, path, query),
            _ => Err(RemoteStoreError::configuration(
                "remote cache URL scheme must be s3, azure, r2, or loopback s3+http",
            )),
        }
    }

    fn parse_azure(authority: &str, path: &str, query: Option<&str>) -> RemoteStoreResult<Self> {
        if query.is_some() || authority.contains(':') {
            return Err(RemoteStoreError::configuration(
                "Azure Blob URLs do not accept query parameters or ports",
            ));
        }
        let account = authority.to_ascii_lowercase();
        if !(3..=24).contains(&account.len())
            || !account
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err(RemoteStoreError::configuration(
                "Azure Blob URL account must contain 3 to 24 lowercase letters or digits",
            ));
        }
        let (container, prefix) = split_bucket_path(path)?;
        let container = normalize_azure_container(container)?;
        let prefix = normalize_path(prefix)?;
        let endpoint = format!("https://{account}.blob.core.windows.net");
        let normalized_url = format!("azure://{account}/{container}{}", normalized_prefix_path(&prefix));
        Self::new(RemoteCacheUrlParts {
            provider: Provider::AzureBlob,
            normalized_url,
            endpoint,
            bucket: container,
            prefix,
            region: "global".to_string(),
            expected_owner: None,
            addressing: "path",
        })
    }

    fn parse_aws(authority: &str, path: &str, query: Option<&str>) -> RemoteStoreResult<Self> {
        if authority.contains(':') {
            return Err(RemoteStoreError::configuration(
                "official S3 URLs do not accept an explicit endpoint or port",
            ));
        }
        let bucket = normalize_aws_bucket(authority)?;
        let prefix = normalize_path(path)?;
        let query = parse_query(query, &["owner", "region"])?;
        let region = required_query(&query, "region")?;
        validate_region(region)?;
        let owner = required_query(&query, "owner")?;
        validate_expected_owner(owner)?;
        let endpoint = format!("https://{bucket}.s3.{region}.amazonaws.com");
        let normalized_url = format!(
            "s3://{bucket}{}?owner={owner}&region={region}",
            normalized_prefix_path(&prefix)
        );
        Self::new(RemoteCacheUrlParts {
            provider: Provider::AwsS3,
            normalized_url,
            endpoint,
            bucket,
            prefix,
            region: region.to_string(),
            expected_owner: Some(owner.to_string()),
            addressing: "virtual-hosted",
        })
    }

    fn parse_r2(authority: &str, path: &str, query: Option<&str>) -> RemoteStoreResult<Self> {
        if query.is_some() || authority.contains(':') {
            return Err(RemoteStoreError::configuration(
                "R2 URLs do not accept query parameters or ports",
            ));
        }
        let account = authority.to_ascii_lowercase();
        if account.len() != 32 || !account.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RemoteStoreError::configuration(
                "R2 URL account ID must contain exactly 32 hexadecimal digits",
            ));
        }
        let (bucket, prefix) = split_bucket_path(path)?;
        let bucket = normalize_compatible_bucket(bucket)?;
        let prefix = normalize_path(prefix)?;
        let endpoint = format!("https://{account}.r2.cloudflarestorage.com");
        let normalized_url = format!("r2://{account}/{bucket}{}", normalized_prefix_path(&prefix));
        Self::new(RemoteCacheUrlParts {
            provider: Provider::CloudflareR2,
            normalized_url,
            endpoint,
            bucket,
            prefix,
            region: "auto".to_string(),
            expected_owner: None,
            addressing: "path",
        })
    }

    fn parse_loopback_fixture(authority: &str, path: &str, query: Option<&str>) -> RemoteStoreResult<Self> {
        let (host, port) = normalize_endpoint(authority)?;
        let loopback = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
        if !loopback || port.is_none() {
            return Err(RemoteStoreError::configuration(
                "cleartext remote cache URLs require a loopback IP and explicit port",
            ));
        }
        let (bucket, prefix) = split_bucket_path(path)?;
        let bucket = normalize_compatible_bucket(bucket)?;
        let prefix = normalize_path(prefix)?;
        let query = parse_query(query, &["region"])?;
        let region = required_query(&query, "region")?;
        validate_region(region)?;
        let normalized_authority = match port {
            Some(port) => format!("{host}:{port}"),
            None => return Err(RemoteStoreError::configuration("loopback fixture port is missing")),
        };
        let endpoint = format!("http://{normalized_authority}");
        let normalized_url = format!(
            "s3+http://{normalized_authority}/{bucket}{}?region={region}",
            normalized_prefix_path(&prefix)
        );
        Self::new(RemoteCacheUrlParts {
            provider: Provider::S3Compatible,
            normalized_url,
            endpoint,
            bucket,
            prefix,
            region: region.to_string(),
            expected_owner: None,
            addressing: "path",
        })
    }
    fn new(parts: RemoteCacheUrlParts) -> RemoteStoreResult<Self> {
        let RemoteCacheUrlParts {
            provider,
            normalized_url,
            endpoint,
            bucket,
            prefix,
            region,
            expected_owner,
            addressing,
        } = parts;
        let identity = authority_id(
            provider,
            &endpoint,
            &bucket,
            &prefix,
            &region,
            expected_owner.as_deref(),
            addressing,
        )?;
        Ok(Self {
            provider,
            normalized_url,
            endpoint,
            bucket,
            prefix,
            region,
            expected_owner,
            addressing,
            identity,
        })
    }

    pub(super) fn identity(&self) -> &RemoteAuthorityId {
        &self.identity
    }

    pub(super) const fn provider_name(&self) -> &'static str {
        self.provider.as_str()
    }

    pub(super) const fn protocol_name(&self) -> &'static str {
        PROTOCOL
    }

    pub(super) fn normalized_url(&self) -> &str {
        &self.normalized_url
    }

    pub(super) const fn is_official_aws(&self) -> bool {
        matches!(self.provider, Provider::AwsS3)
    }

    pub(super) const fn is_azure_blob(&self) -> bool {
        matches!(self.provider, Provider::AzureBlob)
    }

    pub(super) fn is_loopback_fixture(&self) -> bool {
        matches!(self.provider, Provider::S3Compatible) && self.endpoint.starts_with("http://")
    }

    pub(super) const fn supports_s3_transport(&self) -> bool {
        matches!(
            self.provider,
            Provider::AwsS3 | Provider::CloudflareR2 | Provider::S3Compatible
        )
    }

    pub(super) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(super) fn bucket(&self) -> &str {
        &self.bucket
    }

    pub(super) fn prefix(&self) -> &str {
        &self.prefix
    }

    pub(super) fn region(&self) -> &str {
        &self.region
    }

    pub(super) fn expected_owner(&self) -> Option<&str> {
        self.expected_owner.as_deref()
    }

    pub(super) const fn addressing(&self) -> &'static str {
        self.addressing
    }
}

fn normalize_azure_container(value: &str) -> RemoteStoreResult<String> {
    let container = value.to_ascii_lowercase();
    if !(3..=63).contains(&container.len())
        || !container
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !container.as_bytes()[0].is_ascii_alphanumeric()
        || !container.as_bytes()[container.len() - 1].is_ascii_alphanumeric()
        || container.contains("--")
    {
        return Err(RemoteStoreError::configuration(
            "Azure Blob URL container is not a canonical container name",
        ));
    }
    Ok(container)
}

fn validate_url_bytes(value: &str) -> RemoteStoreResult<()> {
    if value.is_empty()
        || value.len() > MAX_URL_BYTES
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace() || byte == b'\\' || byte == b'#')
    {
        return Err(RemoteStoreError::configuration(
            "remote cache URL is empty, oversized, non-ASCII, or contains forbidden bytes",
        ));
    }
    Ok(())
}

fn split_query(value: &str) -> RemoteStoreResult<(&str, Option<&str>)> {
    let mut parts = value.split('?');
    let location = parts.next().unwrap_or_default();
    let query = parts.next();
    if parts.next().is_some() || query == Some("") {
        return Err(RemoteStoreError::configuration(
            "remote cache URL query is empty or ambiguous",
        ));
    }
    Ok((location, query))
}

fn parse_query<'a>(value: Option<&'a str>, expected: &[&str]) -> RemoteStoreResult<Vec<(&'a str, &'a str)>> {
    let value =
        value.ok_or_else(|| RemoteStoreError::configuration("remote cache URL is missing required query data"))?;
    let mut fields = Vec::new();
    for pair in value.split('&') {
        let (name, value) = pair
            .split_once('=')
            .filter(|(name, value)| !name.is_empty() && !value.is_empty())
            .ok_or_else(|| RemoteStoreError::configuration("remote cache URL query is malformed"))?;
        if name.contains('%') || value.contains('%') || !expected.contains(&name) {
            return Err(RemoteStoreError::configuration(
                "remote cache URL query contains encoded or unknown data",
            ));
        }
        if fields.iter().any(|(existing, _)| existing == &name) {
            return Err(RemoteStoreError::configuration(
                "remote cache URL query contains a duplicate key",
            ));
        }
        fields.push((name, value));
    }
    if fields.len() != expected.len() {
        return Err(RemoteStoreError::configuration(
            "remote cache URL query omits a required key",
        ));
    }
    fields.sort_unstable_by_key(|(name, _)| *name);
    Ok(fields)
}

fn required_query<'a>(fields: &[(&'a str, &'a str)], name: &str) -> RemoteStoreResult<&'a str> {
    fields
        .iter()
        .find_map(|(candidate, value)| (*candidate == name).then_some(*value))
        .ok_or_else(|| RemoteStoreError::configuration("remote cache URL omits a required query key"))
}

fn split_bucket_path(path: &str) -> RemoteStoreResult<(&str, &str)> {
    let (bucket, prefix) = path
        .split_once('/')
        .map_or((path, ""), |(bucket, prefix)| (bucket, prefix));
    if bucket.is_empty() {
        return Err(RemoteStoreError::configuration("remote cache URL has an empty bucket"));
    }
    Ok((bucket, prefix))
}

fn normalize_endpoint(authority: &str) -> RemoteStoreResult<(String, Option<u16>)> {
    let (host, port) = if let Some(value) = authority.strip_prefix('[') {
        let (host, suffix) = value
            .split_once(']')
            .ok_or_else(|| RemoteStoreError::configuration("remote cache endpoint has an invalid IPv6 host"))?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(parse_port(suffix.strip_prefix(':').ok_or_else(|| {
                RemoteStoreError::configuration("remote cache endpoint has an ambiguous port")
            })?)?)
        };
        let address = host
            .parse::<IpAddr>()
            .map_err(|_| RemoteStoreError::configuration("remote cache endpoint has an invalid IPv6 host"))?;
        (format!("[{address}]"), port)
    } else {
        let colon_count = authority.bytes().filter(|byte| *byte == b':').count();
        if colon_count > 1 {
            return Err(RemoteStoreError::configuration(
                "remote cache IPv6 endpoints require brackets",
            ));
        }
        let (host, port) = authority.split_once(':').map_or_else(
            || Ok((authority, None)),
            |(host, port)| Ok((host, Some(parse_port(port)?))),
        )?;
        let host = normalize_host(host)?;
        (host, port)
    };
    Ok((host, port))
}

fn parse_port(value: &str) -> RemoteStoreResult<u16> {
    if value.is_empty() || value.len() > 5 || value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(RemoteStoreError::configuration(
            "remote cache endpoint port is invalid or noncanonical",
        ));
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| RemoteStoreError::configuration("remote cache endpoint port is invalid"))
}

fn normalize_host(value: &str) -> RemoteStoreResult<String> {
    if value.is_empty() || value.len() > 253 || value.ends_with('.') || value.contains('%') {
        return Err(RemoteStoreError::configuration("remote cache endpoint host is invalid"));
    }
    if let Ok(address) = value.parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    let value = value.to_ascii_lowercase();
    if value.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || !label.as_bytes()[0].is_ascii_alphanumeric()
            || !label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            || !label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err(RemoteStoreError::configuration("remote cache endpoint host is invalid"));
    }
    Ok(value)
}

fn normalize_aws_bucket(value: &str) -> RemoteStoreResult<String> {
    let bucket = value.to_ascii_lowercase();
    let valid_length = (3..=63).contains(&bucket.len());
    let valid_bytes = bucket
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.'));
    let valid_edges = bucket
        .as_bytes()
        .first()
        .zip(bucket.as_bytes().last())
        .is_some_and(|(first, last)| first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric());
    let reserved = bucket.starts_with("xn--")
        || bucket.starts_with("sthree-")
        || bucket.starts_with("amzn_s3_demo_")
        || ["-s3alias", "--ol-s3", ".mrap", "--x-s3", "--table-s3"]
            .iter()
            .any(|suffix| bucket.ends_with(suffix));
    if !valid_length
        || !valid_bytes
        || !valid_edges
        || bucket.contains("..")
        || bucket
            .split('.')
            .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
        || bucket.parse::<std::net::Ipv4Addr>().is_ok()
        || reserved
    {
        return Err(RemoteStoreError::configuration(
            "official S3 URL bucket is not a classic DNS bucket name",
        ));
    }
    Ok(bucket)
}

fn normalize_compatible_bucket(value: &str) -> RemoteStoreResult<String> {
    let bucket = normalize_segment(value)?;
    if bucket.len() > MAX_BUCKET_BYTES || matches!(bucket.as_str(), "." | "..") {
        return Err(RemoteStoreError::configuration("remote cache URL bucket is invalid"));
    }
    Ok(bucket)
}

fn normalize_path(value: &str) -> RemoteStoreResult<String> {
    let value = value.trim_matches('/');
    if value.len() > MAX_PREFIX_BYTES {
        return Err(RemoteStoreError::configuration(
            "remote cache URL prefix exceeds its byte bound",
        ));
    }
    if value.is_empty() {
        return Ok(String::new());
    }
    value
        .split('/')
        .map(normalize_segment)
        .collect::<RemoteStoreResult<Vec<_>>>()
        .and_then(|segments| {
            if segments
                .iter()
                .any(|segment| segment.is_empty() || matches!(segment.as_str(), "." | ".."))
            {
                Err(RemoteStoreError::configuration(
                    "remote cache URL prefix contains an empty or dot segment",
                ))
            } else {
                Ok(segments.join("/"))
            }
        })
}

fn normalize_segment(value: &str) -> RemoteStoreResult<String> {
    let bytes = value.as_bytes();
    let mut normalized = String::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            let high = bytes.get(index + 1).copied().and_then(hex_value);
            let low = bytes.get(index + 2).copied().and_then(hex_value);
            let decoded = high
                .zip(low)
                .map(|(high, low)| high * 16 + low)
                .ok_or_else(|| RemoteStoreError::configuration("remote cache URL contains invalid percent encoding"))?;
            if matches!(decoded, b'/' | b'\\' | 0) || decoded.is_ascii_control() {
                return Err(RemoteStoreError::configuration(
                    "remote cache URL contains an encoded separator or control byte",
                ));
            }
            if unreserved(decoded) {
                normalized.push(char::from(decoded));
            } else if pchar(decoded) {
                normalized.push('%');
                normalized.push(hex_digit(decoded >> 4));
                normalized.push(hex_digit(decoded & 0x0f));
            } else {
                return Err(RemoteStoreError::configuration(
                    "remote cache URL contains unsupported encoded data",
                ));
            }
            index += 3;
            continue;
        }
        if !pchar(byte) {
            return Err(RemoteStoreError::configuration(
                "remote cache URL path contains a noncanonical byte",
            ));
        }
        normalized.push(char::from(byte));
        index += 1;
    }
    Ok(normalized)
}

const fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

const fn pchar(byte: u8) -> bool {
    unreserved(byte)
        || matches!(
            byte,
            b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'@'
        )
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        _ => char::from(b'A' + value - 10),
    }
}

fn normalized_prefix_path(prefix: &str) -> String {
    if prefix.is_empty() {
        String::new()
    } else {
        format!("/{prefix}")
    }
}

fn validate_region(region: &str) -> RemoteStoreResult<()> {
    if region.is_empty()
        || region.len() > MAX_REGION_BYTES
        || !region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !region.as_bytes()[0].is_ascii_alphanumeric()
        || !region.as_bytes()[region.len() - 1].is_ascii_alphanumeric()
        || region.contains("--")
    {
        return Err(RemoteStoreError::configuration("remote cache region is invalid"));
    }
    Ok(())
}

fn validate_expected_owner(owner: &str) -> RemoteStoreResult<()> {
    if owner.len() != 12 || !owner.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RemoteStoreError::configuration(
            "official S3 URL owner must contain exactly 12 decimal digits",
        ));
    }
    Ok(())
}

fn authority_id(
    provider: Provider,
    endpoint: &str,
    bucket: &str,
    prefix: &str,
    region: &str,
    expected_owner: Option<&str>,
    addressing: &str,
) -> RemoteStoreResult<RemoteAuthorityId> {
    let mut hasher = Sha256::new();
    hasher.update(AUTHORITY_DOMAIN);
    for (tag, value) in [
        ("provider", provider.as_str()),
        ("protocol", PROTOCOL),
        ("endpoint", endpoint),
        ("bucket", bucket),
        ("prefix", prefix),
        ("region", region),
        ("expected-owner", expected_owner.unwrap_or("")),
        ("addressing", addressing),
    ] {
        let tag_length = u32::try_from(tag.len())
            .map_err(|_| RemoteStoreError::integrity("remote cache authority tag length is out of range"))?;
        hasher.update(&tag_length.to_le_bytes());
        hasher.update(tag.as_bytes());
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    RemoteAuthorityId::parse(format!(
        "remote-authority-v1-sha256-{}",
        ContentDigest::from_sha256_bytes(hasher.finalize())
    ))
    .map_err(|_| RemoteStoreError::integrity("remote cache authority identity could not be derived"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> RemoteCacheAuthority {
        RemoteCacheAuthority::parse(value).expect("valid remote URL")
    }

    #[test]
    fn official_s3_normalizes_query_prefix_and_bucket_case() {
        let authority = parse("s3://Rail-Cache//team/%61?region=us-east-1&owner=123456789012");
        assert_eq!(
            authority.normalized_url(),
            "s3://rail-cache/team/a?owner=123456789012&region=us-east-1"
        );
        assert_eq!(authority.provider_name(), "aws-s3");
    }

    #[test]
    fn provider_authorities_are_distinct() {
        let aws = parse("s3://rail-cache/team?owner=123456789012&region=us-east-1");
        let r2 = parse("r2://0123456789abcdef0123456789abcdef/rail-cache/team");
        assert_ne!(aws.identity(), r2.identity());
    }

    #[test]
    fn r2_derives_only_the_account_endpoint_and_auto_region() {
        let authority = parse("r2://0123456789ABCDEF0123456789ABCDEF/cache/team");
        assert_eq!(
            authority.normalized_url(),
            "r2://0123456789abcdef0123456789abcdef/cache/team"
        );
        assert_eq!(authority.provider_name(), "cloudflare-r2");
        assert_eq!(
            authority.endpoint(),
            "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com"
        );
        assert_eq!(authority.region, "auto");
        assert_eq!(authority.addressing, "path");
    }

    #[test]
    fn r2_rejects_noncanonical_accounts_and_jurisdiction_queries() {
        for invalid in [
            "r2://account/cache/team",
            "r2://0123456789abcdef0123456789abcdeg/cache/team",
            "r2://0123456789abcdef0123456789abcdef:443/cache/team",
            "r2://0123456789abcdef0123456789abcdef/cache/team?jurisdiction=eu",
        ] {
            assert!(RemoteCacheAuthority::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn azure_derives_only_the_public_blob_endpoint() {
        let authority = parse("azure://RailCache01/Cargo-Rail//team/%61");
        assert_eq!(authority.normalized_url(), "azure://railcache01/cargo-rail/team/a");
        assert_eq!(authority.endpoint(), "https://railcache01.blob.core.windows.net");
        assert_eq!(authority.bucket(), "cargo-rail");
        assert_eq!(authority.provider_name(), "azure-blob");
        assert!(authority.is_azure_blob());
        assert!(!authority.supports_s3_transport());
    }

    #[test]
    fn cleartext_requires_explicit_loopback_authority() {
        for valid in [
            "s3+http://127.0.0.1:80/cache/team?region=test-1",
            "s3+http://127.0.0.1:9000/cache/team?region=test-1",
        ] {
            let authority = parse(valid);
            let reparsed = parse(authority.normalized_url());
            assert_eq!(reparsed.normalized_url(), authority.normalized_url());
            assert_eq!(reparsed.identity(), authority.identity());
        }
        for invalid in [
            "s3+http://127.0.0.1/cache/team?region=test-1",
            "s3+http://localhost:9000/cache/team?region=test-1",
            "s3+http://192.0.2.1:9000/cache/team?region=test-1",
        ] {
            assert!(RemoteCacheAuthority::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn parser_rejects_credential_and_ambiguity_surfaces() {
        for invalid in [
            "s3://user:secret@rail-cache/team?owner=123456789012&region=us-east-1",
            "s3://rail-cache/team#fragment?owner=123456789012&region=us-east-1",
            "s3://rail-cache/a%2fb?owner=123456789012&region=us-east-1",
            "s3://rail-cache/../team?owner=123456789012&region=us-east-1",
            "s3://rail-cache/team?owner=123456789012&owner=123456789012&region=us-east-1",
            "s3://rail-cache/team?owner=123456789012&region=us-east-1&token=secret",
            "s3+https://cache.example/bucket?region=us-east-1",
            "s3+https://cache.example:0443/bucket?region=us-east-1",
            "azure://account/container?token=secret",
            "azure://account/container--name",
            "azure://account/ab",
        ] {
            assert!(RemoteCacheAuthority::parse(invalid).is_err(), "accepted {invalid}");
        }
    }
}
