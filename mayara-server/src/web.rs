use axum::{
    debug_handler,
    extract::{ConnectInfo, Path, State},
    http::{header, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, put},
    Json, Router,
};
use axum_embed::ServeEmbed;
use hyper;
use log::{debug, trace};
use miette::Result;
#[cfg(not(feature = "dev"))]
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
#[cfg(feature = "dev")]
use tower_http::services::ServeDir;
use std::{
    collections::{BTreeMap, HashMap},
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{net::TcpListener, sync::broadcast};
use tokio_graceful_shutdown::SubsystemHandle;

mod axum_fix;

use axum_fix::{Message, WebSocket, WebSocketUpgrade};

use mayara_server::{
    radar::{Legend, RadarError, RadarInfo},
    storage::{AppDataKey, SharedStorage, create_shared_storage},
    ProtoAssets, Session,
};

// ARPA types from mayara-core for v6 API
use mayara_core::arpa::{ArpaSettings, ArpaTarget};

// Guard zone types from mayara-core
use mayara_core::guard_zones::{GuardZone, GuardZoneStatus};

// Trail types from mayara-core
use mayara_core::trails::{TrailData, TrailSettings};

// Dual-range types from mayara-core
use mayara_core::dual_range::{DualRangeConfig, DualRangeState as CoreDualRangeState};

// RadarEngine from mayara-core - unified feature processor management
use mayara_core::engine::RadarEngine;

// Capability types from mayara-core for v5 API
use mayara_core::capabilities::{builder::build_capabilities_from_model_with_key, RadarStateV5, SupportedFeature};
use mayara_core::models;

// Standalone Radar API v2 paths (matches SignalK Radar API v2 structure)
const RADARS_URI: &str = "/v2/api/radars";
const RADAR_CAPABILITIES_URI: &str = "/v2/api/radars/{radar_id}/capabilities";
const RADAR_STATE_URI: &str = "/v2/api/radars/{radar_id}/state";
const SPOKES_URI: &str = "/v2/api/radars/{radar_id}/spokes";
const CONTROL_URI: &str = "/v2/api/radars/{radar_id}/control";
const CONTROL_VALUE_URI: &str = "/v2/api/radars/{radar_id}/controls/{control_id}";
const TARGETS_URI: &str = "/v2/api/radars/{radar_id}/targets";
const TARGET_URI: &str = "/v2/api/radars/{radar_id}/targets/{target_id}";
const ARPA_SETTINGS_URI: &str = "/v2/api/radars/{radar_id}/arpa/settings";
// Guard zones
const GUARD_ZONES_URI: &str = "/v2/api/radars/{radar_id}/guardZones";
const GUARD_ZONE_URI: &str = "/v2/api/radars/{radar_id}/guardZones/{zone_id}";
// Trails
const TRAILS_URI: &str = "/v2/api/radars/{radar_id}/trails";
const TRAIL_URI: &str = "/v2/api/radars/{radar_id}/trails/{target_id}";
const TRAIL_SETTINGS_URI: &str = "/v2/api/radars/{radar_id}/trails/settings";
// Dual-range
const DUAL_RANGE_URI: &str = "/v2/api/radars/{radar_id}/dualRange";
const DUAL_RANGE_SPOKES_URI: &str = "/v2/api/radars/{radar_id}/dualRange/spokes";

// Non-radar endpoints
const INTERFACES_URI: &str = "/v2/api/interfaces";

// SignalK applicationData API (for settings persistence)
const APP_DATA_URI: &str = "/signalk/v1/applicationData/global/{appid}/{version}/{*key}";

#[cfg(not(feature = "dev"))]
#[derive(RustEmbed, Clone)]
#[folder = "$OUT_DIR/gui/"]
struct Assets;

#[cfg(not(feature = "dev"))]
#[derive(RustEmbed, Clone)]
#[folder = "$OUT_DIR/web/"]
struct ProtoWebAssets;

/// Rustdoc HTML documentation - served at /rustdoc/
/// Generate with: cargo doc --no-deps -p mayara-core -p mayara-server
/// Only available when built with `rustdoc` feature.
#[cfg(feature = "rustdoc")]
#[derive(RustEmbed, Clone)]
#[folder = "../target/doc/"]
struct RustdocAssets;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Socket operation failed")]
    Io(#[from] io::Error),
}

/// Shared RadarEngine for all feature processors (ARPA, GuardZones, Trails, DualRange)
type SharedEngine = Arc<RwLock<RadarEngine>>;

#[derive(Clone)]
pub struct Web {
    session: Session,
    shutdown_tx: broadcast::Sender<()>,
    /// Unified engine for all radar feature processors
    engine: SharedEngine,
    /// Local storage for applicationData API
    storage: SharedStorage,
}

impl Web {
    pub fn new(session: Session) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);

        Web {
            session,
            shutdown_tx,
            engine: Arc::new(RwLock::new(RadarEngine::new())),
            storage: create_shared_storage(),
        }
    }

    /// Ensure a radar exists in the engine (lazy initialization)
    /// The engine uses "virtual" radars since actual controller management
    /// is done by the Session. We just need the feature processors.
    fn ensure_radar_in_engine(&self, radar_id: &str) {
        let mut engine = self.engine.write().unwrap();
        if !engine.contains(radar_id) {
            // Add a Furuno radar as placeholder - the brand doesn't matter
            // since we're only using the feature processors (ARPA, GuardZones, etc.)
            // not the controller functionality
            engine.add_furuno(radar_id, "0.0.0.0");
        }
    }

    /// Ensure radar exists in engine with model info (needed for dual-range)
    fn ensure_radar_in_engine_with_model(&self, radar_id: &str, model_name: &str) {
        let mut engine = self.engine.write().unwrap();
        if !engine.contains(radar_id) {
            engine.add_furuno(radar_id, "0.0.0.0");
        }
        // Set model info (creates dual_range controller if model supports it)
        engine.set_model_info(radar_id, model_name);
    }

    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), WebError> {
        let port = self.session.read().unwrap().args.port.clone();
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port))
                .await
                .map_err(|e| WebError::Io(e))?;

        // In dev mode, serve files from filesystem for live reload
        // In production, use embedded files
        // Note: CARGO_MANIFEST_DIR is /home/dirk/dev/mayara-server/mayara-server
        // So we go up two levels to reach /home/dirk/dev/mayara-gui
        #[cfg(feature = "dev")]
        let serve_assets = ServeDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../mayara-gui"));
        #[cfg(not(feature = "dev"))]
        let serve_assets = ServeEmbed::<Assets>::new();

        #[cfg(feature = "dev")]
        let proto_web_assets = ServeDir::new(concat!(env!("OUT_DIR"), "/web"));
        #[cfg(not(feature = "dev"))]
        let proto_web_assets = ServeEmbed::<ProtoWebAssets>::new();

        let proto_assets = ServeEmbed::<ProtoAssets>::new();
        #[cfg(feature = "rustdoc")]
        let rustdoc_assets = ServeEmbed::<RustdocAssets>::new();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_tx = self.shutdown_tx.clone(); // Clone as self used in with_state() and with_graceful_shutdown() below

        let app = Router::new()
            // Standalone Radar API v1 (matches SignalK structure for GUI compatibility)
            .route(RADARS_URI, get(get_radars))
            .route(RADAR_CAPABILITIES_URI, get(get_radar_capabilities))
            .route(RADAR_STATE_URI, get(get_radar_state))
            .route(SPOKES_URI, get(spokes_handler))
            .route(CONTROL_URI, get(control_handler))
            .route(CONTROL_VALUE_URI, put(set_control_value))
            .route(TARGETS_URI, get(get_targets).post(acquire_target))
            .route(TARGET_URI, delete(cancel_target))
            .route(ARPA_SETTINGS_URI, get(get_arpa_settings).put(set_arpa_settings))
            // Guard zones
            .route(GUARD_ZONES_URI, get(get_guard_zones).post(create_guard_zone))
            .route(GUARD_ZONE_URI, get(get_guard_zone).put(update_guard_zone).delete(delete_guard_zone))
            // Trails
            .route(TRAILS_URI, get(get_all_trails).delete(clear_all_trails))
            .route(TRAIL_URI, get(get_trail).delete(clear_trail))
            .route(TRAIL_SETTINGS_URI, get(get_trail_settings).put(set_trail_settings))
            // Dual-range
            .route(DUAL_RANGE_URI, get(get_dual_range).put(set_dual_range))
            .route(DUAL_RANGE_SPOKES_URI, get(dual_range_spokes_handler))
            // Other endpoints
            .route(INTERFACES_URI, get(get_interfaces))
            // SignalK applicationData API
            .route(APP_DATA_URI, get(get_app_data).put(put_app_data).delete(delete_app_data))
            // Apply no-cache middleware to all API routes
            .layer(middleware::from_fn(no_cache_middleware))
            // Static assets (no middleware - can be cached)
            .nest_service("/protobuf", proto_web_assets)
            .nest_service("/proto", proto_assets);

        // Conditionally add rustdoc assets if feature enabled
        #[cfg(feature = "rustdoc")]
        let app = app.nest_service("/rustdoc", rustdoc_assets);

        let app = app.fallback_service(serve_assets)
            .with_state(self)
            .into_make_service_with_connect_info::<SocketAddr>();

        #[cfg(feature = "dev")]
        log::info!("Starting HTTP web server on port {} (DEV MODE - serving from filesystem)", port);
        #[cfg(not(feature = "dev"))]
        log::info!("Starting HTTP web server on port {}", port);

        tokio::select! { biased;
            _ = subsys.on_shutdown_requested() => {
                let _ = shutdown_tx.send(());
            },
            r = axum::serve(listener, app)
                    .with_graceful_shutdown(
                        async move {
                            _ = shutdown_rx.recv().await;
                        }
                    ) => {
                return r.map_err(|e| WebError::Io(e));
            }
        }
        Ok(())
    }
}

/// Middleware to add no-cache headers to API responses
async fn no_cache_middleware(request: axum::http::Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-cache, no-store, must-revalidate".parse().unwrap(),
    );
    response
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RadarApi {
    id: String,
    name: String,
    brand: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    spokes_per_revolution: u16,
    max_spoke_len: u16,
    stream_url: String,
    control_url: String,
    legend: Legend,
}

impl RadarApi {
    fn new(
        id: String,
        name: String,
        brand: String,
        model: Option<String>,
        spokes_per_revolution: u16,
        max_spoke_len: u16,
        stream_url: String,
        control_url: String,
        legend: Legend,
    ) -> Self {
        RadarApi {
            id,
            name,
            brand,
            model,
            spokes_per_revolution,
            max_spoke_len,
            stream_url,
            control_url,
            legend,
        }
    }
}

// SignalK Radar API response format:
//    {"radar-0":{"id":"radar-0","name":"Navico","spokes_per_revolution":2048,"maxSpokeLen":1024,"streamUrl":"ws://localhost:3001/radars/radar-0/spokes"}}
//
#[debug_handler]
async fn get_radars(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: hyper::header::HeaderMap,
) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };

    debug!("Radar state request from {} for host '{}'", addr, host);

    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.session.read().unwrap().args.port
    );

    debug!("target host = '{}'", host);

    let mut api: HashMap<String, RadarApi> = HashMap::new();
    for info in state
        .session
        .read()
        .unwrap()
        .radars
        .as_ref()
        .unwrap()
        .get_active()
        .clone()
    {
        let legend = &info.legend;
        let id = format!("radar-{}", info.id);
        let stream_url = format!("ws://{}/v2/api/radars/{}/spokes", host, id);
        let control_url = format!("ws://{}/v2/api/radars/{}/control", host, id);
        let name = info.controls.user_name();
        let v = RadarApi::new(
            id.to_owned(),
            name,
            info.brand.to_string(),
            info.controls.model_name(),
            info.spokes_per_revolution,
            info.max_spoke_len,
            stream_url,
            control_url,
            legend.clone(),
        );

        api.insert(id.to_owned(), v);
    }
    Json(api).into_response()
}

/// Parameters for radar-specific endpoints
#[derive(Deserialize)]
struct RadarIdParam {
    radar_id: String,
}

/// Convert server Brand to mayara_core Brand for model lookup
fn to_core_brand(brand: mayara_server::Brand) -> mayara_core::Brand {
    match brand {
        mayara_server::Brand::Furuno => mayara_core::Brand::Furuno,
        mayara_server::Brand::Navico => mayara_core::Brand::Navico,
        mayara_server::Brand::Raymarine => mayara_core::Brand::Raymarine,
        mayara_server::Brand::Garmin => mayara_core::Brand::Garmin,
    }
}

/// GET /v2/api/radars/{radar_id}/capabilities
/// Returns the capability manifest for a specific radar (v5 API format)
#[debug_handler]
async fn get_radar_capabilities(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("Capabilities request for radar {}", params.radar_id);

    // Extract data from session inside a block to drop the lock before await
    let build_args = {
        let session = state.session.read().unwrap();
        let radars = session.radars.as_ref().unwrap();

        match radars.get_by_id(&params.radar_id) {
            Some(info) => {
                let core_brand = to_core_brand(info.brand);
                let model_name = info.controls.model_name();

                // Look up model in mayara-core database
                let model_info = model_name
                    .as_deref()
                    .and_then(|m| models::get_model(core_brand, m))
                    .unwrap_or(&models::UNKNOWN_MODEL);

                // Declare supported features for standalone server
                let mut supported_features = vec![
                    SupportedFeature::Arpa,
                    SupportedFeature::GuardZones,
                    SupportedFeature::Trails,
                ];

                // Add DualRange if the radar supports it
                if model_info.has_dual_range {
                    supported_features.push(SupportedFeature::DualRange);
                }

                Some((
                    model_info.clone(),
                    params.radar_id.clone(),
                    info.key(), // Persistent key for installation settings
                    supported_features,
                    info.spokes_per_revolution,
                    info.max_spoke_len,
                ))
            }
            None => None,
        }
    }; // session lock released here

    match build_args {
        Some((model_info, radar_id, radar_key, supported_features, spokes_per_revolution, max_spoke_len)) => {
            // Use spawn_blocking to run capability building on a thread with larger stack
            // This avoids stack overflow in debug builds where ControlDefinition structs
            // (328 bytes each) can overflow the default 2MB async task stack
            let capabilities = tokio::task::spawn_blocking(move || {
                build_capabilities_from_model_with_key(
                    &model_info,
                    &radar_id,
                    Some(&radar_key), // Persistent key for installation settings storage
                    supported_features,
                    spokes_per_revolution,
                    max_spoke_len,
                )
            })
            .await
            .expect("spawn_blocking task failed");

            Json(capabilities).into_response()
        }
        None => RadarError::NoSuchRadar(params.radar_id.to_string()).into_response(),
    }
}

/// GET /v2/api/radars/{radar_id}/state
/// Returns the current state of a radar (v5 API format)
#[debug_handler]
async fn get_radar_state(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("State request for radar {}", params.radar_id);

    let session = state.session.read().unwrap();
    let radars = session.radars.as_ref().unwrap();

    match radars.get_by_id(&params.radar_id) {
        Some(info) => {
            // Build the state dynamically from all registered controls
            // Use BTreeMap for stable JSON key ordering
            let mut controls = BTreeMap::new();

            // Helper to format a control value for the API response
            fn format_control_value(control_id: &str, control: &mayara_server::settings::Control) -> serde_json::Value {
                // Special handling for power/status - return string enum
                if control_id == "power" {
                    let status_val = control.value.unwrap_or(0.0) as i32;
                    let status_str = match status_val {
                        0 => "off",
                        1 => "standby",
                        2 => "transmit",
                        3 => "warming",
                        _ => "standby",
                    };
                    return serde_json::json!(status_str);
                }

                // Controls with auto mode (compound controls)
                if control.auto.is_some() {
                    let mode = if control.auto.unwrap_or(false) { "auto" } else { "manual" };
                    let value = control.value.unwrap_or(0.0);
                    // Return integer for most controls, but preserve decimals for bearing alignment
                    if control_id == "bearingAlignment" {
                        return serde_json::json!({"mode": mode, "value": value});
                    }
                    return serde_json::json!({"mode": mode, "value": value as i32});
                }

                // Controls with enabled flag (like FTC, DopplerMode)
                if control.enabled.is_some() {
                    let enabled = control.enabled.unwrap_or(false);
                    let value = control.value.unwrap_or(0.0) as i32;
                    return serde_json::json!({"enabled": enabled, "value": value});
                }

                // String controls (model name, serial number, etc.)
                if let Some(ref desc) = control.description {
                    return serde_json::json!(desc);
                }

                // Simple numeric controls
                let value = control.value.unwrap_or(0.0);
                // Return as integer for most, decimal for bearing alignment
                if control_id == "bearingAlignment" {
                    serde_json::json!(value)
                } else {
                    serde_json::json!(value as i32)
                }
            }

            // Iterate over all controls the radar has registered
            for (control_id, control) in info.controls.get_all() {
                // Skip internal-only controls
                if control_id == "userName" || control_id == "modelName" {
                    continue;
                }
                controls.insert(control_id.clone(), format_control_value(&control_id, &control));
            }

            // Determine status string for top-level field
            let status = controls
                .get("power")
                .and_then(|v| v.as_str())
                .unwrap_or("standby")
                .to_string();

            let state_v5 = RadarStateV5 {
                id: params.radar_id.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                status,
                controls,
                disabled_controls: vec![],
            };

            Json(state_v5).into_response()
        }
        None => RadarError::NoSuchRadar(params.radar_id.to_string()).into_response(),
    }
}

#[debug_handler]
async fn get_interfaces(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: hyper::header::HeaderMap,
) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };

    debug!("Interface state request from {} for host '{}'", addr, host);

    // Return the locator status from the core locator
    let status = state.session.read().unwrap().locator_status.clone();
    Json(status).into_response()
}

#[debug_handler]
async fn spokes_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(params): Path<RadarIdParam>,
    ws: WebSocketUpgrade,
) -> Response {
    debug!("spokes request from {} for {}", addr, params.radar_id);

    // Disable compression temporarily to debug browser WebSocket issues
    let ws = ws.accept_compression(false);

    match state
        .session
        .read()
        .unwrap()
        .radars
        .as_ref()
        .unwrap()
        .get_by_id(&params.radar_id)
        .clone()
    {
        Some(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();
            let radar_message_rx = radar.message_tx.subscribe();
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| spokes_stream(socket, radar_message_rx, shutdown_rx))
        }
        None => RadarError::NoSuchRadar(params.radar_id.to_string()).into_response(),
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
                    Err(e) => {
                        debug!("Error on RadarMessage channel: {}", e);
                        break;
                    }
                }
            }
        }
    }
}

#[debug_handler]
async fn control_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(params): Path<RadarIdParam>,
    ws: WebSocketUpgrade,
) -> Response {
    debug!("control request from {} for {}", addr, params.radar_id);

    let ws = ws.accept_compression(true);

    match state
        .session
        .read()
        .unwrap()
        .radars
        .as_ref()
        .unwrap()
        .get_by_id(&params.radar_id)
        .clone()
    {
        Some(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();

            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| control_stream(socket, radar, shutdown_rx))
        }
        None => RadarError::NoSuchRadar(params.radar_id.to_string()).into_response(),
    }
}

/// Actual websocket statemachine (one will be spawned per connection)

async fn control_stream(
    mut socket: WebSocket,
    radar: RadarInfo,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let mut broadcast_control_rx = radar.all_clients_rx();
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(60);

    if radar
        .controls
        .send_all_controls(reply_tx.clone())
        .await
        .is_err()
    {
        return;
    }

    debug!("Started /control websocket");

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Shutdown of /control websocket");
                break;
            },
            // this is where we receive directed control messages meant just for us, they
            // are either error replies for an invalid control value or the full list of
            // controls.
            r = reply_rx.recv() => {
                match r {
                    Some(message) => {
                        let message = serde_json::to_string(&message).unwrap();
                        log::trace!("Sending {:?}", message);
                        let ws_message = Message::Text(message.into());

                        if let Err(e) = socket.send(ws_message).await {
                            log::error!("send to websocket client: {e}");
                            break;
                        }

                    },
                    None => {
                        log::error!("Error on Control channel");
                        break;
                    }
                }
            },
            r = broadcast_control_rx.recv() => {
                match r {
                    Ok(message) => {
                        let message: String = serde_json::to_string(&message).unwrap();
                        log::debug!("Sending {:?}", message);
                        let ws_message = Message::Text(message.into());

                        if let Err(e) = socket.send(ws_message).await {
                            log::error!("send to websocket client: {e}");
                            break;
                        }


                    },
                    Err(e) => {
                        log::error!("Error on Control channel: {e}");
                        break;
                    }
                }
            },
            // receive control values from the client
            r = socket.recv() => {
                match r {
                    Some(Ok(message)) => {
                        match message {
                            Message::Text(message) => {
                                if let Ok(control_value) = serde_json::from_str(&message) {
                                    log::debug!("Received ControlValue {:?}", control_value);
                                    let _ = radar.controls.process_client_request(control_value, reply_tx.clone()).await;
                                } else {
                                    log::error!("Unknown JSON string '{}'", message);
                                }

                            },
                            _ => {
                                debug!("Dropping unexpected message {:?}", message);
                            }
                        }

                    },
                    None => {
                        // Stream has closed
                        log::debug!("Control websocket closed");
                        break;
                    }
                    r => {
                        log::error!("Error reading websocket: {:?}", r);
                        break;
                    }
                }
            }
        }
    }
}

// =============================================================================
// Control Value REST API Handler
// =============================================================================

/// Parameters for control-specific endpoints
#[derive(Deserialize)]
struct RadarControlIdParam {
    radar_id: String,
    control_id: String,
}

/// Request body for PUT /radars/{id}/controls/{control_id}
#[derive(Deserialize)]
struct SetControlRequest {
    value: serde_json::Value,
}

/// PUT /v2/api/radars/{radar_id}/controls/{control_id}
/// Sets a control value on the radar
#[debug_handler]
async fn set_control_value(
    State(state): State<Web>,
    Path(params): Path<RadarControlIdParam>,
    Json(request): Json<SetControlRequest>,
) -> Response {
    use mayara_server::settings::ControlValue;

    debug!(
        "PUT control {} = {:?} for radar {}",
        params.control_id, request.value, params.radar_id
    );

    // Get the radar info and control type without holding the lock across await
    let (controls, control_type) = {
        let session = state.session.read().unwrap();
        let radars = session.radars.as_ref().unwrap();

        match radars.get_by_id(&params.radar_id) {
            Some(radar) => {
                // Look up the control by name
                let control = match radar.controls.get_by_name(&params.control_id) {
                    Some(c) => c,
                    None => {
                        // Debug: list all available controls
                        let available: Vec<String> = radar.controls.get_all()
                            .iter()
                            .map(|(k, _)| k.clone())
                            .collect();
                        log::warn!(
                            "Control '{}' not found. Available controls: {:?}",
                            params.control_id,
                            available
                        );
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("Unknown control: {}", params.control_id),
                        )
                            .into_response();
                    }
                };

                // Parse the value - handle compound controls {mode, value} and simple values
                let (value_str, auto) = match &request.value {
                    serde_json::Value::String(s) => {
                        // Try to normalize enum values using core definition
                        let normalized = if let Some(index) = control.enum_value_to_index(s) {
                            control.index_to_enum_value(index).unwrap_or_else(|| s.clone())
                        } else {
                            s.clone()
                        };
                        (normalized, None)
                    },
                    serde_json::Value::Number(n) => (n.to_string(), None),
                    serde_json::Value::Bool(b) => (if *b { "1" } else { "0" }.to_string(), None),
                    serde_json::Value::Object(obj) => {
                        // Check if this is a dopplerMode compound control {"enabled": bool, "mode": "target"|"rain"}
                        if params.control_id == "dopplerMode" {
                            let enabled = obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                            let mode_str = obj.get("mode").and_then(|v| v.as_str()).unwrap_or("target");
                            // Convert mode string to numeric: "target" = 0, "rain" = 1
                            let mode_val = match mode_str {
                                "target" | "targets" => 0,
                                "rain" => 1,
                                _ => 0,
                            };
                            // Pass enabled state via 'auto' field (repurposed), mode as value
                            (mode_val.to_string(), Some(enabled))
                        } else {
                            // Standard compound control: {"mode": "auto"|"manual", "value": N}
                            let mode = obj.get("mode").and_then(|v| v.as_str()).unwrap_or("manual");
                            let auto = Some(mode == "auto");
                            let value = obj.get("value")
                                .map(|v| match v {
                                    serde_json::Value::Number(n) => n.to_string(),
                                    serde_json::Value::String(s) => s.clone(),
                                    _ => v.to_string(),
                                })
                                .unwrap_or_default();
                            (value, auto)
                        }
                    },
                    _ => (request.value.to_string(), None),
                };

                let mut control_value = ControlValue::new(control.id(), value_str);
                control_value.auto = auto;
                (radar.controls.clone(), control_value)
            }
            None => {
                return RadarError::NoSuchRadar(params.radar_id.to_string()).into_response();
            }
        }
    };
    // Lock is released here

    // Create a channel for the reply
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(1);

    // Send the control request
    if let Err(e) = controls
        .process_client_request(control_type, reply_tx)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to send control: {:?}", e))
            .into_response();
    }

    // Wait briefly for a reply (error response)
    // Most controls don't reply on success, only on error
    tokio::select! {
        reply = reply_rx.recv() => {
            match reply {
                Some(cv) if cv.error.is_some() => {
                    return (StatusCode::BAD_REQUEST, cv.error.unwrap()).into_response();
                }
                _ => {}
            }
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
            // No error reply within timeout, assume success
        }
    }

    StatusCode::OK.into_response()
}

// =============================================================================
// ARPA Target API Handlers
// =============================================================================

/// Parameters for target-specific endpoints (includes target_id)
#[derive(Deserialize)]
struct RadarTargetIdParam {
    radar_id: String,
    target_id: u32,
}

/// Response for GET /radars/{id}/targets
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetListResponse {
    radar_id: String,
    timestamp: String,
    targets: Vec<ArpaTarget>,
}

/// Request for POST /radars/{id}/targets (manual acquisition)
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcquireTargetRequest {
    bearing: f64,
    distance: f64,
}

/// Response for POST /radars/{id}/targets
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AcquireTargetResponse {
    success: bool,
    target_id: Option<u32>,
    error: Option<String>,
}

/// GET /radars/{radar_id}/targets - List all tracked ARPA targets
#[debug_handler]
async fn get_targets(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("GET targets for radar {}", params.radar_id);

    let engine = state.engine.read().unwrap();
    let targets = engine.get_targets(&params.radar_id);

    let response = TargetListResponse {
        radar_id: params.radar_id,
        timestamp: chrono::Utc::now().to_rfc3339(),
        targets,
    };

    Json(response).into_response()
}

/// POST /radars/{radar_id}/targets - Manual target acquisition
#[debug_handler]
async fn acquire_target(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
    Json(request): Json<AcquireTargetRequest>,
) -> Response {
    debug!(
        "POST acquire target for radar {} at bearing={}, distance={}",
        params.radar_id, request.bearing, request.distance
    );

    // Validate bearing
    if request.bearing < 0.0 || request.bearing >= 360.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(AcquireTargetResponse {
                success: false,
                target_id: None,
                error: Some("bearing must be 0-360".to_string()),
            }),
        )
            .into_response();
    }

    // Validate distance
    if request.distance <= 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(AcquireTargetResponse {
                success: false,
                target_id: None,
                error: Some("distance must be positive".to_string()),
            }),
        )
            .into_response();
    }

    // Ensure radar exists in engine
    state.ensure_radar_in_engine(&params.radar_id);

    // Current timestamp in milliseconds
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let mut engine = state.engine.write().unwrap();
    match engine.acquire_target(&params.radar_id, request.bearing, request.distance, timestamp) {
        Some(target_id) => {
            debug!("Acquired target {} on radar {}", target_id, params.radar_id);
            Json(AcquireTargetResponse {
                success: true,
                target_id: Some(target_id),
                error: None,
            })
            .into_response()
        }
        None => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(AcquireTargetResponse {
                success: false,
                target_id: None,
                error: Some("max targets reached".to_string()),
            }),
        )
            .into_response(),
    }
}

/// DELETE /radars/{radar_id}/targets/{target_id} - Cancel target tracking
#[debug_handler]
async fn cancel_target(
    State(state): State<Web>,
    Path(params): Path<RadarTargetIdParam>,
) -> Response {
    debug!(
        "DELETE target {} on radar {}",
        params.target_id, params.radar_id
    );

    let mut engine = state.engine.write().unwrap();
    if engine.cancel_target(&params.radar_id, params.target_id) {
        debug!("Cancelled target {} on radar {}", params.target_id, params.radar_id);
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "Target not found").into_response()
    }
}

/// GET /radars/{radar_id}/arpa/settings - Get ARPA settings
#[debug_handler]
async fn get_arpa_settings(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("GET ARPA settings for radar {}", params.radar_id);

    let engine = state.engine.read().unwrap();
    let settings = engine
        .get_arpa_settings(&params.radar_id)
        .unwrap_or_default();

    Json(settings).into_response()
}

/// PUT /radars/{radar_id}/arpa/settings - Update ARPA settings
#[debug_handler]
async fn set_arpa_settings(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
    Json(settings): Json<ArpaSettings>,
) -> Response {
    debug!("PUT ARPA settings for radar {}", params.radar_id);

    // Ensure radar exists in engine
    state.ensure_radar_in_engine(&params.radar_id);

    let mut engine = state.engine.write().unwrap();
    engine.set_arpa_settings(&params.radar_id, settings);
    debug!("Updated ARPA settings for radar {}", params.radar_id);

    StatusCode::OK.into_response()
}

// =============================================================================
// SignalK applicationData API Handlers
// =============================================================================

/// Parameters for applicationData endpoints
#[derive(Deserialize)]
struct AppDataParams {
    appid: String,
    version: String,
    key: String,
}

/// GET /signalk/v1/applicationData/global/{appid}/{version}/{key} - Get stored data
#[debug_handler]
async fn get_app_data(
    State(state): State<Web>,
    Path(params): Path<AppDataParams>,
) -> Response {
    debug!(
        "GET applicationData: {}/{}/{}",
        params.appid, params.version, params.key
    );

    let key = AppDataKey::new(&params.appid, &params.version, &params.key);
    let mut storage = state.storage.write().unwrap();

    match storage.get(&key) {
        Some(value) => Json(value).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// PUT /signalk/v1/applicationData/global/{appid}/{version}/{key} - Store data
#[debug_handler]
async fn put_app_data(
    State(state): State<Web>,
    Path(params): Path<AppDataParams>,
    Json(value): Json<serde_json::Value>,
) -> Response {
    debug!(
        "PUT applicationData: {}/{}/{}",
        params.appid, params.version, params.key
    );

    let key = AppDataKey::new(&params.appid, &params.version, &params.key);
    let mut storage = state.storage.write().unwrap();

    match storage.put(&key, value) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// DELETE /signalk/v1/applicationData/global/{appid}/{version}/{key} - Delete stored data
#[debug_handler]
async fn delete_app_data(
    State(state): State<Web>,
    Path(params): Path<AppDataParams>,
) -> Response {
    debug!(
        "DELETE applicationData: {}/{}/{}",
        params.appid, params.version, params.key
    );

    let key = AppDataKey::new(&params.appid, &params.version, &params.key);
    let mut storage = state.storage.write().unwrap();

    match storage.delete(&key) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// =============================================================================
// Guard Zone API Handlers
// =============================================================================

/// Parameters for zone-specific endpoints
#[derive(Deserialize)]
struct RadarZoneIdParam {
    radar_id: String,
    zone_id: u32,
}

/// Response for GET /radars/{id}/guardZones
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GuardZoneListResponse {
    radar_id: String,
    zones: Vec<GuardZoneStatus>,
}

/// GET /radars/{radar_id}/guardZones - List all guard zones
#[debug_handler]
async fn get_guard_zones(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("GET guard zones for radar {}", params.radar_id);

    let engine = state.engine.read().unwrap();
    let zones = engine.get_guard_zones(&params.radar_id);

    let response = GuardZoneListResponse {
        radar_id: params.radar_id,
        zones,
    };

    Json(response).into_response()
}

/// POST /radars/{radar_id}/guardZones - Create a new guard zone
#[debug_handler]
async fn create_guard_zone(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
    Json(zone): Json<GuardZone>,
) -> Response {
    debug!("POST create guard zone {} for radar {}", zone.id, params.radar_id);

    // Ensure radar exists in engine
    state.ensure_radar_in_engine(&params.radar_id);

    let mut engine = state.engine.write().unwrap();
    engine.set_guard_zone(&params.radar_id, zone.clone());
    debug!("Created guard zone {} on radar {}", zone.id, params.radar_id);

    (StatusCode::CREATED, Json(zone)).into_response()
}

/// GET /radars/{radar_id}/guardZones/{zone_id} - Get a specific guard zone
#[debug_handler]
async fn get_guard_zone(
    State(state): State<Web>,
    Path(params): Path<RadarZoneIdParam>,
) -> Response {
    debug!("GET guard zone {} for radar {}", params.zone_id, params.radar_id);

    let engine = state.engine.read().unwrap();
    if let Some(status) = engine.get_guard_zone(&params.radar_id, params.zone_id) {
        return Json(status).into_response();
    }

    (StatusCode::NOT_FOUND, "Zone not found").into_response()
}

/// PUT /radars/{radar_id}/guardZones/{zone_id} - Update a guard zone
#[debug_handler]
async fn update_guard_zone(
    State(state): State<Web>,
    Path(params): Path<RadarZoneIdParam>,
    Json(zone): Json<GuardZone>,
) -> Response {
    debug!("PUT update guard zone {} for radar {}", params.zone_id, params.radar_id);

    // Ensure radar exists in engine
    state.ensure_radar_in_engine(&params.radar_id);

    // Ensure zone ID matches path
    let mut zone = zone;
    zone.id = params.zone_id;

    let mut engine = state.engine.write().unwrap();
    engine.set_guard_zone(&params.radar_id, zone);
    debug!("Updated guard zone {} on radar {}", params.zone_id, params.radar_id);

    StatusCode::OK.into_response()
}

/// DELETE /radars/{radar_id}/guardZones/{zone_id} - Delete a guard zone
#[debug_handler]
async fn delete_guard_zone(
    State(state): State<Web>,
    Path(params): Path<RadarZoneIdParam>,
) -> Response {
    debug!("DELETE guard zone {} for radar {}", params.zone_id, params.radar_id);

    let mut engine = state.engine.write().unwrap();
    if engine.remove_guard_zone(&params.radar_id, params.zone_id) {
        debug!("Deleted guard zone {} on radar {}", params.zone_id, params.radar_id);
        return StatusCode::NO_CONTENT.into_response();
    }

    (StatusCode::NOT_FOUND, "Zone not found").into_response()
}

// =============================================================================
// Trail API Handlers
// =============================================================================

/// Parameters for trail-specific endpoints (target_id)
#[derive(Deserialize)]
struct RadarTrailIdParam {
    radar_id: String,
    target_id: u32,
}

/// Response for GET /radars/{id}/trails
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrailListResponse {
    radar_id: String,
    timestamp: String,
    trails: Vec<TrailData>,
}

/// GET /radars/{radar_id}/trails - Get all trails
#[debug_handler]
async fn get_all_trails(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("GET all trails for radar {}", params.radar_id);

    let engine = state.engine.read().unwrap();
    let trails = engine.get_all_trails(&params.radar_id);

    let response = TrailListResponse {
        radar_id: params.radar_id,
        timestamp: chrono::Utc::now().to_rfc3339(),
        trails,
    };

    Json(response).into_response()
}

/// GET /radars/{radar_id}/trails/{target_id} - Get trail for a specific target
#[debug_handler]
async fn get_trail(
    State(state): State<Web>,
    Path(params): Path<RadarTrailIdParam>,
) -> Response {
    debug!("GET trail for target {} on radar {}", params.target_id, params.radar_id);

    let engine = state.engine.read().unwrap();
    if let Some(trail_data) = engine.get_trail(&params.radar_id, params.target_id) {
        return Json(trail_data).into_response();
    }

    (StatusCode::NOT_FOUND, "Trail not found").into_response()
}

/// DELETE /radars/{radar_id}/trails - Clear all trails
#[debug_handler]
async fn clear_all_trails(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("DELETE all trails for radar {}", params.radar_id);

    let mut engine = state.engine.write().unwrap();
    engine.clear_all_trails(&params.radar_id);
    debug!("Cleared all trails on radar {}", params.radar_id);

    StatusCode::NO_CONTENT.into_response()
}

/// DELETE /radars/{radar_id}/trails/{target_id} - Clear trail for a specific target
#[debug_handler]
async fn clear_trail(
    State(state): State<Web>,
    Path(params): Path<RadarTrailIdParam>,
) -> Response {
    debug!("DELETE trail for target {} on radar {}", params.target_id, params.radar_id);

    let mut engine = state.engine.write().unwrap();
    engine.clear_trail(&params.radar_id, params.target_id);
    debug!("Cleared trail for target {} on radar {}", params.target_id, params.radar_id);

    StatusCode::NO_CONTENT.into_response()
}

/// GET /radars/{radar_id}/trails/settings - Get trail settings
#[debug_handler]
async fn get_trail_settings(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("GET trail settings for radar {}", params.radar_id);

    let engine = state.engine.read().unwrap();
    let settings = engine
        .get_trail_settings(&params.radar_id)
        .unwrap_or_default();

    Json(settings).into_response()
}

/// PUT /radars/{radar_id}/trails/settings - Update trail settings
#[debug_handler]
async fn set_trail_settings(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
    Json(settings): Json<TrailSettings>,
) -> Response {
    debug!("PUT trail settings for radar {}", params.radar_id);

    // Ensure radar exists in engine
    state.ensure_radar_in_engine(&params.radar_id);

    let mut engine = state.engine.write().unwrap();
    engine.set_trail_settings(&params.radar_id, settings);
    debug!("Updated trail settings for radar {}", params.radar_id);

    StatusCode::OK.into_response()
}

// =============================================================================
// Dual-Range API Handlers
// =============================================================================

/// Response for GET /radars/{id}/dualRange
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DualRangeResponse {
    radar_id: String,
    state: CoreDualRangeState,
    available_ranges: Vec<u32>,
}

/// GET /radars/{radar_id}/dualRange - Get dual-range state
#[debug_handler]
async fn get_dual_range(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
) -> Response {
    debug!("GET dual-range for radar {}", params.radar_id);

    // Check if radar exists and supports dual-range (get model info from session)
    let model_info = {
        let session = state.session.read().unwrap();
        let radars = session.radars.as_ref().unwrap();

        match radars.get_by_id(&params.radar_id) {
            Some(info) => {
                let core_brand = to_core_brand(info.brand);
                let model_name = info.controls.model_name();
                let model_info = model_name
                    .as_deref()
                    .and_then(|m| models::get_model(core_brand, m))
                    .unwrap_or(&models::UNKNOWN_MODEL);

                if !model_info.has_dual_range {
                    return (
                        StatusCode::NOT_FOUND,
                        "Radar does not support dual-range",
                    )
                        .into_response();
                }

                model_info.clone()
            }
            None => return RadarError::NoSuchRadar(params.radar_id.to_string()).into_response(),
        }
    };

    // Get dual-range state from engine
    let engine = state.engine.read().unwrap();
    let dual_state = engine
        .get_dual_range(&params.radar_id)
        .cloned()
        .unwrap_or_else(|| CoreDualRangeState {
            max_secondary_range: model_info.max_dual_range,
            ..Default::default()
        });

    // Filter ranges for secondary display
    let available_ranges: Vec<u32> = model_info
        .range_table
        .iter()
        .filter(|&&r| r <= model_info.max_dual_range)
        .copied()
        .collect();

    let response = DualRangeResponse {
        radar_id: params.radar_id,
        state: dual_state,
        available_ranges,
    };

    Json(response).into_response()
}

/// PUT /radars/{radar_id}/dualRange - Update dual-range configuration
#[debug_handler]
async fn set_dual_range(
    State(state): State<Web>,
    Path(params): Path<RadarIdParam>,
    Json(config): Json<DualRangeConfig>,
) -> Response {
    debug!(
        "PUT dual-range for radar {}: enabled={}, secondary_range={}",
        params.radar_id, config.enabled, config.secondary_range
    );

    // Check if radar exists and supports dual-range (get model info from session)
    let (model_name, model_info) = {
        let session = state.session.read().unwrap();
        let radars = session.radars.as_ref().unwrap();

        match radars.get_by_id(&params.radar_id) {
            Some(info) => {
                let core_brand = to_core_brand(info.brand);
                let model_name_opt = info.controls.model_name();
                let model = model_name_opt
                    .as_deref()
                    .and_then(|m| models::get_model(core_brand, m))
                    .unwrap_or(&models::UNKNOWN_MODEL);

                if !model.has_dual_range {
                    return (
                        StatusCode::NOT_FOUND,
                        "Radar does not support dual-range",
                    )
                        .into_response();
                }

                (model_name_opt, model.clone())
            }
            None => return RadarError::NoSuchRadar(params.radar_id.to_string()).into_response(),
        }
    };

    // Ensure radar exists in engine with model info (creates dual_range controller)
    if let Some(name) = &model_name {
        state.ensure_radar_in_engine_with_model(&params.radar_id, name);
    }

    // Apply config to engine
    let mut engine = state.engine.write().unwrap();
    if !engine.set_dual_range(&params.radar_id, &config) {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Secondary range {} exceeds maximum {}",
                config.secondary_range,
                model_info.max_dual_range
            ),
        )
            .into_response();
    }

    debug!(
        "Updated dual-range for radar {}: enabled={}",
        params.radar_id, config.enabled
    );

    StatusCode::OK.into_response()
}

/// WebSocket handler for secondary range spokes
#[debug_handler]
async fn dual_range_spokes_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(params): Path<RadarIdParam>,
    ws: WebSocketUpgrade,
) -> Response {
    debug!(
        "dual-range spokes request from {} for {}",
        addr, params.radar_id
    );

    let ws = ws.accept_compression(true);

    // Check if radar exists and supports dual-range
    let radar = {
        let session = state.session.read().unwrap();
        let radars = session.radars.as_ref().unwrap();

        match radars.get_by_id(&params.radar_id) {
            Some(info) => {
                let core_brand = to_core_brand(info.brand);
                let model_name = info.controls.model_name();
                let model = model_name
                    .as_deref()
                    .and_then(|m| models::get_model(core_brand, m))
                    .unwrap_or(&models::UNKNOWN_MODEL);

                if !model.has_dual_range {
                    return (
                        StatusCode::NOT_FOUND,
                        "Radar does not support dual-range",
                    )
                        .into_response();
                }

                info.clone()
            }
            None => return RadarError::NoSuchRadar(params.radar_id.to_string()).into_response(),
        }
    };

    let shutdown_rx = state.shutdown_tx.subscribe();
    // For now, use the same message channel as primary spokes
    // A full implementation would have a separate secondary spoke channel
    let radar_message_rx = radar.message_tx.subscribe();

    ws.on_upgrade(move |socket| dual_range_spokes_stream(socket, radar_message_rx, shutdown_rx))
}

/// WebSocket stream for dual-range secondary spokes
async fn dual_range_spokes_stream(
    mut socket: WebSocket,
    mut radar_message_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    // Note: In a full implementation, this would receive spokes processed
    // at the secondary range. For now, it mirrors the primary spoke stream.
    // The actual secondary range processing would happen in the radar protocol handler.
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Shutdown of dual-range websocket");
                break;
            },
            r = radar_message_rx.recv() => {
                match r {
                    Ok(message) => {
                        let len = message.len();
                        let ws_message = Message::Binary(message.into());
                        if let Err(e) = socket.send(ws_message).await {
                            debug!("Error on send to dual-range websocket: {}", e);
                            break;
                        }
                        trace!("Sent dual-range radar message {} bytes", len);
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
