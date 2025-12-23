//! Radar State Tracking
//!
//! This module provides types for tracking the current state of radar controls.
//! State is updated by parsing responses from the radar and can be serialized
//! for the REST API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::protocol::furuno::command::{
    parse_bird_mode_response, parse_blind_sector_response, parse_gain_response,
    parse_main_bang_response, parse_rain_response, parse_range_response, parse_rezboost_response,
    parse_scan_speed_response, parse_sea_response, parse_signal_processing_response,
    parse_status_response, parse_target_analyzer_response, parse_tx_channel_response,
    range_index_to_meters, ControlValue as ParsedControlValue,
};

/// Power state of the radar
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    Off,
    Standby,
    Transmit,
    Warming,
}

impl Default for PowerState {
    fn default() -> Self {
        PowerState::Off
    }
}

/// Control value with auto/manual mode (API format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlValueState {
    pub mode: String, // "auto" or "manual"
    pub value: i32,
}

impl Default for ControlValueState {
    fn default() -> Self {
        ControlValueState {
            mode: "auto".to_string(),
            value: 50,
        }
    }
}

impl From<ParsedControlValue> for ControlValueState {
    fn from(cv: ParsedControlValue) -> Self {
        ControlValueState {
            mode: if cv.auto { "auto" } else { "manual" }.to_string(),
            value: cv.value,
        }
    }
}

/// Target Analyzer (Doppler) state for API
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetAnalyzerState {
    /// Whether Target Analyzer is enabled
    pub enabled: bool,
    /// Mode: "target" or "rain"
    pub mode: String,
}

/// No-Transmit Zone state for API
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoTransmitZone {
    /// Whether this zone is enabled
    pub enabled: bool,
    /// Start angle in degrees (0-359)
    pub start: i32,
    /// End angle in degrees (0-359)
    pub end: i32,
}

/// No-Transmit Zones state (array of zones)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoTransmitZonesState {
    /// Array of zone configurations
    pub zones: Vec<NoTransmitZone>,
}

/// Complete radar state
///
/// Contains current values for all readable controls.
/// Updated by parsing $N responses from the radar.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarState {
    /// Current power state
    pub power: PowerState,

    /// Current range in meters
    pub range: u32,

    /// Gain control state
    pub gain: ControlValueState,

    /// Sea clutter control state
    pub sea: ControlValueState,

    /// Rain clutter control state
    pub rain: ControlValueState,

    /// Noise reduction enabled
    pub noise_reduction: bool,

    /// Interference rejection enabled
    pub interference_rejection: bool,

    /// RezBoost (beam sharpening) level: 0=OFF, 1=Low, 2=Medium, 3=High
    pub beam_sharpening: i32,

    /// Bird Mode level: 0=OFF, 1=Low, 2=Medium, 3=High
    pub bird_mode: i32,

    /// Target Analyzer (Doppler) state
    pub doppler_mode: TargetAnalyzerState,

    /// Scan speed mode: 0=24RPM, 2=Auto
    pub scan_speed: i32,

    /// Main Bang Suppression percentage (0-100)
    pub main_bang_suppression: i32,

    /// TX Channel: 0=Auto, 1-3=Channel 1-3
    pub tx_channel: i32,

    /// No-Transmit Zones (sector blanking)
    pub no_transmit_zones: NoTransmitZonesState,

    /// Timestamp of last update (milliseconds since epoch)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,

    /// Internal flag: when true, accept radar values even in manual mode
    /// Used during state refresh to sync changes made by other clients (chart plotter)
    #[serde(skip)]
    pending_refresh: bool,
}

impl RadarState {
    /// Create a new radar state with default values
    pub fn new() -> Self {
        RadarState::default()
    }

    /// Mark that a state refresh is pending.
    /// When set, the next update_from_response calls will accept radar values
    /// even if we're in manual mode. This allows syncing changes made by
    /// other clients (e.g., chart plotter).
    pub fn mark_pending_refresh(&mut self) {
        self.pending_refresh = true;
    }

    /// Update state by parsing a response line from the radar
    ///
    /// Returns true if the state was updated, false if the line wasn't recognized
    pub fn update_from_response(&mut self, line: &str) -> bool {
        // Try status response ($N69)
        if let Some(transmitting) = parse_status_response(line) {
            self.power = if transmitting {
                PowerState::Transmit
            } else {
                PowerState::Standby
            };
            return true;
        }

        // Try gain response ($N63)
        // In manual mode, preserve the commanded value - radar reports sensor readings,
        // but we want to show what the user set, not what the sensor reads.
        // EXCEPTION: During refresh, always accept radar values to sync external changes.
        if let Some(cv) = parse_gain_response(line) {
            if self.pending_refresh {
                // Refresh mode: accept radar values (sync from chart plotter)
                self.gain = cv.into();
                self.pending_refresh = false; // Clear after first control update
            } else if self.gain.mode == "manual" && !cv.auto {
                // Manual mode: only update mode confirmation, keep commanded value
                // (value was already set when we sent the command)
            } else {
                // Auto mode or switching modes: update everything
                self.gain = cv.into();
            }
            return true;
        }

        // Try sea response ($N64)
        // Same logic: in manual mode, preserve commanded value
        if let Some(cv) = parse_sea_response(line) {
            if self.pending_refresh {
                self.sea = cv.into();
            } else if self.sea.mode == "manual" && !cv.auto {
                // Manual mode: keep commanded value
            } else {
                self.sea = cv.into();
            }
            return true;
        }

        // Try rain response ($N65)
        // Same logic: in manual mode, preserve commanded value
        if let Some(cv) = parse_rain_response(line) {
            if self.pending_refresh {
                self.rain = cv.into();
            } else if self.rain.mode == "manual" && !cv.auto {
                // Manual mode: keep commanded value
            } else {
                self.rain = cv.into();
            }
            return true;
        }

        // Try range response ($N62)
        if let Some(range_index) = parse_range_response(line) {
            if let Some(meters) = range_index_to_meters(range_index) {
                self.range = meters as u32;
                return true;
            }
        }

        // Try signal processing response ($N67)
        if let Some((feature, value)) = parse_signal_processing_response(line) {
            match feature {
                0 => {
                    // Interference rejection: 0=OFF, 2=ON
                    self.interference_rejection = value == 2;
                    return true;
                }
                3 => {
                    // Noise reduction: 0=OFF, 1=ON
                    self.noise_reduction = value == 1;
                    return true;
                }
                _ => {}
            }
        }

        // Try RezBoost response ($NEE)
        if let Some(level) = parse_rezboost_response(line) {
            self.beam_sharpening = level;
            return true;
        }

        // Try Bird Mode response ($NED)
        if let Some(level) = parse_bird_mode_response(line) {
            self.bird_mode = level;
            return true;
        }

        // Try Target Analyzer response ($NEF)
        if let Some(ta) = parse_target_analyzer_response(line) {
            self.doppler_mode = TargetAnalyzerState {
                enabled: ta.enabled,
                mode: if ta.mode == 0 { "target" } else { "rain" }.to_string(),
            };
            return true;
        }

        // Try Scan Speed response ($N89)
        if let Some(mode) = parse_scan_speed_response(line) {
            self.scan_speed = mode;
            return true;
        }

        // Try Main Bang Suppression response ($N83)
        if let Some(percent) = parse_main_bang_response(line) {
            self.main_bang_suppression = percent;
            return true;
        }

        // Try TX Channel response ($NEC)
        if let Some(channel) = parse_tx_channel_response(line) {
            self.tx_channel = channel;
            return true;
        }

        // Try Blind Sector response ($N77)
        if let Some(bs) = parse_blind_sector_response(line) {
            self.no_transmit_zones = NoTransmitZonesState {
                zones: vec![
                    NoTransmitZone {
                        enabled: bs.sector1_width > 0,
                        start: bs.sector1_start,
                        end: (bs.sector1_start + bs.sector1_width) % 360,
                    },
                    NoTransmitZone {
                        enabled: bs.sector2_width > 0,
                        start: bs.sector2_start,
                        end: (bs.sector2_start + bs.sector2_width) % 360,
                    },
                ],
            };
            return true;
        }

        false
    }

    /// Convert to HashMap for API response
    ///
    /// Returns control values in the format expected by the /state endpoint
    pub fn to_controls_map(&self) -> HashMap<String, serde_json::Value> {
        let mut map = HashMap::new();

        // Power state
        let power_str = match self.power {
            PowerState::Off => "off",
            PowerState::Standby => "standby",
            PowerState::Transmit => "transmit",
            PowerState::Warming => "warming",
        };
        map.insert("power".to_string(), serde_json::json!(power_str));

        // Range
        map.insert("range".to_string(), serde_json::json!(self.range));

        // Gain
        map.insert(
            "gain".to_string(),
            serde_json::json!({
                "mode": self.gain.mode,
                "value": self.gain.value
            }),
        );

        // Sea
        map.insert(
            "sea".to_string(),
            serde_json::json!({
                "mode": self.sea.mode,
                "value": self.sea.value
            }),
        );

        // Rain
        map.insert(
            "rain".to_string(),
            serde_json::json!({
                "mode": self.rain.mode,
                "value": self.rain.value
            }),
        );

        // Noise reduction
        map.insert(
            "noiseReduction".to_string(),
            serde_json::json!(self.noise_reduction),
        );

        // Interference rejection
        map.insert(
            "interferenceRejection".to_string(),
            serde_json::json!(self.interference_rejection),
        );

        // RezBoost (beam sharpening)
        map.insert(
            "beamSharpening".to_string(),
            serde_json::json!(self.beam_sharpening),
        );

        // Bird Mode
        map.insert("birdMode".to_string(), serde_json::json!(self.bird_mode));

        // Target Analyzer (Doppler)
        map.insert(
            "dopplerMode".to_string(),
            serde_json::json!({
                "enabled": self.doppler_mode.enabled,
                "mode": self.doppler_mode.mode
            }),
        );

        // Scan Speed
        map.insert("scanSpeed".to_string(), serde_json::json!(self.scan_speed));

        // Main Bang Suppression
        map.insert(
            "mainBangSuppression".to_string(),
            serde_json::json!(self.main_bang_suppression),
        );

        // TX Channel
        map.insert("txChannel".to_string(), serde_json::json!(self.tx_channel));

        // No-Transmit Zones
        map.insert(
            "noTransmitZones".to_string(),
            serde_json::json!({
                "zones": self.no_transmit_zones.zones.iter().map(|z| {
                    serde_json::json!({
                        "enabled": z.enabled,
                        "start": z.start,
                        "end": z.end
                    })
                }).collect::<Vec<_>>()
            }),
        );

        map
    }
}

/// Generate all request commands to query current state
///
/// Returns a vector of command strings that should be sent to the radar
/// to query all readable control values.
pub fn generate_state_requests() -> Vec<String> {
    use crate::protocol::furuno::command::{
        format_request_bird_mode, format_request_blind_sector, format_request_gain,
        format_request_interference_rejection, format_request_main_bang,
        format_request_noise_reduction, format_request_rain, format_request_range,
        format_request_rezboost, format_request_scan_speed, format_request_sea,
        format_request_status, format_request_target_analyzer, format_request_tx_channel,
    };

    vec![
        format_request_status(),
        format_request_range(),
        format_request_gain(),
        format_request_sea(),
        format_request_rain(),
        // Signal processing - query each feature separately
        format_request_noise_reduction(),
        format_request_interference_rejection(),
        // Extended controls
        format_request_rezboost(),
        format_request_bird_mode(),
        format_request_target_analyzer(),
        format_request_scan_speed(),
        format_request_main_bang(),
        format_request_tx_channel(),
        format_request_blind_sector(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_from_status_response() {
        let mut state = RadarState::new();

        // Transmit
        assert!(state.update_from_response("$N69,2,0,0,60,300,0"));
        assert_eq!(state.power, PowerState::Transmit);

        // Standby
        assert!(state.update_from_response("$N69,1,0,0,60,300,0"));
        assert_eq!(state.power, PowerState::Standby);
    }

    #[test]
    fn test_update_from_gain_response() {
        let mut state = RadarState::new();

        // Initial state is auto, so manual mode response should update everything
        assert!(state.update_from_response("$N63,0,75,0,80,0"));
        assert_eq!(state.gain.mode, "manual");
        assert_eq!(state.gain.value, 75);

        // Auto mode, value 50 - should update everything
        assert!(state.update_from_response("$N63,1,50,0,80,0"));
        assert_eq!(state.gain.mode, "auto");
        assert_eq!(state.gain.value, 50);
    }

    #[test]
    fn test_manual_mode_preserves_commanded_value() {
        let mut state = RadarState::new();

        // Simulate user setting gain to 95 in manual mode
        state.gain.mode = "manual".to_string();
        state.gain.value = 95;

        // Radar responds with sensor reading (37), but we should preserve our commanded value
        assert!(state.update_from_response("$N63,0,37,0,80,0"));
        assert_eq!(state.gain.mode, "manual");
        assert_eq!(state.gain.value, 95); // Preserved, not overwritten to 37

        // Same for sea
        state.sea.mode = "manual".to_string();
        state.sea.value = 80;
        assert!(state.update_from_response("$N64,0,40,0,80,0"));
        assert_eq!(state.sea.mode, "manual");
        assert_eq!(state.sea.value, 80); // Preserved

        // Same for rain
        state.rain.mode = "manual".to_string();
        state.rain.value = 60;
        assert!(state.update_from_response("$N65,0,25,0,80,0"));
        assert_eq!(state.rain.mode, "manual");
        assert_eq!(state.rain.value, 60); // Preserved

        // But switching to auto SHOULD update
        assert!(state.update_from_response("$N63,1,50,0,80,0"));
        assert_eq!(state.gain.mode, "auto");
        assert_eq!(state.gain.value, 50); // Updated because mode changed
    }

    #[test]
    fn test_update_from_range_response() {
        let mut state = RadarState::new();

        // Range index 5 = 2778m (1.5nm)
        assert!(state.update_from_response("$N62,5,0,0"));
        assert_eq!(state.range, 2778);

        // Range index 4 = 1852m (1nm)
        assert!(state.update_from_response("$N62,4,0,0"));
        assert_eq!(state.range, 1852);
    }

    #[test]
    fn test_update_from_signal_processing_response() {
        let mut state = RadarState::new();

        // Noise reduction ON
        assert!(state.update_from_response("$N67,0,3,1,0"));
        assert!(state.noise_reduction);

        // Noise reduction OFF
        assert!(state.update_from_response("$N67,0,3,0,0"));
        assert!(!state.noise_reduction);

        // Interference rejection ON
        assert!(state.update_from_response("$N67,0,0,2,0"));
        assert!(state.interference_rejection);

        // Interference rejection OFF
        assert!(state.update_from_response("$N67,0,0,0,0"));
        assert!(!state.interference_rejection);
    }

    #[test]
    fn test_to_controls_map() {
        let mut state = RadarState::new();
        state.power = PowerState::Transmit;
        state.range = 5556;
        state.gain = ControlValueState {
            mode: "manual".to_string(),
            value: 60,
        };

        let map = state.to_controls_map();

        assert_eq!(map.get("power").unwrap(), "transmit");
        assert_eq!(map.get("range").unwrap(), 5556);

        let gain = map.get("gain").unwrap();
        assert_eq!(gain["mode"], "manual");
        assert_eq!(gain["value"], 60);
    }

    #[test]
    fn test_generate_state_requests() {
        let requests = generate_state_requests();

        assert_eq!(requests.len(), 14); // Base + signal processing (2) + extended controls
                                        // Base controls
        assert!(requests.contains(&"$R69\r\n".to_string()));
        assert!(requests.contains(&"$R62\r\n".to_string()));
        assert!(requests.contains(&"$R63\r\n".to_string()));
        assert!(requests.contains(&"$R64\r\n".to_string()));
        assert!(requests.contains(&"$R65\r\n".to_string()));
        // Signal processing - feature-specific queries
        assert!(requests.contains(&"$R67,0,3\r\n".to_string())); // Noise reduction
        assert!(requests.contains(&"$R67,0,0\r\n".to_string())); // Interference rejection
                                                                 // Extended controls
        assert!(requests.contains(&"$REE\r\n".to_string()));
        assert!(requests.contains(&"$RED\r\n".to_string()));
        assert!(requests.contains(&"$REF\r\n".to_string()));
        assert!(requests.contains(&"$R89\r\n".to_string()));
        assert!(requests.contains(&"$R83\r\n".to_string()));
        assert!(requests.contains(&"$REC\r\n".to_string()));
        assert!(requests.contains(&"$R77\r\n".to_string())); // Blind sector
    }

    #[test]
    fn test_update_from_extended_responses() {
        let mut state = RadarState::new();

        // RezBoost
        assert!(state.update_from_response("$NEE,2,0"));
        assert_eq!(state.beam_sharpening, 2);

        // Bird Mode
        assert!(state.update_from_response("$NED,3,0"));
        assert_eq!(state.bird_mode, 3);

        // Target Analyzer - enabled, target mode
        assert!(state.update_from_response("$NEF,1,0,0"));
        assert!(state.doppler_mode.enabled);
        assert_eq!(state.doppler_mode.mode, "target");

        // Target Analyzer - enabled, rain mode
        assert!(state.update_from_response("$NEF,1,1,0"));
        assert!(state.doppler_mode.enabled);
        assert_eq!(state.doppler_mode.mode, "rain");

        // Scan Speed
        assert!(state.update_from_response("$N89,2,0"));
        assert_eq!(state.scan_speed, 2);

        // Main Bang Suppression (127 = ~49%)
        assert!(state.update_from_response("$N83,127,0"));
        assert_eq!(state.main_bang_suppression, 49);

        // TX Channel
        assert!(state.update_from_response("$NEC,2"));
        assert_eq!(state.tx_channel, 2);
    }
}
