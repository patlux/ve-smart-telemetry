//! HTTP client tests against a local ephemeral mock server on 127.0.0.1.
//!
//! These tests never contact the real VictoriaMetrics endpoint
//! (`100.64.0.2:8429` or any other host). Every connection goes to a
//! `tokio::net::TcpListener` bound to port 0 (OS-assigned ephemeral port),
//! which is dropped after each test.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use victron_metrics::victoria_metrics::{
    FailureCategory, Outcome, VictoriaMetricsClient, MAX_INTERIM_RESPONSES,
};

const TS: i64 = 1_700_000_000_000;
const PATH: &str = "/api/v1/import/prometheus";

/// How the mock server replies after reading the full request.
enum MockReply {
    /// Write these raw response bytes.
    Bytes(Vec<u8>),
    /// Write the first chunk, then the second chunk after a short delay
    /// (forces the client to read the response across multiple reads).
    BytesSplit(Vec<u8>, Vec<u8>),
    /// Write these raw response bytes, then keep the connection open (peer
    /// never closes) to prove the client does not wait for close on
    /// no-body statuses.
    BytesThenHold(Vec<u8>),
    /// Accept and read the request, then never respond.
    Stall,
    /// Accept and read the request, then close without responding.
    Close,
}

struct Mock {
    addr: SocketAddr,
    join: JoinHandle<(String, Vec<u8>)>,
}

impl Mock {
    async fn start(reply: MockReply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let join = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            let head_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break buf.len();
                }
                buf.extend_from_slice(&tmp[..n]);
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            // The body may already be in the buffer after the header terminator.
            let mut body = buf[head_end + 4..].to_vec();
            let length: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse().ok())
                })
                .unwrap_or(0);
            if body.len() < length {
                let mut rest = vec![0u8; length - body.len()];
                sock.read_exact(&mut rest).await.unwrap();
                body.extend_from_slice(&rest);
            }
            match reply {
                MockReply::Bytes(b) => {
                    let _ = sock.write_all(&b).await;
                }
                MockReply::BytesSplit(first, second) => {
                    let _ = sock.write_all(&first).await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = sock.write_all(&second).await;
                }
                MockReply::BytesThenHold(b) => {
                    let _ = sock.write_all(&b).await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                MockReply::Stall => {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                MockReply::Close => {}
            }
            (head, body)
        });
        Mock { addr, join }
    }

    fn url(&self) -> String {
        format!("http://{}{}", self.addr, PATH)
    }

    async fn captured(self) -> (String, Vec<u8>) {
        self.join.await.unwrap()
    }
}

fn http_response(status: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

fn chunked_response(status: &str, body: &str) -> Vec<u8> {
    // Simplest chunked body: one chunk + terminal chunk.
    format!(
        "HTTP/1.1 {status}\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
        body.len(),
        body
    )
    .into_bytes()
}

fn client_for(mock: &Mock) -> VictoriaMetricsClient {
    VictoriaMetricsClient::new(&mock.url())
        .unwrap()
        .with_timeouts(Duration::from_millis(400), Duration::from_millis(400))
}

fn encoded_batch() -> String {
    let mut b = victron_metrics::MetricBatchBuilder::new("solar-charger")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    b.gauge(victron_metrics::names::PV_POWER_WATTS, 136.4)
        .unwrap();
    b.encode()
}

#[tokio::test]
async fn success_204_posts_exact_batch_to_import_path() {
    let mock = Mock::start(MockReply::Bytes(
        b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
    ))
    .await;
    let body = encoded_batch();

    let outcome = client_for(&mock).import(&body).await;

    assert_eq!(outcome, Outcome::Success);
    let host_header = format!("Host: {}", mock.addr);
    let (head, sent_body) = mock.captured().await;
    assert!(head.starts_with(&format!("POST {PATH} HTTP/1.1")));
    assert!(head.contains(&host_header));
    assert!(head.contains("Content-Type: text/plain; version=0.0.4"));
    assert!(head.contains("Connection: close"));
    assert_eq!(sent_body, body.as_bytes());
}

#[tokio::test]
async fn no_body_204_succeeds_without_waiting_for_peer_close() {
    // The server answers 204 and then keeps the connection open. A client
    // that treated a missing Content-Length as close-delimited would block
    // until the read timeout (400 ms) and report a timeout; the correct
    // behavior is to return success immediately.
    let mock = Mock::start(MockReply::BytesThenHold(
        b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    assert_eq!(outcome, Outcome::Success);
}

#[tokio::test]
async fn no_body_304_is_read_without_waiting_for_peer_close() {
    let mock = Mock::start(MockReply::BytesThenHold(
        b"HTTP/1.1 304 Not Modified\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    // 304 is not a success for an import POST, but it must be classified
    // immediately (not after a timeout), with no body read.
    match outcome {
        Outcome::Permanent(f) => {
            assert_eq!(f.status, Some(304));
            assert_eq!(f.category, FailureCategory::Http);
        }
        other => panic!("expected permanent 304, got {other:?}"),
    }
}

#[tokio::test]
async fn interim_100_then_204_same_packet_succeeds() {
    // A peer that sends `100 Continue` and the final `204` in the same TCP
    // segment: the client must consume the interim response and classify the
    // final one as success.
    let mock = Mock::start(MockReply::Bytes(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    assert_eq!(outcome, Outcome::Success);
}

#[tokio::test]
async fn interim_100_then_204_split_across_reads_succeeds() {
    // The interim response arrives in its own segment; the final response
    // follows in a later read.
    let mock = Mock::start(MockReply::BytesSplit(
        b"HTTP/1.1 100 Continue\r\n\r\n".to_vec(),
        b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    assert_eq!(outcome, Outcome::Success);
}

#[tokio::test]
async fn multiple_interim_1xx_then_final_succeeds() {
    let mock = Mock::start(MockReply::Bytes(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n"
            .to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    assert_eq!(outcome, Outcome::Success);
}

#[tokio::test]
async fn interim_100_only_peer_that_holds_open_times_out_retryable() {
    // A peer that answers only `100 Continue` and then stays open is not
    // giving a final response: the client must time out and report a
    // retryable failure, never a permanent 1xx classification.
    let mock = Mock::start(MockReply::BytesThenHold(
        b"HTTP/1.1 100 Continue\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    match outcome {
        Outcome::Retryable(f) => {
            assert_eq!(f.category, FailureCategory::Timeout);
        }
        other => panic!("expected retryable timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn interim_101_switching_protocols_is_malformed() {
    // 101 is a protocol switch, not an interim response that leads to a final
    // HTTP response: it must be rejected explicitly as unsupported/malformed.
    let mock = Mock::start(MockReply::Bytes(
        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    match outcome {
        Outcome::Retryable(f) => {
            assert_eq!(f.category, FailureCategory::MalformedResponse);
            assert!(f.message.contains("101"), "message: {}", f.message);
        }
        other => panic!("expected retryable malformed 101, got {other:?}"),
    }
}

#[tokio::test]
async fn too_many_interim_1xx_is_malformed() {
    // More interim responses than the bound: the client must stop reading
    // and report malformed instead of looping forever.
    let mut bytes = Vec::new();
    for _ in 0..=MAX_INTERIM_RESPONSES {
        bytes.extend_from_slice(b"HTTP/1.1 100 Continue\r\n\r\n");
    }
    bytes.extend_from_slice(b"HTTP/1.1 204 No Content\r\n\r\n");
    let mock = Mock::start(MockReply::Bytes(bytes)).await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    match outcome {
        Outcome::Retryable(f) => {
            assert_eq!(f.category, FailureCategory::MalformedResponse);
            assert!(f.message.contains("interim"), "message: {}", f.message);
        }
        other => panic!("expected retryable malformed, got {other:?}"),
    }
}

#[tokio::test]
async fn no_body_204_with_content_length_is_still_empty() {
    // Per RFC 9112 the 204 response never has a body; even a bogus
    // Content-Length must not make the reader consume bytes or wait.
    let mock = Mock::start(MockReply::BytesThenHold(
        b"HTTP/1.1 204 No Content\r\nContent-Length: 10\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&encoded_batch()).await;
    assert_eq!(outcome, Outcome::Success);
}

#[tokio::test]
async fn http_400_is_permanent_with_bounded_message() {
    let mock = Mock::start(MockReply::Bytes(http_response(
        "400 Bad Request",
        "cannot parse prometheus metrics: unexpected token",
    )))
    .await;

    let outcome = client_for(&mock).import(&encoded_batch()).await;

    match outcome {
        Outcome::Permanent(f) => {
            assert_eq!(f.status, Some(400));
            assert_eq!(f.category, FailureCategory::Http);
            assert!(f.message.contains("400"));
            assert!(f.message.contains("unexpected token"));
            assert!(f.message.len() <= 300);
        }
        other => panic!("expected permanent failure, got {other:?}"),
    }
}

#[tokio::test]
async fn http_503_is_retryable() {
    let mock = Mock::start(MockReply::Bytes(http_response(
        "503 Service Unavailable",
        "database is in read-only mode",
    )))
    .await;

    let outcome = client_for(&mock).import(&encoded_batch()).await;
    assert!(matches!(outcome, Outcome::Retryable(_)));
    assert!(outcome.should_retry());
}

#[tokio::test]
async fn http_429_is_retryable() {
    let mock = Mock::start(MockReply::Bytes(http_response("429 Too Many Requests", ""))).await;

    let outcome = client_for(&mock).import(&encoded_batch()).await;
    assert!(matches!(outcome, Outcome::Retryable(_)));
}

#[tokio::test]
async fn read_timeout_is_retryable() {
    let mock = Mock::start(MockReply::Stall).await;

    let outcome = client_for(&mock).import(&encoded_batch()).await;
    match outcome {
        Outcome::Retryable(f) => {
            assert_eq!(f.category, FailureCategory::Timeout);
        }
        other => panic!("expected retryable timeout, got {other:?}"),
    }
}

#[tokio::test]
async fn connection_closed_without_response_is_retryable() {
    let mock = Mock::start(MockReply::Close).await;

    let outcome = client_for(&mock).import(&encoded_batch()).await;
    match outcome {
        Outcome::Retryable(f) => {
            assert_eq!(f.category, FailureCategory::Network);
        }
        other => panic!("expected retryable network failure, got {other:?}"),
    }
}

#[tokio::test]
async fn connection_refused_is_retryable() {
    // Bind an ephemeral port, then drop the listener so nothing accepts.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let client = VictoriaMetricsClient::new(&format!("http://{addr}{PATH}")).unwrap();
    let outcome = client.import(&encoded_batch()).await;
    match outcome {
        Outcome::Retryable(f) => {
            assert_eq!(f.category, FailureCategory::Network);
        }
        other => panic!("expected retryable network failure, got {other:?}"),
    }
}

#[tokio::test]
async fn chunked_error_body_is_parsed_and_bounded() {
    let mock = Mock::start(MockReply::Bytes(chunked_response(
        "400 Bad Request",
        "unexpected token at line 1",
    )))
    .await;

    let outcome = client_for(&mock).import(&encoded_batch()).await;
    match outcome {
        Outcome::Permanent(f) => {
            assert_eq!(f.status, Some(400));
            assert!(
                f.message.contains("unexpected token"),
                "message: {}",
                f.message
            );
        }
        other => panic!("expected permanent failure, got {other:?}"),
    }
}

#[tokio::test]
async fn malformed_response_is_retryable() {
    let mock = Mock::start(MockReply::Bytes(b"this is not http\r\n\r\n".to_vec())).await;

    let outcome = client_for(&mock).import(&encoded_batch()).await;
    match outcome {
        Outcome::Retryable(f) => {
            assert_eq!(f.category, FailureCategory::MalformedResponse);
        }
        other => panic!("expected retryable malformed response, got {other:?}"),
    }
}

#[tokio::test]
async fn https_urls_are_rejected_before_any_io() {
    let err = VictoriaMetricsClient::new("https://100.64.0.2:8429/api/v1/import/prometheus")
        .expect_err("https must be rejected (no TLS in the ARMv6 footprint)");
    assert_eq!(
        err,
        victron_metrics::ClientConfigError::UnsupportedScheme("https".into())
    );
}

// A tiny smoke test that the client can talk to a real TcpStream-backed server
// with a body larger than one TCP segment, exercising partial reads.
#[tokio::test]
async fn large_batch_round_trip() {
    let mut b = victron_metrics::MetricBatchBuilder::new("solar-charger")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    for i in 0..2_000 {
        b.gauge_with("victron_test_series", &[("idx", &i.to_string())], i as f64)
            .unwrap();
    }
    let big_body = b.encode();
    assert!(big_body.len() > 100_000);

    let mock = Mock::start(MockReply::Bytes(
        b"HTTP/1.1 204 No Content\r\n\r\n".to_vec(),
    ))
    .await;
    let outcome = client_for(&mock).import(&big_body).await;
    assert_eq!(outcome, Outcome::Success);

    let (_, sent) = mock.captured().await;
    assert_eq!(sent, big_body.as_bytes());
}
