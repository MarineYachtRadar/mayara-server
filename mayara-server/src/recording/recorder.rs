//! Radar recorder - subscribes to radar broadcast and writes to .mrr file.

use log::{debug, error, info, warn};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

use crate::radar::RadarInfo;
use crate::Brand;

use super::file_format::{MrrFrame, MrrWriter};
use super::manager::{recordings_dir, RecordingManager};

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
    /// Current state
    pub state: String,
    /// Radar being recorded (if any)
    pub radar_id: Option<String>,
    /// Filename being written (if any)
    pub filename: Option<String>,
    /// Subdirectory (if any)
    pub subdirectory: Option<String>,
    /// Number of frames recorded
    pub frame_count: u32,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// File size in bytes (approximate)
    pub size_bytes: u64,
    /// Recording start time (Unix timestamp ms)
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
    /// Stop flag
    stop_flag: Arc<AtomicBool>,
    /// Radar ID being recorded
    radar_id: String,
    /// Filename being written
    filename: String,
    /// Subdirectory (if any)
    subdirectory: Option<String>,
    /// Frame count (updated by recording task)
    frame_count: Arc<std::sync::atomic::AtomicU32>,
    /// Duration in ms (updated by recording task)
    duration_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Approximate size in bytes
    size_bytes: Arc<std::sync::atomic::AtomicU64>,
    /// Start time
    start_time_ms: u64,
}

impl ActiveRecording {
    /// Signal the recording to stop
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }

    /// Check if recording is still running
    pub fn is_running(&self) -> bool {
        !self.stop_flag.load(Ordering::SeqCst)
    }

    /// Get current status
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

    /// Get the radar ID
    pub fn radar_id(&self) -> &str {
        &self.radar_id
    }

    /// Get the filename
    pub fn filename(&self) -> &str {
        &self.filename
    }
}

/// Start recording from a radar
pub async fn start_recording(
    radar_info: &RadarInfo,
    radar_id: &str,
    filename: Option<&str>,
    subdirectory: Option<&str>,
    capabilities_json: &[u8],
    initial_state_json: &[u8],
) -> Result<ActiveRecording, String> {
    let manager = RecordingManager::new();

    // Generate filename if not provided
    let filename = match filename {
        Some(f) => {
            let f = if f.ends_with(".mrr") {
                f.to_string()
            } else {
                format!("{}.mrr", f)
            };
            // Check if file already exists
            let path = manager.get_recording_path(&f, subdirectory);
            if path.exists() {
                return Err(format!("File already exists: {}", f));
            }
            f
        }
        None => {
            let prefix = radar_info
                .controls
                .user_name()
                .filter(|s| !s.is_empty())
                .map(|s| s.replace(' ', "_"))
                .unwrap_or_else(|| format!("radar-{}", radar_info.id));
            manager.generate_filename(Some(&prefix), subdirectory)
        }
    };

    // Ensure subdirectory exists
    if let Some(sub) = subdirectory {
        let sub_path = recordings_dir().join(sub);
        if !sub_path.exists() {
            std::fs::create_dir_all(&sub_path)
                .map_err(|e| format!("Failed to create subdirectory: {}", e))?;
        }
    }

    // Get full path
    let path = manager.get_recording_path(&filename, subdirectory);
    info!("Starting recording to: {}", path.display());

    // Create file
    let file = File::create(&path).map_err(|e| format!("Failed to create file: {}", e))?;
    let writer = BufWriter::new(file);

    // Create MRR writer
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

    // Create shared state
    let stop_flag = Arc::new(AtomicBool::new(false));
    let frame_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let duration_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let size_bytes = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let start_time_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Create active recording handle
    let active = ActiveRecording {
        stop_flag: stop_flag.clone(),
        radar_id: radar_id.to_string(),
        filename: filename.clone(),
        subdirectory: subdirectory.map(String::from),
        frame_count: frame_count.clone(),
        duration_ms: duration_ms.clone(),
        size_bytes: size_bytes.clone(),
        start_time_ms,
    };

    // Subscribe to radar's message broadcast
    let message_rx = radar_info.message_tx.subscribe();

    // Spawn recording task
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

/// Recording task that runs in the background
async fn recording_task(
    mut writer: MrrWriter<BufWriter<File>>,
    mut message_rx: broadcast::Receiver<Vec<u8>>,
    stop_flag: Arc<AtomicBool>,
    frame_count: Arc<std::sync::atomic::AtomicU32>,
    duration_ms: Arc<std::sync::atomic::AtomicU64>,
    size_bytes: Arc<std::sync::atomic::AtomicU64>,
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

        // Use a timeout to periodically check the stop flag
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(100), message_rx.recv()).await;

        match result {
            Ok(Ok(data)) => {
                let timestamp_ms = start.elapsed().as_millis() as u64;
                let frame = MrrFrame::new(timestamp_ms, data);

                // Update size estimate
                approx_size += frame.size() as u64;

                if let Err(e) = writer.write_frame(&frame) {
                    error!("Failed to write frame: {}", e);
                    break;
                }

                frames += 1;

                // Update shared counters (not every frame to reduce overhead)
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
                // Timeout - just continue and check stop flag
            }
        }
    }

    // Update final counters
    let final_duration = start.elapsed().as_millis() as u64;
    frame_count.store(frames, Ordering::Relaxed);
    duration_ms.store(final_duration, Ordering::Relaxed);
    size_bytes.store(approx_size, Ordering::Relaxed);

    // Finish writing
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

    // Mark as stopped
    stop_flag.store(true, Ordering::SeqCst);
}

/// Build initial state JSON from radar controls
pub fn build_initial_state(radar_info: &RadarInfo) -> Vec<u8> {
    let mut state = BTreeMap::new();

    // Get all control values
    for (name, control) in radar_info.controls.get_all() {
        if let Some(value) = control.value {
            state.insert(name, value);
        }
    }

    serde_json::to_vec(&state).unwrap_or_else(|_| b"{}".to_vec())
}

/// Convert Brand enum to numeric ID
fn brand_to_id(brand: Brand) -> u32 {
    match brand {
        Brand::Furuno => 1,
        Brand::Garmin => 2,
        Brand::Navico => 3,
        Brand::Raymarine => 4,
        Brand::Playback => 5,
    }
}

/// Convert numeric ID to Brand enum
pub fn id_to_brand(id: u32) -> Option<Brand> {
    match id {
        1 => Some(Brand::Furuno),
        2 => Some(Brand::Garmin),
        3 => Some(Brand::Navico),
        4 => Some(Brand::Raymarine),
        5 => Some(Brand::Playback),
        _ => None,
    }
}
