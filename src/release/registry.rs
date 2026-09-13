//! Bounded, checksum-bound observation of a Cargo sparse registry.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use crate::error::{RailError, RailResult};

const MAX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

/// The observation is evidence about the index, not proof that an earlier
/// upload was rejected. An absent version after an attempt remains uncertain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RegistryObservation {
    Absent,
    Matching,
    Conflicting { checksum: String, yanked: bool },
    Unavailable { reason: String },
}

#[derive(Debug)]
pub(crate) struct RegistryObserver {
    index: reqwest::Url,
    client: Client,
}

impl RegistryObserver {
    pub(crate) fn new(index: &str) -> RailResult<Self> {
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
        Ok(Self { index, client })
    }

    pub(crate) fn observe(&self, name: &str, version: &str, checksum: &str) -> RegistryObservation {
        match self.read(name, version, checksum) {
            Ok(observation) => observation,
            Err(reason) => RegistryObservation::Unavailable { reason },
        }
    }

    fn read(&self, name: &str, version: &str, checksum: &str) -> Result<RegistryObservation, String> {
        if !valid_name(name) || !valid_checksum(checksum) || semver::Version::parse(version).is_err() {
            return Err("invalid sealed package identity".into());
        }
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
            .timeout(Duration::from_secs(30))
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .map_err(|_| "registry index request failed or timed out")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(RegistryObservation::Absent);
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
        observe_records(&bytes, name, version, checksum)
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn valid_checksum(checksum: &str) -> bool {
    checksum.len() == 64
        && checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

fn observe_records(bytes: &[u8], name: &str, version: &str, checksum: &str) -> Result<RegistryObservation, String> {
    let mut observation = RegistryObservation::Absent;
    let mut versions = BTreeSet::new();
    let wanted = semver::Version::parse(version).map_err(|_| "invalid sealed version")?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err("registry returned an empty index".into());
    }
    for line in bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let record: IndexRecord =
            serde_json::from_slice(line).map_err(|_| "registry returned an invalid index record")?;
        let mut parsed = semver::Version::parse(&record.vers).map_err(|_| "registry returned an invalid version")?;
        parsed.build = semver::BuildMetadata::EMPTY;
        if record.name != name || !valid_checksum(&record.cksum) || !versions.insert(parsed.clone()) {
            return Err("registry returned ambiguous package identities or checksums".into());
        }
        if record.v.is_some_and(|v| !matches!(v, 1 | 2)) {
            return Err("registry returned an unsupported index record version".into());
        }
        let mut wanted = wanted.clone();
        wanted.build = semver::BuildMetadata::EMPTY;
        if parsed == wanted {
            observation = if record.vers == version && record.cksum == checksum && !record.yanked {
                RegistryObservation::Matching
            } else {
                RegistryObservation::Conflicting {
                    checksum: record.cksum,
                    yanked: record.yanked,
                }
            };
        }
    }
    Ok(observation)
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

    fn observe_response(status: u16, headers: &str, body: &str) -> RegistryObservation {
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
        let result = RegistryObserver::new(&format!("sparse+http://{address}/index/"))
            .unwrap()
            .observe("Example_Crate", "1.2.3", DIGEST);
        worker.join().unwrap();
        result
    }

    #[test]
    fn index_observation_binds_exact_name_version_checksum_and_yank_state() {
        assert_eq!(observe_response(404, "", ""), RegistryObservation::Absent);
        assert_eq!(
            observe_response(200, "", &record("1.2.2", DIGEST, false)),
            RegistryObservation::Absent
        );
        assert_eq!(
            observe_response(200, "", &record("1.2.3", DIGEST, false)),
            RegistryObservation::Matching
        );
        assert_eq!(
            observe_response(200, "", &record("1.2.3", DIGEST, true)),
            RegistryObservation::Conflicting {
                checksum: DIGEST.into(),
                yanked: true
            }
        );
        let other = "a".repeat(64);
        assert_eq!(
            observe_response(200, "", &record("1.2.3", &other, false)),
            RegistryObservation::Conflicting {
                checksum: other,
                yanked: false
            }
        );
        assert_eq!(
            observe_response(200, "", &record("1.2.3+different", DIGEST, false)),
            RegistryObservation::Conflicting {
                checksum: DIGEST.into(),
                yanked: false
            }
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
            assert!(
                matches!(
                    observe_response(200, "", &body),
                    RegistryObservation::Unavailable { .. }
                ),
                "accepted {body}"
            );
        }
    }

    #[test]
    fn index_observation_does_not_treat_http_failures_redirects_or_large_bodies_as_absence() {
        for status in [301, 302, 304, 401, 403, 429, 500, 503] {
            assert_eq!(
                observe_response(status, "Location: http://127.0.0.1:1/\r\n", ""),
                RegistryObservation::Unavailable {
                    reason: format!("registry index returned HTTP {status}")
                }
            );
        }
        assert_eq!(
            observe_response(200, "Content-Length: 16777217\r\n", ""),
            RegistryObservation::Unavailable {
                reason: "registry index exceeds the observation size limit".into()
            }
        );
        assert_eq!(
            observe_response(200, "Content-Length: 512\r\n", "{"),
            RegistryObservation::Unavailable {
                reason: "registry index body was incomplete or timed out".into()
            }
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
                observer.observe(name, "1.2.3", DIGEST),
                RegistryObservation::Unavailable {
                    reason: "invalid sealed package identity".into()
                }
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
            for _ in 0..350 {
                std::thread::sleep(Duration::from_millis(100));
                if stream.get_mut().write_all(b" ").is_err() {
                    break;
                }
            }
        });
        let observation = RegistryObserver::new(&format!("http://{address}/index/"))
            .unwrap()
            .observe("Example_Crate", "1.2.3", DIGEST);
        worker.join().unwrap();
        assert_eq!(
            observation,
            RegistryObservation::Unavailable {
                reason: "registry index body was incomplete or timed out".into(),
            }
        );
    }
}
