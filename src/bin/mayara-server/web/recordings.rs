//! REST API routes for radar recording and playback.

use axum::{
    Json,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

use mayara::recording::{
    ActivePlayback, ActiveRecording, PlaybackSettings, PlaybackStatus, RecordingManager,
    RecordingStatus,
    player::{load_recording, unregister_playback_radar},
    recorder::{build_initial_state, start_recording},
};

use super::Web;

/// Shared recording state accessible across request handlers
#[derive(Clone)]
pub struct RecordingState {
    pub active_recording: Arc<RwLock<Option<ActiveRecording>>>,
    pub active_playback: Arc<RwLock<Option<ActivePlayback>>>,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active_recording: Arc::new(RwLock::new(None)),
            active_playback: Arc::new(RwLock::new(None)),
        }
    }
}

// Request/response types

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRecordingRequest {
    radar_id: String,
    filename: Option<String>,
    subdirectory: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadPlaybackRequest {
    filename: String,
    subdirectory: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeekRequest {
    position_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameRequest {
    new_name: String,
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(rename = "dir")]
    subdirectory: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordableRadar {
    id: String,
    name: String,
    brand: String,
}

const RECORDINGS_BASE: &str = "/v2/api/vessels/self/radars/recordings";

/// Maximum size for an uploaded recording file (2 GiB).
const MAX_UPLOAD_SIZE: usize = 2 * 1024 * 1024 * 1024;

fn validate_filename(filename: &str) -> Result<(), &'static str> {
    if filename.is_empty()
        || filename.contains("..")
        || filename.contains('/')
        || filename.contains('\\')
        || filename.starts_with('.')
        || !filename.is_ascii()
    {
        return Err("Invalid filename");
    }
    Ok(())
}

fn validate_upload_filename(filename: &str) -> Result<(), &'static str> {
    validate_filename(filename)?;
    let lower = filename.to_ascii_lowercase();
    if !lower.ends_with(".mrr") && !lower.ends_with(".gz") {
        return Err("Invalid file extension (expected .mrr or .gz)");
    }
    Ok(())
}

/// Extract the filename from a Content-Disposition header value.
///
/// Supports the common `attachment; filename="foo.mrr"` form used by the GUI.
/// Only a quoted `filename` parameter is recognized.
fn parse_content_disposition_filename(value: &str) -> Option<String> {
    for part in value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("filename=") {
            let trimmed = rest.trim().trim_matches('"');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn sanitize_for_header(filename: &str) -> String {
    filename
        .replace('"', "_")
        .replace('\n', "_")
        .replace('\r', "_")
}

pub fn routes(router: axum::Router<Web>) -> axum::Router<Web> {
    router
        // Recording control
        .route(
            &format!("{}/radars", RECORDINGS_BASE),
            get(get_recordable_radars),
        )
        .route(
            &format!("{}/record/start", RECORDINGS_BASE),
            post(start_recording_handler),
        )
        .route(
            &format!("{}/record/stop", RECORDINGS_BASE),
            post(stop_recording_handler),
        )
        .route(
            &format!("{}/record/status", RECORDINGS_BASE),
            get(get_recording_status),
        )
        // Playback control
        .route(
            &format!("{}/playback/load", RECORDINGS_BASE),
            post(load_playback_handler),
        )
        .route(
            &format!("{}/playback/play", RECORDINGS_BASE),
            post(play_handler),
        )
        .route(
            &format!("{}/playback/pause", RECORDINGS_BASE),
            post(pause_handler),
        )
        .route(
            &format!("{}/playback/stop", RECORDINGS_BASE),
            post(stop_playback_handler),
        )
        .route(
            &format!("{}/playback/seek", RECORDINGS_BASE),
            post(seek_handler),
        )
        .route(
            &format!("{}/playback/settings", RECORDINGS_BASE),
            put(settings_handler),
        )
        .route(
            &format!("{}/playback/status", RECORDINGS_BASE),
            get(get_playback_status),
        )
        // File management
        .route(
            &format!("{}/files", RECORDINGS_BASE),
            get(list_recordings_handler),
        )
        .route(
            &format!("{}/files/{{filename}}", RECORDINGS_BASE),
            get(get_recording_handler)
                .delete(delete_recording_handler)
                .put(rename_recording_handler),
        )
        .route(
            &format!("{}/files/{{filename}}/download", RECORDINGS_BASE),
            get(download_recording_handler),
        )
        .route(
            &format!("{}/files/upload", RECORDINGS_BASE),
            post(upload_recording_handler).layer(DefaultBodyLimit::max(MAX_UPLOAD_SIZE)),
        )
        .route(
            &format!("{}/directories", RECORDINGS_BASE),
            get(list_directories_handler).post(create_directory_handler),
        )
        .route(
            &format!("{}/directories/{{name}}", RECORDINGS_BASE),
            delete(delete_directory_handler),
        )
}

// --- Recording control handlers ---

async fn get_recordable_radars(State(state): State<Web>) -> impl IntoResponse {
    let radars = state.radars.get_active();
    let recordable: Vec<RecordableRadar> = radars
        .iter()
        .filter(|r| !r.replay())
        .map(|r| RecordableRadar {
            id: r.key(),
            name: r.controls.user_name(),
            brand: format!("{}", r.brand),
        })
        .collect();

    Json(recordable)
}

async fn start_recording_handler(
    State(state): State<Web>,
    Json(req): Json<StartRecordingRequest>,
) -> impl IntoResponse {
    if let Some(ref f) = req.filename
        && let Err(e) = validate_filename(f)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }
    if let Some(ref sub) = req.subdirectory
        && let Err(e) = validate_filename(sub)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }

    let mut active = state.recording_state.active_recording.write().await;

    if active.is_some() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Recording already in progress"})),
        );
    }

    let radar = match state.radars.get_by_key(&req.radar_id) {
        Some(r) => r,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Radar not found"})),
            );
        }
    };

    // Recording a playback radar would copy the recorded stream back into
    // a new file. Reject it at the API boundary so callers can't bypass
    // the GUI filter in get_recordable_radars.
    if radar.replay() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Cannot record a playback radar"})),
        );
    }

    // Capture the source radar's full identity so playback can reproduce
    // the same legend and brand shape rather than rendering against a
    // generic Brand::Playback default.
    let capabilities_json = mayara::recording::RecordingIdentity::from_radar_info(&radar).to_json();

    let initial_state = build_initial_state(&radar);

    match start_recording(
        &radar,
        &req.radar_id,
        req.filename.as_deref(),
        req.subdirectory.as_deref(),
        &capabilities_json,
        &initial_state,
    )
    .await
    {
        Ok(recording) => {
            let status = recording.status();
            *active = Some(recording);
            (StatusCode::OK, Json(serde_json::to_value(status).unwrap()))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ),
    }
}

async fn stop_recording_handler(State(state): State<Web>) -> impl IntoResponse {
    let mut active = state.recording_state.active_recording.write().await;

    match active.take() {
        Some(recording) => {
            recording.stop();
            (
                StatusCode::OK,
                Json(serde_json::json!({"state": "stopped"})),
            )
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No active recording"})),
        ),
    }
}

async fn get_recording_status(State(state): State<Web>) -> impl IntoResponse {
    let active = state.recording_state.active_recording.read().await;

    match active.as_ref() {
        Some(recording) if recording.is_running() => {
            Json(serde_json::to_value(recording.status()).unwrap())
        }
        _ => Json(serde_json::to_value(RecordingStatus::default()).unwrap()),
    }
}

// --- Playback control handlers ---

async fn load_playback_handler(
    State(state): State<Web>,
    Json(req): Json<LoadPlaybackRequest>,
) -> impl IntoResponse {
    let mut active = state.recording_state.active_playback.write().await;

    // Stop existing playback if any
    if let Some(existing) = active.take() {
        existing.stop();
        unregister_playback_radar(&state.radars, existing.radar_key());
    }

    match load_recording(
        &state.args,
        &state.radars,
        &req.filename,
        req.subdirectory.as_deref(),
    )
    .await
    {
        Ok(playback) => {
            let status = playback.status().await;
            *active = Some(playback);
            (StatusCode::OK, Json(serde_json::to_value(status).unwrap()))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ),
    }
}

async fn play_handler(State(state): State<Web>) -> impl IntoResponse {
    let active = state.recording_state.active_playback.read().await;

    match active.as_ref() {
        Some(playback) => {
            playback.resume();
            (
                StatusCode::OK,
                Json(serde_json::json!({"state": "playing"})),
            )
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No active playback"})),
        ),
    }
}

async fn pause_handler(State(state): State<Web>) -> impl IntoResponse {
    let active = state.recording_state.active_playback.read().await;

    match active.as_ref() {
        Some(playback) => {
            playback.pause();
            (StatusCode::OK, Json(serde_json::json!({"state": "paused"})))
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No active playback"})),
        ),
    }
}

async fn stop_playback_handler(State(state): State<Web>) -> impl IntoResponse {
    let mut active = state.recording_state.active_playback.write().await;

    match active.take() {
        Some(playback) => {
            playback.stop();
            unregister_playback_radar(&state.radars, playback.radar_key());
            (
                StatusCode::OK,
                Json(serde_json::json!({"state": "stopped"})),
            )
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No active playback"})),
        ),
    }
}

async fn seek_handler(State(state): State<Web>, Json(req): Json<SeekRequest>) -> impl IntoResponse {
    let active = state.recording_state.active_playback.read().await;

    match active.as_ref() {
        Some(playback) => {
            playback.seek(req.position_ms).await;
            (StatusCode::OK, Json(serde_json::json!({"ok": true})))
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No active playback"})),
        ),
    }
}

async fn settings_handler(
    State(state): State<Web>,
    Json(req): Json<PlaybackSettings>,
) -> impl IntoResponse {
    let active = state.recording_state.active_playback.read().await;

    match active.as_ref() {
        Some(playback) => {
            if let Some(speed) = req.speed {
                playback.set_speed(speed);
            }
            if let Some(loop_playback) = req.loop_playback {
                playback.set_loop(loop_playback);
            }
            let status = playback.status().await;
            (StatusCode::OK, Json(serde_json::to_value(status).unwrap()))
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No active playback"})),
        ),
    }
}

async fn get_playback_status(State(state): State<Web>) -> impl IntoResponse {
    let active = state.recording_state.active_playback.read().await;

    match active.as_ref() {
        Some(playback) => Json(serde_json::to_value(playback.status().await).unwrap()),
        None => Json(serde_json::to_value(PlaybackStatus::default()).unwrap()),
    }
}

// --- File management handlers ---

async fn list_recordings_handler(Query(query): Query<ListQuery>) -> impl IntoResponse {
    let manager = RecordingManager::new();
    let recordings = manager.list_recordings(query.subdirectory.as_deref());
    let total_size: u64 = recordings.iter().map(|r| r.size).sum();
    let total_count = recordings.len();
    Json(serde_json::json!({
        "recordings": recordings,
        "totalCount": total_count,
        "totalSize": total_size,
    }))
}

async fn get_recording_handler(
    Path(filename): Path<String>,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = validate_filename(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }
    let manager = RecordingManager::new();
    match manager.get_recording(&filename, query.subdirectory.as_deref()) {
        Some(info) => (StatusCode::OK, Json(serde_json::to_value(info).unwrap())),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Recording not found"})),
        ),
    }
}

async fn delete_recording_handler(
    Path(filename): Path<String>,
    Query(query): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = validate_filename(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }
    let manager = RecordingManager::new();
    match manager.delete_recording(&filename, query.subdirectory.as_deref()) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ),
    }
}

async fn rename_recording_handler(
    Path(filename): Path<String>,
    Query(query): Query<ListQuery>,
    Json(req): Json<RenameRequest>,
) -> impl IntoResponse {
    if let Err(e) = validate_filename(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }
    if let Err(e) = validate_filename(&req.new_name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }
    let manager = RecordingManager::new();
    match manager.rename_recording(&filename, &req.new_name, query.subdirectory.as_deref()) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ),
    }
}

async fn list_directories_handler() -> impl IntoResponse {
    let manager = RecordingManager::new();
    Json(manager.list_directories())
}

#[derive(Deserialize)]
struct CreateDirectoryRequest {
    name: String,
}

async fn create_directory_handler(Json(req): Json<CreateDirectoryRequest>) -> impl IntoResponse {
    if let Err(e) = validate_filename(&req.name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }
    let manager = RecordingManager::new();
    match manager.create_directory(&req.name) {
        Ok(()) => (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ),
    }
}

async fn delete_directory_handler(Path(name): Path<String>) -> impl IntoResponse {
    if let Err(e) = validate_filename(&name) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }
    let manager = RecordingManager::new();
    match manager.delete_directory(&name) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        ),
    }
}

async fn download_recording_handler(
    Path(filename): Path<String>,
    Query(query): Query<ListQuery>,
) -> axum::response::Response {
    if let Err(e) = validate_filename(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response();
    }
    if let Some(ref sub) = query.subdirectory
        && let Err(e) = validate_filename(sub)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response();
    }
    let manager = RecordingManager::new();
    let info = match manager.get_recording(&filename, query.subdirectory.as_deref()) {
        Some(info) => info,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "Recording not found"})),
            )
                .into_response();
        }
    };
    let path = info.path;

    match tokio::fs::File::open(&path).await {
        Ok(file) => {
            let stream = tokio_util::io::ReaderStream::new(file);
            let body = Body::from_stream(stream);
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".parse().unwrap(),
            );
            headers.insert(
                axum::http::header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{}\"",
                    sanitize_for_header(&filename)
                )
                .parse()
                .unwrap(),
            );
            (StatusCode::OK, headers, body).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to open file: {}", e)})),
        )
            .into_response(),
    }
}

async fn upload_recording_handler(headers: HeaderMap, body: Body) -> impl IntoResponse {
    let filename = headers
        .get(axum::http::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_disposition_filename);

    let filename = match filename {
        Some(f) => f,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!(
                    {"error": "Missing or invalid Content-Disposition filename"}
                )),
            );
        }
    };

    if let Err(e) = validate_upload_filename(&filename) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        );
    }

    let manager = RecordingManager::new();
    let target_path = manager.get_recording_path(&filename, None);

    if target_path.exists() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "File already exists"})),
        );
    }

    let tmp_path = target_path.with_file_name(format!("{}.upload", filename));
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_path)
        .await
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "Upload already in progress"})),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!(
                    {"error": format!("Failed to create file: {}", e)}
                )),
            );
        }
    };

    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!(
                        {"error": format!("Failed to read upload body: {}", e)}
                    )),
                );
            }
        };
        total += chunk.len() as u64;
        if total > MAX_UPLOAD_SIZE as u64 {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({"error": "Upload exceeds 2 GiB limit"})),
            );
        }
        if let Err(e) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!(
                    {"error": format!("Failed to write file: {}", e)}
                )),
            );
        }
    }

    if let Err(e) = file.sync_data().await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!(
                {"error": format!("Failed to sync file: {}", e)}
            )),
        );
    }
    drop(file);

    if let Err(e) = tokio::fs::rename(&tmp_path, &target_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!(
                {"error": format!("Failed to finalize upload: {}", e)}
            )),
        );
    }

    log::info!("Uploaded recording: {} ({} bytes)", filename, total);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "filename": filename,
            "size": total,
        })),
    )
}
