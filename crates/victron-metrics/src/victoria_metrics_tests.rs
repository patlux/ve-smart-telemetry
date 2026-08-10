use super::*;

#[test]
fn url_parsing() {
    let c = VictoriaMetricsClient::new("http://100.64.0.2:8429/api/v1/import/prometheus").unwrap();
    assert_eq!(c.host(), "100.64.0.2");
    assert_eq!(c.port(), 8429);
    assert_eq!(c.path(), "/api/v1/import/prometheus");

    // No path -> default import path.
    let c = VictoriaMetricsClient::new("http://host.example").unwrap();
    assert_eq!(c.port(), 80);
    assert_eq!(c.path(), DEFAULT_IMPORT_PATH);

    // https rejected.
    assert!(matches!(
        VictoriaMetricsClient::new("https://host.example/import"),
        Err(ClientConfigError::UnsupportedScheme(_))
    ));
    // Missing scheme.
    assert!(matches!(
        VictoriaMetricsClient::new("100.64.0.2:8429"),
        Err(ClientConfigError::UnsupportedScheme(_))
    ));
    // Bad port.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host:notaport"),
        Err(ClientConfigError::InvalidPort(_))
    ));
    // IPv6 rejected.
    assert!(matches!(
        VictoriaMetricsClient::new("http://[::1]:8429/"),
        Err(ClientConfigError::UnsupportedHost(_))
    ));
}

#[test]
fn url_rejects_request_line_and_host_header_injection() {
    // CRLF in the host (Host-header injection).
    assert!(matches!(
        VictoriaMetricsClient::new("http://host\r\nInjected/"),
        Err(ClientConfigError::InvalidHost(_))
    ));
    // Whitespace in the host.
    assert!(matches!(
        VictoriaMetricsClient::new("http://ho st/"),
        Err(ClientConfigError::InvalidHost(_))
    ));
    // Tab in the host.
    assert!(matches!(
        VictoriaMetricsClient::new("http://ho\tst/"),
        Err(ClientConfigError::InvalidHost(_))
    ));
    // Userinfo without password.
    assert!(matches!(
        VictoriaMetricsClient::new("http://user@host/"),
        Err(ClientConfigError::InvalidHost(_))
    ));
    // Userinfo with password.
    assert!(matches!(
        VictoriaMetricsClient::new("http://user:pass@host/"),
        Err(ClientConfigError::InvalidHost(_))
    ));
    // Fragment after the path.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/path#frag"),
        Err(ClientConfigError::QueryOrFragment(_))
    ));
    // Fragment without a path.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host#frag"),
        Err(ClientConfigError::QueryOrFragment(_))
    ));
    // Query string.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/path?x=1"),
        Err(ClientConfigError::QueryOrFragment(_))
    ));
    // CRLF in the path (request-line injection).
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/pa\r\nX-Evil: 1"),
        Err(ClientConfigError::InvalidPath(_))
    ));
    // Whitespace in the path.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/pa th"),
        Err(ClientConfigError::InvalidPath(_))
    ));
    // Tab in the path.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/pa\tth"),
        Err(ClientConfigError::InvalidPath(_))
    ));
    // NUL in the path.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/pa\0th"),
        Err(ClientConfigError::InvalidPath(_))
    ));
    // Backslash in the path.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/pa\\th"),
        Err(ClientConfigError::InvalidPath(_))
    ));
    // Non-ASCII bytes in the path.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host/päth"),
        Err(ClientConfigError::InvalidPath(_))
    ));
}

#[test]
fn url_accepts_plain_ipv4_and_dns_hosts() {
    let c = VictoriaMetricsClient::new("http://100.64.0.2:8429/api/v1/import/prometheus").unwrap();
    assert_eq!(c.host(), "100.64.0.2");
    assert_eq!(c.port(), 8429);

    let c = VictoriaMetricsClient::new("http://host.example/api").unwrap();
    assert_eq!(c.host(), "host.example");
    assert_eq!(c.port(), 80);
    assert_eq!(c.path(), "/api");

    let c = VictoriaMetricsClient::new("http://some_internal_name/api").unwrap();
    assert_eq!(c.host(), "some_internal_name");

    // Port bounds.
    assert!(matches!(
        VictoriaMetricsClient::new("http://host:0/"),
        Err(ClientConfigError::InvalidPort(_))
    ));
    assert!(matches!(
        VictoriaMetricsClient::new("http://host:65536/"),
        Err(ClientConfigError::InvalidPort(_))
    ));
    assert!(matches!(
        VictoriaMetricsClient::new("http://host:/"),
        Err(ClientConfigError::InvalidPort(_))
    ));
    // Empty host.
    assert!(matches!(
        VictoriaMetricsClient::new("http:///api"),
        Err(ClientConfigError::MissingHost)
    ));
    assert!(matches!(
        VictoriaMetricsClient::new("http://:8429/"),
        Err(ClientConfigError::MissingHost)
    ));
}

#[test]
fn request_shape() {
    let req = String::from_utf8(build_request(
        "h.example",
        8429,
        "/api/v1/import/prometheus",
        "m 1 2\n",
    ))
    .unwrap();
    assert_eq!(
        req,
        "POST /api/v1/import/prometheus HTTP/1.1\r\n\
             Host: h.example:8429\r\n\
             Content-Type: text/plain; version=0.0.4\r\n\
             Content-Length: 6\r\n\
             Connection: close\r\n\r\n\
             m 1 2\n"
    );
}

#[test]
fn classification() {
    let client = VictoriaMetricsClient::new("http://localhost:8429").unwrap();
    let c = |code: u16| {
        client.classify(&RawResponse {
            status: code,
            reason: "r".into(),
            body: vec![],
        })
    };
    assert!(c(200).is_success());
    assert!(c(204).is_success());
    assert!(matches!(c(301), Outcome::Permanent(_)));
    assert!(matches!(c(400), Outcome::Permanent(_)));
    assert!(matches!(c(404), Outcome::Permanent(_)));
    assert!(matches!(c(408), Outcome::Retryable(_)));
    assert!(matches!(c(429), Outcome::Retryable(_)));
    assert!(matches!(c(500), Outcome::Retryable(_)));
    assert!(matches!(c(503), Outcome::Retryable(_)));
}
