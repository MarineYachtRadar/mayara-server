//! Furuno report receiver using unified mayara-core controller
//!
//! This module wraps the platform-independent `FurunoController` from mayara-core,
//! polling it in an async loop and applying state updates to the server's control system.
//!
//! The controller emits [`ControllerEvent`]s that this receiver handles to update
//! the server's shared state (e.g., registering the radar with ranges when model is detected).

use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use tokio_graceful_shutdown::SubsystemHandle;

// Use unified controller and events from mayara-core
use mayara_core::controllers::FurunoController;
use mayara_core::ControllerEvent;

use super::settings;
use super::RadarModel;
use crate::radar::{RadarError, RadarInfo, SharedRadars, Status};
use crate::settings::ControlUpdate;
use crate::storage::load_installation_settings;
use crate::tokio_io::TokioIoProvider;
use crate::Session;

// Debug I/O wrapper for protocol analysis (dev feature only)
#[cfg(feature = "dev")]
use crate::debug::DebugIoProvider;

/// Type alias for the I/O provider used by FurunoReportReceiver.
/// When dev feature is enabled, wraps TokioIoProvider with DebugIoProvider.
#[cfg(feature = "dev")]
type FurunoIoProvider = DebugIoProvider<TokioIoProvider>;

#[cfg(not(feature = "dev"))]
type FurunoIoProvider = TokioIoProvider;

/// Furuno report receiver that uses the unified core controller
pub struct FurunoReportReceiver {
    #[allow(dead_code)]
    session: Session, // Kept for potential future use
    /// Shared radar registry - used to update radar info when model is detected
    radars: SharedRadars,
    info: RadarInfo,
    key: String,
    /// Unified controller from mayara-core
    controller: FurunoController,
    /// I/O provider for the controller (wrapped with DebugIoProvider when dev feature enabled)
    io: FurunoIoProvider,
    /// Poll interval for the controller
    poll_interval: Duration,
}

impl FurunoReportReceiver {
    pub fn new(session: Session, info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();
        let radar_addr = info.addr.ip().to_string();

        // Get SharedRadars from session - needed to update radar info when model is detected
        let radars = session
            .read()
            .unwrap()
            .radars
            .clone()
            .expect("SharedRadars must be initialized before creating report receiver");

        // Create the unified controller from mayara-core
        let controller = FurunoController::new(&key, &radar_addr);

        // Create I/O provider - wrapped with DebugIoProvider when dev feature enabled
        #[cfg(feature = "dev")]
        let io = {
            let inner = TokioIoProvider::new();
            if let Some(hub) = session.debug_hub() {
                log::debug!("{}: Using DebugIoProvider for protocol analysis", key);
                DebugIoProvider::new(inner, hub, key.clone(), "furuno".to_string())
            } else {
                // Fallback if debug_hub not initialized (shouldn't happen)
                log::warn!("{}: DebugHub not available, using plain TokioIoProvider", key);
                DebugIoProvider::new(
                    inner,
                    std::sync::Arc::new(crate::debug::DebugHub::new()),
                    key.clone(),
                    "furuno".to_string(),
                )
            }
        };

        #[cfg(not(feature = "dev"))]
        let io = TokioIoProvider::new();

        FurunoReportReceiver {
            session,
            radars,
            info,
            key,
            controller,
            io,
            poll_interval: Duration::from_millis(100), // 10Hz polling
        }
    }

    /// Main run loop - polls the core controller and handles commands
    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        log::info!("{}: report receiver starting (unified controller)", self.key);

        let mut command_rx = self.info.control_update_subscribe();
        // Check if model was already known from persistence (loaded before we start)
        let mut model_known = self.info.controls.model_name().is_some();

        // Use interval instead of sleep - sleep() in select! doesn't work correctly
        let mut poll_interval = interval(self.poll_interval);
        poll_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.key);
                    self.controller.shutdown(&mut self.io);
                    return Ok(());
                },

                _ = poll_interval.tick() => {
                    // Poll the controller and handle events
                    let events = self.controller.poll(&mut self.io);
                    for event in events {
                        self.handle_controller_event(event, &mut model_known);
                    }

                    // Apply state updates from controller to server controls
                    // and push to SharedRadars so REST API reflects current state
                    if self.apply_controller_state(model_known) {
                        self.radars.update(&self.info);
                    }
                },

                r = command_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(update) => {
                            if let Err(e) = self.process_control_update(update).await {
                                log::error!("{}: control update error: {:?}", self.key, e);
                            }
                        },
                    }
                }
            }
        }
    }

    /// Handle events from the core controller
    fn handle_controller_event(&mut self, event: ControllerEvent, model_known: &mut bool) {
        match event {
            ControllerEvent::Connected => {
                log::info!("{}: Controller connected to radar", self.key);
            }
            ControllerEvent::Disconnected => {
                log::warn!("{}: Controller disconnected from radar", self.key);
            }
            ControllerEvent::ModelDetected { model, version } => {
                log::info!(
                    "{}: Model detected: {} (firmware {})",
                    self.key, model, version
                );
                *model_known = true;

                // Convert to RadarModel enum
                let radar_model = RadarModel::from_name(&model);

                // Update RadarInfo with model-specific settings (ranges, controls)
                // This is the critical step that sets ranges from mayara-core's model database
                settings::update_when_model_known(&mut self.info, radar_model, &version);

                // CRITICAL: Push the updated RadarInfo to SharedRadars
                // This makes the radar visible in the API (get_active() filters by ranges.len() > 0)
                self.radars.update(&self.info);

                log::info!(
                    "{}: Radar registered with {} ranges",
                    self.key,
                    self.info.ranges.len()
                );

                // Restore persisted installation settings (write-only controls)
                self.restore_installation_settings();
            }
            ControllerEvent::OperatingHoursUpdated { hours } => {
                self.set_value("operatingHours", hours as f32);
            }
            ControllerEvent::TransmitHoursUpdated { hours } => {
                self.set_value("transmitHours", hours as f32);
            }
        }
    }

    /// Apply controller state to server controls
    /// Returns true if any control value changed (caller should update SharedRadars)
    fn apply_controller_state(&mut self, model_known: bool) -> bool {
        // Clone state to avoid borrow checker issues with self.set_* methods
        let state = self.controller.radar_state().clone();
        let mut changed = false;

        // Apply power state
        let power_status = match state.power {
            mayara_core::state::PowerState::Off => Status::Off,
            mayara_core::state::PowerState::Standby => Status::Standby,
            mayara_core::state::PowerState::Transmit => Status::Transmit,
            mayara_core::state::PowerState::Warming => Status::Preparing,
        };
        changed |= self.set_value_changed("power", power_status as i32 as f32);

        // Apply range
        if state.range > 0 {
            changed |= self.set_value_changed("range", state.range as f32);
        }

        // Apply gain, sea, rain with auto mode
        changed |= self.set_value_auto_changed("gain", state.gain.value as f32, state.gain.mode == "auto");
        changed |= self.set_value_auto_changed("sea", state.sea.value as f32, state.sea.mode == "auto");
        changed |= self.set_value_auto_changed("rain", state.rain.value as f32, state.rain.mode == "auto");

        // Model-specific controls are only available after model detection
        // (update_when_model_known adds these controls)
        if !model_known {
            return changed;
        }

        // Apply signal processing controls
        changed |= self.set_value_changed("noiseReduction", if state.noise_reduction { 1.0 } else { 0.0 });
        changed |= self.set_value_changed("interferenceRejection", if state.interference_rejection { 1.0 } else { 0.0 });

        // Apply extended controls
        changed |= self.set_value_changed("beamSharpening", state.beam_sharpening as f32);
        changed |= self.set_value_changed("birdMode", state.bird_mode as f32);
        changed |= self.set_value_changed("scanSpeed", state.scan_speed as f32);
        changed |= self.set_value_changed("mainBangSuppression", state.main_bang_suppression as f32);
        changed |= self.set_value_changed("txChannel", state.tx_channel as f32);

        // Apply Doppler mode (mode is "target" or "rain" string)
        // Protocol uses: mode=0 for Target, mode=1 for Rain
        // This is a compound control with enabled state, not auto mode
        let doppler_mode_value = match state.doppler_mode.mode.as_str() {
            "target" | "targets" => 0.0,
            "rain" => 1.0,
            _ => 0.0,
        };
        changed |= self.set_value_enabled_changed("dopplerMode", doppler_mode_value, state.doppler_mode.enabled);

        // NOTE: No-transmit zones are NOT synced from radar state here.
        // They are user-controlled values that we persist and restore.
        // The radar's $N77 report may not match what we've sent (race condition),
        // and we want to preserve the user's intent, not overwrite with radar state.
        // NTZ values are only updated via update_no_transmit_zone() when user changes them.

        changed
    }

    /// Process control update from REST API
    async fn process_control_update(&mut self, update: ControlUpdate) -> Result<(), RadarError> {
        let cv = update.control_value;
        let reply_tx = update.reply_tx;

        log::debug!("{}: set_control {} = {}", self.key, cv.id, cv.value);

        let result = self.send_control_to_radar(&cv.id, &cv.value, cv.auto.unwrap_or(false));

        match result {
            Ok(()) => {
                // Update local state immediately after successful command
                // The radar will report back eventually, but we want immediate UI feedback
                if let Ok(num_value) = cv.value.parse::<f32>() {
                    match cv.id.as_str() {
                        // Write-only controls (Installation category)
                        "bearingAlignment" | "antennaHeight" | "autoAcquire" => {
                            self.set_value(&cv.id, num_value);
                            self.radars.update(&self.info);
                            log::debug!("{}: Updated write-only control {} = {}", self.key, cv.id, num_value);
                        }
                        // Compound controls with auto/manual mode
                        "gain" | "sea" | "rain" => {
                            let auto = cv.auto.unwrap_or(false);
                            self.set_value_auto(&cv.id, num_value, auto);
                            self.radars.update(&self.info);
                            log::debug!("{}: Updated {} = {} auto={}", self.key, cv.id, num_value, auto);
                        }
                        // Extended controls - update immediately for responsive UI
                        "beamSharpening" | "birdMode" | "scanSpeed" | "mainBangSuppression"
                        | "txChannel" | "interferenceRejection" | "noiseReduction" => {
                            self.set_value(&cv.id, num_value);
                            self.radars.update(&self.info);
                            log::debug!("{}: Updated extended control {} = {}", self.key, cv.id, num_value);
                        }
                        _ => {}
                    }
                }
                self.info.controls.set_refresh(&cv.id);
                Ok(())
            }
            Err(e) => {
                self.info.controls.send_error_to_client(reply_tx, &cv, &e).await?;
                Ok(())
            }
        }
    }

    /// Send a control command to the radar via the unified controller
    fn send_control_to_radar(&mut self, id: &str, value: &str, auto: bool) -> Result<(), RadarError> {
        // Handle power separately (enum value)
        if id == "power" {
            let transmit = value == "transmit" || value == "Transmit";
            self.controller.set_transmit(&mut self.io, transmit);
            return Ok(());
        }

        // Parse numeric value
        let num_value: i32 = value
            .parse::<f32>()
            .map(|v| v as i32)
            .map_err(|_| RadarError::MissingValue(id.to_string()))?;

        // Dispatch to appropriate controller method
        match id {
            "range" => self.controller.set_range(&mut self.io, num_value as u32),
            "gain" => self.controller.set_gain(&mut self.io, num_value, auto),
            "sea" => self.controller.set_sea(&mut self.io, num_value, auto),
            "rain" => self.controller.set_rain(&mut self.io, num_value, auto),
            "beamSharpening" => self.controller.set_rezboost(&mut self.io, num_value),
            "interferenceRejection" => self.controller.set_interference_rejection(&mut self.io, num_value != 0),
            "noiseReduction" => self.controller.set_noise_reduction(&mut self.io, num_value != 0),
            "scanSpeed" => self.controller.set_scan_speed(&mut self.io, num_value),
            "birdMode" => self.controller.set_bird_mode(&mut self.io, num_value),
            "mainBangSuppression" => self.controller.set_main_bang_suppression(&mut self.io, num_value),
            "txChannel" => self.controller.set_tx_channel(&mut self.io, num_value),
            "bearingAlignment" => self.controller.set_bearing_alignment(&mut self.io, num_value as f64),
            "antennaHeight" => self.controller.set_antenna_height(&mut self.io, num_value),
            "autoAcquire" => self.controller.set_auto_acquire(&mut self.io, num_value != 0),
            "dopplerMode" => {
                // dopplerMode is a compound control: enabled (bool) + mode (enum)
                // The GUI sends {"enabled": bool, "mode": "target"|"rain"}
                // But here we receive the numeric value from the control's internal representation
                // enabled is passed via the 'auto' parameter (repurposed for compound enabled state)
                // mode: 0 = "target", 1 = "rain"
                let mode = num_value;
                self.controller.set_target_analyzer(&mut self.io, auto, mode);
            }
            // No-transmit zone controls - GUI sets individual angles, we send combined command
            // Value of -180 means zone is disabled
            "noTransmitStart1" | "noTransmitEnd1" | "noTransmitStart2" | "noTransmitEnd2" => {
                self.update_no_transmit_zone(id, num_value);
            }
            _ => return Err(RadarError::CannotSetControlType(id.to_string())),
        }

        Ok(())
    }

    // Helper methods for setting control values

    fn set(&mut self, control_type: &str, value: f32, auto: Option<bool>) {
        match self.info.controls.set(control_type, value, auto) {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Trace) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {} enabled {:?}",
                        self.key,
                        control_type,
                        control.value(),
                        control.enabled
                    );
                }
            }
            Ok(None) => {}
        };
    }

    fn set_value(&mut self, control_type: &str, value: f32) {
        self.set(control_type, value, None)
    }

    fn set_value_auto(&mut self, control_type: &str, value: f32, auto: bool) {
        match self.info.controls.set_value_auto(control_type, auto, value) {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Trace) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {} auto {}",
                        self.key,
                        control_type,
                        control.value(),
                        auto
                    );
                }
            }
            Ok(None) => {}
        };
    }

    // Variants that return true if value changed (for apply_controller_state)

    fn set_value_changed(&mut self, control_type: &str, value: f32) -> bool {
        match self.info.controls.set(control_type, value, None) {
            Ok(Some(())) => true,
            _ => false,
        }
    }

    fn set_value_auto_changed(&mut self, control_type: &str, value: f32, auto: bool) -> bool {
        match self.info.controls.set_value_auto(control_type, auto, value) {
            Ok(Some(())) => true,
            _ => false,
        }
    }

    fn set_value_enabled_changed(&mut self, control_type: &str, value: f32, enabled: bool) -> bool {
        match self.info.controls.set_value_auto_enabled(control_type, value, None, Some(enabled)) {
            Ok(Some(())) => true,
            _ => false,
        }
    }

    /// Update no-transmit zone from individual control change.
    /// Reads current state of all 4 values and sends combined blind sector command.
    /// A value of -1 indicates the zone is disabled.
    fn update_no_transmit_zone(&mut self, changed_id: &str, new_value: i32) {
        // Read current values from CONTROL VALUES (not radar state!)
        // This is critical because when GUI sends 4 updates in sequence,
        // the control values are already updated but radar state lags behind.
        let get_control_value = |id: &str| -> i32 {
            self.info.controls.get(id)
                .and_then(|c| c.value.map(|v| v as i32))
                .unwrap_or(-1)
        };

        // Get current control values, applying the new value for the changed control
        let z1_start = if changed_id == "noTransmitStart1" { new_value } else { get_control_value("noTransmitStart1") };
        let z1_end = if changed_id == "noTransmitEnd1" { new_value } else { get_control_value("noTransmitEnd1") };
        let z2_start = if changed_id == "noTransmitStart2" { new_value } else { get_control_value("noTransmitStart2") };
        let z2_end = if changed_id == "noTransmitEnd2" { new_value } else { get_control_value("noTransmitEnd2") };

        // -1 means disabled
        let z1_enabled = z1_start >= 0 && z1_end >= 0;
        let z2_enabled = z2_start >= 0 && z2_end >= 0;

        log::info!(
            "{}: Setting blind sector: z1({}, {}-{}) z2({}, {}-{})",
            self.key,
            z1_enabled, z1_start, z1_end,
            z2_enabled, z2_start, z2_end
        );

        self.controller.set_blind_sector(
            &mut self.io,
            z1_enabled,
            z1_start,
            z1_end,
            z2_enabled,
            z2_start,
            z2_end,
        );

        // Update local state
        self.set_value(changed_id, new_value as f32);
        self.radars.update(&self.info);
    }

    /// Restore persisted installation settings from Application Data API.
    /// These are write-only controls that cannot be read from the radar hardware.
    fn restore_installation_settings(&mut self) {
        if let Some(settings) = load_installation_settings(&self.key) {
            log::info!("{}: Restoring installation settings: {:?}", self.key, settings);

            let mut restored_any = false;

            // Restore bearing alignment
            if let Some(degrees) = settings.bearing_alignment {
                self.controller.set_bearing_alignment(&mut self.io, degrees as f64);
                self.set_value("bearingAlignment", degrees as f32);
                log::info!("{}: Restored bearingAlignment = {}°", self.key, degrees);
                restored_any = true;
            }

            // Restore antenna height
            if let Some(meters) = settings.antenna_height {
                self.controller.set_antenna_height(&mut self.io, meters);
                self.set_value("antennaHeight", meters as f32);
                log::info!("{}: Restored antennaHeight = {}m", self.key, meters);
                restored_any = true;
            }

            // Restore auto acquire (ARPA)
            if let Some(enabled) = settings.auto_acquire {
                self.controller.set_auto_acquire(&mut self.io, enabled);
                self.set_value("autoAcquire", if enabled { 1.0 } else { 0.0 });
                log::info!("{}: Restored autoAcquire = {}", self.key, enabled);
                restored_any = true;
            }

            // CRITICAL: Push updated values to SharedRadars so REST API reflects them
            if restored_any {
                self.radars.update(&self.info);
                log::info!("{}: Updated SharedRadars with restored installation settings", self.key);
            }
        }
    }
}
