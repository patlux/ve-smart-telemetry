//! Strict URL parsing and HTTP request construction for VictoriaMetrics.

use super::{ClientConfigError, DEFAULT_IMPORT_PATH};

pub(super) fn build_request(host: &str, port: u16, path: &str, body: &str) -> Vec<u8> {
    debug_assert!(is_valid_host(host), "host must be validated");
    debug_assert!(is_valid_path(path), "path must be validated");
    let mut request = Vec::with_capacity(128 + body.len());
    request.extend_from_slice(b"POST ");
    request.extend_from_slice(path.as_bytes());
    request.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    request.extend_from_slice(host.as_bytes());
    if port != 80 {
        request.push(b':');
        request.extend_from_slice(port.to_string().as_bytes());
    }
    request.extend_from_slice(b"\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: ");
    request.extend_from_slice(body.len().to_string().as_bytes());
    request.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    request.extend_from_slice(body.as_bytes());
    request
}

pub(super) fn parse_http_url(url: &str) -> Result<(String, u16, String), ClientConfigError> {
    let rest = if let Some(rest) = url.strip_prefix("http://") {
        rest
    } else if url.starts_with("https://") {
        return Err(ClientConfigError::UnsupportedScheme("https".into()));
    } else {
        return Err(ClientConfigError::UnsupportedScheme(
            url.split(':').next().unwrap_or(url).to_owned(),
        ));
    };
    if rest.contains('?') || rest.contains('#') {
        return Err(ClientConfigError::QueryOrFragment(url.to_owned()));
    }
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, DEFAULT_IMPORT_PATH),
    };
    let (host, port) = split_authority(authority)?;
    validate_host(host)?;
    validate_path(path)?;
    Ok((host.to_owned(), port, path.to_owned()))
}

fn split_authority(authority: &str) -> Result<(&str, u16), ClientConfigError> {
    if authority.is_empty() {
        return Err(ClientConfigError::MissingHost);
    }
    if authority.contains('@') {
        return Err(ClientConfigError::InvalidHost(authority.to_owned()));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() {
            return Err(ClientConfigError::MissingHost);
        }
        if host.contains(':') || host.starts_with('[') {
            return Err(ClientConfigError::UnsupportedHost(host.to_owned()));
        }
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|&port| port != 0)
            .ok_or_else(|| ClientConfigError::InvalidPort(port.to_owned()))?;
        Ok((host, port))
    } else {
        Ok((authority, 80))
    }
}

fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
}

fn validate_host(host: &str) -> Result<(), ClientConfigError> {
    is_valid_host(host)
        .then_some(())
        .ok_or_else(|| ClientConfigError::InvalidHost(host.to_owned()))
}

fn is_valid_path(path: &str) -> bool {
    path.starts_with('/')
        && path
            .bytes()
            .all(|byte| (0x21..=0x7e).contains(&byte) && !b" \"<>\\^`{|}#?".contains(&byte))
}

fn validate_path(path: &str) -> Result<(), ClientConfigError> {
    is_valid_path(path)
        .then_some(())
        .ok_or_else(|| ClientConfigError::InvalidPath(path.to_owned()))
}
