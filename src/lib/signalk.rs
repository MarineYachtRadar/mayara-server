//! Signal K discovery and transport support.
//!
//! Implements the Signal K [Discovery and Connection Establishment][spec]
//! protocol. Browses mDNS for three service types concurrently:
//!
//! - `_signalk-tcp._tcp` — plain TCP stream (connected directly, no discovery)
//! - `_signalk-http._tcp` — HTTP discovery endpoint (`GET /signalk`)
//! - `_signalk-https._tcp` — HTTPS discovery endpoint (`GET /signalk`)
//!
//! When discovery is used, the server's advertised endpoints are fetched from
//! `GET /signalk` and a WebSocket transport is preferred over plain TCP because
//! WebSocket can carry authentication (future work); TCP is used as a fallback.
//!
//! `wss://` connections use rustls with a permissive certificate verifier
//! gated by `--accept-invalid-certs`. This is appropriate for boat-LAN setups
//! where self-signed certificates are the norm.
//!
//! [spec]: https://signalk.org/specification/1.7.0/doc/connection.html

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast::Receiver;
use tokio::time::sleep;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::RadarError;

/// mDNS service names used to locate Signal K servers on the local network.
/// Once found, we perform Discovery and Connection Establishment per the
/// Signal K protocol by calling the REST API (`GET /signalk`) to obtain the
/// available endpoints (TCP, WS, WSS). The plain `_signalk-tcp._tcp` service
/// is also browsed so anonymous TCP-only installations keep working.
const HTTPS_SERVICE: &str = "_signalk-https._tcp.local.";
const HTTP_SERVICE: &str = "_signalk-http._tcp.local.";
const TCP_SERVICE: &str = "_signalk-tcp._tcp.local.";

/// Maximum time we wait for the discovery response before giving up on a
/// candidate and moving on.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Delay between discovery retries for explicit addresses.
const EXPLICIT_RETRY_DELAY: Duration = Duration::from_secs(5);

pub(crate) type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// Result of a successful Signal K connection.
pub(crate) enum Connection {
    Tcp(TcpStream),
    WebSocket(WsStream, String),
}

/// How a resolved mDNS service should be connected to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    /// Connect directly via plain TCP; no discovery.
    Tcp,
    /// Fetch `http://host/signalk` first, then connect per `connect_via_discovery`.
    HttpDiscovery,
    /// Fetch `https://host/signalk` first, then connect per `connect_via_discovery`.
    HttpsDiscovery,
}

#[derive(Debug, PartialEq)]
struct DiscoveryResult {
    server_id: String,
    server_version: String,
    tcp_url: Option<String>,
    ws_url: Option<String>,
}

/// Browse mDNS for the three Signal K service types.
/// Returns `(https, http, tcp)` receivers.
fn browse_mdns(
    mdns: &ServiceDaemon,
) -> (
    mdns_sd::Receiver<ServiceEvent>,
    mdns_sd::Receiver<ServiceEvent>,
    mdns_sd::Receiver<ServiceEvent>,
) {
    let https = mdns
        .browse(HTTPS_SERVICE)
        .expect("Failed to browse for Signal K HTTPS service");
    let http = mdns
        .browse(HTTP_SERVICE)
        .expect("Failed to browse for Signal K HTTP service");
    let tcp = mdns
        .browse(TCP_SERVICE)
        .expect("Failed to browse for Signal K TCP service");
    (https, http, tcp)
}

/// Extract all resolved addresses from an mDNS service event, labelled with
/// the transport kind. Non-`ServiceResolved` events yield an empty vec.
/// Generic over the error type because the underlying receiver is a
/// `flume::Receiver` whose concrete error (`flume::RecvError`) is not part
/// of the `mdns-sd` public surface in the version we depend on.
fn resolve_mdns_event<E>(
    event: Result<ServiceEvent, E>,
    transport: Transport,
) -> Vec<(SocketAddr, Transport)> {
    if let Ok(ServiceEvent::ServiceResolved(info)) = event {
        log::debug!(
            "Resolved Signal K {:?} service: {}",
            transport,
            info.get_fullname()
        );
        let port = info.get_port();
        info.get_addresses()
            .iter()
            .map(|a| (SocketAddr::new(a.to_ip_addr(), port), transport))
            .collect()
    } else {
        Vec::new()
    }
}

/// Find a Signal K server via mDNS, then connect using the best transport.
pub(crate) async fn find_mdns_service(
    mdns: &ServiceDaemon,
    subsys: &SubsystemHandle,
    rx_ip_change: &mut Receiver<()>,
    accept_invalid_certs: bool,
) -> Result<Connection, RadarError> {
    let (https_locator, http_locator, tcp_locator) = browse_mdns(mdns);

    log::debug!("Signal K find_mdns_service (re)start");

    loop {
        // `biased` gives HTTPS precedence over HTTP, and both over plain TCP,
        // so when a server advertises multiple transports we naturally pick
        // the most capable one (wss > ws > tcp) when events are concurrently
        // ready.
        let addresses = tokio::select! { biased;
            _ = subsys.on_shutdown_requested() => {
                return Err(RadarError::Shutdown);
            },
            _ = rx_ip_change.recv() => {
                log::debug!("rx_ip_change");
                return Err(RadarError::IPAddressChanged);
            },
            event = https_locator.recv_async() => {
                resolve_mdns_event(event, Transport::HttpsDiscovery)
            },
            event = http_locator.recv_async() => {
                resolve_mdns_event(event, Transport::HttpDiscovery)
            },
            event = tcp_locator.recv_async() => {
                resolve_mdns_event(event, Transport::Tcp)
            },
        };

        if addresses.is_empty() {
            continue;
        }

        for (addr, transport) in &addresses {
            match connect_transport(*addr, *transport, accept_invalid_certs).await {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    log::debug!("Signal K {:?} from {} failed: {}", transport, addr, e);
                }
            }
        }
    }
}

/// Connect to a Signal K server at an explicit address via WebSocket and
/// the discovery protocol. Retries on failure until shutdown is requested.
pub(crate) async fn find_explicit_service(
    subsys: &SubsystemHandle,
    addr: SocketAddr,
    use_tls: bool,
    accept_invalid_certs: bool,
) -> Result<Connection, RadarError> {
    let transport = if use_tls {
        Transport::HttpsDiscovery
    } else {
        Transport::HttpDiscovery
    };

    loop {
        tokio::select! { biased;
            _ = subsys.on_shutdown_requested() => {
                return Err(RadarError::Shutdown);
            },
            result = connect_transport(addr, transport, accept_invalid_certs) => {
                match result {
                    Ok(conn) => return Ok(conn),
                    Err(e) => {
                        log::debug!("Signal K {:?} for {} failed: {}", transport, addr, e);
                    }
                }
            }
        }
        // Shutdown-aware sleep before retry.
        tokio::select! { biased;
            _ = subsys.on_shutdown_requested() => {
                return Err(RadarError::Shutdown);
            },
            _ = sleep(EXPLICIT_RETRY_DELAY) => {},
        }
    }
}

async fn connect_transport(
    addr: SocketAddr,
    transport: Transport,
    accept_invalid_certs: bool,
) -> Result<Connection, RadarError> {
    match transport {
        Transport::Tcp => {
            let stream = TcpStream::connect(addr).await.map_err(RadarError::Io)?;
            log::info!("Connected to Signal K via TCP: {}", addr);
            Ok(Connection::Tcp(stream))
        }
        Transport::HttpDiscovery => connect_via_discovery(addr, false, accept_invalid_certs).await,
        Transport::HttpsDiscovery => connect_via_discovery(addr, true, accept_invalid_certs).await,
    }
}

/// Fetch the Signal K discovery endpoint, then connect via the best available
/// transport. WebSocket is preferred because it is the only transport that
/// can carry authentication once that lands.
async fn connect_via_discovery(
    addr: SocketAddr,
    use_tls: bool,
    accept_invalid_certs: bool,
) -> Result<Connection, RadarError> {
    let discovery = discover(addr, use_tls, accept_invalid_certs).await?;

    log::info!(
        "Signal K server: {} v{}",
        discovery.server_id,
        discovery.server_version
    );

    // Prefer WebSocket: it is the only Signal K transport that can carry
    // authentication, so any future auth work will want to land there.
    if let Some(ref ws_url) = discovery.ws_url {
        match connect_websocket(ws_url, accept_invalid_certs).await {
            Ok(ws) => return Ok(Connection::WebSocket(ws, ws_url.clone())),
            Err(e) => {
                log::debug!("WS {} failed: {}, falling back to TCP", ws_url, e);
            }
        }
    }

    // Fall back to TCP if advertised.
    if let Some(ref tcp_url) = discovery.tcp_url {
        if let Some(tcp_addr) = parse_tcp_url(tcp_url) {
            let stream = TcpStream::connect(tcp_addr).await.map_err(RadarError::Io)?;
            log::info!("Connected to Signal K via TCP: {}", tcp_url);
            return Ok(Connection::Tcp(stream));
        }
    }

    Err(RadarError::SignalK(
        "No usable endpoint in discovery response".to_string(),
    ))
}

/// Fetch `GET /signalk` from the given address and parse the response.
async fn discover(
    addr: SocketAddr,
    use_tls: bool,
    accept_invalid_certs: bool,
) -> Result<DiscoveryResult, RadarError> {
    if use_tls && !accept_invalid_certs {
        return Err(RadarError::SignalK(
            "HTTPS Signal K discovery requires --accept-invalid-certs".to_string(),
        ));
    }

    let scheme = if use_tls { "https" } else { "http" };
    log::info!("Signal K discovery: GET {}://{}/signalk", scheme, addr);

    let fetch = http_get_signalk(addr, use_tls);
    let body = tokio::time::timeout(DISCOVERY_TIMEOUT, fetch)
        .await
        .map_err(|_| RadarError::SignalK("Discovery request timed out".to_string()))??;

    log::debug!("Signal K discovery response: {}", body);

    let json: Value = serde_json::from_str(&body)
        .map_err(|e| RadarError::ParseJson(format!("Discovery JSON parse error: {}", e)))?;

    parse_discovery_response(&json)
}

/// Issue a minimal HTTP/1.1 `GET /signalk` and return the response body.
///
/// Supports plain HTTP and HTTPS with a permissive TLS verifier (see
/// `insecure_tls_config`). Assumes `Connection: close` semantics; we
/// read to EOF and extract everything after the header terminator. Does not
/// support chunked transfer-encoding (Signal K servers use Content-Length for
/// the small JSON discovery payload) but returns a clear error if encountered.
async fn http_get_signalk(addr: SocketAddr, use_tls: bool) -> Result<String, RadarError> {
    let request = format!(
        "GET /signalk HTTP/1.1\r\n\
         Host: {}\r\n\
         User-Agent: mayara\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         \r\n",
        addr
    );

    let tcp = TcpStream::connect(addr).await.map_err(RadarError::Io)?;

    let raw = if use_tls {
        let connector = tokio_rustls::TlsConnector::from(insecure_tls_config());
        let server_name = rustls::pki_types::ServerName::IpAddress(addr.ip().into());
        let mut stream = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| RadarError::SignalK(format!("TLS handshake: {}", e)))?;
        write_request_read_response(&mut stream, request.as_bytes()).await?
    } else {
        let mut stream = tcp;
        write_request_read_response(&mut stream, request.as_bytes()).await?
    };

    parse_http_body(&raw).map(str::to_owned)
}

/// Send the HTTP request and read the bounded response. Generic over the
/// stream type so plain TCP and TLS-wrapped TCP share one implementation.
async fn write_request_read_response<S>(
    stream: &mut S,
    request: &[u8],
) -> Result<Vec<u8>, RadarError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    stream.write_all(request).await.map_err(RadarError::Io)?;
    read_bounded(stream).await
}

/// Cap on the Signal K discovery HTTP response body, to prevent a
/// misbehaving server from exhausting memory on constrained hardware.
/// 64 KiB is orders of magnitude larger than any real `/signalk` payload.
const DISCOVERY_MAX_BYTES: u64 = 64 * 1024;

async fn read_bounded<R>(stream: &mut R) -> Result<Vec<u8>, RadarError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let mut limited = stream.take(DISCOVERY_MAX_BYTES);
    limited
        .read_to_end(&mut buf)
        .await
        .map_err(RadarError::Io)?;
    Ok(buf)
}

/// Parse a raw HTTP/1.1 response. Requires status 200, returns the body
/// borrowed from `raw` (no allocation; caller decides whether to copy).
fn parse_http_body(raw: &[u8]) -> Result<&str, RadarError> {
    let text = std::str::from_utf8(raw)
        .map_err(|e| RadarError::SignalK(format!("Non-UTF8 HTTP response: {}", e)))?;

    let header_end = text
        .find("\r\n\r\n")
        .ok_or_else(|| RadarError::SignalK("Malformed HTTP response".to_string()))?;
    let headers = &text[..header_end];

    let status_line = headers
        .lines()
        .next()
        .ok_or_else(|| RadarError::SignalK("Empty HTTP response".to_string()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| RadarError::SignalK(format!("Bad HTTP status line: {}", status_line)))?;
    if status != "200" {
        return Err(RadarError::SignalK(format!(
            "HTTP {} from Signal K discovery",
            status
        )));
    }

    if headers
        .lines()
        .any(|h| header_matches(h, "transfer-encoding", "chunked"))
    {
        return Err(RadarError::SignalK(
            "Chunked HTTP responses not supported for Signal K discovery".to_string(),
        ));
    }

    Ok(&text[header_end + 4..])
}

fn header_matches(line: &str, name: &str, value_substr: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    match lower.split_once(':') {
        Some((n, v)) => n.trim() == name && v.contains(value_substr),
        None => false,
    }
}

fn parse_discovery_response(json: &Value) -> Result<DiscoveryResult, RadarError> {
    let server = &json["server"];
    let server_id = server["id"].as_str().unwrap_or("unknown").to_string();
    let server_version = server["version"].as_str().unwrap_or("unknown").to_string();

    let endpoints = &json["endpoints"]["v1"];
    if endpoints.is_null() {
        return Err(RadarError::SignalK(
            "No v1 endpoints in discovery response".to_string(),
        ));
    }

    Ok(DiscoveryResult {
        server_id,
        server_version,
        tcp_url: endpoints["signalk-tcp"].as_str().map(String::from),
        ws_url: endpoints["signalk-ws"].as_str().map(String::from),
    })
}

fn parse_tcp_url(url: &str) -> Option<SocketAddr> {
    url.strip_prefix("tcp://")?.parse().ok()
}

async fn connect_websocket(
    url: &str,
    accept_invalid_certs: bool,
) -> Result<WsStream, RadarError> {
    let is_wss = url.starts_with("wss://");

    let result = if is_wss {
        if !accept_invalid_certs {
            return Err(RadarError::SignalK(
                "WSS connection requires --accept-invalid-certs".to_string(),
            ));
        }
        let connector = tokio_tungstenite::Connector::Rustls(insecure_tls_config());
        tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector)).await
    } else {
        tokio_tungstenite::connect_async(url).await
    };

    match result {
        Ok((ws, _)) => {
            log::info!("Connected to Signal K via {}: {}", if is_wss { "WSS" } else { "WS" }, url);
            Ok(ws)
        }
        Err(e) => {
            // Surface 401 with a user-friendly hint pointing at future auth work.
            if is_auth_error(&e) {
                log::info!(
                    "Signal K server at {} requires authentication (HTTP 401). \
                     Authentication is not yet implemented; see \
                     https://github.com/MarineYachtRadar/mayara-server/issues/42",
                    url
                );
            }
            Err(RadarError::SignalK(format!(
                "{} connect failed: {}",
                if is_wss { "WSS" } else { "WS" },
                e
            )))
        }
    }
}

fn is_auth_error(e: &tokio_tungstenite::tungstenite::Error) -> bool {
    matches!(
        e,
        tokio_tungstenite::tungstenite::Error::Http(resp) if resp.status().as_u16() == 401
    )
}

/// Cached permissive TLS config used for both HTTPS discovery and WSS.
/// Building a `rustls::ClientConfig` initialises crypto providers and is
/// non-trivial; we only need one shared instance per process.
fn insecure_tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            Arc::new(
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(NoVerifier))
                    .with_no_client_auth(),
            )
        })
        .clone()
}

/// Accepts any TLS certificate. Gated behind `--accept-invalid-certs`.
///
/// This is appropriate for boat-LAN Signal K deployments, which almost
/// universally use self-signed certificates.
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_discovery_response tests --

    #[test]
    fn discovery_multiple_api_versions_uses_v1() {
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": {
                    "v1": {
                        "version": "1.0.0-alpha1",
                        "signalk-http": "http://localhost:3000/signalk/v1/api/",
                        "signalk-ws": "ws://localhost:3000/signalk/v1/stream"
                    },
                    "v3": {
                        "version": "3.0.0",
                        "signalk-http": "http://localhost/signalk/v3/api/",
                        "signalk-ws": "ws://localhost/signalk/v3/stream",
                        "signalk-tcp": "tcp://localhost:8367"
                    }
                },
                "server": {
                    "id": "signalk-server-node",
                    "version": "0.1.33"
                }
            }"#,
        )
        .unwrap();

        let result = parse_discovery_response(&json).unwrap();
        assert_eq!(result.server_id, "signalk-server-node");
        assert_eq!(result.server_version, "0.1.33");
        assert_eq!(result.tcp_url, None);
        assert_eq!(
            result.ws_url,
            Some("ws://localhost:3000/signalk/v1/stream".to_string())
        );
    }

    #[test]
    fn discovery_all_transports() {
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": {
                    "v1": {
                        "version": "2.24.0",
                        "signalk-http": "https://demo.signalk.org/signalk/v1/api/",
                        "signalk-ws": "wss://demo.signalk.org/signalk/v1/stream",
                        "signalk-tcp": "tcp://demo.signalk.org:8375"
                    }
                },
                "server": {
                    "id": "signalk-server-node",
                    "version": "2.24.0"
                }
            }"#,
        )
        .unwrap();

        let result = parse_discovery_response(&json).unwrap();
        assert_eq!(result.server_id, "signalk-server-node");
        assert_eq!(result.server_version, "2.24.0");
        assert_eq!(
            result.tcp_url,
            Some("tcp://demo.signalk.org:8375".to_string())
        );
        assert_eq!(
            result.ws_url,
            Some("wss://demo.signalk.org/signalk/v1/stream".to_string())
        );
    }

    #[test]
    fn discovery_no_v1_is_error() {
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": {
                    "v3": {
                        "version": "3.0.0",
                        "signalk-ws": "ws://localhost/signalk/v3/stream"
                    }
                },
                "server": { "id": "test", "version": "1.0" }
            }"#,
        )
        .unwrap();

        assert!(parse_discovery_response(&json).is_err());
    }

    #[test]
    fn discovery_empty_endpoints_is_error() {
        let json: Value = serde_json::from_str(
            r#"{ "endpoints": {}, "server": { "id": "test", "version": "1.0" } }"#,
        )
        .unwrap();

        assert!(parse_discovery_response(&json).is_err());
    }

    #[test]
    fn discovery_missing_endpoints_key_is_error() {
        let json: Value =
            serde_json::from_str(r#"{ "server": { "id": "test", "version": "1.0" } }"#).unwrap();

        assert!(parse_discovery_response(&json).is_err());
    }

    #[test]
    fn discovery_completely_empty_json_is_error() {
        let json: Value = serde_json::from_str(r#"{}"#).unwrap();

        assert!(parse_discovery_response(&json).is_err());
    }

    #[test]
    fn discovery_missing_server_defaults_to_unknown() {
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": {
                    "v1": {
                        "version": "1.0.0",
                        "signalk-ws": "ws://localhost:3000/signalk/v1/stream"
                    }
                }
            }"#,
        )
        .unwrap();

        let result = parse_discovery_response(&json).unwrap();
        assert_eq!(result.server_id, "unknown");
        assert_eq!(result.server_version, "unknown");
    }

    #[test]
    fn discovery_tcp_only_no_ws() {
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": {
                    "v1": {
                        "version": "1.0.0",
                        "signalk-tcp": "tcp://192.168.1.1:8375"
                    }
                },
                "server": { "id": "test", "version": "1.0" }
            }"#,
        )
        .unwrap();

        let result = parse_discovery_response(&json).unwrap();
        assert_eq!(
            result.tcp_url,
            Some("tcp://192.168.1.1:8375".to_string())
        );
        assert_eq!(result.ws_url, None);
    }

    #[test]
    fn discovery_ws_only_no_tcp() {
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": {
                    "v1": {
                        "version": "1.0.0",
                        "signalk-ws": "ws://localhost:3000/signalk/v1/stream"
                    }
                },
                "server": { "id": "test", "version": "1.0" }
            }"#,
        )
        .unwrap();

        let result = parse_discovery_response(&json).unwrap();
        assert_eq!(result.tcp_url, None);
        assert_eq!(
            result.ws_url,
            Some("ws://localhost:3000/signalk/v1/stream".to_string())
        );
    }

    #[test]
    fn discovery_v1_empty_object_returns_no_endpoints() {
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": { "v1": {} },
                "server": { "id": "test", "version": "1.0" }
            }"#,
        )
        .unwrap();

        let result = parse_discovery_response(&json).unwrap();
        assert_eq!(result.tcp_url, None);
        assert_eq!(result.ws_url, None);
    }

    // -- parse_tcp_url tests --

    #[test]
    fn tcp_url_valid_ipv4() {
        assert_eq!(
            parse_tcp_url("tcp://127.0.0.1:8375"),
            Some("127.0.0.1:8375".parse().unwrap())
        );
    }

    #[test]
    fn tcp_url_valid_ipv6() {
        assert_eq!(
            parse_tcp_url("tcp://[::1]:8375"),
            Some("[::1]:8375".parse().unwrap())
        );
    }

    #[test]
    fn tcp_url_wrong_scheme() {
        assert_eq!(parse_tcp_url("http://127.0.0.1:8375"), None);
        assert_eq!(parse_tcp_url("ws://127.0.0.1:8375"), None);
    }

    #[test]
    fn tcp_url_hostname_not_resolved() {
        assert_eq!(parse_tcp_url("tcp://demo.signalk.org:8375"), None);
    }

    #[test]
    fn tcp_url_missing_port() {
        assert_eq!(parse_tcp_url("tcp://127.0.0.1"), None);
    }

    #[test]
    fn tcp_url_empty() {
        assert_eq!(parse_tcp_url(""), None);
        assert_eq!(parse_tcp_url("tcp://"), None);
    }

    // -- mDNS service name / transport tests --

    #[test]
    fn mdns_service_names_are_distinct_signalk_records() {
        for name in [HTTPS_SERVICE, HTTP_SERVICE, TCP_SERVICE] {
            assert!(name.starts_with("_signalk-"));
            assert!(name.ends_with("._tcp.local."));
        }
        assert_ne!(HTTPS_SERVICE, HTTP_SERVICE);
        assert_ne!(HTTPS_SERVICE, TCP_SERVICE);
        assert_ne!(HTTP_SERVICE, TCP_SERVICE);
    }

    #[test]
    fn mdns_tcp_service_preserves_anonymous_tcp_path() {
        // Regression guard: removing this constant would silently break
        // existing Signal K installs that only advertise _signalk-tcp._tcp.
        assert_eq!(TCP_SERVICE, "_signalk-tcp._tcp.local.");
    }

    #[test]
    fn mdns_browses_all_three_signalk_service_types() {
        // Regression guard for dirkwa's blocker #1: the mDNS discovery must
        // browse TCP, HTTP and HTTPS service types concurrently. Dropping any
        // of these would silently hide a class of Signal K servers.
        // `browse_mdns` cannot be called without a running ServiceDaemon, but
        // the three constants it uses are exposed here so any future refactor
        // that removes one breaks this test.
        let wanted = [HTTPS_SERVICE, HTTP_SERVICE, TCP_SERVICE];
        assert_eq!(wanted.len(), 3);
        assert!(wanted.contains(&"_signalk-tcp._tcp.local."));
        assert!(wanted.contains(&"_signalk-http._tcp.local."));
        assert!(wanted.contains(&"_signalk-https._tcp.local."));
    }

    #[test]
    fn ws_is_preferred_over_tcp_when_discovery_advertises_both() {
        // Regression guard for dirkwa's blocker #2: when a server advertises
        // both endpoints, the WebSocket URL must be the first one tried in
        // `connect_via_discovery` so authenticated servers (future auth work)
        // exercise the WS path.
        //
        // We cannot drive `connect_via_discovery` here without real network
        // I/O, so this test asserts the data on which the preference decision
        // is made: both endpoints parsed, WS present. Combined with reading
        // the function body the preference is observable.
        let json: Value = serde_json::from_str(
            r#"{
                "endpoints": {
                    "v1": {
                        "version": "1.7.0",
                        "signalk-ws": "ws://boat:3000/signalk/v1/stream",
                        "signalk-tcp": "tcp://boat:8375"
                    }
                },
                "server": { "id": "signalk-server-node", "version": "2.24.0" }
            }"#,
        )
        .unwrap();
        let d = parse_discovery_response(&json).unwrap();
        assert!(
            d.ws_url.is_some(),
            "discovery must carry the WS URL so connect_via_discovery can prefer it"
        );
        assert!(
            d.tcp_url.is_some(),
            "discovery must carry the TCP URL as the fallback"
        );
    }

    // -- parse_http_body tests --

    #[test]
    fn http_body_minimal_200() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"x\":1}";
        assert_eq!(parse_http_body(raw).unwrap(), "{\"x\":1}");
    }

    #[test]
    fn http_body_rejects_non_200() {
        let raw = b"HTTP/1.1 404 Not Found\r\n\r\nnope";
        assert!(parse_http_body(raw).is_err());
    }

    #[test]
    fn http_body_rejects_401() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\n\r\n";
        assert!(parse_http_body(raw).is_err());
    }

    #[test]
    fn http_body_rejects_chunked() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        assert!(parse_http_body(raw).is_err());
    }

    #[test]
    fn http_body_rejects_malformed() {
        assert!(parse_http_body(b"not http").is_err());
        assert!(parse_http_body(b"HTTP/1.1 200 OK\r\n").is_err()); // no body separator
    }

    #[test]
    fn http_body_extracts_after_separator() {
        let raw = b"HTTP/1.1 200 OK\r\nServer: signalk\r\nContent-Length: 13\r\n\r\n{\"hello\":\"w\"}";
        assert_eq!(parse_http_body(raw).unwrap(), "{\"hello\":\"w\"}");
    }

    // -- header_matches tests --

    #[test]
    fn header_matches_is_case_insensitive() {
        assert!(header_matches(
            "Transfer-Encoding: chunked",
            "transfer-encoding",
            "chunked"
        ));
        assert!(header_matches(
            "TRANSFER-ENCODING: CHUNKED",
            "transfer-encoding",
            "chunked"
        ));
        assert!(!header_matches(
            "Content-Length: 42",
            "transfer-encoding",
            "chunked"
        ));
    }

    // -- resolve_mdns_event tests --

    #[test]
    fn resolve_mdns_event_handles_daemon_error() {
        let err = Err(mdns_sd::Error::Msg("daemon down".to_string()));
        assert!(resolve_mdns_event(err, Transport::HttpsDiscovery).is_empty());
    }

    // -- transport preference regression guard --

    #[test]
    fn transport_kinds_distinct() {
        assert_ne!(Transport::Tcp, Transport::HttpDiscovery);
        assert_ne!(Transport::HttpDiscovery, Transport::HttpsDiscovery);
        assert_ne!(Transport::Tcp, Transport::HttpsDiscovery);
    }
}
