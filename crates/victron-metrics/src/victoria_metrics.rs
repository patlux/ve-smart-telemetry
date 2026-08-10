//! Async VictoriaMetrics import client.
//!
//! POSTs Prometheus text batches to
//! `POST {base}/api/v1/import/prometheus` over plaintext HTTP/1.1 on
//! `tokio::net::TcpStream`. There is no TLS, no HTTP library, and no listener
//! anywhere in this crate — the Pi only ever makes outbound connections.
//!
//! Outcomes are classified into [`Outcome::Success`], [`Outcome::Retryable`]
//! (network errors, timeouts, 408/429/5xx), and [`Outcome::Permanent`]
//! (other 4xx). Retry scheduling and backoff belong to the service layer, not
//! here.
//!
//! Interim 1xx responses (e.g. `100 Continue`) are informational, never final:
//! the client consumes a bounded number of them and keeps reading until the
//! final response. `101 Switching Protocols` is treated as unsupported/
//! malformed instead of waiting for an ordinary final HTTP response.

use std::fmt;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::response::RawResponse;

#[path = "http_url.rs"]
mod http_url;
use http_url::{build_request, parse_http_url};

/// Default connect timeout.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Default read timeout.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Cap on bytes read from an error response body (diagnostics only).
pub const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
/// Cap on header bytes read.
pub(crate) const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Cap on interim 1xx responses consumed before the final response. A peer
/// that keeps answering 1xx beyond this bound is treated as malformed.
pub const MAX_INTERIM_RESPONSES: usize = 8;
/// Cap on characters kept from a failure message.
pub const MAX_MESSAGE_CHARS: usize = 300;
/// Default import path used when the configured URL has no path.
pub const DEFAULT_IMPORT_PATH: &str = "/api/v1/import/prometheus";

/// Category of a failed import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCategory {
    /// TCP connect refused/reset or socket I/O failure.
    Network,
    /// A deadline was exceeded (connect, read, or overall).
    Timeout,
    /// The server answered with an HTTP status code.
    Http,
    /// The peer did not speak HTTP or the response could not be parsed.
    MalformedResponse,
}

/// A failed import with a bounded diagnostic message.
///
/// The message never contains sensitive payloads: it holds the HTTP status,
/// a short reason phrase, and at most [`MAX_MESSAGE_CHARS`] of the error body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    /// Failure category.
    pub category: FailureCategory,
    /// HTTP status code when the server answered (`None` otherwise).
    pub status: Option<u16>,
    /// Bounded, non-sensitive diagnostic message.
    pub message: String,
}

impl ImportFailure {
    fn new(category: FailureCategory, status: Option<u16>, message: impl Into<String>) -> Self {
        ImportFailure {
            category,
            status,
            message: message.into(),
        }
    }
}

impl fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} import failed{}: {}",
            self.category,
            self.status
                .map(|s| format!(" (HTTP {s})"))
                .unwrap_or_default(),
            self.message
        )
    }
}

/// Result of one import attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The batch was accepted (HTTP 2xx).
    Success,
    /// The batch may succeed on a later attempt (network/timeout/408/429/5xx).
    Retryable(ImportFailure),
    /// The batch can never succeed as-is (other 4xx, e.g. 400 malformed lines).
    Permanent(ImportFailure),
}

impl Outcome {
    /// True when the batch was accepted.
    pub fn is_success(&self) -> bool {
        matches!(self, Outcome::Success)
    }

    /// True when the batch should be retried (retryable failures only).
    pub fn should_retry(&self) -> bool {
        matches!(self, Outcome::Retryable(_))
    }
}

/// Error raised while constructing the client (invalid URL, unsupported
/// scheme). These are configuration/programming errors, not delivery outcomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientConfigError {
    /// Only `http://` is supported (TLS is intentionally not compiled in).
    UnsupportedScheme(String),
    /// The URL had no usable host.
    MissingHost,
    /// The port could not be parsed.
    InvalidPort(String),
    /// Host contains characters this minimal client cannot route (e.g. IPv6).
    UnsupportedHost(String),
    /// Host violates the IPv4/DNS charset (whitespace, control characters,
    /// userinfo `@`, brackets, …).
    InvalidHost(String),
    /// Path contains bytes that must not appear on an HTTP request line
    /// (control characters, whitespace, `"` `<>\\^`{|}`, `#`, `?`, …) or
    /// does not start with `/`.
    InvalidPath(String),
    /// The URL contains a query (`?`) or fragment (`#`), which the import
    /// client never sends.
    QueryOrFragment(String),
}

impl fmt::Display for ClientConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientConfigError::UnsupportedScheme(s) => write!(
                f,
                "unsupported scheme {s:?}: only http:// is compiled in (no TLS for the internal endpoint)"
            ),
            ClientConfigError::MissingHost => write!(f, "URL has no host"),
            ClientConfigError::InvalidPort(p) => write!(f, "invalid port {p:?}"),
            ClientConfigError::UnsupportedHost(h) => {
                write!(f, "unsupported host {h:?} (IPv6/brackets not supported)")
            }
            ClientConfigError::InvalidHost(h) => write!(
                f,
                "invalid host {h:?}: must be a plain IPv4 address or DNS name without userinfo, whitespace, or control characters"
            ),
            ClientConfigError::InvalidPath(p) => write!(
                f,
                "invalid path {p:?}: must start with '/' and contain only safe request-line bytes (no control characters, whitespace, or reserved delimiters)"
            ),
            ClientConfigError::QueryOrFragment(u) => write!(
                f,
                "URL {u:?} contains a query or fragment; the import client never sends one"
            ),
        }
    }
}

impl std::error::Error for ClientConfigError {}

/// Minimal async HTTP/1.1 client for the VictoriaMetrics import endpoint.
///
/// One batch per request, `Connection: close`, explicit `Content-Length`.
/// Bounded error bodies and bounded overall deadline
/// (`connect_timeout + read_timeout`).
#[derive(Debug, Clone)]
pub struct VictoriaMetricsClient {
    host: String,
    port: u16,
    path: String,
    connect_timeout: Duration,
    read_timeout: Duration,
    max_error_body: usize,
}

impl VictoriaMetricsClient {
    /// Creates a client for a plaintext `http://host[:port][/path]` URL.
    ///
    /// When the URL has no path, [`DEFAULT_IMPORT_PATH`] is used.
    /// `https://` is rejected with a clear error: TLS is deliberately not part
    /// of the ARMv6 dependency graph for this internal endpoint.
    ///
    /// Only plain IPv4 or DNS hosts are accepted (no IPv6 brackets, no
    /// userinfo, no query/fragment); the host and path are validated so that
    /// neither the request line nor the `Host` header can carry control
    /// characters or whitespace.
    pub fn new(base_url: &str) -> Result<Self, ClientConfigError> {
        let (host, port, path) = parse_http_url(base_url)?;
        Ok(VictoriaMetricsClient {
            host,
            port,
            path,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            max_error_body: MAX_ERROR_BODY_BYTES,
        })
    }

    /// Overrides connect and read timeouts.
    pub fn with_timeouts(mut self, connect: Duration, read: Duration) -> Self {
        self.connect_timeout = connect;
        self.read_timeout = read;
        self
    }

    /// Overrides the cap on error-response body bytes kept for diagnostics.
    pub fn with_max_error_body(mut self, max: usize) -> Self {
        self.max_error_body = max;
        self
    }

    /// Target host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Target port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Request path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// POSTs one Prometheus text batch and classifies the outcome.
    ///
    /// The whole attempt is bounded by `connect_timeout + read_timeout`; every
    /// socket read is additionally bounded by `read_timeout`.
    pub async fn import(&self, body: &str) -> Outcome {
        let total = self.connect_timeout + self.read_timeout;
        match timeout(total, self.import_inner(body)).await {
            Ok(outcome) => outcome,
            Err(_) => Outcome::Retryable(ImportFailure::new(
                FailureCategory::Timeout,
                None,
                "overall deadline exceeded",
            )),
        }
    }

    async fn import_inner(&self, body: &str) -> Outcome {
        let connect = timeout(
            self.connect_timeout,
            TcpStream::connect((self.host.as_str(), self.port)),
        )
        .await;
        let mut stream = match connect {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return Outcome::Retryable(ImportFailure::new(
                    FailureCategory::Network,
                    None,
                    format!("connect failed: {e}"),
                ))
            }
            Err(_) => {
                return Outcome::Retryable(ImportFailure::new(
                    FailureCategory::Timeout,
                    None,
                    "connect timed out",
                ))
            }
        };

        let request = build_request(&self.host, self.port, &self.path, body);
        match timeout(self.read_timeout, stream.write_all(&request)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return Outcome::Retryable(ImportFailure::new(
                    FailureCategory::Network,
                    None,
                    format!("request write failed: {e}"),
                ))
            }
            Err(_) => {
                return Outcome::Retryable(ImportFailure::new(
                    FailureCategory::Timeout,
                    None,
                    "request write timed out",
                ))
            }
        }

        match self.read_response(&mut stream).await {
            Ok(raw) => self.classify(&raw),
            Err(failure) => Outcome::Retryable(failure),
        }
    }

    async fn read_response(&self, stream: &mut TcpStream) -> Result<RawResponse, ImportFailure> {
        use crate::response::{parse_headers, parse_status_line, ResponseReader};
        let mut reader = ResponseReader::new(stream, self.read_timeout);
        let mut interim = 0usize;
        loop {
            if interim > MAX_INTERIM_RESPONSES {
                return Err(ImportFailure::new(
                    FailureCategory::MalformedResponse,
                    None,
                    format!("too many interim 1xx responses (>{MAX_INTERIM_RESPONSES})"),
                ));
            }
            let head = reader
                .read_head(MAX_HEADER_BYTES)
                .await
                .map_err(|(cat, msg)| ImportFailure::new(cat, None, msg))?;

            let (status_line, header_block) = head;
            let status = parse_status_line(&status_line).ok_or_else(|| {
                ImportFailure::new(
                    FailureCategory::MalformedResponse,
                    None,
                    format!("unparseable status line: {}", truncate(&status_line, 80)),
                )
            })?;
            let (code, reason) = status;

            if (100..=199).contains(&code) {
                // Informational responses are interim, not final: consume the
                // head (1xx never carries a body) and keep reading. 101 is
                // special: the peer switched protocols, so no ordinary final
                // HTTP response will follow — treat it as unsupported.
                if code == 101 {
                    return Err(ImportFailure::new(
                        FailureCategory::MalformedResponse,
                        Some(101),
                        "HTTP 101 Switching Protocols is not supported",
                    ));
                }
                interim += 1;
                continue;
            }

            let headers = parse_headers(&header_block);
            let body = reader
                .read_body(code, &headers, self.max_error_body)
                .await
                .map_err(|(cat, msg)| ImportFailure::new(cat, Some(code), msg))?;

            return Ok(RawResponse {
                status: code,
                reason,
                body,
            });
        }
    }

    fn classify(&self, raw: &RawResponse) -> Outcome {
        let message = format!(
            "HTTP {} {}{}",
            raw.status,
            raw.reason,
            if raw.body.is_empty() {
                String::new()
            } else {
                format!(": {}", truncate_lossy(&raw.body, MAX_MESSAGE_CHARS))
            }
        );
        let failure = ImportFailure::new(FailureCategory::Http, Some(raw.status), message);
        match raw.status {
            200..=299 => Outcome::Success,
            408 | 429 | 500..=599 => Outcome::Retryable(failure),
            _ => Outcome::Permanent(failure),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let mut t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        t.push('…');
    }
    t
}

fn truncate_lossy(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    truncate(&s, max)
}

#[cfg(test)]
#[path = "victoria_metrics_tests.rs"]
mod tests;
