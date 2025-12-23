//! Radar playback - reads .mrr files and emits frames as a virtual radar.

use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};

use crate::locator::LocatorId;
use crate::radar::{RadarInfo, SharedRadars};
use crate::settings::SharedControls;
use crate::Brand;
use crate::Session;

use super::file_format::MrrReader;
use super::manager::RecordingManager;
use super::recorder::id_to_brand;

/// Playback state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Loaded,
    Playing,
    Paused,
    Stopped,
}

impl std::fmt::Display for PlaybackState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlaybackState::Idle => write!(f, "idle"),
            PlaybackState::Loaded => write!(f, "loaded"),
            PlaybackState::Playing => write!(f, "playing"),
            PlaybackState::Paused => write!(f, "paused"),
            PlaybackState::Stopped => write!(f, "stopped"),
        }
    }
}

/// Playback status information
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackStatus {
    /// Current state
    pub state: String,
    /// Loaded filename (if any)
    pub filename: Option<String>,
    /// Virtual radar ID (if loaded)
    pub radar_id: Option<String>,
    /// Current position in milliseconds
    pub position_ms: u64,
    /// Total duration in milliseconds
    pub duration_ms: u64,
    /// Current frame number
    pub frame: u32,
    /// Total frame count
    pub frame_count: u32,
    /// Playback speed multiplier
    pub speed: f32,
    /// Loop playback
    pub loop_playback: bool,
}

impl Default for PlaybackStatus {
    fn default() -> Self {
        Self {
            state: "idle".to_string(),
            filename: None,
            radar_id: None,
            position_ms: 0,
            duration_ms: 0,
            frame: 0,
            frame_count: 0,
            speed: 1.0,
            loop_playback: false,
        }
    }
}

/// Playback settings
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSettings {
    #[serde(default)]
    pub speed: Option<f32>,
    #[serde(default)]
    pub loop_playback: Option<bool>,
}

/// Active playback handle
pub struct ActivePlayback {
    /// Stop flag
    stop_flag: Arc<AtomicBool>,
    /// Pause flag
    pause_flag: Arc<AtomicBool>,
    /// Current position in ms
    position_ms: Arc<AtomicU64>,
    /// Current frame number
    frame: Arc<AtomicU32>,
    /// Playback speed (stored as fixed point: 100 = 1.0x)
    speed: Arc<AtomicU32>,
    /// Loop playback
    loop_playback: Arc<AtomicBool>,
    /// Loaded filename
    filename: String,
    /// Virtual radar ID
    radar_id: String,
    /// Radar key for SharedRadars (includes brand prefix)
    radar_key: String,
    /// Total duration
    duration_ms: u64,
    /// Total frame count
    frame_count: u32,
    /// State
    state: Arc<RwLock<PlaybackState>>,
    /// Seek target (None = no seek, Some(ms) = seek to position)
    seek_target: Arc<RwLock<Option<u64>>>,
}

impl ActivePlayback {
    /// Signal the playback to stop
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }

    /// Pause playback
    pub fn pause(&self) {
        self.pause_flag.store(true, Ordering::SeqCst);
    }

    /// Resume playback
    pub fn resume(&self) {
        self.pause_flag.store(false, Ordering::SeqCst);
    }

    /// Check if playback is paused
    pub fn is_paused(&self) -> bool {
        self.pause_flag.load(Ordering::SeqCst)
    }

    /// Check if playback is stopped
    pub fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::SeqCst)
    }

    /// Set playback speed (1.0 = normal, 0.5 = half, 2.0 = double)
    pub fn set_speed(&self, speed: f32) {
        let speed_fixed = (speed * 100.0) as u32;
        self.speed
            .store(speed_fixed.clamp(10, 1000), Ordering::SeqCst);
    }

    /// Get playback speed
    pub fn get_speed(&self) -> f32 {
        self.speed.load(Ordering::SeqCst) as f32 / 100.0
    }

    /// Set loop playback
    pub fn set_loop(&self, loop_playback: bool) {
        self.loop_playback.store(loop_playback, Ordering::SeqCst);
    }

    /// Seek to position
    pub async fn seek(&self, position_ms: u64) {
        let mut target = self.seek_target.write().await;
        *target = Some(position_ms);
    }

    /// Get radar ID
    pub fn radar_id(&self) -> &str {
        &self.radar_id
    }

    /// Get radar key (for SharedRadars operations)
    pub fn radar_key(&self) -> &str {
        &self.radar_key
    }

    /// Get filename
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Get current status
    pub async fn status(&self) -> PlaybackStatus {
        let state = self.state.read().await;
        PlaybackStatus {
            state: state.to_string(),
            filename: Some(self.filename.clone()),
            radar_id: Some(self.radar_id.clone()),
            position_ms: self.position_ms.load(Ordering::Relaxed),
            duration_ms: self.duration_ms,
            frame: self.frame.load(Ordering::Relaxed),
            frame_count: self.frame_count,
            speed: self.get_speed(),
            loop_playback: self.loop_playback.load(Ordering::Relaxed),
        }
    }

    /// Update state
    #[allow(dead_code)]
    async fn set_state(&self, new_state: PlaybackState) {
        let mut state = self.state.write().await;
        *state = new_state;
    }
}

/// Load a recording and prepare for playback (doesn't start playing yet)
pub async fn load_recording(
    session: Session,
    radars: &SharedRadars,
    filename: &str,
    subdirectory: Option<&str>,
) -> Result<ActivePlayback, String> {
    let manager = RecordingManager::new();
    let path = manager.get_recording_path(filename, subdirectory);

    if !path.exists() {
        return Err(format!("Recording not found: {}", filename));
    }

    info!("Loading recording: {}", path.display());

    // Open and parse the file
    let file = File::open(&path).map_err(|e| format!("Failed to open file: {}", e))?;
    let reader = BufReader::new(file);
    let mrr_reader =
        MrrReader::open(reader).map_err(|e| format!("Failed to parse recording: {}", e))?;

    let header = mrr_reader.header();
    let footer = mrr_reader.footer();

    // Determine brand from recorded value
    let brand = id_to_brand(header.radar_brand).unwrap_or(Brand::Playback);

    // Generate virtual radar ID and key
    // The key format must match RadarInfo::new() which constructs: "{brand}-{serial_no}"
    let base_name = filename.trim_end_matches(".mrr");
    let radar_id = format!("playback-{}", base_name);
    let serial_no = format!("Playback-{}", base_name);
    let radar_key = format!("{}-{}", brand, serial_no);

    // Create minimal controls for playback (read-only)
    let controls = SharedControls::new(session.clone(), HashMap::new());

    // Set power to "transmit" so the GUI shows the radar as active
    // Status enum: Off=0, Standby=1, Transmit=2
    let _ = controls.set("power", 2.0, None);

    // Set model name from capabilities if available
    if let Ok(caps_str) = std::str::from_utf8(mrr_reader.capabilities()) {
        if let Ok(caps) = serde_json::from_str::<serde_json::Value>(caps_str) {
            if let Some(model) = caps.get("radarModel").and_then(|m| m.as_str()) {
                controls.set_model_name(model.to_string());
            }
        }
    }

    // Create fake addresses for playback radar
    let fake_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 0);

    // Create RadarInfo for the virtual radar
    let info = RadarInfo::new(
        session,
        LocatorId::Playback,
        brand,
        Some(&serial_no),
        None, // which
        header.pixel_values as u8,
        header.spokes_per_rev as usize,
        header.max_spoke_len as usize,
        fake_addr,
        Ipv4Addr::LOCALHOST,
        fake_addr,
        fake_addr,
        fake_addr,
        controls,
        false, // doppler
    );

    // Remove any existing playback radar with the same key (defensive cleanup)
    radars.remove(&radar_key);

    // Register the playback radar and get its message_tx
    let message_tx = match radars.located(info) {
        Some(registered_info) => registered_info.message_tx.clone(),
        None => {
            return Err(format!(
                "Failed to register playback radar with key '{}' (registration rejected)",
                radar_key
            ));
        }
    };

    info!(
        "Registered playback radar: {} ({}ms, {} frames)",
        radar_id, footer.duration_ms, footer.frame_count
    );

    // Create shared state
    let stop_flag = Arc::new(AtomicBool::new(false));
    let pause_flag = Arc::new(AtomicBool::new(true)); // Start paused (loaded but not playing)
    let position_ms = Arc::new(AtomicU64::new(0));
    let frame = Arc::new(AtomicU32::new(0));
    let speed = Arc::new(AtomicU32::new(100)); // 1.0x
    let loop_playback = Arc::new(AtomicBool::new(false));
    let state = Arc::new(RwLock::new(PlaybackState::Loaded));
    let seek_target = Arc::new(RwLock::new(None));

    let active = ActivePlayback {
        stop_flag: stop_flag.clone(),
        pause_flag: pause_flag.clone(),
        position_ms: position_ms.clone(),
        frame: frame.clone(),
        speed: speed.clone(),
        loop_playback: loop_playback.clone(),
        filename: filename.to_string(),
        radar_id: radar_id.clone(),
        radar_key: radar_key.clone(),
        duration_ms: footer.duration_ms,
        frame_count: footer.frame_count,
        state: state.clone(),
        seek_target: seek_target.clone(),
    };

    // Spawn the playback task
    let path_clone = path.clone();
    tokio::spawn(async move {
        playback_task(
            path_clone,
            message_tx,
            stop_flag,
            pause_flag,
            position_ms,
            frame,
            speed,
            loop_playback,
            state,
            seek_target,
        )
        .await;
    });

    Ok(active)
}

/// Playback task that runs in the background
async fn playback_task(
    path: PathBuf,
    message_tx: broadcast::Sender<Vec<u8>>,
    stop_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    position_ms: Arc<AtomicU64>,
    frame_counter: Arc<AtomicU32>,
    speed: Arc<AtomicU32>,
    loop_playback: Arc<AtomicBool>,
    state: Arc<RwLock<PlaybackState>>,
    seek_target: Arc<RwLock<Option<u64>>>,
) {
    debug!("Playback task started for {}", path.display());

    loop {
        // Check if stopped
        if stop_flag.load(Ordering::SeqCst) {
            debug!("Playback stop flag detected");
            break;
        }

        // Check if paused
        if pause_flag.load(Ordering::SeqCst) {
            // Update state to paused if we were playing
            {
                let mut s = state.write().await;
                if *s == PlaybackState::Playing {
                    *s = PlaybackState::Paused;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }

        // Update state to playing
        {
            let mut s = state.write().await;
            if *s == PlaybackState::Loaded || *s == PlaybackState::Paused {
                *s = PlaybackState::Playing;
            }
        }

        // Open file for this playback iteration
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to open file for playback: {}", e);
                break;
            }
        };

        let reader = BufReader::new(file);
        let mut mrr_reader = match MrrReader::open(reader) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to parse recording for playback: {}", e);
                break;
            }
        };

        // Check for seek target, or resume from current position
        {
            let mut target = seek_target.write().await;
            let seek_ms = target
                .take()
                .unwrap_or_else(|| position_ms.load(Ordering::Relaxed));
            if seek_ms > 0 {
                if let Err(e) = mrr_reader.seek_to_timestamp(seek_ms) {
                    warn!("Seek to {}ms failed: {}", seek_ms, e);
                }
            }
        }

        let playback_start = Instant::now();
        let mut first_frame_ts: Option<u64> = None;
        let mut frames_played = 0u32;

        // Main playback loop
        loop {
            // Check if stopped
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }

            // Check if paused
            if pause_flag.load(Ordering::SeqCst) {
                break; // Will re-enter outer loop and wait
            }

            // Check for seek request
            {
                let mut target = seek_target.write().await;
                if let Some(seek_ms) = target.take() {
                    if let Err(e) = mrr_reader.seek_to_timestamp(seek_ms) {
                        warn!("Seek failed: {}", e);
                    }
                    // Reset timing
                    first_frame_ts = None;
                    continue;
                }
            }

            // Read next frame
            let frame = match mrr_reader.read_frame() {
                Ok(Some(f)) => f,
                Ok(None) => {
                    // End of file
                    if loop_playback.load(Ordering::SeqCst) {
                        // Rewind and continue
                        if let Err(e) = mrr_reader.rewind() {
                            error!("Failed to rewind: {}", e);
                            break;
                        }
                        first_frame_ts = None;
                        continue;
                    } else {
                        // Playback complete
                        break;
                    }
                }
                Err(e) => {
                    error!("Failed to read frame: {}", e);
                    break;
                }
            };

            // Calculate timing
            let speed_factor = speed.load(Ordering::SeqCst) as f64 / 100.0;
            let frame_ts = frame.timestamp_ms;

            if first_frame_ts.is_none() {
                first_frame_ts = Some(frame_ts);
            }

            let relative_ts = frame_ts - first_frame_ts.unwrap();
            let target_elapsed = Duration::from_millis((relative_ts as f64 / speed_factor) as u64);
            let actual_elapsed = playback_start.elapsed();

            if target_elapsed > actual_elapsed {
                let wait_time = target_elapsed - actual_elapsed;
                // Don't wait too long in one go (check for stop/pause)
                let max_wait = Duration::from_millis(100);
                if wait_time > max_wait {
                    tokio::time::sleep(max_wait).await;
                    continue; // Re-check conditions
                }
                tokio::time::sleep(wait_time).await;
            }

            // Send the frame data to connected clients
            if let Err(e) = message_tx.send(frame.data) {
                // No receivers - this is fine, just means no clients connected
                log::trace!("No receivers for playback frame: {}", e);
            }

            // Update counters
            position_ms.store(frame_ts, Ordering::Relaxed);
            frames_played += 1;
            frame_counter.store(frames_played, Ordering::Relaxed);
        }

        // Check if we should continue the outer loop
        // - If paused, stay in outer loop (will wait and resume)
        // - If stopped, exit
        // - If not looping and not paused, exit (playback finished)
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        if !pause_flag.load(Ordering::SeqCst) && !loop_playback.load(Ordering::SeqCst) {
            // Not paused and not looping = playback finished naturally
            break;
        }
    }

    // Update state to stopped and set stop flag
    stop_flag.store(true, Ordering::SeqCst);
    {
        let mut s = state.write().await;
        *s = PlaybackState::Stopped;
    }

    info!("Playback finished for {}", path.display());
}

/// Unregister a playback radar by its key
pub fn unregister_playback_radar(radars: &SharedRadars, radar_key: &str) {
    info!("Unregistering playback radar: key={}", radar_key);
    radars.remove(radar_key);
}
