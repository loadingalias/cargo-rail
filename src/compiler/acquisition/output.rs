//! Bounded streaming readers for Cargo's child output.

use cargo_metadata::Message;
use serde::Deserialize as _;
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufRead, Read};

pub(crate) const MAX_CARGO_JSON_LINE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_CARGO_MESSAGES: usize = 262_144;
pub(crate) const MAX_RETAINED_CARGO_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_RETAINED_STDERR_BYTES: usize = 16 * 1024;
const MAX_INVALID_LINE_EXCERPT_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CargoStreamFailureClass {
    Read,
    TruncatedJson,
    OversizedLine,
    MalformedMessage,
    MessageLimit,
    RetainedByteLimit,
}

impl fmt::Display for CargoStreamFailureClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Read => "reader failure",
            Self::TruncatedJson => "truncated JSON",
            Self::OversizedLine => "oversized line",
            Self::MalformedMessage => "unexpected message shape",
            Self::MessageLimit => "message-count limit",
            Self::RetainedByteLimit => "retained-byte limit",
        })
    }
}

#[derive(Debug)]
pub(crate) struct CargoStreamError {
    class: CargoStreamFailureClass,
    detail: String,
}

impl CargoStreamError {
    fn new(class: CargoStreamFailureClass, detail: impl Into<String>) -> Self {
        Self {
            class,
            detail: detail.into(),
        }
    }

    pub(crate) const fn class(&self) -> CargoStreamFailureClass {
        self.class
    }
}

impl fmt::Display for CargoStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.class, self.detail)
    }
}

impl std::error::Error for CargoStreamError {}

#[derive(Debug)]
pub(crate) struct CargoStdout {
    retained: Vec<u8>,
    bytes_read: u64,
    messages_read: usize,
}

impl CargoStdout {
    pub(crate) fn retained(&self) -> &[u8] {
        &self.retained
    }

    pub(crate) const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub(crate) const fn retained_bytes(&self) -> usize {
        self.retained.len()
    }

    pub(crate) const fn messages_read(&self) -> usize {
        self.messages_read
    }
}

#[derive(Debug)]
pub(crate) struct CargoStderr {
    tail: Vec<u8>,
    bytes_read: u64,
}

impl CargoStderr {
    pub(crate) fn tail(&self) -> &[u8] {
        &self.tail
    }

    pub(crate) const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub(crate) const fn retained_bytes(&self) -> usize {
        self.tail.len()
    }
}

pub(crate) fn read_cargo_stdout(reader: impl Read) -> Result<CargoStdout, CargoStreamError> {
    read_cargo_stdout_with_limits(
        reader,
        MAX_CARGO_JSON_LINE_BYTES,
        MAX_CARGO_MESSAGES,
        MAX_RETAINED_CARGO_BYTES,
    )
}

fn read_cargo_stdout_with_limits(
    reader: impl Read,
    max_line_bytes: usize,
    max_messages: usize,
    max_retained_bytes: usize,
) -> Result<CargoStdout, CargoStreamError> {
    let mut reader = io::BufReader::new(reader);
    let mut line = Vec::new();
    let mut retained = Vec::new();
    let mut bytes_read = 0_u64;
    let mut messages_read = 0usize;

    loop {
        line.clear();
        let Some(line_bytes) = read_bounded_line(&mut reader, &mut line, max_line_bytes)? else {
            break;
        };
        bytes_read = bytes_read.checked_add(line_bytes).ok_or_else(|| {
            CargoStreamError::new(CargoStreamFailureClass::Read, "Cargo stdout byte count overflowed")
        })?;
        messages_read = messages_read.checked_add(1).ok_or_else(|| {
            CargoStreamError::new(CargoStreamFailureClass::MessageLimit, "Cargo message count overflowed")
        })?;
        if messages_read > max_messages {
            return Err(CargoStreamError::new(
                CargoStreamFailureClass::MessageLimit,
                format!("Cargo emitted more than {max_messages} bounded output lines"),
            ));
        }

        let payload = line
            .strip_suffix(b"\n")
            .ok_or_else(|| CargoStreamError::new(CargoStreamFailureClass::TruncatedJson, invalid_excerpt(&line)))?;
        // Cargo forwards test and rustc_driver stdout beside its JSON message
        // stream. Preserve cargo_metadata's text-line contract without giving
        // malformed JSON a text fallback: every line is bounded and counted,
        // valid UTF-8 text is discarded, and object-shaped lines must decode
        // as Cargo messages.
        if payload.iter().copied().find(|byte| !byte.is_ascii_whitespace()) != Some(b'{') {
            std::str::from_utf8(payload).map_err(|error| {
                CargoStreamError::new(
                    CargoStreamFailureClass::MalformedMessage,
                    format!(
                        "non-UTF-8 Cargo text output: {error}; excerpt={}",
                        invalid_excerpt(payload)
                    ),
                )
            })?;
            continue;
        }
        let mut deserializer = serde_json::Deserializer::from_slice(payload);
        let message = Message::deserialize(&mut deserializer).map_err(|error| {
            CargoStreamError::new(
                CargoStreamFailureClass::MalformedMessage,
                format!("{error}; excerpt={}", invalid_excerpt(payload)),
            )
        })?;
        if !deserializer.end().is_ok() {
            return Err(CargoStreamError::new(
                CargoStreamFailureClass::MalformedMessage,
                format!("trailing data; excerpt={}", invalid_excerpt(payload)),
            ));
        }

        if matches!(
            message,
            Message::CompilerArtifact(_) | Message::CompilerMessage(_) | Message::BuildScriptExecuted(_)
        ) {
            let next = retained.len().checked_add(line.len()).ok_or_else(|| {
                CargoStreamError::new(
                    CargoStreamFailureClass::RetainedByteLimit,
                    "retained Cargo output byte count overflowed",
                )
            })?;
            if next > max_retained_bytes {
                return Err(CargoStreamError::new(
                    CargoStreamFailureClass::RetainedByteLimit,
                    format!("required Cargo messages exceeded the {max_retained_bytes}-byte retention bound"),
                ));
            }
            retained.extend_from_slice(&line);
        }
    }

    Ok(CargoStdout {
        retained,
        bytes_read,
        messages_read,
    })
}

pub(crate) fn read_cargo_stderr_tail(reader: impl Read) -> io::Result<CargoStderr> {
    read_cargo_stderr_tail_with_limit(reader, MAX_RETAINED_STDERR_BYTES)
}

fn read_cargo_stderr_tail_with_limit(mut reader: impl Read, limit: usize) -> io::Result<CargoStderr> {
    let mut tail = VecDeque::with_capacity(limit);
    let mut chunk = [0_u8; 8192];
    let mut bytes_read = 0_u64;
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(u64::try_from(read).map_err(|_| io::Error::other("stderr read size exceeds u64"))?)
            .ok_or_else(|| io::Error::other("Cargo stderr byte count overflowed"))?;
        if limit == 0 {
            continue;
        }
        if read >= limit {
            tail.clear();
            tail.extend(&chunk[read - limit..read]);
            continue;
        }
        let excess = tail.len().saturating_add(read).saturating_sub(limit);
        tail.drain(..excess);
        tail.extend(&chunk[..read]);
    }
    Ok(CargoStderr {
        tail: tail.into_iter().collect(),
        bytes_read,
    })
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    max_line_bytes: usize,
) -> Result<Option<u64>, CargoStreamError> {
    let mut bytes_read = 0_u64;
    loop {
        let available = reader.fill_buf().map_err(|error| {
            CargoStreamError::new(CargoStreamFailureClass::Read, format!("reading Cargo stdout: {error}"))
        })?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(CargoStreamError::new(
                    CargoStreamFailureClass::TruncatedJson,
                    invalid_excerpt(line),
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        bytes_read = bytes_read
            .checked_add(
                u64::try_from(take).map_err(|_| {
                    CargoStreamError::new(CargoStreamFailureClass::Read, "Cargo stdout chunk exceeds u64")
                })?,
            )
            .ok_or_else(|| CargoStreamError::new(CargoStreamFailureClass::Read, "Cargo line byte count overflowed"))?;
        if line.len().saturating_add(take) > max_line_bytes {
            reader.consume(take);
            drain_line(reader)?;
            return Err(CargoStreamError::new(
                CargoStreamFailureClass::OversizedLine,
                format!("Cargo JSON line exceeded the {max_line_bytes}-byte bound"),
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(Some(bytes_read));
        }
    }
}

fn drain_line(reader: &mut impl BufRead) -> Result<(), CargoStreamError> {
    loop {
        let available = reader.fill_buf().map_err(|error| {
            CargoStreamError::new(CargoStreamFailureClass::Read, format!("draining Cargo stdout: {error}"))
        })?;
        if available.is_empty() {
            return Ok(());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |position| position + 1);
        reader.consume(take);
        if newline.is_some() {
            return Ok(());
        }
    }
}

fn invalid_excerpt(bytes: &[u8]) -> String {
    let end = bytes.len().min(MAX_INVALID_LINE_EXCERPT_BYTES);
    let mut excerpt = String::from_utf8_lossy(&bytes[..end]).into_owned();
    excerpt = excerpt.replace(['\r', '\n'], " ");
    if end < bytes.len() {
        excerpt.push('…');
    }
    format!("{excerpt:?}")
}

#[cfg(test)]
mod tests {
    use super::{CargoStreamFailureClass, read_cargo_stderr_tail_with_limit, read_cargo_stdout_with_limits};

    const ARTIFACT: &str = r#"{"reason":"compiler-artifact","package_id":"path+file:///unit#0.1.0","manifest_path":"/unit/Cargo.toml","target":{"kind":["lib"],"crate_types":["lib"],"name":"unit","src_path":"/unit/src/lib.rs","edition":"2024","doc":true,"doctest":true,"test":true},"profile":{"opt_level":"0","debuginfo":0,"debug_assertions":true,"overflow_checks":true,"test":false},"features":[],"filenames":[],"executable":null,"fresh":false}"#;

    #[test]
    fn stream_retains_only_messages_needed_by_acquisition() {
        let input = format!("{{\"reason\":\"build-finished\",\"success\":true}}\n{ARTIFACT}\n");
        let output = read_cargo_stdout_with_limits(input.as_bytes(), 4096, 4, 4096).expect("bounded stream");
        assert_eq!(output.messages_read(), 2);
        assert_eq!(output.bytes_read(), u64::try_from(input.len()).unwrap());
        assert_eq!(output.retained(), format!("{ARTIFACT}\n").as_bytes());
    }

    #[test]
    fn stream_counts_and_discards_bounded_compiler_text_lines() {
        let input = format!("\n\r\nrunning 2 tests\n{ARTIFACT}\n");
        let output = read_cargo_stdout_with_limits(input.as_bytes(), 4096, 4, 4096).expect("bounded stream");
        assert_eq!(output.messages_read(), 4);
        assert_eq!(output.bytes_read(), u64::try_from(input.len()).unwrap());
        assert_eq!(output.retained(), format!("{ARTIFACT}\n").as_bytes());

        let invalid =
            read_cargo_stdout_with_limits(b"\xff\n" as &[u8], 16, 1, 16).expect_err("non-UTF-8 compiler stdout");
        assert_eq!(invalid.class(), CargoStreamFailureClass::MalformedMessage);
    }

    #[test]
    fn stream_classifies_truncated_malformed_and_oversized_json() {
        let truncated = read_cargo_stdout_with_limits(br#"{"reason":"build-finished"# as &[u8], 128, 4, 128)
            .expect_err("unterminated line");
        assert_eq!(truncated.class(), CargoStreamFailureClass::TruncatedJson);

        let malformed =
            read_cargo_stdout_with_limits(b"{\"reason\":17}\n" as &[u8], 128, 4, 128).expect_err("unexpected message");
        assert_eq!(malformed.class(), CargoStreamFailureClass::MalformedMessage);

        let oversized = read_cargo_stdout_with_limits(b"{\"reason\":\"build-finished\"}\n" as &[u8], 8, 4, 128)
            .expect_err("oversized line");
        assert_eq!(oversized.class(), CargoStreamFailureClass::OversizedLine);
    }

    #[test]
    fn stream_enforces_message_and_required_retention_bounds() {
        let messages =
            b"{\"reason\":\"build-finished\",\"success\":true}\n{\"reason\":\"build-finished\",\"success\":true}\n";
        let message_limit =
            read_cargo_stdout_with_limits(messages as &[u8], 128, 1, 128).expect_err("message-count limit");
        assert_eq!(message_limit.class(), CargoStreamFailureClass::MessageLimit);

        let artifact = format!("{ARTIFACT}\n");
        let retained_limit = read_cargo_stdout_with_limits(artifact.as_bytes(), 4096, 1, artifact.len() - 1)
            .expect_err("required-retention limit");
        assert_eq!(retained_limit.class(), CargoStreamFailureClass::RetainedByteLimit);
    }

    #[test]
    fn stderr_reader_retains_only_the_configured_tail() {
        let stderr = read_cargo_stderr_tail_with_limit(b"0123456789" as &[u8], 4).expect("stderr tail");
        assert_eq!(stderr.bytes_read(), 10);
        assert_eq!(stderr.tail(), b"6789");
    }
}
