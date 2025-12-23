//! Furuno TCP report parsing (pure, WASM-compatible)
//!
//! This module contains pure parsing functions for Furuno TCP protocol reports.
//! No I/O operations - just `&str` → `Result<FurunoReport>` functions.
//!
//! These functions can be used by:
//! - mayara-server for live radar control
//! - WASM clients for playback/simulation
//! - Testing frameworks

use super::command::range_index_to_meters;
use super::Model;
use crate::error::ParseError;

// =============================================================================
// Parsed Report Types
// =============================================================================

/// A parsed Furuno TCP report
#[derive(Debug, Clone, PartialEq)]
pub enum FurunoReport {
    /// Radar power status (standby/transmit)
    Status(StatusReport),
    /// Gain settings
    Gain(GainReport),
    /// Sea clutter settings
    Sea(SeaReport),
    /// Rain clutter settings
    Rain(RainReport),
    /// Current range
    Range(RangeReport),
    /// Operating hours
    OnTime(OnTimeReport),
    /// Module/firmware information
    Modules(ModulesReport),
    /// Alive check (keepalive response)
    AliveCheck,
    /// Custom picture settings (all settings combined)
    CustomPictureAll(CustomPictureAllReport),
    /// Antenna type information
    AntennaType(AntennaTypeReport),
    /// Blind sector (no-transmit zones)
    BlindSector(BlindSectorReport),
    /// Main bang suppression
    MainBangSize(MainBangReport),
    /// Antenna height
    AntennaHeight(AntennaHeightReport),
    /// Near STC (sensitivity time control)
    NearSTC(i32),
    /// Middle STC
    MiddleSTC(i32),
    /// Far STC
    FarSTC(i32),
    /// Wake up count
    WakeUpCount(i32),
    /// Unknown/unhandled command (for logging)
    Unknown { command_id: u8, values: Vec<f32> },
}

/// Radar power status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarState {
    Preparing = 0,
    Standby = 1,
    Transmit = 2,
    Off = 3,
}

impl From<i32> for RadarState {
    fn from(v: i32) -> Self {
        match v {
            0 => RadarState::Preparing,
            1 => RadarState::Standby,
            2 => RadarState::Transmit,
            _ => RadarState::Off,
        }
    }
}

/// Status report ($N69)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusReport {
    pub state: RadarState,
}

/// Gain report ($N63)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainReport {
    pub auto: bool,
    pub value: f32,
    pub auto_value: f32,
}

/// Sea clutter report ($N64)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeaReport {
    pub auto: bool,
    pub value: f32,
}

/// Rain clutter report ($N65)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RainReport {
    pub auto: bool,
    pub value: f32,
}

/// Range report ($N62)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeReport {
    /// Range in meters (converted from wire index)
    pub range_meters: i32,
}

/// Operating time report ($N8E)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OnTimeReport {
    /// Operating time in hours
    pub hours: f32,
}

/// Module/firmware information ($N96)
#[derive(Debug, Clone, PartialEq)]
pub struct ModulesReport {
    /// List of module parts with firmware code and version
    pub parts: Vec<ModulePart>,
}

/// A single module part from firmware report
#[derive(Debug, Clone, PartialEq)]
pub struct ModulePart {
    /// Firmware code (e.g., "0359360")
    pub code: String,
    /// Version string (e.g., "01.05")
    pub version: String,
}

/// Custom picture all settings ($N66)
#[derive(Debug, Clone, PartialEq)]
pub struct CustomPictureAllReport {
    pub values: Vec<f32>,
}

/// Antenna type report ($N6E)
#[derive(Debug, Clone, PartialEq)]
pub struct AntennaTypeReport {
    pub values: Vec<f32>,
}

/// Blind sector (no-transmit zones) report ($N77)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlindSectorReport {
    pub sector1_start: i32,
    pub sector1_end: i32,
    pub sector2_start: i32,
    pub sector2_end: i32,
}

/// Main bang suppression report ($N83)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MainBangReport {
    /// Value 0-255
    pub value: i32,
}

/// Antenna height report ($N84)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AntennaHeightReport {
    /// Height in meters
    pub meters: i32,
}

// =============================================================================
// Command ID enum (for parsing)
// =============================================================================

/// Known Furuno command IDs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandId {
    Connect = 0x60,
    DispMode = 0x61,
    Range = 0x62,
    Gain = 0x63,
    Sea = 0x64,
    Rain = 0x65,
    CustomPictureAll = 0x66,
    CustomPicture = 0x67,
    Status = 0x69,
    U6D = 0x6D,
    AntennaType = 0x6E,
    BlindSector = 0x77,
    Att = 0x80,
    MainBangSize = 0x83,
    AntennaHeight = 0x84,
    NearSTC = 0x85,
    MiddleSTC = 0x86,
    FarSTC = 0x87,
    AntennaRevolution = 0x89,
    AntennaSwitch = 0x8A,
    AntennaNo = 0x8D,
    OnTime = 0x8E,
    Modules = 0x96,
    Drift = 0x9E,
    ConningPosition = 0xAA,
    WakeUpCount = 0xAC,
    STCRange = 0xD2,
    CustomMemory = 0xD3,
    BuildUpTime = 0xD4,
    DisplayUnitInformation = 0xD5,
    CustomATFSettings = 0xE0,
    AliveCheck = 0xE3,
    ATFSettings = 0xEA,
    BearingResolutionSetting = 0xEE,
    AccuShip = 0xF0,
    RangeSelect = 0xFE,
}

impl CommandId {
    /// Try to convert a u8 to a CommandId
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x60 => Some(CommandId::Connect),
            0x61 => Some(CommandId::DispMode),
            0x62 => Some(CommandId::Range),
            0x63 => Some(CommandId::Gain),
            0x64 => Some(CommandId::Sea),
            0x65 => Some(CommandId::Rain),
            0x66 => Some(CommandId::CustomPictureAll),
            0x67 => Some(CommandId::CustomPicture),
            0x69 => Some(CommandId::Status),
            0x6D => Some(CommandId::U6D),
            0x6E => Some(CommandId::AntennaType),
            0x77 => Some(CommandId::BlindSector),
            0x80 => Some(CommandId::Att),
            0x83 => Some(CommandId::MainBangSize),
            0x84 => Some(CommandId::AntennaHeight),
            0x85 => Some(CommandId::NearSTC),
            0x86 => Some(CommandId::MiddleSTC),
            0x87 => Some(CommandId::FarSTC),
            0x89 => Some(CommandId::AntennaRevolution),
            0x8A => Some(CommandId::AntennaSwitch),
            0x8D => Some(CommandId::AntennaNo),
            0x8E => Some(CommandId::OnTime),
            0x96 => Some(CommandId::Modules),
            0x9E => Some(CommandId::Drift),
            0xAA => Some(CommandId::ConningPosition),
            0xAC => Some(CommandId::WakeUpCount),
            0xD2 => Some(CommandId::STCRange),
            0xD3 => Some(CommandId::CustomMemory),
            0xD4 => Some(CommandId::BuildUpTime),
            0xD5 => Some(CommandId::DisplayUnitInformation),
            0xE0 => Some(CommandId::CustomATFSettings),
            0xE3 => Some(CommandId::AliveCheck),
            0xEA => Some(CommandId::ATFSettings),
            0xEE => Some(CommandId::BearingResolutionSetting),
            0xF0 => Some(CommandId::AccuShip),
            0xFE => Some(CommandId::RangeSelect),
            _ => None,
        }
    }
}

// =============================================================================
// Firmware to Model Mapping
// =============================================================================

/// Map Furuno firmware code to radar model
///
/// Based on TZ Fec.Wrapper.SensorProperty.GetRadarSensorType
///
/// # Example
/// ```
/// use mayara_core::protocol::furuno::report::firmware_to_model;
/// use mayara_core::protocol::furuno::Model;
///
/// assert_eq!(firmware_to_model("0359360"), Model::DRS4DNXT);
/// assert_eq!(firmware_to_model("0359421"), Model::DRS6ANXT);
/// ```
pub fn firmware_to_model(firmware_code: &str) -> Model {
    match firmware_code {
        "0359235" => Model::DRS,
        "0359255" => Model::FAR14x7,
        "0359204" => Model::FAR21x7,
        "0359321" => Model::FAR14x7,
        "0359338" => Model::DRS4DL,
        "0359367" => Model::DRS4DL,
        "0359281" => Model::FAR3000,
        "0359286" => Model::FAR3000,
        "0359477" => Model::FAR3000,
        "0359360" => Model::DRS4DNXT,
        "0359421" => Model::DRS6ANXT,
        "0359355" => Model::DRS6AXCLASS,
        "0359344" => Model::FAR15x3,
        "0359397" => Model::FAR14x6,
        "0359422" => Model::DRS12ANXT,
        "0359423" => Model::DRS25ANXT,
        _ => Model::Unknown,
    }
}

// =============================================================================
// Main Parsing Function
// =============================================================================

/// Parse a Furuno TCP report line
///
/// Report format: `$N{cmd_hex},{arg1},{arg2},...\r\n`
///
/// # Arguments
/// * `line` - Raw line from TCP stream (may include leading garbage, \r\n)
///
/// # Returns
/// * `Ok(FurunoReport)` - Parsed report
/// * `Err(ParseError)` - If parsing fails
///
/// # Example
/// ```
/// use mayara_core::protocol::furuno::report::{parse_report, FurunoReport, RadarState};
///
/// let report = parse_report("$N69,2,0,0,60,300,0\r\n").unwrap();
/// match report {
///     FurunoReport::Status(s) => assert_eq!(s.state, RadarState::Transmit),
///     _ => panic!("Expected Status report"),
/// }
/// ```
#[inline(never)]
pub fn parse_report(line: &str) -> Result<FurunoReport, ParseError> {
    // Find the start of the report ($ character)
    let line = match line.find('$') {
        Some(pos) => &line[pos..],
        None => {
            return Err(ParseError::InvalidPacket(
                "No $ found in report".to_string(),
            ))
        }
    };

    // Minimum length check: $Nxx (4 chars)
    if line.len() < 4 {
        return Err(ParseError::TooShort {
            expected: 4,
            actual: line.len(),
        });
    }

    // Check for $N prefix (response/notification)
    let (prefix, rest) = line.split_at(2);
    if prefix != "$N" {
        return Err(ParseError::InvalidPacket(format!(
            "Expected $N prefix, got {:?}",
            prefix
        )));
    }

    // Trim trailing \r\n
    let rest = rest
        .trim_end_matches("\r\n")
        .trim_end_matches('\r')
        .trim_end_matches('\n');

    // Parse command ID (hex, before first comma or end)
    let mut parts = rest.split(',');
    let cmd_str = parts
        .next()
        .ok_or(ParseError::InvalidPacket("No command ID found".to_string()))?;
    let cmd = u8::from_str_radix(cmd_str.trim(), 16)
        .map_err(|_| ParseError::InvalidPacket(format!("Invalid command hex: {}", cmd_str)))?;

    // Collect remaining values as strings, then parse as f32
    let strings: Vec<&str> = parts.collect();
    let numbers: Vec<f32> = strings
        .iter()
        .map(|s| s.trim().parse::<f32>().unwrap_or(0.0))
        .collect();

    // Match command ID and parse accordingly
    let command_id = CommandId::from_u8(cmd);

    match command_id {
        Some(CommandId::Status) => {
            if numbers.is_empty() {
                return Err(ParseError::InvalidPacket(
                    "Status command missing state value".to_string(),
                ));
            }
            Ok(FurunoReport::Status(StatusReport {
                state: RadarState::from(numbers[0] as i32),
            }))
        }

        Some(CommandId::Gain) => {
            if numbers.len() < 5 {
                return Err(ParseError::InvalidPacket(format!(
                    "Gain command needs 5 values, got {}",
                    numbers.len()
                )));
            }
            let auto = numbers[2] as u8 > 0;
            let value = if auto { numbers[3] } else { numbers[1] };
            let auto_value = numbers[3];
            Ok(FurunoReport::Gain(GainReport {
                auto,
                value,
                auto_value,
            }))
        }

        Some(CommandId::Sea) => {
            if numbers.len() < 2 {
                return Err(ParseError::InvalidPacket(format!(
                    "Sea command needs 2 values, got {}",
                    numbers.len()
                )));
            }
            Ok(FurunoReport::Sea(SeaReport {
                auto: numbers[0] as u8 > 0,
                value: numbers[1],
            }))
        }

        Some(CommandId::Rain) => {
            if numbers.len() < 2 {
                return Err(ParseError::InvalidPacket(format!(
                    "Rain command needs 2 values, got {}",
                    numbers.len()
                )));
            }
            Ok(FurunoReport::Rain(RainReport {
                auto: numbers[0] as u8 > 0,
                value: numbers[1],
            }))
        }

        Some(CommandId::Range) => {
            if numbers.is_empty() {
                return Err(ParseError::InvalidPacket(
                    "Range command missing index".to_string(),
                ));
            }
            // Convert wire index to meters
            let wire_index = numbers[0] as i32;
            let range_meters = range_index_to_meters(wire_index).unwrap_or(0);
            Ok(FurunoReport::Range(RangeReport { range_meters }))
        }

        Some(CommandId::OnTime) => {
            if numbers.is_empty() {
                return Err(ParseError::InvalidPacket(
                    "OnTime command missing seconds".to_string(),
                ));
            }
            Ok(FurunoReport::OnTime(OnTimeReport {
                hours: numbers[0] / 3600.0,
            }))
        }

        Some(CommandId::Modules) => {
            // Parse module strings: "code-version,code-version,..."
            let parts: Vec<ModulePart> = strings
                .iter()
                .filter_map(|s| {
                    let s = s.trim();
                    if s.is_empty() {
                        return None;
                    }
                    s.split_once('-').map(|(code, version)| ModulePart {
                        code: code.to_string(),
                        version: version.to_string(),
                    })
                })
                .collect();
            Ok(FurunoReport::Modules(ModulesReport { parts }))
        }

        Some(CommandId::AliveCheck) => Ok(FurunoReport::AliveCheck),

        Some(CommandId::CustomPictureAll) => {
            Ok(FurunoReport::CustomPictureAll(CustomPictureAllReport {
                values: numbers,
            }))
        }

        Some(CommandId::AntennaType) => Ok(FurunoReport::AntennaType(AntennaTypeReport {
            values: numbers,
        })),

        Some(CommandId::BlindSector) => {
            if numbers.len() < 4 {
                return Err(ParseError::InvalidPacket(format!(
                    "BlindSector needs 4 values, got {}",
                    numbers.len()
                )));
            }
            Ok(FurunoReport::BlindSector(BlindSectorReport {
                sector1_start: numbers[0] as i32,
                sector1_end: numbers[1] as i32,
                sector2_start: numbers[2] as i32,
                sector2_end: numbers[3] as i32,
            }))
        }

        Some(CommandId::MainBangSize) => {
            if numbers.is_empty() {
                return Err(ParseError::InvalidPacket(
                    "MainBangSize missing value".to_string(),
                ));
            }
            Ok(FurunoReport::MainBangSize(MainBangReport {
                value: numbers[0] as i32,
            }))
        }

        Some(CommandId::AntennaHeight) => {
            if numbers.len() < 2 {
                return Err(ParseError::InvalidPacket(
                    "AntennaHeight missing value".to_string(),
                ));
            }
            Ok(FurunoReport::AntennaHeight(AntennaHeightReport {
                meters: numbers[1] as i32,
            }))
        }

        Some(CommandId::NearSTC) => Ok(FurunoReport::NearSTC(
            numbers.first().copied().unwrap_or(0.0) as i32,
        )),

        Some(CommandId::MiddleSTC) => Ok(FurunoReport::MiddleSTC(
            numbers.first().copied().unwrap_or(0.0) as i32,
        )),

        Some(CommandId::FarSTC) => Ok(FurunoReport::FarSTC(
            numbers.first().copied().unwrap_or(0.0) as i32,
        )),

        Some(CommandId::WakeUpCount) => Ok(FurunoReport::WakeUpCount(
            numbers.first().copied().unwrap_or(0.0) as i32,
        )),

        // Unknown or unhandled commands
        _ => Ok(FurunoReport::Unknown {
            command_id: cmd,
            values: numbers,
        }),
    }
}

/// Extract the radar model from a ModulesReport
///
/// The first part's firmware code determines the model.
pub fn model_from_modules(modules: &ModulesReport) -> Model {
    modules
        .parts
        .first()
        .map(|p| firmware_to_model(&p.code))
        .unwrap_or(Model::Unknown)
}

/// Extract firmware version from a ModulesReport
pub fn version_from_modules(modules: &ModulesReport) -> Option<String> {
    modules.parts.first().map(|p| p.version.clone())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_status_transmit() {
        let report = parse_report("$N69,2,0,0,60,300,0\r\n").unwrap();
        match report {
            FurunoReport::Status(s) => assert_eq!(s.state, RadarState::Transmit),
            _ => panic!("Expected Status report"),
        }
    }

    #[test]
    fn test_parse_status_standby() {
        let report = parse_report("$N69,1,0,0,60,300,0").unwrap();
        match report {
            FurunoReport::Status(s) => assert_eq!(s.state, RadarState::Standby),
            _ => panic!("Expected Status report"),
        }
    }

    #[test]
    fn test_parse_gain() {
        // Manual mode: auto=0, value at index 1
        let report = parse_report("$N63,50,50,0,80,0").unwrap();
        match report {
            FurunoReport::Gain(g) => {
                assert!(!g.auto);
                assert_eq!(g.value, 50.0);
            }
            _ => panic!("Expected Gain report"),
        }

        // Auto mode: auto=1 (at index 2), value at index 3
        let report = parse_report("$N63,50,50,1,80,0").unwrap();
        match report {
            FurunoReport::Gain(g) => {
                assert!(g.auto);
                assert_eq!(g.value, 80.0);
            }
            _ => panic!("Expected Gain report"),
        }
    }

    #[test]
    fn test_parse_range() {
        // Wire index 5 = 2778 meters (1.5nm)
        let report = parse_report("$N62,5,0,0").unwrap();
        match report {
            FurunoReport::Range(r) => assert_eq!(r.range_meters, 2778),
            _ => panic!("Expected Range report"),
        }
    }

    #[test]
    fn test_parse_ontime() {
        let report = parse_report("$N8E,3600").unwrap();
        match report {
            FurunoReport::OnTime(o) => assert_eq!(o.hours, 1.0),
            _ => panic!("Expected OnTime report"),
        }
    }

    #[test]
    fn test_parse_modules() {
        let report =
            parse_report("$N96,0359360-01.05,0359358-01.01,0359359-01.01,0359361-01.05,,,")
                .unwrap();
        match report {
            FurunoReport::Modules(m) => {
                assert_eq!(m.parts.len(), 4);
                assert_eq!(m.parts[0].code, "0359360");
                assert_eq!(m.parts[0].version, "01.05");
            }
            _ => panic!("Expected Modules report"),
        }
    }

    #[test]
    fn test_model_from_modules() {
        let modules = ModulesReport {
            parts: vec![ModulePart {
                code: "0359360".to_string(),
                version: "01.05".to_string(),
            }],
        };
        assert_eq!(model_from_modules(&modules), Model::DRS4DNXT);
    }

    #[test]
    fn test_firmware_to_model() {
        assert_eq!(firmware_to_model("0359360"), Model::DRS4DNXT);
        assert_eq!(firmware_to_model("0359421"), Model::DRS6ANXT);
        assert_eq!(firmware_to_model("0359355"), Model::DRS6AXCLASS);
        assert_eq!(firmware_to_model("unknown"), Model::Unknown);
    }

    #[test]
    fn test_parse_with_garbage_prefix() {
        // Sometimes TCP data has garbage before the $
        let report = parse_report("\x00\x01garbage$N69,2,0,0,60,300,0\r\n").unwrap();
        match report {
            FurunoReport::Status(s) => assert_eq!(s.state, RadarState::Transmit),
            _ => panic!("Expected Status report"),
        }
    }

    #[test]
    fn test_parse_alive_check() {
        let report = parse_report("$NE3").unwrap();
        assert_eq!(report, FurunoReport::AliveCheck);
    }

    #[test]
    fn test_parse_unknown_command() {
        let report = parse_report("$NFF,1,2,3").unwrap();
        match report {
            FurunoReport::Unknown { command_id, values } => {
                assert_eq!(command_id, 0xFF);
                assert_eq!(values, vec![1.0, 2.0, 3.0]);
            }
            _ => panic!("Expected Unknown report"),
        }
    }

    #[test]
    fn test_parse_sea() {
        let report = parse_report("$N64,0,60,50,0,0,0").unwrap();
        match report {
            FurunoReport::Sea(s) => {
                assert!(!s.auto);
                assert_eq!(s.value, 60.0);
            }
            _ => panic!("Expected Sea report"),
        }
    }

    #[test]
    fn test_parse_rain() {
        let report = parse_report("$N65,1,30,0,0,0,0").unwrap();
        match report {
            FurunoReport::Rain(r) => {
                assert!(r.auto);
                assert_eq!(r.value, 30.0);
            }
            _ => panic!("Expected Rain report"),
        }
    }
}
