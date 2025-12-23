//! Garmin radar protocol parsing (xHD series)
//!
//! Garmin radars use a simple multicast-based discovery:
//! - Report address: 239.254.2.0:50100 - Status reports from radar
//! - Data address: 239.254.2.0:50102 - Spoke data from radar
//! - Send port: 50101 - Commands to radar (on radar's IP)
//!
//! Unlike other brands, Garmin doesn't have a structured beacon packet.
//! Discovery happens by receiving any packet on the report multicast address.

use serde::Deserialize;

use crate::error::ParseError;
use crate::radar::{RadarDiscovery, RadarStatus};
use crate::Brand;

// =============================================================================
// Network Constants
// =============================================================================

/// Report multicast address
pub const REPORT_ADDR: &str = "239.254.2.0";
/// Report multicast port
pub const REPORT_PORT: u16 = 50100;

/// Data multicast address
pub const DATA_ADDR: &str = "239.254.2.0";
/// Data multicast port
pub const DATA_PORT: u16 = 50102;

/// Command port (on radar's IP address)
pub const SEND_PORT: u16 = 50101;

// =============================================================================
// Radar Characteristics
// =============================================================================

/// Spokes per revolution for Garmin xHD
pub const SPOKES_PER_REVOLUTION: u16 = 1440;

/// Maximum spoke length (pixels)
pub const MAX_SPOKE_LEN: u16 = 1024;

/// Pixel depth (values 0-15 for 4-bit)
pub const PIXEL_VALUES: u8 = 16;

// =============================================================================
// Report Packet Types
// =============================================================================

/// Scan speed report
pub const REPORT_SCAN_SPEED: u32 = 0x0916;
/// Transmit state report
pub const REPORT_TRANSMIT_STATE: u32 = 0x0919;
/// Range report (meters)
pub const REPORT_RANGE: u32 = 0x091e;
/// Autogain mode (0=manual, 2=auto)
pub const REPORT_AUTOGAIN: u32 = 0x0924;
/// Gain value
pub const REPORT_GAIN: u32 = 0x0925;
/// Autogain level (0=low, 1=high)
pub const REPORT_AUTOGAIN_LEVEL: u32 = 0x091d;
/// Bearing alignment (value/32 = degrees)
pub const REPORT_BEARING_ALIGNMENT: u32 = 0x0930;
/// Crosstalk rejection
pub const REPORT_CROSSTALK: u32 = 0x0932;
/// Rain clutter mode
pub const REPORT_RAIN_MODE: u32 = 0x0933;
/// Rain clutter level
pub const REPORT_RAIN_LEVEL: u32 = 0x0934;
/// Sea clutter mode
pub const REPORT_SEA_MODE: u32 = 0x0939;
/// Sea clutter level
pub const REPORT_SEA_LEVEL: u32 = 0x093a;
/// Sea clutter auto level
pub const REPORT_SEA_AUTO_LEVEL: u32 = 0x093b;
/// No transmit zone mode
pub const REPORT_NTZ_MODE: u32 = 0x093f;
/// No transmit zone start (value/32 = degrees)
pub const REPORT_NTZ_START: u32 = 0x0940;
/// No transmit zone end (value/32 = degrees)
pub const REPORT_NTZ_END: u32 = 0x0941;
/// Timed idle mode
pub const REPORT_TIMED_IDLE_MODE: u32 = 0x0942;
/// Timed idle time
pub const REPORT_TIMED_IDLE_TIME: u32 = 0x0943;
/// Timed idle run time
pub const REPORT_TIMED_IDLE_RUN: u32 = 0x0944;
/// Scanner status
pub const REPORT_SCANNER_STATUS: u32 = 0x0992;
/// Scanner status change countdown (ms)
pub const REPORT_STATUS_CHANGE: u32 = 0x0993;
/// Scanner message (contains model info)
pub const REPORT_SCANNER_MESSAGE: u32 = 0x099b;

// =============================================================================
// Parsed Report Types
// =============================================================================

/// Parsed Garmin report packet
#[derive(Debug, Clone)]
pub enum Report {
    /// Scan speed (RPM or similar)
    ScanSpeed(u32),
    /// Transmit state
    TransmitState(TransmitState),
    /// Range in meters
    Range(u32),
    /// Gain settings (mode, value, level)
    Gain {
        mode: GainMode,
        value: u32,
        level: GainLevel,
    },
    /// Bearing alignment in degrees
    BearingAlignment(f32),
    /// Crosstalk rejection
    CrosstalkRejection(u32),
    /// Rain clutter settings
    RainClutter { mode: u32, level: u32 },
    /// Sea clutter settings
    SeaClutter {
        mode: u32,
        level: u32,
        auto_level: u32,
    },
    /// No transmit zone settings
    NoTransmitZone {
        mode: u32,
        start_deg: f32,
        end_deg: f32,
    },
    /// Timed idle settings
    TimedIdle { mode: u32, time: u32, run_time: u32 },
    /// Scanner status
    ScannerStatus { status: u32, change_in_ms: u32 },
    /// Scanner message (model info etc.)
    ScannerMessage(String),
    /// Unknown report type
    Unknown {
        packet_type: u32,
        value: u32,
        raw: Vec<u8>,
    },
}

/// Transmit state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmitState {
    Off,
    Standby,
    Transmit,
    WarmingUp,
    Unknown(u32),
}

impl TransmitState {
    pub fn from_value(v: u32) -> Self {
        match v {
            0 => TransmitState::Off,
            1 => TransmitState::Standby,
            2 => TransmitState::Transmit,
            3 => TransmitState::WarmingUp,
            _ => TransmitState::Unknown(v),
        }
    }

    pub fn to_radar_status(self) -> RadarStatus {
        match self {
            TransmitState::Off => RadarStatus::Off,
            TransmitState::Standby => RadarStatus::Standby,
            TransmitState::Transmit => RadarStatus::Transmit,
            TransmitState::WarmingUp => RadarStatus::Warming,
            TransmitState::Unknown(_) => RadarStatus::Unknown,
        }
    }
}

/// Gain mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainMode {
    Manual,
    Auto,
    Unknown(u32),
}

impl GainMode {
    pub fn from_value(v: u32) -> Self {
        match v {
            0 => GainMode::Manual,
            2 => GainMode::Auto,
            _ => GainMode::Unknown(v),
        }
    }
}

/// Autogain level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GainLevel {
    Low,
    High,
    Unknown(u32),
}

impl GainLevel {
    pub fn from_value(v: u32) -> Self {
        match v {
            0 => GainLevel::Low,
            1 => GainLevel::High,
            _ => GainLevel::Unknown(v),
        }
    }
}

// =============================================================================
// Spoke Data Structures
// =============================================================================

/// Garmin spoke header
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct SpokeHeader {
    /// Packet type (usually 0x2904)
    pub packet_type: [u8; 4],
    /// Data length
    pub len: [u8; 4],
    /// Bearing (angle) in Garmin units
    pub bearing: [u8; 2],
    /// Range in meters
    pub range: [u8; 4],
    /// Unknown field
    _u01: [u8; 2],
}

/// Spoke header size in bytes
pub const SPOKE_HEADER_SIZE: usize = std::mem::size_of::<SpokeHeader>();

/// Parsed spoke header
#[derive(Debug, Clone)]
pub struct ParsedSpokeHeader {
    /// Spoke bearing in degrees (0-360)
    pub bearing_deg: f32,
    /// Range in meters
    pub range_m: u32,
    /// Raw bearing value
    pub bearing_raw: u16,
}

// =============================================================================
// Parsing Functions
// =============================================================================

/// Check if data looks like a Garmin report packet
pub fn is_report_packet(data: &[u8]) -> bool {
    // Minimum size: 4 (type) + 4 (len) + 1 (data)
    data.len() >= 9
}

/// Parse a Garmin report packet
///
/// Report packets have the format:
/// - u32: packet type
/// - u32: data length
/// - \[u8; len\]: data (can be 1, 2, or 4 bytes typically)
pub fn parse_report(data: &[u8]) -> Result<Report, ParseError> {
    if data.len() < 9 {
        return Err(ParseError::TooShort {
            expected: 9,
            actual: data.len(),
        });
    }

    let packet_type = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let data = &data[8..];

    if data.len() < len {
        return Err(ParseError::LengthMismatch {
            header_len: len,
            actual_len: data.len(),
        });
    }

    // Extract value based on length (1, 2, or 4 bytes)
    let value: u32 = match len {
        1 => data[0] as u32,
        2 => u16::from_le_bytes(data[0..2].try_into().unwrap()) as u32,
        4 => u32::from_le_bytes(data[0..4].try_into().unwrap()),
        _ => 0,
    };

    let report = match packet_type {
        REPORT_SCAN_SPEED => Report::ScanSpeed(value),
        REPORT_TRANSMIT_STATE => Report::TransmitState(TransmitState::from_value(value)),
        REPORT_RANGE => Report::Range(value),
        REPORT_BEARING_ALIGNMENT => Report::BearingAlignment(value as i32 as f32 / 32.0),
        REPORT_CROSSTALK => Report::CrosstalkRejection(value),
        REPORT_SCANNER_STATUS => Report::ScannerStatus {
            status: value,
            change_in_ms: 0,
        },
        REPORT_STATUS_CHANGE => Report::ScannerStatus {
            status: 0,
            change_in_ms: value,
        },
        REPORT_SCANNER_MESSAGE if len >= 80 => {
            // Model info starts at offset 16, 64 bytes max
            let info_bytes: [u8; 64] = data[16..80].try_into().unwrap();
            let msg = crate::protocol::c_string(&info_bytes).unwrap_or_default();
            Report::ScannerMessage(msg)
        }
        _ => Report::Unknown {
            packet_type,
            value,
            raw: data[..len].to_vec(),
        },
    };

    Ok(report)
}

/// Parse a spoke header
pub fn parse_spoke_header(data: &[u8]) -> Result<ParsedSpokeHeader, ParseError> {
    if data.len() < SPOKE_HEADER_SIZE {
        return Err(ParseError::TooShort {
            expected: SPOKE_HEADER_SIZE,
            actual: data.len(),
        });
    }

    let header: SpokeHeader = bincode::deserialize(&data[..SPOKE_HEADER_SIZE])?;

    let bearing_raw = u16::from_le_bytes(header.bearing);
    let range_m = u32::from_le_bytes(header.range);

    // Convert bearing to degrees (Garmin uses 0-4096 for 360 degrees)
    let bearing_deg = (bearing_raw as f32 / 4096.0) * 360.0;

    Ok(ParsedSpokeHeader {
        bearing_deg,
        range_m,
        bearing_raw,
    })
}

/// Create a RadarDiscovery from a Garmin report source
///
/// Garmin discovery is different from other brands - we just need the
/// source IP address of any report packet.
pub fn create_discovery(source_addr: &str) -> RadarDiscovery {
    RadarDiscovery {
        brand: Brand::Garmin,
        model: Some("xHD".to_string()),
        name: format!("Garmin xHD @ {}", source_addr),
        address: source_addr.to_string(),
        data_port: DATA_PORT,
        command_port: SEND_PORT,
        spokes_per_revolution: SPOKES_PER_REVOLUTION,
        max_spoke_len: MAX_SPOKE_LEN,
        pixel_values: PIXEL_VALUES,
        serial_number: None,
        nic_address: None, // Set by locator
        suffix: None,
        data_address: None,
        report_address: None,
        send_address: None,
    }
}

// =============================================================================
// Command Creation
// =============================================================================

/// Create a transmit state command
pub fn create_transmit_command(transmit: bool) -> Vec<u8> {
    let state = if transmit { 2u32 } else { 1u32 }; // 2 = transmit, 1 = standby
    create_command(0x0919, state)
}

/// Create a range command
pub fn create_range_command(range_meters: u32) -> Vec<u8> {
    create_command(0x091e, range_meters)
}

/// Create a gain command
pub fn create_gain_command(auto: bool, value: u32) -> Vec<u8> {
    let mut cmds = Vec::new();
    let mode = if auto { 2u32 } else { 0u32 };
    cmds.extend(create_command(0x0924, mode));
    cmds.extend(create_command(0x0925, value));
    cmds
}

/// Create a sea clutter command
pub fn create_sea_clutter_command(auto: bool, value: u32) -> Vec<u8> {
    let mode = if auto { 1u32 } else { 0u32 };
    let mut cmds = Vec::new();
    cmds.extend(create_command(0x0939, mode));
    cmds.extend(create_command(0x093a, value));
    cmds
}

/// Create a rain clutter command
pub fn create_rain_clutter_command(auto: bool, value: u32) -> Vec<u8> {
    let mode = if auto { 1u32 } else { 0u32 };
    let mut cmds = Vec::new();
    cmds.extend(create_command(0x0933, mode));
    cmds.extend(create_command(0x0934, value));
    cmds
}

/// Create a bearing alignment command
pub fn create_bearing_alignment_command(degrees: f32) -> Vec<u8> {
    let value = (degrees * 32.0) as i32 as u32;
    create_command(0x0930, value)
}

/// Create a no-transmit zone command
pub fn create_ntz_command(enabled: bool, start_deg: f32, end_deg: f32) -> Vec<u8> {
    let mut cmds = Vec::new();
    let mode = if enabled { 1u32 } else { 0u32 };
    let start = (start_deg * 32.0) as i32 as u32;
    let end = (end_deg * 32.0) as i32 as u32;
    cmds.extend(create_command(0x093f, mode));
    cmds.extend(create_command(0x0940, start));
    cmds.extend(create_command(0x0941, end));
    cmds
}

/// Create a raw command packet
fn create_command(packet_type: u32, value: u32) -> Vec<u8> {
    let mut cmd = Vec::with_capacity(12);
    cmd.extend_from_slice(&packet_type.to_le_bytes());
    cmd.extend_from_slice(&4u32.to_le_bytes()); // length = 4
    cmd.extend_from_slice(&value.to_le_bytes());
    cmd
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transmit_state() {
        assert_eq!(TransmitState::from_value(0), TransmitState::Off);
        assert_eq!(TransmitState::from_value(1), TransmitState::Standby);
        assert_eq!(TransmitState::from_value(2), TransmitState::Transmit);
        assert_eq!(TransmitState::from_value(3), TransmitState::WarmingUp);
        assert_eq!(TransmitState::from_value(99), TransmitState::Unknown(99));
    }

    #[test]
    fn test_gain_mode() {
        assert_eq!(GainMode::from_value(0), GainMode::Manual);
        assert_eq!(GainMode::from_value(2), GainMode::Auto);
        assert_eq!(GainMode::from_value(5), GainMode::Unknown(5));
    }

    #[test]
    fn test_gain_level() {
        assert_eq!(GainLevel::from_value(0), GainLevel::Low);
        assert_eq!(GainLevel::from_value(1), GainLevel::High);
        assert_eq!(GainLevel::from_value(7), GainLevel::Unknown(7));
    }

    #[test]
    fn test_parse_report_too_short() {
        let data = [0u8; 5];
        assert!(matches!(
            parse_report(&data),
            Err(ParseError::TooShort { .. })
        ));
    }

    #[test]
    fn test_parse_range_report() {
        // Packet type 0x091e (range), length 4, value 1000
        let data = [
            0x1e, 0x09, 0x00, 0x00, // packet_type
            0x04, 0x00, 0x00, 0x00, // length
            0xe8, 0x03, 0x00, 0x00, // value = 1000
        ];
        let report = parse_report(&data).unwrap();
        match report {
            Report::Range(r) => assert_eq!(r, 1000),
            _ => panic!("Expected Range report"),
        }
    }

    #[test]
    fn test_parse_transmit_report() {
        // Packet type 0x0919 (transmit), length 4, value 2 (transmitting)
        let data = [
            0x19, 0x09, 0x00, 0x00, // packet_type
            0x04, 0x00, 0x00, 0x00, // length
            0x02, 0x00, 0x00, 0x00, // value = 2
        ];
        let report = parse_report(&data).unwrap();
        match report {
            Report::TransmitState(s) => assert_eq!(s, TransmitState::Transmit),
            _ => panic!("Expected TransmitState report"),
        }
    }

    #[test]
    fn test_parse_bearing_alignment_report() {
        // Packet type 0x0930 (bearing), length 4, value 320 (10 degrees)
        let data = [
            0x30, 0x09, 0x00, 0x00, // packet_type
            0x04, 0x00, 0x00, 0x00, // length
            0x40, 0x01, 0x00, 0x00, // value = 320
        ];
        let report = parse_report(&data).unwrap();
        match report {
            Report::BearingAlignment(deg) => {
                assert!((deg - 10.0).abs() < 0.01);
            }
            _ => panic!("Expected BearingAlignment report"),
        }
    }

    #[test]
    fn test_create_discovery() {
        let disc = create_discovery("192.168.1.100");
        assert_eq!(disc.brand, Brand::Garmin);
        assert_eq!(disc.model, Some("xHD".to_string()));
        assert_eq!(disc.data_port, 50102);
        assert_eq!(disc.command_port, 50101);
        assert_eq!(disc.spokes_per_revolution, 1440);
    }

    #[test]
    fn test_create_transmit_command() {
        let cmd = create_transmit_command(true);
        assert_eq!(cmd.len(), 12);
        let packet_type = u32::from_le_bytes(cmd[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(cmd[4..8].try_into().unwrap());
        let value = u32::from_le_bytes(cmd[8..12].try_into().unwrap());
        assert_eq!(packet_type, 0x0919);
        assert_eq!(len, 4);
        assert_eq!(value, 2); // transmit

        let cmd = create_transmit_command(false);
        let value = u32::from_le_bytes(cmd[8..12].try_into().unwrap());
        assert_eq!(value, 1); // standby
    }

    #[test]
    fn test_create_range_command() {
        let cmd = create_range_command(5000);
        let packet_type = u32::from_le_bytes(cmd[0..4].try_into().unwrap());
        let value = u32::from_le_bytes(cmd[8..12].try_into().unwrap());
        assert_eq!(packet_type, 0x091e);
        assert_eq!(value, 5000);
    }

    #[test]
    fn test_spoke_header_size() {
        // Verify our header struct is the expected size
        assert_eq!(SPOKE_HEADER_SIZE, 16);
    }
}
