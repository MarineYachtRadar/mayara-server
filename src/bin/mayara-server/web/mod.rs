use axum::{
    Json, Router, debug_handler,
    extract::{Path, State},
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
    net::{IpAddr, Ipv6Addr, SocketAddr},
    str::FromStr,
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc},
};
use tokio_graceful_shutdown::SubsystemHandle;
use tokio_rustls::{TlsAcceptor, server::TlsStream};
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

    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), WebError> {
        let port = self.args.port;
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )
        .map_err(WebError::Io)?;
        socket.set_only_v6(false).map_err(WebError::Io)?;
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

        let tls_acceptor = match (&self.args.tls_cert, &self.args.tls_key) {
            (Some(cert), Some(key)) => {
                let config = load_tls_config(cert, key)?;
                Some(TlsAcceptor::from(Arc::new(config)))
            }
            _ => None,
        };

        let serve_assets = ServeEmbed::<Assets>::new();
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
                "Starting HTTPS web server on port {} (pid {})",
                port,
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
                "Starting HTTP web server on port {} (pid {})",
                port,
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

async fn quit_handler(State(state): State<Web>) -> &'static str {
    let _ = state.shutdown_tx.send(());
    "bye\n"
}

async fn endpoints(State(state): State<Web>, headers: hyper::header::HeaderMap) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };
    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.args.port
    );

    let mut endpoints = Endpoints {
        endpoints: HashMap::new(),
        server: Server {
            version: VERSION,
            id: PACKAGE,
        },
    };
    let (http_scheme, ws_scheme) = if state.tls {
        ("https", "wss")
    } else {
        ("http", "ws")
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
            let shutdown_rx = state.shutdown_tx.subscribe();
            let radar_message_rx = radar.message_tx.subscribe();
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.permessage_deflate()
                .on_upgrade(move |socket| spokes_stream(socket, radar_message_rx, shutdown_rx))
        }
        None => RadarError::NoSuchRadar(params.id).into_response(),
    }
}

/// Actual websocket statemachine (one will be spawned per connection)

async fn spokes_stream(
    mut socket: WebSocket,
    mut radar_message_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
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
                        let ws_message = Message::Binary(message.into());
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
