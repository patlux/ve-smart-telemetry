//! `victron-cli check-victoriametrics` — transport reachability probe.
//!
//! This command is deliberately a **transport probe only** (std-only, no
//! sibling crate needed): it parses the URL strictly, resolves the host and
//! opens a TCP connection to the configured port. It does **not** claim
//! import-path validation — full POST validation against
//! `/api/v1/import/prometheus` with retry classification awaits the sibling
//! `victron-metrics` crate and is reported as not wired.
//!
//! Only plaintext `http://` URLs are accepted (matching the `victron-metrics`
//! client): `https://`, userinfo, query/fragment, whitespace/control
//! injection and invalid ports/paths are rejected. The default target is
//! loopback; the production endpoint is never probed by default.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use clap::Args;

use crate::{runtime, CliError};

#[derive(Debug, Args)]
pub struct CheckVictoriaMetrics {
    /// VictoriaMetrics import endpoint (plaintext http:// only).
    #[arg(long, default_value = "http://127.0.0.1:8429/api/v1/import/prometheus")]
    pub url: String,

    /// Connect timeout in milliseconds.
    #[arg(long, default_value_t = 3000)]
    pub timeout_ms: u64,
}

/// Split a plaintext `http://` URL into (host, port, path). Std-only, keeps
/// the CLI dependency-free. Mirrors the `victron-metrics` client's URL rules:
/// no https, no userinfo, no query/fragment, no whitespace/control bytes,
/// valid non-zero port, absolute safe path.
pub fn parse_endpoint(url: &str) -> Result<(String, u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        format!("URL must be a plaintext http:// URL (https is not supported): {url}")
    })?;
    if rest.contains('?') || rest.contains('#') {
        return Err(format!("URL must not contain a query or fragment: {url}"));
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("empty host in URL: {url}"));
    }
    if authority.contains('@') {
        return Err(format!("userinfo is not supported: {url}"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            if h.is_empty() || h.contains(':') || h.starts_with('[') {
                return Err(format!("unsupported host in URL: {url}"));
            }
            let port: u16 = p
                .parse()
                .ok()
                .filter(|&p| p != 0)
                .ok_or_else(|| format!("invalid port in URL: {url}"))?;
            (h.to_string(), port)
        }
        None => (authority.to_string(), 80),
    };
    if !is_valid_host(&host) {
        return Err(format!(
            "invalid host in URL: {url} (must be a plain IPv4 address or DNS name without whitespace or control characters)"
        ));
    }
    if !is_valid_path(path) {
        return Err(format!(
            "invalid path in URL: {url} (must start with '/' and contain only safe request-line bytes)"
        ));
    }
    Ok((host, port, path.to_string()))
}

/// Charset check for the host part: plain IPv4 or DNS names only.
fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
}

/// Charset check for the request-target path: absolute (`/`-prefixed) and
/// free of any byte that could corrupt the request line.
fn is_valid_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    path.bytes()
        .all(|b| (0x21..=0x7e).contains(&b) && !b" \"<>\\^`{|}#?".contains(&b))
}

impl CheckVictoriaMetrics {
    pub fn run(&self) -> Result<(), CliError> {
        let (host, port, path) = parse_endpoint(&self.url).map_err(runtime)?;
        println!("transport probe: {host}:{port} (path {path})");

        let addr: SocketAddr = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| runtime(format!("DNS resolution failed for {host}: {e}")))?
            .next()
            .ok_or_else(|| runtime(format!("no address resolved for {host}")))?;

        let timeout = Duration::from_millis(self.timeout_ms);
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => {
                println!("reachable: {addr}");
                println!(
                    "note: transport OK. Import-path validation (POST /api/v1/import/prometheus, \
                     retry classification) is not wired yet: requires victron-metrics."
                );
                Ok(())
            }
            Err(e) => Err(runtime(format!(
                "unreachable: {addr} ({e}). Only outbound HTTP from the Pi to \
                 the configured VictoriaMetrics address is intended; no inbound listener."
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_target_is_loopback_not_production() {
        let cmd = CheckVictoriaMetrics {
            url: "http://127.0.0.1:8429/api/v1/import/prometheus".into(),
            timeout_ms: 100,
        };
        let (host, port, path) = parse_endpoint(&cmd.url).unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 8429);
        assert_eq!(path, "/api/v1/import/prometheus");
        assert!(
            !cmd.url.contains("100.64.0.2"),
            "the production endpoint must never be the default"
        );
    }

    #[test]
    fn parses_url_forms() {
        assert_eq!(
            parse_endpoint("http://100.64.0.2:8429/api/v1/import/prometheus").unwrap(),
            (
                "100.64.0.2".into(),
                8429,
                "/api/v1/import/prometheus".into()
            )
        );
        assert_eq!(
            parse_endpoint("http://metrics.example.com").unwrap(),
            ("metrics.example.com".into(), 80, "/".into())
        );
        assert_eq!(
            parse_endpoint("http://localhost:8429/").unwrap(),
            ("localhost".into(), 8429, "/".into())
        );
    }

    #[test]
    fn rejects_https_and_non_http_schemes() {
        for bad in [
            "https://metrics.example.com",
            "ftp://metrics.example.com",
            "100.64.0.2:8429",
        ] {
            assert!(parse_endpoint(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn rejects_userinfo_query_fragment_and_injection() {
        for bad in [
            "http://user:pass@127.0.0.1:8429/",
            "http://127.0.0.1:8429/api?x=1",
            "http://127.0.0.1:8429/api#frag",
            "http://127.0.0.1:8429/api/v1/import/prometheus\n",
            "http://127.0.0.1:8429/api/v1/import/prometheus ",
            "http://[::1]:8429/",
            "http://:8429",
            "http://host:notaport",
            "http://host:0/",
            "http://127.0.0.1:8429/api/v1/import/prometheus<",
        ] {
            assert!(parse_endpoint(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn tcp_probe_succeeds_against_local_listener() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let cmd = CheckVictoriaMetrics {
            url: format!("http://127.0.0.1:{port}/api/v1/import/prometheus"),
            timeout_ms: 2000,
        };
        // connect_timeout succeeds; the listener backlog accepts the connect.
        assert!(cmd.run().is_ok(), "loopback probe should succeed");
    }

    #[test]
    fn tcp_probe_fails_against_closed_port() {
        let cmd = CheckVictoriaMetrics {
            url: "http://127.0.0.1:1/api/v1/import/prometheus".into(),
            timeout_ms: 500,
        };
        assert!(cmd.run().is_err(), "closed port should fail the probe");
    }
}
