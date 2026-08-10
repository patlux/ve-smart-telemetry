//! HTTP/1.1 response reading for the VictoriaMetrics import client.
//!
//! Reads a status line + header block, then a body that is either
//! content-length delimited, chunked, or close-delimited. Interim 1xx
//! responses are consumed by the caller (bounded) before the final response's
//! body is read; 204 and 304 never carry a body and are returned as empty
//! immediately, without waiting for peer close. All reads are bounded (header
//! cap, error-body cap, per-read timeout) so a misbehaving peer cannot exhaust
//! memory or stall the collector.

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

use std::time::Duration;

use crate::victoria_metrics::FailureCategory;

/// Parsed response: status code, reason phrase, and body bytes (bounded).
pub(crate) struct RawResponse {
    pub(crate) status: u16,
    pub(crate) reason: String,
    pub(crate) body: Vec<u8>,
}

/// Reads an HTTP/1.1 head (status line + headers) then a body that is either
/// chunked, content-length delimited, or close-delimited.
pub(crate) struct ResponseReader<'a> {
    stream: &'a mut TcpStream,
    read_timeout: Duration,
    buffer: Vec<u8>,
}

impl<'a> ResponseReader<'a> {
    pub(crate) fn new(stream: &'a mut TcpStream, read_timeout: Duration) -> Self {
        ResponseReader {
            stream,
            read_timeout,
            buffer: Vec::with_capacity(1024),
        }
    }

    /// Reads until `\r\n\r\n`, returning (status line, header block).
    pub(crate) async fn read_head(
        &mut self,
        max: usize,
    ) -> Result<(String, String), (FailureCategory, String)> {
        let end = loop {
            if let Some(pos) = find_double_crlf(&self.buffer) {
                break pos;
            }
            if self.buffer.len() > max {
                return Err((
                    FailureCategory::MalformedResponse,
                    "response headers too large".into(),
                ));
            }
            self.read_more(512).await?;
        };
        let head = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
        self.buffer.drain(..end + 4);
        let (status_line, rest) = head.split_once("\r\n").unwrap_or((&head, ""));
        Ok((status_line.to_owned(), rest.to_owned()))
    }

    /// Reads the response body according to HTTP/1.1 body rules, bounded.
    ///
    /// Statuses that never carry a body — 204 No Content and 304 Not Modified
    /// (RFC 9112 §6.3) — return an empty body without waiting for peer close,
    /// even when no `Content-Length` is present. Interim 1xx responses never
    /// reach this method: the caller consumes them before reading the final
    /// response's body.
    pub(crate) async fn read_body(
        &mut self,
        code: u16,
        headers: &[(String, String)],
        max: usize,
    ) -> Result<Vec<u8>, (FailureCategory, String)> {
        if code == 204 || code == 304 {
            return Ok(Vec::new());
        }
        let chunked = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")
        });
        if chunked {
            return self.read_chunked(max).await;
        }
        let content_length = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok());
        match content_length {
            Some(n) => {
                let take = n.min(max + 1);
                while self.buffer.len() < take {
                    self.read_more(4096).await?;
                }
                let mut body = self.buffer.drain(..take).collect::<Vec<u8>>();
                body.truncate(max);
                Ok(body)
            }
            None => {
                // Close-delimited body: read until EOF (bounded).
                let mut body = self.buffer.drain(..).collect::<Vec<u8>>();
                loop {
                    if body.len() > max {
                        break;
                    }
                    let mut chunk = [0u8; 4096];
                    match self.timed_read(&mut chunk).await? {
                        0 => break,
                        n => body.extend_from_slice(&chunk[..n]),
                    }
                }
                body.truncate(max);
                Ok(body)
            }
        }
    }

    async fn read_chunked(&mut self, max: usize) -> Result<Vec<u8>, (FailureCategory, String)> {
        let mut body = Vec::new();
        loop {
            let line = self.read_line(128).await?;
            let size_str = line.split(';').next().unwrap_or("").trim();
            let size = usize::from_str_radix(size_str, 16).map_err(|_| {
                (
                    FailureCategory::MalformedResponse,
                    format!("bad chunk size {line:?}"),
                )
            })?;
            if size == 0 {
                // Consume trailers up to the final CRLF.
                loop {
                    let t = self.read_line(128).await?;
                    if t.is_empty() {
                        break;
                    }
                }
                break;
            }
            let take = size.min(max + 1 - body.len());
            while self.buffer.len() < take {
                self.read_more(4096).await?;
            }
            body.extend_from_slice(&self.buffer.drain(..take).collect::<Vec<u8>>());
            if body.len() > max {
                break;
            }
            // Consume the CRLF after chunk data.
            let crlf = self.read_exact(2).await?;
            if crlf != b"\r\n" {
                return Err((
                    FailureCategory::MalformedResponse,
                    "missing CRLF after chunk".into(),
                ));
            }
        }
        body.truncate(max);
        Ok(body)
    }

    async fn read_line(&mut self, max: usize) -> Result<String, (FailureCategory, String)> {
        loop {
            if let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
                let mut line = self.buffer.drain(..=pos).collect::<Vec<u8>>();
                while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            if self.buffer.len() > max {
                return Err((FailureCategory::MalformedResponse, "line too long".into()));
            }
            self.read_more(256).await?;
        }
    }

    async fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, (FailureCategory, String)> {
        while self.buffer.len() < n {
            self.read_more(n).await?;
        }
        Ok(self.buffer.drain(..n).collect())
    }

    async fn read_more(&mut self, n: usize) -> Result<(), (FailureCategory, String)> {
        let mut chunk = vec![0u8; n.max(256)];
        let read = match self.timed_read(&mut chunk).await? {
            0 => {
                return Err((
                    FailureCategory::Network,
                    "connection closed prematurely".into(),
                ))
            }
            n => n,
        };
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(())
    }

    async fn timed_read(&mut self, buf: &mut [u8]) -> Result<usize, (FailureCategory, String)> {
        match timeout(self.read_timeout, self.stream.read(buf)).await {
            Ok(Ok(n)) => Ok(n),
            Ok(Err(e)) => Err((FailureCategory::Network, format!("read error: {e}"))),
            Err(_) => Err((FailureCategory::Timeout, "read timed out".into())),
        }
    }
}

pub(crate) fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

pub(crate) fn parse_status_line(line: &str) -> Option<(u16, String)> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let code = parts.next()?.parse::<u16>().ok()?;
    let reason = parts.next().unwrap_or("").to_owned();
    Some((code, reason))
}

pub(crate) fn parse_headers(block: &str) -> Vec<(String, String)> {
    block
        .split("\r\n")
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_status_parsing() {
        assert_eq!(
            parse_status_line("HTTP/1.1 204 No Content"),
            Some((204, "No Content".into()))
        );
        assert_eq!(parse_status_line("garbage"), None);
        let headers = parse_headers("Content-Length: 5\r\nTransfer-Encoding: chunked\r\n");
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn finds_double_crlf_only_at_boundary() {
        assert_eq!(find_double_crlf(b"a\r\nb\r\n\r\nc"), Some(4));
        assert_eq!(find_double_crlf(b"no boundary here"), None);
    }
}
