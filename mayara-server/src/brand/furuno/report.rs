//! Furuno report receiver using unified mayara-core controller
//!
//! This module wraps the platform-independent `FurunoController` from mayara-core,
//! polling it in an async loop and applying state updates to the server's control system.

use std::time::Duration;
use tokio::time::sleep;
use tokio_graceful_shutdown::SubsystemHandle;

// Use unified controller from mayara-core
use mayara_core::controllers::FurunoController;

use super::settings;
use super::RadarModel;
use crate::radar::{RadarError, RadarInfo, Status};
use crate::settings::ControlUpdate;
use crate::tokio_io::TokioIoProvider;
use crate::Session;

/// Furuno report receiver that uses the unified core controller
pub struct FurunoReportReceiver {
    #[allow(dead_code)]
    session: Session, // Kept for potential future use
    info: RadarInfo,
    key: String,
    /// Unified controller from mayara-core
    controller: FurunoController,
    /// I/O provider for the controller
    io: TokioIoProvider,
    /// Poll interval for the controller
    poll_interval: Duration,
    /// Whether model info has been received
    model_known: bool,
}

impl FurunoReportReceiver {
    pub fn new(session: Session, info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();
        let radar_addr = info.addr.ip().to_string();

        // Create the unified controller from mayara-core
        let controller = FurunoController::new(&key, &radar_addr);
        let io = TokioIoProvider::new();

        FurunoReportReceiver {
            session,
            info,
            key,
            controller,
            io,
            poll_interval: Duration::from_millis(100), // 10Hz polling
            model_known: false,
        }
    }

    /// Main run loop - polls the core controller and handles commands
    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        log::info!("{}: report receiver starting (unified controller)", self.key);

        let mut command_rx = self.info.control_update_subscribe();

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.key);
                    self.controller.shutdown(&mut self.io);
                    return Ok(());
                },

                _ = sleep(self.poll_interval) => {
                    // Poll the controller
                    self.controller.poll(&mut self.io);

                    // Apply state updates from controller to server controls
                    self.apply_controller_state();

                    // Check for model info
                    self.check_model_info();
                },

                r = command_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(update) => {
                            if let Err(e) = self.process_control_update(update).await {
                                log::error!("{}: control update error: {:?}", self.key, e);
                                // Don't return on control errors, keep running
                            }
                        },
                    }
                }
            }
        }
    }

    /// Apply controller state to server controls
    fn apply_controller_state(&mut self) {
        // Clone state to avoid borrow checker issues with self.set_* methods
        let state = self.controller.radar_state().clone();

        // Apply power state
        let power_status = match state.power {
            mayara_core::state::PowerState::Off => Status::Off,
            mayara_core::state::PowerState::Standby => Status::Standby,
            mayara_core::state::PowerState::Transmit => Status::Transmit,
            mayara_core::state::PowerState::Warming => Status::Preparing,
        };
        self.set_value("power", power_status as i32 as f32);

        // Apply range
        if state.range > 0 {
            self.set_value("range", state.range as f32);
        }

        // Apply gain, sea, rain with auto mode
        self.set_value_auto("gain", state.gain.value as f32, state.gain.mode == "auto");
        self.set_value_auto("sea", state.sea.value as f32, state.sea.mode == "auto");
        self.set_value_auto("rain", state.rain.value as f32, state.rain.mode == "auto");

        // Apply signal processing controls
        self.set_value("noiseReduction", if state.noise_reduction { 1.0 } else { 0.0 });
        self.set_value("interferenceRejection", if state.interference_rejection { 1.0 } else { 0.0 });

        // Apply extended controls
        self.set_value("beamSharpening", state.beam_sharpening as f32);
        self.set_value("birdMode", state.bird_mode as f32);
        self.set_value("scanSpeed", state.scan_speed as f32);
        self.set_value("mainBangSuppression", state.main_bang_suppression as f32);
        self.set_value("txChannel", state.tx_channel as f32);

        // Apply Doppler mode (mode is "target" or "rain" string)
        let doppler_mode_value = match state.doppler_mode.mode.as_str() {
            "off" => 0.0,
            "target" | "targets" => 1.0,
            "rain" => 2.0,
            _ => 0.0,
        };
        self.set("dopplerMode", doppler_mode_value, Some(state.doppler_mode.enabled));

        // Apply no-transmit zones
        if !state.no_transmit_zones.zones.is_empty() {
            if let Some(z1) = state.no_transmit_zones.zones.first() {
                self.set_value("noTransmitStart1", z1.start as f32);
                self.set_value("noTransmitEnd1", z1.end as f32);
            }
            if let Some(z2) = state.no_transmit_zones.zones.get(1) {
                self.set_value("noTransmitStart2", z2.start as f32);
                self.set_value("noTransmitEnd2", z2.end as f32);
            }
        }
    }

    /// Check for model info from controller
    fn check_model_info(&mut self) {
        if self.model_known {
            return;
        }

        if let Some(model_name) = self.controller.model() {
            self.model_known = true;

            // Convert to RadarModel
            let model = RadarModel::from_name(model_name);
            let version = self.controller.firmware_version().unwrap_or("unknown");

            log::info!(
                "{}: Radar model {} version {}",
                self.key,
                model.as_str(),
                version
            );

            settings::update_when_model_known(&mut self.info, model, version);
        }

        // Apply operating hours if available
        if let Some(hours) = self.controller.operating_hours() {
            self.set_value("operatingHours", hours as f32);
        }
    }

    /// Process control update from REST API
    async fn process_control_update(&mut self, update: ControlUpdate) -> Result<(), RadarError> {
        let cv = update.control_value;
        let reply_tx = update.reply_tx;

        log::debug!("{}: set_control {} = {}", self.key, cv.id, cv.value);

        let result = self.send_control_to_radar(&cv.id, &cv.value, cv.auto.unwrap_or(false));

        match result {
            Ok(()) => {
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

}
