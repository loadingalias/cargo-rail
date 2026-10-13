//! Bounded, checksum-bound observation of a Cargo sparse registry.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;

use crate::error::{RailError, RailResult};

const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct RegistryObserver {
    index: reqwest::Url,
    client: Client,
    request_timeout: Duration,
}

impl RegistryObserver {
    pub(crate) fn new(index: &str) -> RailResult<Self> {
        Self::with_request_timeout(index, Duration::from_secs(30))
    }

    fn with_request_timeout(index: &str, request_timeout: Duration) -> RailResult<Self> {
        let source = index.strip_prefix("sparse+").unwrap_or(index);
        let index =
            reqwest::Url::parse(source).map_err(|_| RailError::message("invalid release registry index URL"))?;
        if index.as_str() != source
            || !(index.scheme() == "https"
                || (index.scheme() == "http"
                    && index
                        .host_str()
                        .is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))))
            || !index.username().is_empty()
            || index.password().is_some()
            || index.query().is_some()
            || index.fragment().is_some()
            || !index.path().ends_with('/')
        {
            return Err(RailError::message(
                "release observation requires an HTTPS sparse index without credentials, query, or fragment",
            ));
        }
        let mut client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent(concat!("cargo-rail/", env!("CARGO_PKG_VERSION")));
        if index
            .host_str()
            .is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
        {
            client = client.no_proxy();
        }
        let client = client
            .build()
            .map_err(|_| RailError::message("could not initialize release registry observation"))?;
        Ok(Self {
            index,
            client,
            request_timeout,
        })
    }

    pub(crate) fn has_published_versions(&self, name: &str) -> Result<bool, String> {
        if !valid_name(name) {
            return Err("invalid package name".into());
        }
        let Some(bytes) = self.read_index(name)? else {
            return Ok(false);
        };
        // Validate every record, including yanked versions, before claiming prior publication.
        version_record(&bytes, name, &semver::Version::new(0, 0, 0))?;
        Ok(true)
    }

    fn read_index(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let lower = name.to_ascii_lowercase();
        let path = match lower.len() {
            1 => format!("1/{lower}"),
            2 => format!("2/{lower}"),
            3 => format!("3/{}/{lower}", lower.get(..1).ok_or("invalid package name")?),
            _ => format!(
                "{}/{}/{lower}",
                lower.get(..2).ok_or("invalid package name")?,
                lower.get(2..4).ok_or("invalid package name")?
            ),
        };
        let url = self.index.join(&path).map_err(|_| "invalid registry index path")?;
        // A request deadline also bounds a body that keeps delivering bytes.
        let response = self
            .client
            .get(url)
            .timeout(self.request_timeout)
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .map_err(|_| "registry index request failed or timed out")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status() != reqwest::StatusCode::OK {
            return Err(format!("registry index returned HTTP {}", response.status().as_u16()));
        }
        if response.content_length().is_some_and(|size| size > MAX_INDEX_BYTES) {
            return Err("registry index exceeds the observation size limit".into());
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_INDEX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "registry index body was incomplete or timed out")?;
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err("registry index exceeds the observation size limit".into());
        }
        Ok(Some(bytes))
    }
}

impl RegistryObserver {
    /// The index record for one version: its checksum and whether it is yanked.
    fn record(&self, name: &str, version: &semver::Version) -> Result<Option<(String, bool)>, String> {
        if !valid_name(name) {
            return Err("invalid package name".into());
        }
        match self.read_index(name)? {
            None => Ok(None),
            Some(bytes) => version_record(&bytes, name, version),
        }
    }

    fn get(&self, url: reqwest::Url, limit: u64, what: &str) -> Result<Vec<u8>, String> {
        let response = self
            .client
            .get(url)
            .timeout(Duration::from_secs(120))
            .send()
            .map_err(|_| format!("{what} request failed or timed out"))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(format!("{what} returned HTTP {}", response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        response
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| format!("{what} body was incomplete or timed out"))?;
        if bytes.len() as u64 > limit {
            return Err(format!("{what} exceeds its size limit"));
        }
        Ok(bytes)
    }
}

/// Largest `.crate` a release reads back from a registry.
const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

/// The registry a release publishes to, observed through its sparse index.
#[derive(Debug)]
pub(crate) struct PublicationRegistry {
    name: String,
    observer: RegistryObserver,
    download: String,
}

/// One version a registry already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Published {
    pub(crate) checksum: String,
    pub(crate) yanked: bool,
}

#[derive(Deserialize)]
struct IndexConfig {
    dl: String,
    #[serde(default, rename = "auth-required")]
    auth_required: bool,
}

#[derive(Deserialize)]
struct VcsInfo {
    git: Option<VcsGit>,
}

#[derive(Deserialize)]
struct VcsGit {
    sha1: String,
    #[serde(default)]
    dirty: bool,
}

impl PublicationRegistry {
    /// Resolve `name` through Cargo's configuration: crates.io, or `[registries.NAME].index`.
    pub(crate) fn resolve(name: &str, cargo_config: Option<&serde_json::Value>) -> RailResult<Self> {
        let index = if name == "crates-io" {
            CRATES_IO_INDEX.to_owned()
        } else {
            let variable = format!("CARGO_REGISTRIES_{}_INDEX", name.to_ascii_uppercase().replace('-', "_"));
            std::env::var(&variable)
                .ok()
                .or_else(|| {
                    cargo_config
                        .and_then(|settings| settings["registries"][name]["index"].as_str())
                        .map(str::to_owned)
                })
                .ok_or_else(|| {
                    RailError::with_help(
                        format!("Cargo registry '{name}' is not configured"),
                        format!("declare [registries.{name}] index in Cargo configuration, or set {variable}"),
                    )
                })?
        };
        if !index.starts_with("sparse+") {
            return Err(RailError::with_help(
                format!("Cargo registry '{name}' does not use a sparse index"),
                "release observes registries through the sparse protocol; configure the registry's `sparse+https://` index",
            ));
        }
        let observer = RegistryObserver::new(&index)?;
        let config = observer
            .index
            .join("config.json")
            .map_err(|_| RailError::message("invalid registry index URL"))?;
        let config: IndexConfig = serde_json::from_slice(
            &observer
                .get(config, 64 * 1024, "registry configuration")
                .map_err(RailError::message)?,
        )
        .map_err(|_| RailError::message(format!("registry '{name}' returned an invalid config.json")))?;
        if config.auth_required {
            return Err(RailError::with_help(
                format!("registry '{name}' requires authentication to read packages"),
                "release reads published packages to prove their source commit; publish to a registry with public reads",
            ));
        }
        Ok(Self {
            name: name.to_owned(),
            observer,
            download: config.dl,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The registry's record of `crate_name` at `version`, if it holds one.
    pub(crate) fn version(&self, crate_name: &str, version: &semver::Version) -> RailResult<Option<Published>> {
        self.observer
            .record(crate_name, version)
            .map(|record| record.map(|(checksum, yanked)| Published { checksum, yanked }))
            .map_err(|reason| {
                RailError::message(format!(
                    "could not read {crate_name} {version} from registry '{}': {reason}",
                    self.name
                ))
            })
    }

    /// The Git commit Cargo recorded when it packaged the published `crate_name` at `version`.
    ///
    /// `None` means the package records no clean Git commit, so its source cannot be proven.
    pub(crate) fn source_commit(
        &self,
        crate_name: &str,
        version: &semver::Version,
        published: &Published,
    ) -> RailResult<Option<String>> {
        let failure = |reason: String| {
            RailError::message(format!(
                "could not read the published {crate_name} {version} from registry '{}': {reason}",
                self.name
            ))
        };
        let url = download_url(&self.download, crate_name, version, &published.checksum).map_err(failure)?;
        let bytes = self
            .observer
            .get(url, MAX_PACKAGE_BYTES, "package download")
            .map_err(failure)?;
        if crate::source::ContentDigest::sha256(&bytes).to_string() != published.checksum {
            return Err(failure(
                "the downloaded bytes do not match the index checksum".to_owned(),
            ));
        }
        let wanted = format!("{crate_name}-{version}/.cargo_vcs_info.json");
        let Some(info) = tar_entry(flate2::read::GzDecoder::new(bytes.as_slice()), &wanted).map_err(failure)? else {
            return Ok(None);
        };
        let info: VcsInfo =
            serde_json::from_slice(&info).map_err(|_| failure("its .cargo_vcs_info.json is invalid".to_owned()))?;
        Ok(info.git.filter(|git| !git.dirty).map(|git| git.sha1))
    }
}

pub(crate) const CRATES_IO_INDEX: &str = "sparse+https://index.crates.io/";

/// Expand a registry `dl` template the way Cargo does.
fn download_url(template: &str, name: &str, version: &semver::Version, checksum: &str) -> Result<reqwest::Url, String> {
    // Package names are ASCII (`valid_name`), so byte ranges are character ranges.
    let part = |range: std::ops::Range<usize>| name.get(range).unwrap_or_default();
    let prefix = match name.len() {
        1 => "1".to_owned(),
        2 => "2".to_owned(),
        3 => format!("3/{}", part(0..1)),
        _ => format!("{}/{}", part(0..2), part(2..4)),
    };
    // Cargo's `dl` markers, spelled without literal braces so they read as text, not format arguments.
    let marker = |name: &str| format!("{}{name}{}", '{', '}');
    let markers = ["crate", "version", "prefix", "lowerprefix", "sha256-checksum"].map(marker);
    let url = if markers.iter().any(|marker| template.contains(marker.as_str())) {
        template
            .replace(&marker("crate"), name)
            .replace(&marker("version"), &version.to_string())
            .replace(&marker("lowerprefix"), &prefix.to_ascii_lowercase())
            .replace(&marker("prefix"), &prefix)
            .replace(&marker("sha256-checksum"), checksum)
    } else {
        format!("{}/{name}/{version}/download", template.trim_end_matches('/'))
    };
    let url = reqwest::Url::parse(&url).map_err(|_| "the registry download URL is invalid".to_owned())?;
    let loopback = url
        .host_str()
        .is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback) {
        return Err("the registry download URL is not HTTPS".to_owned());
    }
    Ok(url)
}

/// Read one regular file from a tar stream, following ustar prefixes and PAX or GNU long names.
fn tar_entry(mut reader: impl std::io::Read, wanted: &str) -> Result<Option<Vec<u8>>, String> {
    const BLOCK: usize = 512;
    let mut long_name: Option<String> = None;
    let mut read_total: u64 = 0;
    loop {
        let mut header = [0_u8; BLOCK];
        if reader.read_exact(&mut header).is_err() || header.iter().all(|byte| *byte == 0) {
            return Ok(None);
        }
        let field = |range: std::ops::Range<usize>| {
            let bytes = &header[range];
            let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        };
        let size = u64::from_str_radix(field(124..136).trim(), 8).map_err(|_| "the package tar header is invalid")?;
        read_total = read_total.saturating_add(size);
        if read_total > 4 * MAX_PACKAGE_BYTES {
            return Err("the package expands beyond its size limit".to_owned());
        }
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        let kind = header[156];
        let take = |reader: &mut dyn std::io::Read| -> Result<Vec<u8>, String> {
            let mut data = vec![0; usize::try_from(size).map_err(|_| "the package entry is too large")?];
            reader.read_exact(&mut data).map_err(|_| "the package is truncated")?;
            std::io::copy(&mut reader.take(padded - size), &mut std::io::sink())
                .map_err(|_| "the package is truncated")?;
            Ok(data)
        };
        match kind {
            b'L' => {
                let data = take(&mut reader)?;
                long_name = Some(String::from_utf8_lossy(&data).trim_end_matches('\0').to_owned());
            }
            b'x' => {
                let data = take(&mut reader)?;
                long_name = String::from_utf8_lossy(&data).lines().find_map(|line| {
                    line.split_once(' ')
                        .and_then(|(_, record)| record.strip_prefix("path="))
                        .map(str::to_owned)
                });
            }
            _ => {
                let path = long_name.take().unwrap_or_else(|| {
                    let (prefix, name) = (field(345..500), field(0..100));
                    if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    }
                });
                if path == wanted && matches!(kind, b'0' | 0) {
                    if size > 1024 * 1024 {
                        return Err("the package metadata entry is too large".to_owned());
                    }
                    return take(&mut reader).map(Some);
                }
                std::io::copy(&mut (&mut reader).take(padded), &mut std::io::sink())
                    .map_err(|_| "the package is truncated")?;
            }
        }
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

// Registry records are an extensible external format. Serde rejects duplicate
// authority fields; unrelated Cargo metadata does not grant publication authority.
#[derive(Deserialize)]
struct IndexRecord {
    name: String,
    vers: String,
    cksum: String,
    yanked: bool,
    v: Option<u32>,
}

/// The checksum and yank state of `version` in one package's index file, after validating every record.
///
/// Build metadata does not distinguish versions, as on crates.io.
fn version_record(bytes: &[u8], name: &str, version: &semver::Version) -> Result<Option<(String, bool)>, String> {
    let mut found = None;
    let mut versions = BTreeSet::new();
    let mut wanted = version.clone();
    wanted.build = semver::BuildMetadata::EMPTY;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err("registry returned an empty index".into());
    }
    for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let record: IndexRecord =
            serde_json::from_slice(line).map_err(|_| "registry returned an invalid index record")?;
        let mut parsed = semver::Version::parse(&record.vers).map_err(|_| "registry returned an invalid version")?;
        parsed.build = semver::BuildMetadata::EMPTY;
        if record.name != name
            || !crate::source::ContentDigest::is_sha256_hex(&record.cksum)
            || !versions.insert(parsed.clone())
        {
            return Err("registry returned ambiguous package identities or checksums".into());
        }
        if record.v.is_some_and(|v| !matches!(v, 1 | 2)) {
            return Err("registry returned an unsupported index record version".into());
        }
        if parsed == wanted {
            found = Some((record.cksum, record.yanked));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Write as _};
    use std::net::TcpListener;

    const DIGEST: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn record(version: &str, checksum: &str, yanked: bool) -> String {
        format!(
            r#"{{"name":"Example_Crate","vers":"{version}","cksum":"{checksum}","yanked":{yanked},"deps":[],"v":2}}"#
        )
    }

    type Record = Result<Option<(String, bool)>, String>;

    fn observe_response(status: u16, headers: &str, body: &str) -> Record {
        with_response(status, headers, body, |observer| {
            observer.record("Example_Crate", &semver::Version::new(1, 2, 3))
        })
    }

    fn with_response<T>(status: u16, headers: &str, body: &str, operation: impl FnOnce(&RegistryObserver) -> T) -> T {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!("HTTP/1.1 {status} Result\r\n{headers}Connection: close\r\n\r\n{body}");
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut stream = std::io::BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            assert_eq!(line, "GET /index/ex/am/example_crate HTTP/1.1\r\n");
            loop {
                line.clear();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
            }
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        });
        let observer = RegistryObserver::new(&format!("sparse+http://{address}/index/")).unwrap();
        let result = operation(&observer);
        worker.join().unwrap();
        result
    }

    #[test]
    fn first_release_requires_an_absent_registry_package() {
        for (status, body, expected) in [
            (404, String::new(), Ok(false)),
            (200, record("1.2.3", DIGEST, true), Ok(true)),
            (200, String::new(), Err("registry returned an empty index".to_string())),
            (503, String::new(), Err("registry index returned HTTP 503".to_string())),
        ] {
            assert_eq!(
                with_response(status, "", &body, |observer| observer
                    .has_published_versions("Example_Crate")),
                expected
            );
        }
    }

    #[test]
    fn index_observation_binds_exact_name_version_checksum_and_yank_state() {
        assert_eq!(observe_response(404, "", ""), Ok(None));
        assert_eq!(observe_response(200, "", &record("1.2.2", DIGEST, false)), Ok(None));
        assert_eq!(
            observe_response(200, "", &record("1.2.3", DIGEST, false)),
            Ok(Some((DIGEST.into(), false)))
        );
        assert_eq!(
            observe_response(200, "", &record("1.2.3", DIGEST, true)),
            Ok(Some((DIGEST.into(), true)))
        );
        let other = "a".repeat(64);
        assert_eq!(
            observe_response(200, "", &record("1.2.3", &other, false)),
            Ok(Some((other, false)))
        );
        assert_eq!(
            observe_response(200, "", &record("1.2.3+different", DIGEST, false)),
            Ok(Some((DIGEST.into(), false))),
            "build metadata does not make a different version"
        );
    }

    #[test]
    fn index_observation_rejects_ambiguous_or_incomplete_remote_evidence() {
        let good = record("1.2.3", DIGEST, false);
        for body in [
            String::new(),
            "\n\n".into(),
            "not JSON".into(),
            good.replace("Example_Crate", "another_crate"),
            good.replace("\"v\":2", "\"v\":3"),
            good.replace(DIGEST, "bad"),
            good.replace("\"yanked\":false", "\"yanked\":null"),
            good.replace("\"yanked\":false", "\"yanked\":false,\"yanked\":true"),
            format!("{good}\n{good}"),
            format!("{good}\n{}", record("1.2.3+duplicate", DIGEST, false)),
            format!("{good}\n{{"),
        ] {
            assert!(observe_response(200, "", &body).is_err(), "accepted {body}");
        }
    }

    #[test]
    fn index_observation_does_not_treat_http_failures_redirects_or_large_bodies_as_absence() {
        for status in [301, 302, 304, 401, 403, 429, 500, 503] {
            assert_eq!(
                observe_response(status, "Location: http://127.0.0.1:1/\r\n", ""),
                Err(format!("registry index returned HTTP {status}"))
            );
        }
        assert_eq!(
            observe_response(200, "Content-Length: 16777217\r\n", ""),
            Err("registry index exceeds the observation size limit".into())
        );
        assert_eq!(
            observe_response(200, "Content-Length: 512\r\n", "{"),
            Err("registry index body was incomplete or timed out".into())
        );
    }

    #[test]
    fn index_observation_rejects_unsafe_destinations_and_package_paths() {
        for index in [
            "http://example.invalid/",
            "https://example.invalid",
            "https://example.invalid/?token=x",
            "https://user:secret@example.invalid/",
            "https://example.invalid/#fragment",
            "https://example.invalid/a/../",
            "https://example.invalid/\n",
            "file:///tmp/index/",
        ] {
            let error = RegistryObserver::new(index).unwrap_err();
            assert!(error.to_string().contains("release"), "{error}");
            assert!(!error.to_string().contains("secret"), "{error}");
        }
        let observer = RegistryObserver::new("https://example.invalid/").unwrap();
        for name in ["../escape", "a/b", ".", "", "α", "name?query", "name#fragment"] {
            assert_eq!(
                observer.record(name, &semver::Version::new(1, 2, 3)),
                Err("invalid package name".into())
            );
        }
    }

    #[test]
    fn index_observation_deadline_includes_a_trickling_response_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut stream = std::io::BufReader::new(stream);
            loop {
                let mut line = String::new();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{")
                .unwrap();
            // Keep every individual read active beyond the whole-request deadline.
            // The observer must cancel before this incomplete JSON reaches EOF.
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(10));
                if stream.get_mut().write_all(b" ").is_err() {
                    break;
                }
            }
        });
        let observation =
            RegistryObserver::with_request_timeout(&format!("http://{address}/index/"), Duration::from_millis(200))
                .unwrap()
                .record("Example_Crate", &semver::Version::new(1, 2, 3));
        worker.join().unwrap();
        assert_eq!(
            observation,
            Err("registry index body was incomplete or timed out".into())
        );
    }
}
