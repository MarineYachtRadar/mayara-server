use axum::{
    debug_handler,
    extract::{ConnectInfo, Path, State},
    http::{header, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use axum_embed::ServeEmbed;
use flate2::{write::GzEncoder, Compression};
use hyper;
use log::{debug, trace};
use miette::Result;
#[cfg(not(feature = "dev"))]
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{net::TcpListener, sync::broadcast};
use tokio_graceful_shutdown::SubsystemHandle;
#[cfg(feature = "dev")]
use tower_http::services::ServeDir;

mod axum_fix;

use axum_fix::{Message, WebSocket, WebSocketUpgrade};

use mayara_server::{
    radar::{Legend, RadarError, RadarInfo},
    recording::{
        build_initial_state, load_recording, start_recording, unregister_playback_radar,
        ActivePlayback, ActiveRecording, PlaybackSettings, PlaybackStatus, RecordingInfo,
        RecordingManager, RecordingStatus,
    },
    storage::{create_shared_storage, AppDataKey, SharedStorage},
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
use mayara_core::capabilities::{
    builder::build_capabilities_from_model_with_key, RadarStateV5, SupportedFeature,
};
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

// Recordings API - File management
const RECORDINGS_URI: &str = "/v2/api/recordings/files";
const RECORDING_URI: &str = "/v2/api/recordings/files/{filename}";
const RECORDING_DOWNLOAD_URI: &str = "/v2/api/recordings/files/{filename}/download";
const RECORDING_UPLOAD_URI: &str = "/v2/api/recordings/files/upload";
const RECORDINGS_DIRS_URI: &str = "/v2/api/recordings/directories";
const RECORDING_DIR_URI: &str = "/v2/api/recordings/directories/{name}";
// Recordings API - Recording control
const RECORD_RADARS_URI: &str = "/v2/api/recordings/radars";
const RECORD_START_URI: &str = "/v2/api/recordings/record/start";
const RECORD_STOP_URI: &str = "/v2/api/recordings/record/stop";
const RECORD_STATUS_URI: &str = "/v2/api/recordings/record/status";
// Recordings API - Playback control
const PLAYBACK_LOAD_URI: &str = "/v2/api/recordings/playback/load";
const PLAYBACK_PLAY_URI: &str = "/v2/api/recordings/playback/play";
const PLAYBACK_PAUSE_URI: &str = "/v2/api/recordings/playback/pause";
const PLAYBACK_STOP_URI: &str = "/v2/api/recordings/playback/stop";
const PLAYBACK_SEEK_URI: &str = "/v2/api/recordings/playback/seek";
const PLAYBACK_SETTINGS_URI: &str = "/v2/api/recordings/playback/settings";
const PLAYBACK_STATUS_URI: &str = "/v2/api/recordings/playback/status";

// Debug API (dev mode only)
#[cfg(feature = "dev")]
const DEBUG_WS_URI: &str = "/v2/api/debug";
#[cfg(feature = "dev")]
const DEBUG_EVENTS_URI: &str = "/v2/api/debug/events";
#[cfg(feature = "dev")]
const DEBUG_RECORDING_START_URI: &str = "/v2/api/debug/recording/start";
#[cfg(feature = "dev")]
const DEBUG_RECORDING_STOP_URI: &str = "/v2/api/debug/recording/stop";
#[cfg(feature = "dev")]
const DEBUG_RECORDINGS_URI: &str = "/v2/api/debug/recordings";

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

/// Shared RecordingManager for recordings API
type SharedRecordingManager = Arc<RwLock<RecordingManager>>;

/// Shared active recording state
type SharedActiveRecording = Arc<RwLock<Option<ActiveRecording>>>;

/// Shared active playback state
type SharedActivePlayback = Arc<tokio::sync::RwLock<Option<ActivePlayback>>>;

/// Shared DebugHub for protocol debugging (dev mode only)
#[cfg(feature = "dev")]
type SharedDebugHub = Arc<mayara_server::debug::DebugHub>;

#[derive(Clone)]
pub struct Web {
    session: Session,
    shutdown_tx: broadcast::Sender<()>,
    /// Unified engine for all radar feature processors
    engine: SharedEngine,
    /// Local storage for applicationData API
    storage: SharedStorage,
    /// Recording file manager
    recording_manager: SharedRecordingManager,
    /// Active recording (if any)
    active_recording: SharedActiveRecording,
    /// Active playback (if any)
    active_playback: SharedActivePlayback,
    /// Debug hub for protocol analysis (dev mode only)
    #[cfg(feature = "dev")]
    debug_hub: SharedDebugHub,
}

impl Web {
    pub fn new(session: Session) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);

        // Get the debug hub from the session (shared with all components)
        #[cfg(feature = "dev")]
        let debug_hub = session
            .debug_hub()
            .expect("DebugHub should be initialized in Session when dev feature is enabled");

        Web {
            session,
            shutdown_tx,
            engine: Arc::new(RwLock::new(RadarEngine::new())),
            storage: create_shared_storage(),
            recording_manager: Arc::new(RwLock::new(RecordingManager::new())),
            active_recording: Arc::new(RwLock::new(None)),
            active_playback: Arc::new(tokio::sync::RwLock::new(None)),
            #[cfg(feature = "dev")]
            debug_hub,
        }
    }

    /// Get the debug hub (dev mode only)
    #[cfg(feature = "dev")]
    pub fn debug_hub(&self) -> &SharedDebugHub {
        &self.debug_hub
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
            .route(
                ARPA_SETTINGS_URI,
                get(get_arpa_settings).put(set_arpa_settings),
            )
            // Guard zones
            .route(
                GUARD_ZONES_URI,
                get(get_guard_zones).post(create_guard_zone),
            )
            .route(
                GUARD_ZONE_URI,
                get(get_guard_zone)
                    .put(update_guard_zone)
                    .delete(delete_guard_zone),
            )
            // Trails
            .route(TRAILS_URI, get(get_all_trails).delete(clear_all_trails))
            .route(TRAIL_URI, get(get_trail).delete(clear_trail))
            .route(
                TRAIL_SETTINGS_URI,
                get(get_trail_settings).put(set_trail_settings),
            )
            // Dual-range
            .route(DUAL_RANGE_URI, get(get_dual_range).put(set_dual_range))
            .route(DUAL_RANGE_SPOKES_URI, get(dual_range_spokes_handler))
            // Other endpoints
            .route(INTERFACES_URI, get(get_interfaces))
            // SignalK applicationData API
            .route(
                APP_DATA_URI,
                get(get_app_data).put(put_app_data).delete(delete_app_data),
            )
            // Recordings API - File management
            .route(RECORDINGS_URI, get(list_recordings))
            .route(
                RECORDING_URI,
                get(get_recording)
                    .delete(delete_recording)
                    .put(update_recording),
            )
            .route(RECORDING_DOWNLOAD_URI, get(download_recording))
            .route(RECORDING_UPLOAD_URI, post(upload_recording))
            .route(
                RECORDINGS_DIRS_URI,
                get(list_directories).post(create_directory),
            )
            .route(RECORDING_DIR_URI, delete(delete_directory))
            // Recordings API - Recording control
            .route(RECORD_RADARS_URI, get(get_recordable_radars))
            .route(RECORD_START_URI, post(start_recording_handler))
            .route(RECORD_STOP_URI, post(stop_recording_handler))
            .route(RECORD_STATUS_URI, get(get_recording_status))
            // Recordings API - Playback control
            .route(PLAYBACK_LOAD_URI, post(playback_load_handler))
            .route(PLAYBACK_PLAY_URI, post(playback_play_handler))
            .route(PLAYBACK_PAUSE_URI, post(playback_pause_handler))
            .route(PLAYBACK_STOP_URI, post(playback_stop_handler))
            .route(PLAYBACK_SEEK_URI, post(playback_seek_handler))
            .route(PLAYBACK_SETTINGS_URI, put(playback_settings_handler))
            .route(PLAYBACK_STATUS_URI, get(playback_status_handler));

        // Debug API routes (dev mode only)
        #[cfg(feature = "dev")]
        let app = app
            .route(DEBUG_WS_URI, get(debug_ws_handler))
            .route(DEBUG_EVENTS_URI, get(debug_events_handler))
            .route(
                DEBUG_RECORDING_START_URI,
                post(debug_recording_start_handler),
            )
            .route(DEBUG_RECORDING_STOP_URI, post(debug_recording_stop_handler))
            .route(DEBUG_RECORDINGS_URI, get(debug_recordings_list_handler));

        let app = app
            // Apply no-cache middleware to all API routes
            .layer(middleware::from_fn(no_cache_middleware))
            // Static assets (no middleware - can be cached)
            .nest_service("/protobuf", proto_web_assets)
            .nest_service("/proto", proto_assets);

        // Conditionally add rustdoc assets if feature enabled
        #[cfg(feature = "rustdoc")]
        let app = app.nest_service("/rustdoc", rustdoc_assets);

        let app = app
            .fallback_service(serve_assets)
            .with_state(self)
            .into_make_service_with_connect_info::<SocketAddr>();

        #[cfg(feature = "dev")]
        log::info!(
            "Starting HTTP web server on port {} (DEV MODE - serving from filesystem)",
            port
        );
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
async fn no_cache_middleware(
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
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
        let name = info
            .controls
            .user_name()
            .unwrap_or_else(|| info.key().to_string());
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
        // Playback uses recorded capabilities, brand doesn't matter for model lookup
        mayara_server::Brand::Playback => mayara_core::Brand::Furuno,
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
                    info.pixel_values(),
                ))
            }
            None => None,
        }
    }; // session lock released here

    match build_args {
        Some((
            model_info,
            radar_id,
            radar_key,
            supported_features,
            spokes_per_revolution,
            max_spoke_len,
            pixel_values,
        )) => {
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
                    pixel_values,
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
async fn get_radar_state(State(state): State<Web>, Path(params): Path<RadarIdParam>) -> Response {
    debug!("State request for radar {}", params.radar_id);

    let session = state.session.read().unwrap();
    let radars = session.radars.as_ref().unwrap();

    match radars.get_by_id(&params.radar_id) {
        Some(info) => {
            // Build the state dynamically from all registered controls
            // Use BTreeMap for stable JSON key ordering
            let mut controls = BTreeMap::new();

            // Helper to format a control value for the API response
            fn format_control_value(
                control_id: &str,
                control: &mayara_server::settings::Control,
            ) -> serde_json::Value {
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
                    let mode = if control.auto.unwrap_or(false) {
                        "auto"
                    } else {
                        "manual"
                    };
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
                controls.insert(
                    control_id.clone(),
                    format_control_value(&control_id, &control),
                );
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
                            log::warn!("Error on send to websocket: {}", e);
                            break;
                        }
                        trace!("Sent radar message {} bytes", len);
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Channel lagged - receiver fell behind, skip missed messages
                        log::warn!("Websocket receiver lagged, skipped {} messages", n);
                        // Continue to receive next message
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // Channel closed - sender was dropped (radar disconnected or playback stopped)
                        debug!("RadarMessage channel closed");
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
                        let available: Vec<String> = radar
                            .controls
                            .get_all()
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
                            control
                                .index_to_enum_value(index)
                                .unwrap_or_else(|| s.clone())
                        } else {
                            s.clone()
                        };
                        (normalized, None)
                    }
                    serde_json::Value::Number(n) => (n.to_string(), None),
                    serde_json::Value::Bool(b) => (if *b { "1" } else { "0" }.to_string(), None),
                    serde_json::Value::Object(obj) => {
                        // Check if this is a dopplerMode compound control {"enabled": bool, "mode": "target"|"rain"}
                        if params.control_id == "dopplerMode" {
                            let enabled = obj
                                .get("enabled")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let mode_str =
                                obj.get("mode").and_then(|v| v.as_str()).unwrap_or("target");
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
                            let value = obj
                                .get("value")
                                .map(|v| match v {
                                    serde_json::Value::Number(n) => n.to_string(),
                                    serde_json::Value::String(s) => s.clone(),
                                    _ => v.to_string(),
                                })
                                .unwrap_or_default();
                            (value, auto)
                        }
                    }
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
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to send control: {:?}", e),
        )
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
async fn get_targets(State(state): State<Web>, Path(params): Path<RadarIdParam>) -> Response {
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
    match engine.acquire_target(
        &params.radar_id,
        request.bearing,
        request.distance,
        timestamp,
    ) {
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
        debug!(
            "Cancelled target {} on radar {}",
            params.target_id, params.radar_id
        );
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "Target not found").into_response()
    }
}

/// GET /radars/{radar_id}/arpa/settings - Get ARPA settings
#[debug_handler]
async fn get_arpa_settings(State(state): State<Web>, Path(params): Path<RadarIdParam>) -> Response {
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
async fn get_app_data(State(state): State<Web>, Path(params): Path<AppDataParams>) -> Response {
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
async fn delete_app_data(State(state): State<Web>, Path(params): Path<AppDataParams>) -> Response {
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
async fn get_guard_zones(State(state): State<Web>, Path(params): Path<RadarIdParam>) -> Response {
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
    debug!(
        "POST create guard zone {} for radar {}",
        zone.id, params.radar_id
    );

    // Ensure radar exists in engine
    state.ensure_radar_in_engine(&params.radar_id);

    let mut engine = state.engine.write().unwrap();
    engine.set_guard_zone(&params.radar_id, zone.clone());
    debug!(
        "Created guard zone {} on radar {}",
        zone.id, params.radar_id
    );

    (StatusCode::CREATED, Json(zone)).into_response()
}

/// GET /radars/{radar_id}/guardZones/{zone_id} - Get a specific guard zone
#[debug_handler]
async fn get_guard_zone(
    State(state): State<Web>,
    Path(params): Path<RadarZoneIdParam>,
) -> Response {
    debug!(
        "GET guard zone {} for radar {}",
        params.zone_id, params.radar_id
    );

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
    debug!(
        "PUT update guard zone {} for radar {}",
        params.zone_id, params.radar_id
    );

    // Ensure radar exists in engine
    state.ensure_radar_in_engine(&params.radar_id);

    // Ensure zone ID matches path
    let mut zone = zone;
    zone.id = params.zone_id;

    let mut engine = state.engine.write().unwrap();
    engine.set_guard_zone(&params.radar_id, zone);
    debug!(
        "Updated guard zone {} on radar {}",
        params.zone_id, params.radar_id
    );

    StatusCode::OK.into_response()
}

/// DELETE /radars/{radar_id}/guardZones/{zone_id} - Delete a guard zone
#[debug_handler]
async fn delete_guard_zone(
    State(state): State<Web>,
    Path(params): Path<RadarZoneIdParam>,
) -> Response {
    debug!(
        "DELETE guard zone {} for radar {}",
        params.zone_id, params.radar_id
    );

    let mut engine = state.engine.write().unwrap();
    if engine.remove_guard_zone(&params.radar_id, params.zone_id) {
        debug!(
            "Deleted guard zone {} on radar {}",
            params.zone_id, params.radar_id
        );
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
async fn get_all_trails(State(state): State<Web>, Path(params): Path<RadarIdParam>) -> Response {
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
async fn get_trail(State(state): State<Web>, Path(params): Path<RadarTrailIdParam>) -> Response {
    debug!(
        "GET trail for target {} on radar {}",
        params.target_id, params.radar_id
    );

    let engine = state.engine.read().unwrap();
    if let Some(trail_data) = engine.get_trail(&params.radar_id, params.target_id) {
        return Json(trail_data).into_response();
    }

    (StatusCode::NOT_FOUND, "Trail not found").into_response()
}

/// DELETE /radars/{radar_id}/trails - Clear all trails
#[debug_handler]
async fn clear_all_trails(State(state): State<Web>, Path(params): Path<RadarIdParam>) -> Response {
    debug!("DELETE all trails for radar {}", params.radar_id);

    let mut engine = state.engine.write().unwrap();
    engine.clear_all_trails(&params.radar_id);
    debug!("Cleared all trails on radar {}", params.radar_id);

    StatusCode::NO_CONTENT.into_response()
}

/// DELETE /radars/{radar_id}/trails/{target_id} - Clear trail for a specific target
#[debug_handler]
async fn clear_trail(State(state): State<Web>, Path(params): Path<RadarTrailIdParam>) -> Response {
    debug!(
        "DELETE trail for target {} on radar {}",
        params.target_id, params.radar_id
    );

    let mut engine = state.engine.write().unwrap();
    engine.clear_trail(&params.radar_id, params.target_id);
    debug!(
        "Cleared trail for target {} on radar {}",
        params.target_id, params.radar_id
    );

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
async fn get_dual_range(State(state): State<Web>, Path(params): Path<RadarIdParam>) -> Response {
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
                    return (StatusCode::NOT_FOUND, "Radar does not support dual-range")
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
                    return (StatusCode::NOT_FOUND, "Radar does not support dual-range")
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
                config.secondary_range, model_info.max_dual_range
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
                    return (StatusCode::NOT_FOUND, "Radar does not support dual-range")
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
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        debug!("Dual-range websocket receiver lagged, skipped {} messages", n);
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        debug!("Dual-range RadarMessage channel closed");
                        break;
                    }
                }
            }
        }
    }
}

// ============================================================================
// Recordings API handlers
// ============================================================================

/// Query parameters for listing recordings
#[derive(Debug, Deserialize)]
struct RecordingsQuery {
    #[serde(rename = "dir")]
    subdirectory: Option<String>,
}

/// Request body for updating a recording
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateRecordingRequest {
    new_name: Option<String>,
    directory: Option<String>,
}

/// Request body for creating a directory
#[derive(Debug, Deserialize)]
struct CreateDirectoryRequest {
    name: String,
}

/// Response for recordings list
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordingsResponse {
    recordings: Vec<RecordingInfo>,
    total_count: usize,
    total_size: u64,
}

/// GET /v2/api/recordings/files - List recordings
#[debug_handler]
async fn list_recordings(
    State(state): State<Web>,
    axum::extract::Query(query): axum::extract::Query<RecordingsQuery>,
) -> Response {
    debug!("GET recordings list, dir={:?}", query.subdirectory);

    let manager = state.recording_manager.read().unwrap();
    let recordings = manager.list_recordings(query.subdirectory.as_deref());
    let total_count = recordings.len();
    let total_size: u64 = recordings.iter().map(|r| r.size).sum();

    Json(RecordingsResponse {
        recordings,
        total_count,
        total_size,
    })
    .into_response()
}

/// GET /v2/api/recordings/files/{filename} - Get recording info
#[debug_handler]
async fn get_recording(
    State(state): State<Web>,
    Path(filename): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RecordingsQuery>,
) -> Response {
    debug!("GET recording info: {}", filename);

    let manager = state.recording_manager.read().unwrap();
    match manager.get_recording(&filename, query.subdirectory.as_deref()) {
        Some(info) => Json(info).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// DELETE /v2/api/recordings/files/{filename} - Delete a recording
#[debug_handler]
async fn delete_recording(
    State(state): State<Web>,
    Path(filename): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RecordingsQuery>,
) -> Response {
    debug!("DELETE recording: {}", filename);

    let manager = state.recording_manager.read().unwrap();
    match manager.delete_recording(&filename, query.subdirectory.as_deref()) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// PUT /v2/api/recordings/files/{filename} - Rename or move a recording
#[debug_handler]
async fn update_recording(
    State(state): State<Web>,
    Path(filename): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RecordingsQuery>,
    Json(request): Json<UpdateRecordingRequest>,
) -> Response {
    debug!("PUT recording: {} -> {:?}", filename, request);

    let manager = state.recording_manager.read().unwrap();
    let subdirectory = query.subdirectory.as_deref();

    // Handle rename
    if let Some(new_name) = request.new_name {
        match manager.rename_recording(&filename, &new_name, subdirectory) {
            Ok(()) => {}
            Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
        }
    }

    // Handle move to different directory
    if let Some(new_directory) = request.directory {
        let new_dir = if new_directory.is_empty() {
            None
        } else {
            Some(new_directory.as_str())
        };
        match manager.move_recording(&filename, subdirectory, new_dir) {
            Ok(()) => {}
            Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
        }
    }

    StatusCode::OK.into_response()
}

/// GET /v2/api/recordings/files/{filename}/download - Download a recording file (gzip compressed)
#[debug_handler]
async fn download_recording(
    State(state): State<Web>,
    Path(filename): Path<String>,
    axum::extract::Query(query): axum::extract::Query<RecordingsQuery>,
) -> Response {
    debug!("GET download recording: {}", filename);

    let manager = state.recording_manager.read().unwrap();
    let path = manager.get_recording_path(&filename, query.subdirectory.as_deref());

    if !path.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Read the file
    match std::fs::read(&path) {
        Ok(data) => {
            // Compress with gzip
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            if let Err(e) = encoder.write_all(&data) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Compression error: {}", e),
                )
                    .into_response();
            }
            let compressed = match encoder.finish() {
                Ok(data) => data,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Compression error: {}", e),
                    )
                        .into_response()
                }
            };

            debug!(
                "Compressed {} from {} to {} bytes ({:.1}% reduction)",
                filename,
                data.len(),
                compressed.len(),
                (1.0 - compressed.len() as f64 / data.len() as f64) * 100.0
            );

            // Return compressed file with .mrr.gz extension
            let gz_filename = format!("{}.gz", filename);
            let headers = [
                (header::CONTENT_TYPE, "application/gzip"),
                (
                    header::CONTENT_DISPOSITION,
                    &format!("attachment; filename=\"{}\"", gz_filename),
                ),
            ];
            (headers, compressed).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /v2/api/recordings/files/upload - Upload a recording file
/// Accepts raw .mrr or .mrr.gz file data in request body
/// Filename is taken from Content-Disposition header
#[debug_handler]
async fn upload_recording(
    State(state): State<Web>,
    axum::extract::Query(query): axum::extract::Query<RecordingsQuery>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Extract filename from Content-Disposition header
    let filename = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            // Parse "attachment; filename="xxx.mrr""
            s.split("filename=")
                .nth(1)
                .map(|f| f.trim_matches('"').trim_matches('\'').to_string())
        })
        .unwrap_or_else(|| {
            // Generate a default filename if not provided
            let manager = state.recording_manager.read().unwrap();
            manager.generate_filename(Some("upload"), query.subdirectory.as_deref())
        });

    debug!("POST upload recording: {} ({} bytes)", filename, body.len());

    let manager = state.recording_manager.read().unwrap();
    match manager.save_upload(&filename, &body, query.subdirectory.as_deref()) {
        Ok(info) => Json(info).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

/// GET /v2/api/recordings/directories - List directories
#[debug_handler]
async fn list_directories(State(state): State<Web>) -> Response {
    debug!("GET recordings directories");

    let manager = state.recording_manager.read().unwrap();
    let dirs = manager.list_directories();

    Json(dirs).into_response()
}

/// POST /v2/api/recordings/directories - Create a directory
#[debug_handler]
async fn create_directory(
    State(state): State<Web>,
    Json(request): Json<CreateDirectoryRequest>,
) -> Response {
    debug!("POST create directory: {}", request.name);

    let manager = state.recording_manager.read().unwrap();
    match manager.create_directory(&request.name) {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// DELETE /v2/api/recordings/directories/{name} - Delete a directory
#[debug_handler]
async fn delete_directory(State(state): State<Web>, Path(name): Path<String>) -> Response {
    debug!("DELETE directory: {}", name);

    let manager = state.recording_manager.read().unwrap();
    match manager.delete_directory(&name) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

// ============================================================================
// Recording Control Endpoints
// ============================================================================

/// Info about a recordable radar
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordableRadar {
    id: String,
    name: String,
    brand: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

/// GET /v2/api/recordings/radars - List radars available for recording
#[debug_handler]
async fn get_recordable_radars(State(state): State<Web>) -> Response {
    debug!("GET recordable radars");

    let session = state.session.read().unwrap();
    let radars = match &session.radars {
        Some(r) => r,
        None => return Json(Vec::<RecordableRadar>::new()).into_response(),
    };

    let mut result = Vec::new();
    for info in radars.get_active() {
        let radar_id = format!("radar-{}", info.id);
        result.push(RecordableRadar {
            id: radar_id,
            name: info
                .controls
                .user_name()
                .unwrap_or_else(|| info.key().to_string()),
            brand: format!("{:?}", info.brand),
            model: info.controls.model_name(),
        });
    }

    Json(result).into_response()
}

/// Request body for starting a recording
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRecordingRequest {
    radar_id: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    subdirectory: Option<String>,
}

/// POST /v2/api/recordings/record/start - Start recording from a radar
#[debug_handler]
async fn start_recording_handler(
    State(state): State<Web>,
    Json(request): Json<StartRecordingRequest>,
) -> Response {
    debug!("POST start recording for radar: {}", request.radar_id);

    // Check if already recording
    {
        let active = state.active_recording.read().unwrap();
        if let Some(ref recording) = *active {
            if recording.is_running() {
                return (
                    StatusCode::CONFLICT,
                    format!("Already recording radar {}", recording.radar_id()),
                )
                    .into_response();
            }
        }
    }

    // Get radar info
    let (radar_info, capabilities_json) = {
        let session = state.session.read().unwrap();
        let radars = match &session.radars {
            Some(r) => r,
            None => {
                return (StatusCode::SERVICE_UNAVAILABLE, "No radars available").into_response()
            }
        };

        let radar = match radars.get_by_id(&request.radar_id) {
            Some(r) => r,
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    format!("Radar not found: {}", request.radar_id),
                )
                    .into_response()
            }
        };

        // Build capabilities JSON
        let core_brand = to_core_brand(radar.brand);
        let model_name = radar.controls.model_name();
        let model_info = model_name
            .as_deref()
            .and_then(|name| models::get_model(core_brand, name))
            .unwrap_or(&models::UNKNOWN_MODEL);

        // Declare supported features for recording
        let mut supported_features = vec![
            SupportedFeature::Arpa,
            SupportedFeature::GuardZones,
            SupportedFeature::Trails,
        ];
        if model_info.has_dual_range {
            supported_features.push(SupportedFeature::DualRange);
        }

        let capabilities = build_capabilities_from_model_with_key(
            model_info,
            &request.radar_id,
            Some(&radar.key()),
            supported_features,
            radar.spokes_per_revolution,
            radar.max_spoke_len,
            radar.pixel_values(),
        );

        let capabilities_json =
            serde_json::to_vec(&capabilities).unwrap_or_else(|_| b"{}".to_vec());

        (radar, capabilities_json)
    };

    // Build initial state JSON
    let initial_state_json = build_initial_state(&radar_info);

    // Start recording
    match start_recording(
        &radar_info,
        &request.radar_id,
        request.filename.as_deref(),
        request.subdirectory.as_deref(),
        &capabilities_json,
        &initial_state_json,
    )
    .await
    {
        Ok(active) => {
            let status = active.status();
            // Store the active recording
            {
                let mut recording = state.active_recording.write().unwrap();
                *recording = Some(active);
            }
            (StatusCode::OK, Json(status)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// POST /v2/api/recordings/record/stop - Stop the current recording
#[debug_handler]
async fn stop_recording_handler(State(state): State<Web>) -> Response {
    debug!("POST stop recording");

    let mut active = state.active_recording.write().unwrap();
    match active.take() {
        Some(recording) => {
            recording.stop();
            // Return final status
            let status = RecordingStatus {
                state: "stopped".to_string(),
                radar_id: Some(recording.radar_id().to_string()),
                filename: Some(recording.filename().to_string()),
                ..Default::default()
            };
            (StatusCode::OK, Json(status)).into_response()
        }
        None => (StatusCode::OK, Json(RecordingStatus::default())).into_response(),
    }
}

/// GET /v2/api/recordings/record/status - Get current recording status
#[debug_handler]
async fn get_recording_status(State(state): State<Web>) -> Response {
    debug!("GET recording status");

    let active = state.active_recording.read().unwrap();
    let status = match &*active {
        Some(recording) if recording.is_running() => recording.status(),
        Some(recording) => {
            // Recording finished
            RecordingStatus {
                state: "finished".to_string(),
                radar_id: Some(recording.radar_id().to_string()),
                filename: Some(recording.filename().to_string()),
                ..Default::default()
            }
        }
        None => RecordingStatus::default(),
    };

    Json(status).into_response()
}

// ============================================================================
// Playback Control Endpoints
// ============================================================================

/// Request body for loading a recording for playback
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackLoadRequest {
    filename: String,
    #[serde(default)]
    subdirectory: Option<String>,
}

/// Request body for seeking
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackSeekRequest {
    position_ms: u64,
}

/// POST /v2/api/recordings/playback/load - Load a recording for playback
#[debug_handler]
async fn playback_load_handler(
    State(state): State<Web>,
    Json(request): Json<PlaybackLoadRequest>,
) -> Response {
    debug!("POST playback load: {}", request.filename);

    // Stop and clean up any existing playback first
    {
        let mut active = state.active_playback.write().await;
        if let Some(playback) = active.take() {
            log::info!(
                "Stopping existing playback before loading new: {}",
                playback.filename()
            );
            playback.stop();
            // Unregister the old playback radar
            let session = state.session.read().unwrap();
            if let Some(radars) = session.radars.as_ref() {
                unregister_playback_radar(radars, playback.radar_key());
            }
            // Drop playback here to release any resources
            drop(playback);
        }
    }

    // Small delay to allow old playback task to fully stop
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Get radars from session
    let radars = {
        let session = state.session.read().unwrap();
        match &session.radars {
            Some(r) => r.clone(),
            None => {
                return (StatusCode::SERVICE_UNAVAILABLE, "Radars not initialized").into_response()
            }
        }
    };

    // Load the recording
    match load_recording(
        state.session.clone(),
        &radars,
        &request.filename,
        request.subdirectory.as_deref(),
    )
    .await
    {
        Ok(playback) => {
            let status = playback.status().await;
            // Store the active playback
            {
                let mut active = state.active_playback.write().await;
                *active = Some(playback);
            }
            (StatusCode::OK, Json(status)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// POST /v2/api/recordings/playback/play - Start or resume playback
#[debug_handler]
async fn playback_play_handler(State(state): State<Web>) -> Response {
    debug!("POST playback play");

    let active = state.active_playback.read().await;
    match &*active {
        Some(playback) if !playback.is_stopped() => {
            playback.resume();
            let status = playback.status().await;
            (StatusCode::OK, Json(status)).into_response()
        }
        Some(_) => (StatusCode::GONE, "Playback has stopped").into_response(),
        None => (StatusCode::NOT_FOUND, "No recording loaded").into_response(),
    }
}

/// POST /v2/api/recordings/playback/pause - Pause playback
#[debug_handler]
async fn playback_pause_handler(State(state): State<Web>) -> Response {
    debug!("POST playback pause");

    let active = state.active_playback.read().await;
    match &*active {
        Some(playback) if !playback.is_stopped() => {
            playback.pause();
            let status = playback.status().await;
            (StatusCode::OK, Json(status)).into_response()
        }
        Some(_) => (StatusCode::GONE, "Playback has stopped").into_response(),
        None => (StatusCode::NOT_FOUND, "No recording loaded").into_response(),
    }
}

/// POST /v2/api/recordings/playback/stop - Stop playback and unload
#[debug_handler]
async fn playback_stop_handler(State(state): State<Web>) -> Response {
    debug!("POST playback stop");

    let mut active = state.active_playback.write().await;
    match active.take() {
        Some(playback) => {
            playback.stop();
            // Unregister the playback radar from radars list
            {
                let session = state.session.read().unwrap();
                if let Some(radars) = session.radars.as_ref() {
                    unregister_playback_radar(radars, playback.radar_key());
                }
            }
            let status = PlaybackStatus {
                state: "stopped".to_string(),
                filename: Some(playback.filename().to_string()),
                radar_id: Some(playback.radar_id().to_string()),
                ..Default::default()
            };
            (StatusCode::OK, Json(status)).into_response()
        }
        None => (StatusCode::OK, Json(PlaybackStatus::default())).into_response(),
    }
}

/// POST /v2/api/recordings/playback/seek - Seek to position
#[debug_handler]
async fn playback_seek_handler(
    State(state): State<Web>,
    Json(request): Json<PlaybackSeekRequest>,
) -> Response {
    debug!("POST playback seek to {}ms", request.position_ms);

    let active = state.active_playback.read().await;
    match &*active {
        Some(playback) if !playback.is_stopped() => {
            playback.seek(request.position_ms).await;
            let status = playback.status().await;
            (StatusCode::OK, Json(status)).into_response()
        }
        Some(_) => (StatusCode::GONE, "Playback has stopped").into_response(),
        None => (StatusCode::NOT_FOUND, "No recording loaded").into_response(),
    }
}

/// PUT /v2/api/recordings/playback/settings - Update playback settings
#[debug_handler]
async fn playback_settings_handler(
    State(state): State<Web>,
    Json(settings): Json<PlaybackSettings>,
) -> Response {
    debug!("PUT playback settings: {:?}", settings);

    let active = state.active_playback.read().await;
    match &*active {
        Some(playback) if !playback.is_stopped() => {
            if let Some(speed) = settings.speed {
                playback.set_speed(speed);
            }
            if let Some(loop_playback) = settings.loop_playback {
                playback.set_loop(loop_playback);
            }
            let status = playback.status().await;
            (StatusCode::OK, Json(status)).into_response()
        }
        Some(_) => (StatusCode::GONE, "Playback has stopped").into_response(),
        None => (StatusCode::NOT_FOUND, "No recording loaded").into_response(),
    }
}

/// GET /v2/api/recordings/playback/status - Get current playback status
#[debug_handler]
async fn playback_status_handler(State(state): State<Web>) -> Response {
    debug!("GET playback status");

    let active = state.active_playback.read().await;
    let status = match &*active {
        Some(playback) => playback.status().await,
        None => PlaybackStatus::default(),
    };

    Json(status).into_response()
}

// =============================================================================
// Debug API Handlers (dev mode only)
// =============================================================================

#[cfg(feature = "dev")]
mod debug_handlers {
    use super::*;
    use mayara_server::debug::{
        recording::{RecordedRadar, RecordingManager as DebugRecordingManager},
        DebugEvent,
    };

    /// Query parameters for debug events
    #[derive(Debug, Deserialize)]
    pub struct DebugEventsQuery {
        /// Filter by radar ID
        #[serde(default)]
        pub radar_id: Option<String>,
        /// Maximum number of events to return
        #[serde(default)]
        pub limit: Option<usize>,
        /// Start from this event ID
        #[serde(default)]
        pub after: Option<u64>,
    }

    /// WebSocket message from client
    #[derive(Debug, Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    pub enum DebugClientMessage {
        /// Subscribe to events with optional filter
        Subscribe {
            #[serde(default)]
            radar_id: Option<String>,
        },
        /// Get historical events
        GetHistory {
            #[serde(default)]
            limit: Option<usize>,
        },
        /// Pause event streaming
        Pause,
        /// Resume event streaming
        Resume,
    }

    /// WebSocket message to client
    #[derive(Debug, Serialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    pub enum DebugServerMessage {
        /// A debug event
        Event(DebugEvent),
        /// Historical events
        History { events: Vec<DebugEvent> },
        /// Connection established
        Connected { event_count: usize },
        /// Error message
        Error { message: String },
    }

    /// Request body for starting debug recording
    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct StartDebugRecordingRequest {
        /// Radars to include in the recording
        #[serde(default)]
        pub radars: Vec<RecordedRadarInfo>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct RecordedRadarInfo {
        pub radar_id: String,
        pub brand: String,
        #[serde(default)]
        pub model: Option<String>,
        #[serde(default)]
        pub address: Option<String>,
    }

    /// Response for debug recording operations
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DebugRecordingResponse {
        pub success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub filename: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error: Option<String>,
    }

    /// Response for listing debug recordings
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct DebugRecordingsListResponse {
        pub recordings: Vec<mayara_server::debug::recording::RecordingInfo>,
    }

    /// GET /v2/api/debug - WebSocket for real-time debug events
    #[debug_handler]
    pub async fn debug_ws_handler(
        State(state): State<Web>,
        ConnectInfo(addr): ConnectInfo<SocketAddr>,
        ws: WebSocketUpgrade,
    ) -> Response {
        debug!("Debug WebSocket connection from {}", addr);

        let hub = state.debug_hub.clone();
        let shutdown_rx = state.shutdown_tx.subscribe();

        ws.on_upgrade(move |socket| debug_ws_stream(socket, hub, shutdown_rx))
    }

    /// WebSocket stream for debug events
    async fn debug_ws_stream(
        mut socket: WebSocket,
        hub: SharedDebugHub,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) {
        use futures_util::SinkExt;

        // Subscribe to the debug event broadcast
        let mut event_rx = hub.subscribe();
        let mut paused = false;
        let mut radar_filter: Option<String> = None;

        // Send initial connected message
        let connected = DebugServerMessage::Connected {
            event_count: hub.event_count(),
        };
        if let Ok(json) = serde_json::to_string(&connected) {
            let _ = socket.send(Message::Text(json.into())).await;
        }

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    debug!("Shutdown of debug websocket");
                    break;
                }
                // Receive commands from client
                msg = socket.recv() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(cmd) = serde_json::from_str::<DebugClientMessage>(&text) {
                                match cmd {
                                    DebugClientMessage::Subscribe { radar_id } => {
                                        radar_filter = radar_id;
                                        debug!("Debug WS: filter set to {:?}", radar_filter);
                                    }
                                    DebugClientMessage::GetHistory { limit } => {
                                        let events = hub.get_events(
                                            radar_filter.as_deref(),
                                            limit.unwrap_or(100),
                                            None,
                                        );
                                        let msg = DebugServerMessage::History { events };
                                        if let Ok(json) = serde_json::to_string(&msg) {
                                            let _ = socket.send(Message::Text(json.into())).await;
                                        }
                                    }
                                    DebugClientMessage::Pause => {
                                        paused = true;
                                        debug!("Debug WS: paused");
                                    }
                                    DebugClientMessage::Resume => {
                                        paused = false;
                                        debug!("Debug WS: resumed");
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            debug!("Debug websocket closed");
                            break;
                        }
                        _ => {}
                    }
                }
                // Broadcast events to client
                event = event_rx.recv() => {
                    if paused {
                        continue;
                    }
                    match event {
                        Ok(event) => {
                            debug!("Debug WS: received event #{} for radar {}", event.id, event.radar_id);
                            // Apply radar filter
                            if let Some(ref filter) = radar_filter {
                                if &event.radar_id != filter {
                                    debug!("Debug WS: filtered out event (filter: {})", filter);
                                    continue;
                                }
                            }
                            let msg = DebugServerMessage::Event(event);
                            if let Ok(json) = serde_json::to_string(&msg) {
                                if socket.send(Message::Text(json.into())).await.is_err() {
                                    debug!("Debug WS: failed to send event");
                                    break;
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            debug!("Debug WS lagged, missed {} events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
            }
        }
    }

    /// GET /v2/api/debug/events - Get historical debug events
    #[debug_handler]
    pub async fn debug_events_handler(
        State(state): State<Web>,
        axum::extract::Query(query): axum::extract::Query<DebugEventsQuery>,
    ) -> Response {
        debug!("GET debug events: {:?}", query);

        let events = state.debug_hub.get_events(
            query.radar_id.as_deref(),
            query.limit.unwrap_or(100),
            query.after,
        );

        Json(events).into_response()
    }

    /// POST /v2/api/debug/recording/start - Start debug recording
    #[debug_handler]
    pub async fn debug_recording_start_handler(
        State(state): State<Web>,
        Json(request): Json<StartDebugRecordingRequest>,
    ) -> Response {
        debug!("POST debug recording start");

        // Get the debug recorder from the hub
        let recorder = state.debug_hub.recorder();

        // Convert radar info
        let radars: Vec<RecordedRadar> = request
            .radars
            .into_iter()
            .map(|r| RecordedRadar {
                radar_id: r.radar_id,
                brand: r.brand,
                model: r.model,
                address: r.address,
            })
            .collect();

        match recorder.start(radars) {
            Ok(filename) => Json(DebugRecordingResponse {
                success: true,
                filename: Some(filename),
                error: None,
            })
            .into_response(),
            Err(e) => (
                StatusCode::CONFLICT,
                Json(DebugRecordingResponse {
                    success: false,
                    filename: None,
                    error: Some(e.to_string()),
                }),
            )
                .into_response(),
        }
    }

    /// POST /v2/api/debug/recording/stop - Stop debug recording
    #[debug_handler]
    pub async fn debug_recording_stop_handler(State(state): State<Web>) -> Response {
        debug!("POST debug recording stop");

        let recorder = state.debug_hub.recorder();

        match recorder.stop() {
            Ok(path) => Json(DebugRecordingResponse {
                success: true,
                filename: path.file_name().map(|n| n.to_string_lossy().to_string()),
                error: None,
            })
            .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(DebugRecordingResponse {
                    success: false,
                    filename: None,
                    error: Some(e.to_string()),
                }),
            )
                .into_response(),
        }
    }

    /// GET /v2/api/debug/recordings - List debug recordings
    #[debug_handler]
    pub async fn debug_recordings_list_handler(State(state): State<Web>) -> Response {
        debug!("GET debug recordings list");

        let recorder = state.debug_hub.recorder();
        let output_dir = recorder.output_dir().to_path_buf();
        let manager = DebugRecordingManager::new(output_dir);

        match manager.list() {
            Ok(recordings) => Json(DebugRecordingsListResponse { recordings }).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use mayara_server::debug::{DebugEventPayload, EventSource, IoDirection, ProtocolType};

        #[test]
        fn test_debug_server_message_serialization() {
            // Test that DebugServerMessage serializes correctly with distinct type fields
            let event = DebugEvent {
                id: 123,
                timestamp: 1000,
                radar_id: "radar-1".to_string(),
                brand: "furuno".to_string(),
                source: EventSource::IoProvider,
                payload: DebugEventPayload::Data {
                    direction: IoDirection::Recv,
                    protocol: ProtocolType::Tcp,
                    local_addr: None,
                    remote_addr: "172.31.1.4".to_string(),
                    remote_port: 10050,
                    raw_hex: "24 4e 36 33".to_string(),
                    raw_ascii: "$N63".to_string(),
                    decoded: None,
                    length: 4,
                },
            };

            let msg = DebugServerMessage::Event(event);
            let json = serde_json::to_string_pretty(&msg).unwrap();

            // Verify both type fields are present and distinct
            assert!(
                json.contains(r#""type": "event""#),
                "Should have WebSocket message type 'event'"
            );
            assert!(
                json.contains(r#""eventType": "data""#),
                "Should have payload eventType 'data'"
            );

            // Verify parsed type field is correct
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.get("type").and_then(|v| v.as_str()), Some("event"));
            assert_eq!(
                parsed.get("eventType").and_then(|v| v.as_str()),
                Some("data")
            );
        }
    }
}

#[cfg(feature = "dev")]
use debug_handlers::*;
