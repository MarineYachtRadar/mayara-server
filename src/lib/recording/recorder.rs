//! Radar recorder - subscribes to radar broadcast and writes to .mrr file.

use log::{debug, error, info, warn};
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

use crate::Brand;
use crate::radar::RadarInfo;

use super::file_format::{MrrFrame, MrrWriter};
use super::manager::{RecordingManager, recordings_dir};

/// Recording state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Stopping,
}

/// Recording status information
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub state: String,
    pub radar_id: Option<String>,
    pub filename: Option<String>,
    pub subdirectory: Option<String>,
    pub frame_count: u32,
    pub duration_ms: u64,
    pub size_bytes: u64,
    pub start_time_ms: Option<u64>,
}

impl Default for RecordingStatus {
    fn default() -> Self {
        Self {
            state: "idle".to_string(),
            radar_id: None,
            filename: None,
            subdirectory: None,
            frame_count: 0,
            duration_ms: 0,
            size_bytes: 0,
            start_time_ms: None,
        }
    }
}

/// Active recording handle
pub struct ActiveRecording {
    stop_flag: Arc<AtomicBool>,
    radar_id: String,
    filename: String,
    subdirectory: Option<String>,
    frame_count: Arc<AtomicU32>,
    duration_ms: Arc<AtomicU64>,
    size_bytes: Arc<AtomicU64>,
    start_time_ms: u64,
}

impl ActiveRecording {
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        !self.stop_flag.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> RecordingStatus {
        RecordingStatus {
            state: "recording".to_string(),
            radar_id: Some(self.radar_id.clone()),
            filename: Some(self.filename.clone()),
            subdirectory: self.subdirectory.clone(),
            frame_count: self.frame_count.load(Ordering::Relaxed),
            duration_ms: self.duration_ms.load(Ordering::Relaxed),
            size_bytes: self.size_bytes.load(Ordering::Relaxed),
            start_time_ms: Some(self.start_time_ms),
        }
    }

    pub fn radar_id(&self) -> &str {
        &self.radar_id
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }
}

/// Start recording from a radar
fn is_valid_name(name: &str) -> bool {
    !name.contains('/') && !name.contains('\\') && !name.contains("..")
}

pub async fn start_recording(
    radar_info: &RadarInfo,
    radar_key: &str,
    filename: Option<&str>,
    subdirectory: Option<&str>,
    capabilities_json: &[u8],
    initial_state_json: &[u8],
) -> Result<ActiveRecording, String> {
    // Validate inputs before any filesystem operations
    if let Some(f) = filename {
        if !is_valid_name(f) {
            return Err("Invalid filename".to_string());
        }
    }
    if let Some(sub) = subdirectory {
        if !is_valid_name(sub) {
            return Err("Invalid subdirectory".to_string());
        }
    }

    let manager = RecordingManager::new();

    let filename = match filename {
        Some(f) => {
            let f = if f.ends_with(".mrr") {
                f.to_string()
            } else {
                format!("{}.mrr", f)
            };
            let path = manager.get_recording_path(&f, subdirectory);
            if path.exists() {
                return Err(format!("File already exists: {}", f));
            }
            f
        }
        None => {
            let prefix = radar_info.controls.user_name().replace(' ', "_");
            let prefix = if prefix.is_empty() {
                format!("radar-{}", radar_key)
            } else {
                prefix
            };
            manager.generate_filename(Some(&prefix), subdirectory)
        }
    };

    if let Some(sub) = subdirectory {
        let sub_path = recordings_dir().join(sub);
        if !sub_path.exists() {
            std::fs::create_dir_all(&sub_path)
                .map_err(|e| format!("Failed to create subdirectory: {}", e))?;
        }
    }

    let path = manager.get_recording_path(&filename, subdirectory);
    info!("Starting recording to: {}", path.display());

    let file = File::create(&path).map_err(|e| format!("Failed to create file: {}", e))?;
    let writer = BufWriter::new(file);

    let brand_id = brand_to_id(radar_info.brand);
    let mrr_writer = MrrWriter::new(
        writer,
        brand_id,
        radar_info.spokes_per_revolution as u32,
        radar_info.max_spoke_len as u32,
        radar_info.pixel_values as u32,
        capabilities_json,
        initial_state_json,
    )
    .map_err(|e| format!("Failed to create MRR writer: {}", e))?;

    let stop_flag = Arc::new(AtomicBool::new(false));
    let frame_count = Arc::new(AtomicU32::new(0));
    let duration_ms = Arc::new(AtomicU64::new(0));
    let size_bytes = Arc::new(AtomicU64::new(0));

    let start_time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let active = ActiveRecording {
        stop_flag: stop_flag.clone(),
        radar_id: radar_key.to_string(),
        filename: filename.clone(),
        subdirectory: subdirectory.map(String::from),
        frame_count: frame_count.clone(),
        duration_ms: duration_ms.clone(),
        size_bytes: size_bytes.clone(),
        start_time_ms,
    };

    let message_rx = radar_info.message_tx.subscribe();

    let path_clone = path.clone();
    tokio::spawn(async move {
        recording_task(
            mrr_writer,
            message_rx,
            stop_flag,
            frame_count,
            duration_ms,
            size_bytes,
            path_clone,
        )
        .await;
    });

    Ok(active)
}

async fn recording_task(
    mut writer: MrrWriter<BufWriter<File>>,
    mut message_rx: broadcast::Receiver<Vec<u8>>,
    stop_flag: Arc<AtomicBool>,
    frame_count: Arc<AtomicU32>,
    duration_ms: Arc<AtomicU64>,
    size_bytes: Arc<AtomicU64>,
    path: PathBuf,
) {
    let start = std::time::Instant::now();
    let mut frames = 0u32;
    let mut approx_size = 0u64;

    debug!("Recording task started for {}", path.display());

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            debug!("Recording stop flag detected");
            break;
        }

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(100), message_rx.recv()).await;

        match result {
            Ok(Ok(data)) => {
                let timestamp_ms = start.elapsed().as_millis() as u64;
                let frame = MrrFrame::new(timestamp_ms, data);

                approx_size += frame.size() as u64;

                if let Err(e) = writer.write_frame(&frame) {
                    error!("Failed to write frame: {}", e);
                    break;
                }

                frames += 1;

                if frames % 10 == 0 {
                    frame_count.store(frames, Ordering::Relaxed);
                    duration_ms.store(timestamp_ms, Ordering::Relaxed);
                    size_bytes.store(approx_size, Ordering::Relaxed);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                warn!("Recording lagged, missed {} messages", n);
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                info!("Radar broadcast channel closed");
                break;
            }
            Err(_) => {
                // Timeout - check stop flag
            }
        }
    }

    let final_duration = start.elapsed().as_millis() as u64;
    frame_count.store(frames, Ordering::Relaxed);
    duration_ms.store(final_duration, Ordering::Relaxed);
    size_bytes.store(approx_size, Ordering::Relaxed);

    match writer.finish() {
        Ok(()) => {
            info!(
                "Recording finished: {} frames, {}ms duration, {} bytes (approx)",
                frames, final_duration, approx_size
            );
        }
        Err(e) => {
            error!("Failed to finish recording: {}", e);
        }
    }

    stop_flag.store(true, Ordering::SeqCst);
}

/// Build initial state JSON from radar controls
pub fn build_initial_state(radar_info: &RadarInfo) -> Vec<u8> {
    let controls = radar_info.controls.get_controls();
    let mut state = std::collections::BTreeMap::new();
    for (id, control) in &controls {
        if let Some(value) = control.value() {
            state.insert(format!("{:?}", id), value);
        }
    }
    serde_json::to_vec(&state).unwrap_or_else(|_| b"{}".to_vec())
}

pub fn brand_to_id(brand: Brand) -> u32 {
    match brand {
        Brand::Furuno => 1,
        Brand::Garmin => 2,
        Brand::Navico => 3,
        Brand::Raymarine => 4,
        Brand::Emulator => 5,
        Brand::Playback => 6,
        Brand::Koden => 7,
    }
}

pub fn id_to_brand(id: u32) -> Option<Brand> {
    match id {
        1 => Some(Brand::Furuno),
        2 => Some(Brand::Garmin),
        3 => Some(Brand::Navico),
        4 => Some(Brand::Raymarine),
        5 => Some(Brand::Emulator),
        6 => Some(Brand::Playback),
        7 => Some(Brand::Koden),
        _ => None,
    }
}
