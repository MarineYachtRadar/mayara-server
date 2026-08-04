use axum::{
    Json, Router, debug_handler,
    extract::{Path, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use axum_embed::ServeEmbed;
use http::Uri;
use log::{debug, trace};
use miette::Result;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc},
};
use tokio_graceful_shutdown::SubsystemHandle;
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tower::ServiceBuilder;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use utoipa::ToSchema;

#[allow(dead_code)]
mod axum_extract_ws; // Our own WebSocketUpgrade that supports compression and other features we need
use axum_extract_ws::Message;
use axum_extract_ws::WebSocket;
use axum_extract_ws::WebSocketUpgrade;

mod recordings;
mod signalk;

pub use signalk::v2::generate_openapi_json;

use mayara::{
    Cli, InterfaceApi, PACKAGE, VERSION,
    radar::{RadarError, SharedRadars},
    start_session,
};

// Embedded files from the $project/web directory
#[derive(RustEmbed, Clone)]
#[folder = "web/"]
struct Assets;

#[derive(Error, Debug)]
pub enum WebError {
    #[error(
        "Port {0} is already in use. Another instance of mayara-server may be running, or another application is using this port. Use --port to specify a different port."
    )]
    PortInUse(u16),
    #[error("Socket operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("TLS configuration error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("No private key found in {0}")]
    NoPrivateKey(String),
}

struct TlsListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = self.listener.accept().await.expect("accept failed");
            match self.acceptor.accept(stream).await {
                Ok(tls_stream) => return (tls_stream, addr),
                Err(e) => {
                    log::debug!("TLS handshake failed from {}: {}", addr, e);
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

fn load_tls_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<rustls::ServerConfig, WebError> {
    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", cert_path.display(), e)))?;
    let key_file = std::fs::File::open(key_path)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", key_path.display(), e)))?;

    let certs: Vec<_> =
        rustls_pemfile::certs(&mut io::BufReader::new(cert_file)).collect::<Result<_, _>>()?;
    let key = rustls_pemfile::private_key(&mut io::BufReader::new(key_file))?
        .ok_or_else(|| WebError::NoPrivateKey(key_path.display().to_string()))?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(config)
}

#[derive(Clone)]
pub struct Web {
    radars: SharedRadars,
    args: Cli,
    tls: bool,
    shutdown_tx: broadcast::Sender<()>,
    tx_interface_request: broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
    recording_state: recordings::RecordingState,
}

impl Web {
    pub async fn new(subsys: &SubsystemHandle, args: Cli) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);

        let tls = args.tls_cert.is_some() && args.tls_key.is_some();
        let (radars, tx_interface_request) = start_session(subsys, args.clone()).await;

        Web {
            radars,
            args,
            tls,
            shutdown_tx,
            tx_interface_request,
            recording_state: recordings::RecordingState::new(),
        }
    }

    pub async fn run(self, subsys: &mut SubsystemHandle) -> Result<(), WebError> {
        let port = self.args.port;
        // `--parent` means we serve one local chart plotter, so the web server
        // stays on the loopback interface. IPv4 loopback rather than IPv6:
        // clients asking for `localhost` fall back from ::1 to 127.0.0.1 on
        // their own, and a v6 socket cannot answer a v4 client.
        let embedded = self.args.parent.is_some();
        let (domain, addr) = if embedded {
            (
                socket2::Domain::IPV4,
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            )
        } else {
            (
                socket2::Domain::IPV6,
                SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
            )
        };
        let socket =
            socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
                .map_err(WebError::Io)?;
        if !embedded {
            socket.set_only_v6(false).map_err(WebError::Io)?;
        }
        socket.set_reuse_address(true).map_err(WebError::Io)?;
        socket.set_nonblocking(true).map_err(WebError::Io)?;
        socket.bind(&addr.into()).map_err(|e| {
            if e.kind() == io::ErrorKind::AddrInUse {
                WebError::PortInUse(port)
            } else {
                WebError::Io(e)
            }
        })?;
        socket.listen(1024).map_err(WebError::Io)?;
        let listener = TcpListener::from_std(socket.into()).map_err(WebError::Io)?;

        // Announce the bound port, not the requested one: `--port 0` asks the
        // kernel for a free port, and clients need the one we actually got.
        // Discovery is a convenience, so a failure here must not stop the web
        // server. Nothing to announce when embedded: the server is not
        // reachable from the network at all.
        let bound_addr = listener.local_addr().map_err(WebError::Io)?;
        let bound_port = bound_addr.port();
        let _advertiser = if embedded {
            None
        } else {
            match mayara::network::mdns_advertise::Advertiser::start(bound_port, self.tls) {
                Ok(advertiser) => {
                    log::info!(
                        "Advertising {} port {} on mDNS",
                        advertiser.fullname(),
                        bound_port
                    );
                    Some(advertiser)
                }
                Err(e) => {
                    log::warn!("Cannot advertise web server on mDNS: {}", e);
                    None
                }
            }
        };

        let tls_acceptor = match (&self.args.tls_cert, &self.args.tls_key) {
            (Some(cert), Some(key)) => {
                let config = load_tls_config(cert, key)?;
                Some(TlsAcceptor::from(Arc::new(config)))
            }
            _ => None,
        };

        // Wrap the embedded GUI in `Cache-Control: no-cache` so a fresh
        // mayara image's `web/gui/*` is picked up on the next normal F5
        // (browser revalidates via the ETags axum-embed already emits).
        // Without this header, the browser's heuristic cache holds onto
        // viewer.js/layout.css across mayara updates and the user has
        // to hard-refresh (Ctrl+Shift+R) to see new GUI features.
        let serve_assets = ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::overriding(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache"),
            ))
            .service(ServeEmbed::<Assets>::new());
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_tx = self.shutdown_tx.clone();

        let router = Router::new()
            .route("/", get(root_redirect))
            .route("/signalk", get(endpoints))
            .route("/quit", get(quit_handler));
        let router = signalk::v2::routes(router);
        let router = recordings::routes(router).route(
            "/signalk/{*rest}",
            get(api_fallback)
                .put(api_fallback)
                .post(api_fallback)
                .delete(api_fallback),
        );

        let router = router
            .fallback_service(serve_assets)
            .layer(TraceLayer::new_for_http())
            .with_state(self);

        let shutdown = async move { _ = shutdown_rx.recv().await };

        if let Some(acceptor) = tls_acceptor {
            let app = router.into_make_service();
            log::info!(
                "Starting HTTPS web server on {} (pid {})",
                bound_addr,
                std::process::id()
            );
            let tls_listener = TlsListener { listener, acceptor };
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    let _ = shutdown_tx.send(());
                },
                r = axum::serve(tls_listener, app).with_graceful_shutdown(shutdown) => {
                    return r.map_err(WebError::Io);
                }
            }
        } else {
            let app = router.into_make_service();
            log::info!(
                "Starting HTTP web server on {} (pid {})",
                bound_addr,
                std::process::id()
            );
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    let _ = shutdown_tx.send(());
                },
                r = axum::serve(listener, app).with_graceful_shutdown(shutdown) => {
                    return r.map_err(WebError::Io);
                }
            }
        }
        Ok(())
    }
}

// {
//   "endpoints": {
//     "v1": {
//       "version": "1.0.0-alpha1",
//       "signalk-http": "http://localhost:3000/signalk/v1/api/",
//       "signalk-ws": "ws://localhost:3000/signalk/v1/stream"
//     },
//     "v3": {
//       "version": "3.0.0",
//       "signalk-http": "http://localhost/signalk/v3/api/",
//       "signalk-ws": "ws://localhost/signalk/v3/stream",
//       "signalk-tcp": "tcp://localhost:8367"
//     }
//   },
//   "server": {
//     "id": "signalk-server-node",
//     "version": "0.1.33"
//   }
// }

#[derive(Serialize, ToSchema)]
struct Endpoints {
    endpoints: HashMap<String, Endpoint>,
    server: Server,
    /// mayara-specific upstream-navigation health, so a GUI can surface why
    /// own-ship position / AIS may be missing. Not part of the Signal K spec;
    /// ignored by standard clients.
    #[schema(value_type = Object)]
    nav: mayara::navdata::NavStatus,
}

#[derive(Serialize, ToSchema)]
struct Endpoint {
    version: String,
    #[serde(rename = "signalk-http")]
    http: String,
    #[serde(rename = "signalk-ws")]
    ws: String,
}
#[derive(Serialize, ToSchema)]
struct Server {
    version: &'static str,
    id: &'static str,
}

async fn api_fallback(uri: Uri) -> Response {
    let endpoints = signalk::v2::api_endpoint_list();
    (
        http::StatusCode::NOT_FOUND,
        format!(
            "No route matches '{}'. Valid API endpoints:\n  {}\n",
            uri.path(),
            endpoints.join("\n  ")
        ),
    )
        .into_response()
}

async fn root_redirect() -> Redirect {
    Redirect::to("/gui/")
}

/// Derive the public-facing host, HTTP scheme, and WS scheme from request headers.
///
/// Checks `X-Forwarded-Host` before `Host` so that proxies which overwrite the
/// `Host` header with the backend address (and forward the original via
/// `X-Forwarded-Host`) are handled correctly. Proxies that preserve the
/// original `Host` header (Traefik default, nginx `proxy_set_header Host
/// $host`) work equally well — `X-Forwarded-Host` is simply absent in that
/// case and `Host` is used directly.
///
/// `X-Forwarded-Proto` and `X-Forwarded-Host` are trusted unconditionally;
/// ensure mayara is only reachable from trusted proxies when running behind one.
pub(crate) fn derive_public_base(
    headers: &hyper::header::HeaderMap,
    tls: bool,
    fallback_port: u16,
) -> (String, &'static str, &'static str) {
    // X-Forwarded-Proto may be comma-chained ("https, http") when requests
    // pass through multiple proxies; the leftmost value is the original.
    // Accept any casing — some proxies emit "HTTPS" or "Https".
    let proxied_https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().eq_ignore_ascii_case("https"))
        .unwrap_or(false);

    let behind_proxy = headers.contains_key("x-forwarded-proto");

    // X-Forwarded-Host takes precedence over Host for proxies that overwrite it.
    let raw_host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(axum::http::header::HOST))
        .and_then(|v| v.to_str().ok());

    let host = match raw_host {
        Some(h) if h.contains(':') => h.to_string(),
        Some(h) if behind_proxy => {
            // Proxy present, no port in host: proxy is on the protocol's
            // default port (80/443) — omit port, let the scheme imply it.
            h.to_string()
        }
        Some(h) => format!("{}:{}", h, fallback_port),
        None => format!("localhost:{}", fallback_port),
    };

    let (http_scheme, ws_scheme) = if proxied_https || (!behind_proxy && tls) {
        ("https", "wss")
    } else {
        ("http", "ws")
    };

    (host, http_scheme, ws_scheme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderMap;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                hyper::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                hyper::header::HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn no_headers_uses_fallback() {
        let (host, http, ws) = derive_public_base(&HeaderMap::new(), false, 6502);
        assert_eq!(host, "localhost:6502");
        assert_eq!(http, "http");
        assert_eq!(ws, "ws");
    }

    #[test]
    fn no_headers_native_tls_uses_https() {
        let (host, http, ws) = derive_public_base(&HeaderMap::new(), true, 6502);
        assert_eq!(host, "localhost:6502");
        assert_eq!(http, "https");
        assert_eq!(ws, "wss");
    }

    #[test]
    fn host_with_port_preserved() {
        let h = headers(&[("host", "halos.local:4434")]);
        let (host, http, ws) = derive_public_base(&h, false, 6502);
        assert_eq!(host, "halos.local:4434");
        assert_eq!(http, "http");
        assert_eq!(ws, "ws");
    }

    #[test]
    fn host_without_port_appends_fallback_on_direct_access() {
        let h = headers(&[("host", "halos.local")]);
        let (host, _, _) = derive_public_base(&h, false, 6502);
        assert_eq!(host, "halos.local:6502");
    }

    #[test]
    fn forwarded_proto_https_sets_wss() {
        let h = headers(&[("host", "halos.local:4434"), ("x-forwarded-proto", "https")]);
        let (host, http, ws) = derive_public_base(&h, false, 6502);
        assert_eq!(host, "halos.local:4434");
        assert_eq!(http, "https");
        assert_eq!(ws, "wss");
    }

    #[test]
    fn forwarded_proto_https_no_port_in_host_omits_port() {
        // Proxy on standard port 443: Host has no port, X-Forwarded-Proto: https.
        // We must NOT append fallback_port — the URL should be wss://halos.local.
        let h = headers(&[("host", "halos.local"), ("x-forwarded-proto", "https")]);
        let (host, http, ws) = derive_public_base(&h, false, 6502);
        assert_eq!(host, "halos.local");
        assert_eq!(http, "https");
        assert_eq!(ws, "wss");
    }

    #[test]
    fn forwarded_proto_case_insensitive() {
        for proto in &["HTTPS", "Https", "hTTpS"] {
            let h = headers(&[("host", "halos.local:4434"), ("x-forwarded-proto", proto)]);
            let (_, http, ws) = derive_public_base(&h, false, 6502);
            assert_eq!(http, "https", "proto={proto}");
            assert_eq!(ws, "wss", "proto={proto}");
        }
    }

    #[test]
    fn forwarded_proto_comma_chained_takes_first() {
        // Multiple proxies can append values: "https, http"
        let h = headers(&[
            ("host", "halos.local:4434"),
            ("x-forwarded-proto", "https, http"),
        ]);
        let (_, http, ws) = derive_public_base(&h, false, 6502);
        assert_eq!(http, "https");
        assert_eq!(ws, "wss");
    }

    #[test]
    fn forwarded_proto_http_gives_ws() {
        let h = headers(&[("host", "halos.local:8080"), ("x-forwarded-proto", "http")]);
        let (_, http, ws) = derive_public_base(&h, false, 6502);
        assert_eq!(http, "http");
        assert_eq!(ws, "ws");
    }

    #[test]
    fn forwarded_proto_unknown_falls_back_to_ws() {
        let h = headers(&[("host", "halos.local:8080"), ("x-forwarded-proto", "ftp")]);
        let (_, http, ws) = derive_public_base(&h, false, 6502);
        assert_eq!(http, "http");
        assert_eq!(ws, "ws");
    }

    #[test]
    fn native_tls_without_proxy_uses_https() {
        let h = headers(&[("host", "halos.local:8443")]);
        let (host, http, ws) = derive_public_base(&h, true, 6502);
        assert_eq!(host, "halos.local:8443");
        assert_eq!(http, "https");
        assert_eq!(ws, "wss");
    }

    #[test]
    fn proxy_overrides_native_tls_flag() {
        // Proxy says http even though server has TLS certs loaded — trust proxy.
        let h = headers(&[("host", "halos.local:80"), ("x-forwarded-proto", "http")]);
        let (_, http, ws) = derive_public_base(&h, true, 6502);
        assert_eq!(http, "http");
        assert_eq!(ws, "ws");
    }

    // X-Forwarded-Host tests

    #[test]
    fn x_forwarded_host_takes_precedence_over_host() {
        // Proxy overwrites Host with backend address; original is in X-Forwarded-Host.
        let h = headers(&[
            ("host", "127.0.0.1:6502"),
            ("x-forwarded-host", "halos.local:4434"),
            ("x-forwarded-proto", "https"),
        ]);
        let (host, http, ws) = derive_public_base(&h, false, 6502);
        assert_eq!(host, "halos.local:4434");
        assert_eq!(http, "https");
        assert_eq!(ws, "wss");
    }

    #[test]
    fn x_forwarded_host_without_port_and_https_omits_port() {
        let h = headers(&[
            ("host", "127.0.0.1:6502"),
            ("x-forwarded-host", "halos.local"),
            ("x-forwarded-proto", "https"),
        ]);
        let (host, _, _) = derive_public_base(&h, false, 6502);
        assert_eq!(host, "halos.local");
    }

    #[test]
    fn x_forwarded_host_absent_falls_back_to_host() {
        // No X-Forwarded-Host → ordinary Host header is used.
        let h = headers(&[("host", "halos.local:4434"), ("x-forwarded-proto", "https")]);
        let (host, _, _) = derive_public_base(&h, false, 6502);
        assert_eq!(host, "halos.local:4434");
    }
}

async fn quit_handler(State(state): State<Web>) -> &'static str {
    let _ = state.shutdown_tx.send(());
    "bye\n"
}

async fn endpoints(State(state): State<Web>, headers: hyper::header::HeaderMap) -> Response {
    let (host, http_scheme, ws_scheme) = derive_public_base(&headers, state.tls, state.args.port);

    let mut endpoints = Endpoints {
        endpoints: HashMap::new(),
        server: Server {
            version: VERSION,
            id: PACKAGE,
        },
        nav: mayara::navdata::nav_status(&state.args),
    };
    endpoints.endpoints.insert(
        "v2".to_string(),
        Endpoint {
            version: "v2".to_string(),
            http: format!("{}://{}{}", http_scheme, host, signalk::v2::BASE_URI),
            ws: format!("{}://{}{}", ws_scheme, host, signalk::v2::CONTROL_URI),
        },
    );

    Json(endpoints).into_response()
}

#[derive(Deserialize)]
struct WebSocketHandlerParameters {
    id: String,
}

#[debug_handler]
async fn spokes_handler(
    State(state): State<Web>,
    Path(params): Path<WebSocketHandlerParameters>,
    ws: WebSocketUpgrade,
) -> Response {
    debug!("stream request for {}", params.id);

    match state.radars.get_by_key(&params.id) {
        Some(radar) => {
            // Exit idle before subscribing so the frame already in flight
            // when the WS connects gets decoded, not the next one. The
            // data_loop's 5s periodic re-check would catch this eventually
            // but the window between subscribe and the first revolution is
            // exactly what users see as "PPI took a couple seconds to fill"
            // — better to flip synchronously here.
            //
            // Wake twice: before AND after subscribe. The data_loop's 5 s
            // tick could race between the first wake_up and message_tx.
            // subscribe(); it sees power=Standby and receiver_count==0
            // (subscribe hasn't completed yet) and flips us back to idle.
            // The post-subscribe wake_up wins this race — by the time it
            // runs, subscribe has incremented receiver_count, so even if
            // the next tick fires immediately afterwards it computes
            // should_idle() = false and leaves the flag alone.
            radar.wake_up();
            let shutdown_rx = state.shutdown_tx.subscribe();
            let radar_message_rx = radar.message_tx.subscribe();
            radar.wake_up();
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            let ws = if state.args.no_websocket_compression {
                ws
            } else {
                ws.permessage_deflate()
            };
            ws.on_upgrade(move |socket| spokes_stream(socket, radar_message_rx, shutdown_rx))
        }
        None => RadarError::NoSuchRadar(params.id).into_response(),
    }
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn spokes_stream(
    mut socket: WebSocket,
    mut radar_message_rx: tokio::sync::broadcast::Receiver<bytes::Bytes>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Shutdown of websocket");
                break;
            },
            r = radar_message_rx.recv() => {
                match r {
                    Ok(message) => {
                        let len = message.len();
                        // `Message::Binary` already takes `Bytes`, so this is
                        // a refcount bump rather than a memcpy of `len` bytes.
                        let ws_message = Message::Binary(message);
                        if let Err(e) = socket.send(ws_message).await {
                            debug!("Error on send to websocket: {}", e);
                            break;
                        }
                        trace!("Sent radar message {} bytes", len);
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        debug!("Spoke stream lagged by {} messages, resuming", n);
                    },
                    Err(e) => {
                        debug!("Error on RadarMessage channel: {}", e);
                        break;
                    }
                }
            }
        }
    }
}
