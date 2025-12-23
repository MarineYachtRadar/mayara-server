//! Furuno Control Dispatch
//!
//! Centralized dispatch for control commands and response parsing.
//! This module provides single-entry-point functions that route control
//! operations to the appropriate wire protocol functions.

use super::command::*;

// =============================================================================
// Control Update Enum
// =============================================================================

/// Parsed control value from radar response
#[derive(Debug, Clone, PartialEq)]
pub enum ControlUpdate {
    /// Power state: transmitting?
    Power(bool),
    /// Range in meters (converted from wire index)
    Range(i32),
    /// Gain with auto mode
    Gain { auto: bool, value: i32 },
    /// Sea clutter with auto mode
    Sea { auto: bool, value: i32 },
    /// Rain clutter with auto mode
    Rain { auto: bool, value: i32 },
    /// Noise reduction enabled
    NoiseReduction(bool),
    /// Interference rejection enabled
    InterferenceRejection(bool),
    /// Beam sharpening level (0=OFF, 1=Low, 2=Med, 3=High)
    BeamSharpening(i32),
    /// Bird mode level (0=OFF, 1=Low, 2=Med, 3=High)
    BirdMode(i32),
    /// Doppler/Target Analyzer mode
    DopplerMode { enabled: bool, mode: i32 },
    /// Scan speed (0=24RPM, 2=Auto)
    ScanSpeed(i32),
    /// Main bang suppression (0-100%)
    MainBangSuppression(i32),
    /// TX Channel (0=Auto, 1-3=Channel)
    TxChannel(i32),
    /// Blind sector / no-transmit zones
    BlindSector(BlindSectorState),
    /// Operating time in seconds
    OperatingTime(i32),
}

// =============================================================================
// Command Formatting Dispatch
// =============================================================================

/// Format a control command for Furuno radars
///
/// Returns the wire protocol string to send, or None if control not supported.
///
/// # Arguments
/// * `control_id` - The control identifier (e.g., "gain", "power", "beamSharpening")
/// * `value` - The control value (interpretation depends on control type)
/// * `auto` - Whether auto mode is enabled (for controls that support it)
///
/// # Example
/// ```
/// use mayara_core::protocol::furuno::dispatch::format_control_command;
///
/// // Set gain to 50 in manual mode
/// let cmd = format_control_command("gain", 50, false);
/// assert_eq!(cmd, Some("$S63,0,50,0,80,0\r\n".to_string()));
///
/// // Set gain to auto mode
/// let cmd = format_control_command("gain", 50, true);
/// assert_eq!(cmd, Some("$S63,1,50,0,80,0\r\n".to_string()));
/// ```
pub fn format_control_command(control_id: &str, value: i32, auto: bool) -> Option<String> {
    match control_id {
        // Base controls
        "power" => Some(format_status_command(value == 2)),
        "range" => Some(format_range_command(value)),
        "gain" => Some(format_gain_command(value, auto)),
        "sea" => Some(format_sea_command(value, auto)),
        "rain" => Some(format_rain_command(value, auto)),

        // Extended controls - signal processing
        "noiseReduction" => Some(format_noise_reduction_command(value != 0)),
        "interferenceRejection" => Some(format_interference_rejection_command(value != 0)),

        // Extended controls - NXT features
        "beamSharpening" => Some(format_rezboost_command(value, 0)),
        "birdMode" => Some(format_bird_mode_command(value, 0)),
        "dopplerMode" => {
            // For dopplerMode: auto=enabled, value=mode (0=Target, 1=Rain)
            Some(format_target_analyzer_command(auto, value, 0))
        }

        // Extended controls - general
        "scanSpeed" => Some(format_scan_speed_command(value)),
        "mainBangSuppression" => Some(format_main_bang_command(value)),
        "txChannel" => Some(format_tx_channel_command(value)),
        "autoAcquire" => Some(format_auto_acquire_command(value != 0)),

        // Installation settings
        "bearingAlignment" => Some(format_heading_align_command(value * 10)), // degrees -> tenths
        "antennaHeight" => Some(format_antenna_height_command(value)),

        // Unknown control
        _ => None,
    }
}

/// Format a request command to query current state of a control
///
/// Returns the wire protocol string to send, or None if control has no request.
///
/// # Example
/// ```
/// use mayara_core::protocol::furuno::dispatch::format_request_command;
///
/// let cmd = format_request_command("gain");
/// assert_eq!(cmd, Some("$R63\r\n".to_string()));
/// ```
pub fn format_request_command(control_id: &str) -> Option<String> {
    match control_id {
        // Base controls
        "power" => Some(format_request_status()),
        "range" => Some(format_request_range()),
        "gain" => Some(format_request_gain()),
        "sea" => Some(format_request_sea()),
        "rain" => Some(format_request_rain()),

        // Extended controls - signal processing
        "noiseReduction" => Some(format_request_noise_reduction()),
        "interferenceRejection" => Some(format_request_interference_rejection()),

        // Extended controls - NXT features
        "beamSharpening" => Some(format_request_rezboost()),
        "birdMode" => Some(format_request_bird_mode()),
        "dopplerMode" => Some(format_request_target_analyzer()),

        // Extended controls - general
        "scanSpeed" => Some(format_request_scan_speed()),
        "mainBangSuppression" => Some(format_request_main_bang()),
        "txChannel" => Some(format_request_tx_channel()),
        "noTransmitZones" => Some(format_request_blind_sector()),

        // Operating info
        "operatingHours" => Some(format_request_ontime()),

        // Controls without request commands
        "autoAcquire" | "bearingAlignment" | "antennaHeight" => None,

        // Unknown control
        _ => None,
    }
}

// =============================================================================
// Response Parsing Dispatch
// =============================================================================

/// Parse a response line and return the control update if recognized
///
/// This function tries all known response parsers and returns the first match.
///
/// # Example
/// ```
/// use mayara_core::protocol::furuno::dispatch::{parse_control_response, ControlUpdate};
///
/// // Parse a gain response
/// let update = parse_control_response("$N63,0,75,0,80,0");
/// assert_eq!(update, Some(ControlUpdate::Gain { auto: false, value: 75 }));
///
/// // Parse a power response (transmitting)
/// let update = parse_control_response("$N69,2,0,0,60,300,0");
/// assert_eq!(update, Some(ControlUpdate::Power(true)));
/// ```
#[inline(never)]
pub fn parse_control_response(line: &str) -> Option<ControlUpdate> {
    // Try base control parsers
    if let Some(transmitting) = parse_status_response(line) {
        return Some(ControlUpdate::Power(transmitting));
    }

    if let Some(wire_index) = parse_range_response(line) {
        // Convert wire index to meters
        let range_meters = range_index_to_meters(wire_index).unwrap_or(0);
        return Some(ControlUpdate::Range(range_meters));
    }

    if let Some(cv) = parse_gain_response(line) {
        return Some(ControlUpdate::Gain {
            auto: cv.auto,
            value: cv.value,
        });
    }

    if let Some(cv) = parse_sea_response(line) {
        return Some(ControlUpdate::Sea {
            auto: cv.auto,
            value: cv.value,
        });
    }

    if let Some(cv) = parse_rain_response(line) {
        return Some(ControlUpdate::Rain {
            auto: cv.auto,
            value: cv.value,
        });
    }

    // Try signal processing parser (noise reduction / interference rejection)
    if let Some((feature, value)) = parse_signal_processing_response(line) {
        match feature {
            0 => return Some(ControlUpdate::InterferenceRejection(value != 0)),
            3 => return Some(ControlUpdate::NoiseReduction(value != 0)),
            _ => {}
        }
    }

    // Try extended control parsers
    if let Some(level) = parse_rezboost_response(line) {
        return Some(ControlUpdate::BeamSharpening(level));
    }

    if let Some(level) = parse_bird_mode_response(line) {
        return Some(ControlUpdate::BirdMode(level));
    }

    if let Some(state) = parse_target_analyzer_response(line) {
        return Some(ControlUpdate::DopplerMode {
            enabled: state.enabled,
            mode: state.mode,
        });
    }

    if let Some(mode) = parse_scan_speed_response(line) {
        return Some(ControlUpdate::ScanSpeed(mode));
    }

    if let Some(percent) = parse_main_bang_response(line) {
        return Some(ControlUpdate::MainBangSuppression(percent));
    }

    if let Some(channel) = parse_tx_channel_response(line) {
        return Some(ControlUpdate::TxChannel(channel));
    }

    if let Some(state) = parse_blind_sector_response(line) {
        return Some(ControlUpdate::BlindSector(state));
    }

    // No parser matched
    None
}

/// Get the control ID that a ControlUpdate corresponds to
///
/// Useful for routing updates to the correct control in the server.
pub fn control_update_id(update: &ControlUpdate) -> &'static str {
    match update {
        ControlUpdate::Power(_) => "power",
        ControlUpdate::Range(_) => "range",
        ControlUpdate::Gain { .. } => "gain",
        ControlUpdate::Sea { .. } => "sea",
        ControlUpdate::Rain { .. } => "rain",
        ControlUpdate::NoiseReduction(_) => "noiseReduction",
        ControlUpdate::InterferenceRejection(_) => "interferenceRejection",
        ControlUpdate::BeamSharpening(_) => "beamSharpening",
        ControlUpdate::BirdMode(_) => "birdMode",
        ControlUpdate::DopplerMode { .. } => "dopplerMode",
        ControlUpdate::ScanSpeed(_) => "scanSpeed",
        ControlUpdate::MainBangSuppression(_) => "mainBangSuppression",
        ControlUpdate::TxChannel(_) => "txChannel",
        ControlUpdate::BlindSector(_) => "noTransmitZones",
        ControlUpdate::OperatingTime(_) => "operatingHours",
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_control_command_gain() {
        // Manual mode
        let cmd = format_control_command("gain", 50, false);
        assert_eq!(cmd, Some("$S63,0,50,0,80,0\r\n".to_string()));

        // Auto mode
        let cmd = format_control_command("gain", 50, true);
        assert_eq!(cmd, Some("$S63,1,50,0,80,0\r\n".to_string()));
    }

    #[test]
    fn test_format_control_command_power() {
        // Transmit (value == 2)
        let cmd = format_control_command("power", 2, false);
        assert_eq!(cmd, Some("$S69,2,0,0,60,300,0\r\n".to_string()));

        // Standby (value != 2)
        let cmd = format_control_command("power", 1, false);
        assert_eq!(cmd, Some("$S69,1,0,0,60,300,0\r\n".to_string()));
    }

    #[test]
    fn test_format_control_command_extended() {
        let cmd = format_control_command("beamSharpening", 2, false);
        assert_eq!(cmd, Some("$SEE,2,0\r\n".to_string()));

        let cmd = format_control_command("birdMode", 1, false);
        assert_eq!(cmd, Some("$SED,1,0\r\n".to_string()));

        let cmd = format_control_command("noiseReduction", 1, false);
        assert_eq!(cmd, Some("$S67,0,3,1,0\r\n".to_string()));

        let cmd = format_control_command("interferenceRejection", 1, false);
        assert_eq!(cmd, Some("$S67,0,0,2,0\r\n".to_string()));
    }

    #[test]
    fn test_format_control_command_unknown() {
        let cmd = format_control_command("unknownControl", 0, false);
        assert_eq!(cmd, None);
    }

    #[test]
    fn test_format_request_command() {
        assert_eq!(format_request_command("gain"), Some("$R63\r\n".to_string()));
        assert_eq!(
            format_request_command("power"),
            Some("$R69\r\n".to_string())
        );
        assert_eq!(
            format_request_command("beamSharpening"),
            Some("$REE\r\n".to_string())
        );
        assert_eq!(format_request_command("unknownControl"), None);
    }

    #[test]
    fn test_parse_control_response_gain() {
        let update = parse_control_response("$N63,0,75,0,80,0");
        assert_eq!(
            update,
            Some(ControlUpdate::Gain {
                auto: false,
                value: 75
            })
        );

        let update = parse_control_response("$N63,1,50,0,80,0");
        assert_eq!(
            update,
            Some(ControlUpdate::Gain {
                auto: true,
                value: 50
            })
        );
    }

    #[test]
    fn test_parse_control_response_power() {
        let update = parse_control_response("$N69,2,0,0,60,300,0");
        assert_eq!(update, Some(ControlUpdate::Power(true)));

        let update = parse_control_response("$N69,1,0,0,60,300,0");
        assert_eq!(update, Some(ControlUpdate::Power(false)));
    }

    #[test]
    fn test_parse_control_response_extended() {
        let update = parse_control_response("$NEE,2,0");
        assert_eq!(update, Some(ControlUpdate::BeamSharpening(2)));

        let update = parse_control_response("$NED,1,0");
        assert_eq!(update, Some(ControlUpdate::BirdMode(1)));

        let update = parse_control_response("$NEF,1,0,0");
        assert_eq!(
            update,
            Some(ControlUpdate::DopplerMode {
                enabled: true,
                mode: 0
            })
        );
    }

    #[test]
    fn test_parse_control_response_signal_processing() {
        // Noise reduction ON
        let update = parse_control_response("$N67,0,3,1,0");
        assert_eq!(update, Some(ControlUpdate::NoiseReduction(true)));

        // Interference rejection ON
        let update = parse_control_response("$N67,0,0,2,0");
        assert_eq!(update, Some(ControlUpdate::InterferenceRejection(true)));
    }

    #[test]
    fn test_parse_control_response_unknown() {
        let update = parse_control_response("$NXX,1,2,3");
        assert_eq!(update, None);

        let update = parse_control_response("garbage");
        assert_eq!(update, None);
    }

    #[test]
    fn test_control_update_id() {
        assert_eq!(control_update_id(&ControlUpdate::Power(true)), "power");
        assert_eq!(
            control_update_id(&ControlUpdate::Gain {
                auto: false,
                value: 50
            }),
            "gain"
        );
        assert_eq!(
            control_update_id(&ControlUpdate::BeamSharpening(2)),
            "beamSharpening"
        );
    }
}
