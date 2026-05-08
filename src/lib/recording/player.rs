//! Radar playback - reads .mrr files and emits frames as a virtual radar.

use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, broadcast};

use crate::Brand;
use crate::Cli;
use crate::radar::SharedRadars;
use crate::radar::settings::{ControlId, SharedControls, new_string};

use super::file_format::MrrReader;
use super::identity::RecordingIdentity;
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
    pub state: String,
    pub filename: Option<String>,
    pub radar_id: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub frame: u32,
    pub frame_count: u32,
    pub speed: f32,
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
    stop_flag: Arc<AtomicBool>,
    pause_flag: Arc<AtomicBool>,
    position_ms: Arc<AtomicU64>,
    frame: Arc<AtomicU32>,
    /// Playback speed stored as fixed point: 100 = 1.0x
    speed: Arc<AtomicU32>,
    loop_playback: Arc<AtomicBool>,
    filename: String,
    radar_key: String,
    duration_ms: u64,
    frame_count: u32,
    state: Arc<RwLock<PlaybackState>>,
    seek_target: Arc<RwLock<Option<u64>>>,
}

impl ActivePlayback {
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }

    pub fn pause(&self) {
        self.pause_flag.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.pause_flag.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.pause_flag.load(Ordering::SeqCst)
    }

    pub fn is_stopped(&self) -> bool {
        self.stop_flag.load(Ordering::SeqCst)
    }

    pub fn set_speed(&self, speed: f32) {
        let speed_fixed = (speed * 100.0) as u32;
        self.speed
            .store(speed_fixed.clamp(10, 1000), Ordering::SeqCst);
    }

    pub fn get_speed(&self) -> f32 {
        self.speed.load(Ordering::SeqCst) as f32 / 100.0
    }

    pub fn set_loop(&self, loop_playback: bool) {
        self.loop_playback.store(loop_playback, Ordering::SeqCst);
    }

    pub async fn seek(&self, position_ms: u64) {
        let mut target = self.seek_target.write().await;
        *target = Some(position_ms);
    }

    pub fn radar_key(&self) -> &str {
        &self.radar_key
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub async fn status(&self) -> PlaybackStatus {
        let state = self.state.read().await;
        PlaybackStatus {
            state: state.to_string(),
            filename: Some(self.filename.clone()),
            radar_id: Some(self.radar_key.clone()),
            position_ms: self.position_ms.load(Ordering::Relaxed),
            duration_ms: self.duration_ms,
            frame: self.frame.load(Ordering::Relaxed),
            frame_count: self.frame_count,
            speed: self.get_speed(),
            loop_playback: self.loop_playback.load(Ordering::Relaxed),
        }
    }
}

/// Create minimal controls for a playback radar (read-only). UserName and
/// ModelName are populated from the recorded identity when available so the
/// GUI's radar list matches what was recorded.
///
/// Full control reproduction (Range, Gain, etc.) is intentionally out of
/// scope: the player would need to dispatch to brand-specific control
/// initializers and replay control deltas. This function only ensures the
/// radar shows up with a recognizable name.
fn playback_controls(
    radar_id: String,
    sk_client_tx: broadcast::Sender<crate::stream::SignalKDelta>,
    args: &Cli,
    identity: &RecordingIdentity,
) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::UserName).build(&mut controls);
    let label = identity
        .user_name
        .clone()
        .unwrap_or_else(|| format!("Playback: {}", radar_id));
    controls
        .get_mut(&ControlId::UserName)
        .unwrap()
        .set_string(label);

    new_string(ControlId::ModelName).build(&mut controls);
    let model = identity
        .model_name
        .clone()
        .unwrap_or_else(|| "Recording Playback".to_string());
    controls
        .get_mut(&ControlId::ModelName)
        .unwrap()
        .set_string(model);

    SharedControls::new(radar_id, sk_client_tx, args, controls)
}

/// Load a recording and prepare for playback
pub async fn load_recording(
    args: &Cli,
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

    let file = File::open(&path).map_err(|e| format!("Failed to open file: {}", e))?;
    let reader = BufReader::new(file);
    let mrr_reader =
        MrrReader::open(reader).map_err(|e| format!("Failed to parse recording: {}", e))?;

    let header = mrr_reader.header();
    let footer = mrr_reader.footer();

    // Recover the source radar's identity so the playback radar wears the
    // original brand, legend shape, and pixel-value layout. If the
    // capabilities slot is missing or malformed we synthesize a minimal
    // identity from the file header (still using its `radar_brand` id);
    // Brand::Playback is only used further down as the fallback when
    // `id_to_brand(identity.brand_id)` itself fails to resolve.
    let identity = match RecordingIdentity::from_json(mrr_reader.capabilities()) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                "Recording {} has unreadable identity ({}); falling back to generic Playback brand",
                filename, e
            );
            // Synthesize a minimal identity from the header so we can still play
            // the file, even if the legend won't match the source radar.
            RecordingIdentity {
                brand_id: header.radar_brand,
                spokes_per_revolution: header.spokes_per_rev as u16,
                max_spoke_len: header.max_spoke_len as u16,
                pixel_values: header.pixel_values as u8,
                doppler_levels: 0,
                has_rain_class: false,
                doppler: false,
                targets: crate::TargetMode::None,
                dual_range: false,
                dual: None,
                sparse_spokes: false,
                stationary: false,
                serial_no: None,
                user_name: None,
                model_name: None,
            }
        }
    };

    let brand = id_to_brand(identity.brand_id).unwrap_or(Brand::Playback);

    let base_name = filename.trim_end_matches(".mrr");
    // Tag the serial so the radar key is stable per-file and identifiable as
    // a playback radar via the `replay` flag (not the brand itself).
    let serial_no = format!("PB-{}", base_name);
    let fake_addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);

    let mut info = crate::radar::RadarInfo::new(
        radars,
        args,
        brand,
        Some(&serial_no),
        identity.dual.as_deref(),
        identity.pixel_values,
        identity.spokes_per_revolution as usize,
        identity.max_spoke_len as usize,
        fake_addr,
        Ipv4Addr::LOCALHOST,
        fake_addr,
        fake_addr,
        fake_addr,
        |id, tx| playback_controls(id, tx, args, &identity),
        identity.doppler,
        identity.sparse_spokes,
    );

    // Reshape the legend to match the source radar's wire format. Without
    // these calls the legend would default to the no-Doppler / no-rain-class
    // shape and incoming spoke indices would land in the wrong palette slots
    // (see fix(furuno): rebuild wire-to-legend LUT after legend reshape).
    if identity.doppler_levels > 0 {
        info.set_doppler_levels(identity.doppler_levels);
    }
    if identity.has_rain_class {
        info.set_has_rain_class(identity.has_rain_class);
    }
    info.set_replay(true);

    let radar_key = info.key();

    // Remove any existing playback radar with the same key
    radars.remove(&radar_key);

    if let Some(mut info) = radars.add(info) {
        // Set ranges so the radar appears as active in the GUI
        let ranges =
            crate::radar::range::Ranges::new_by_distance(&vec![header.max_spoke_len as i32]);
        info.set_ranges(ranges);

        // Set power to transmit so GUI shows radar as active
        let _ = info.controls.set(
            &ControlId::Power,
            crate::radar::Power::Transmit as i32 as f64,
            None,
        );

        // Apply the recorded `SpokeProcessing` value so playback uses the
        // same processor mode the source radar was set to (e.g. Reduce for
        // Furuno NXT). Without this the GUI defaults to Auto, which picks
        // Smooth for high-spoke-count radars and renders sparse-spoke data
        // as moiré interference.
        //
        // Range-check the value at this file → memory boundary: the only
        // valid modes are 0=Clean, 1=Fill, 2=Reduce, 3=Smooth.
        match serde_json::from_slice::<serde_json::Value>(mrr_reader.initial_state()) {
            Ok(state) => {
                if let Some(val) = state.get("SpokeProcessing").and_then(|v| v.as_i64()) {
                    if (0..=3).contains(&val) {
                        info.controls.set_spoke_processing(val as i32);
                    } else {
                        warn!(
                            "Recording {}: ignoring out-of-range SpokeProcessing value {}",
                            filename, val
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Recording {}: malformed initial_state JSON ({}); SpokeProcessing not restored",
                    filename, e
                );
            }
        }

        radars.update(&mut info);

        let message_tx = info.message_tx.clone();

        info!(
            "Registered playback radar: {} ({}ms, {} frames)",
            radar_key, footer.duration_ms, footer.frame_count
        );

        let stop_flag = Arc::new(AtomicBool::new(false));
        let pause_flag = Arc::new(AtomicBool::new(true)); // Start paused
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
            radar_key: radar_key.clone(),
            duration_ms: footer.duration_ms,
            frame_count: footer.frame_count,
            state: state.clone(),
            seek_target: seek_target.clone(),
        };

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
    } else {
        Err(format!("Failed to register playback radar '{}'", radar_key))
    }
}

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
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        if pause_flag.load(Ordering::SeqCst) {
            {
                let mut s = state.write().await;
                if *s == PlaybackState::Playing {
                    *s = PlaybackState::Paused;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }

        {
            let mut s = state.write().await;
            if *s == PlaybackState::Loaded || *s == PlaybackState::Paused {
                *s = PlaybackState::Playing;
            }
        }

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

        let mut playback_start = Instant::now();
        let mut first_frame_ts: Option<u64> = None;
        let mut frames_played = 0u32;

        loop {
            if stop_flag.load(Ordering::SeqCst) {
                break;
            }

            if pause_flag.load(Ordering::SeqCst) {
                break;
            }

            {
                let mut target = seek_target.write().await;
                if let Some(seek_ms) = target.take() {
                    if let Err(e) = mrr_reader.seek_to_timestamp(seek_ms) {
                        warn!("Seek failed: {}", e);
                    }
                    first_frame_ts = None;
                    playback_start = Instant::now();
                    continue;
                }
            }

            let frame = match mrr_reader.read_frame() {
                Ok(Some(f)) => f,
                Ok(None) => {
                    if loop_playback.load(Ordering::SeqCst) {
                        if let Err(e) = mrr_reader.rewind() {
                            error!("Failed to rewind: {}", e);
                            break;
                        }
                        first_frame_ts = None;
                        playback_start = Instant::now();
                        continue;
                    } else {
                        break;
                    }
                }
                Err(e) => {
                    error!("Failed to read frame: {}", e);
                    break;
                }
            };

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
                let max_wait = Duration::from_millis(100);
                if wait_time > max_wait {
                    tokio::time::sleep(max_wait).await;
                    continue;
                }
                tokio::time::sleep(wait_time).await;
            }

            if let Err(e) = message_tx.send(frame.data) {
                log::trace!("No receivers for playback frame: {}", e);
            }

            position_ms.store(frame_ts, Ordering::Relaxed);
            frames_played += 1;
            frame_counter.store(frames_played, Ordering::Relaxed);
        }

        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        if !pause_flag.load(Ordering::SeqCst) && !loop_playback.load(Ordering::SeqCst) {
            break;
        }
    }

    stop_flag.store(true, Ordering::SeqCst);
    {
        let mut s = state.write().await;
        *s = PlaybackState::Stopped;
    }

    info!("Playback finished for {}", path.display());
}

/// Unregister a playback radar
pub fn unregister_playback_radar(radars: &SharedRadars, radar_key: &str) {
    info!("Unregistering playback radar: key={}", radar_key);
    radars.remove(radar_key);
}
