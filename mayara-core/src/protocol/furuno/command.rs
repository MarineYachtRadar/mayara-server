//! Furuno radar command formatting
//!
//! Pure functions for building Furuno protocol command strings.
//! No I/O operations - just returns formatted strings ready to send.

use std::fmt::Write;

// =============================================================================
// Command Mode
// =============================================================================

/// Command mode prefix character
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    /// Set a value (prefix 'S')
    Set,
    /// Request current value (prefix 'R')
    Request,
    /// New/response (prefix 'N')
    New,
}

impl CommandMode {
    /// Get the character prefix for this command mode
    pub fn as_char(self) -> char {
        match self {
            CommandMode::Set => 'S',
            CommandMode::Request => 'R',
            CommandMode::New => 'N',
        }
    }
}

// =============================================================================
// Command IDs
// =============================================================================

/// Furuno command IDs (hex values used in protocol)
/// See docs/furuno/protocol.md for complete reference
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandId {
    Connect = 0x60,
    Range = 0x62,
    Gain = 0x63,
    Sea = 0x64,
    Rain = 0x65,
    CustomPictureAll = 0x66,
    /// Multi-purpose signal processing (feature=0: IntReject, feature=3: NoiseReduction)
    SignalProcessing = 0x67,
    Status = 0x69,
    BlindSector = 0x77,
    HeadingAlign = 0x81,
    MainBangSize = 0x83,
    AntennaHeight = 0x84,
    ScanSpeed = 0x89,
    /// Operating time in seconds (total power-on time)
    OnTime = 0x8E,
    /// Transmit time in seconds (total time radar has been transmitting)
    TxTime = 0x8F,
    /// Module/firmware information
    Modules = 0x96,
    AliveCheck = 0xE3,
    TxChannel = 0xEC,
    BirdMode = 0xED,
    RezBoost = 0xEE,
    TargetAnalyzer = 0xEF,
    AutoAcquire = 0xF0,
}

impl CommandId {
    /// Get the hex value for this command
    pub fn as_hex(self) -> u8 {
        self as u8
    }
}

// =============================================================================
// Login Protocol
// =============================================================================

/// Login message sent to port 10000 to get dynamic command port
/// From fnet.dll function "login_via_copyright"
pub const LOGIN_MESSAGE: [u8; 56] = [
    0x08, 0x01, 0x00, 0x38, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    // "COPYRIGHT (C) 2001 FURUNO ELECTRIC CO.,LTD. "
    0x43, 0x4f, 0x50, 0x59, 0x52, 0x49, 0x47, 0x48, 0x54, 0x20, 0x28, 0x43, 0x29, 0x20, 0x32, 0x30,
    0x30, 0x31, 0x20, 0x46, 0x55, 0x52, 0x55, 0x4e, 0x4f, 0x20, 0x45, 0x4c, 0x45, 0x43, 0x54, 0x52,
    0x49, 0x43, 0x20, 0x43, 0x4f, 0x2e, 0x2c, 0x4c, 0x54, 0x44, 0x2e, 0x20,
];

/// Expected header in login response (8 bytes)
pub const LOGIN_RESPONSE_HEADER: [u8; 8] = [0x09, 0x01, 0x00, 0x0c, 0x01, 0x00, 0x00, 0x00];

/// Parse login response to extract the dynamic command port
///
/// The radar responds with 12 bytes total:
/// - Bytes 0-7: Header (LOGIN_RESPONSE_HEADER)
/// - Bytes 8-9: Port offset (big-endian)
/// - Bytes 10-11: Unknown
///
/// Returns the port number (BASE_PORT + offset) if valid
pub fn parse_login_response(data: &[u8]) -> Option<u16> {
    if data.len() < 12 {
        return None;
    }
    if data[0..8] != LOGIN_RESPONSE_HEADER {
        return None;
    }
    // Port offset is in bytes 8-9, big-endian
    let port_offset = ((data[8] as u16) << 8) | (data[9] as u16);
    Some(super::BASE_PORT + port_offset)
}

// =============================================================================
// Command Formatting Functions
// =============================================================================

/// Format a generic Furuno command
///
/// # Arguments
/// * `mode` - Command mode (Set, Request, New)
/// * `id` - Command ID
/// * `args` - Command arguments
///
/// # Returns
/// Formatted command string with \r\n terminator
///
/// # Example
/// ```
/// use mayara_core::protocol::furuno::command::{format_command, CommandMode, CommandId};
/// let cmd = format_command(CommandMode::Set, CommandId::Status, &[2, 0, 0, 60, 300, 0]);
/// assert_eq!(cmd, "$S69,2,0,0,60,300,0\r\n");
/// ```
pub fn format_command(mode: CommandMode, id: CommandId, args: &[i32]) -> String {
    let mut message = format!("${}{:X}", mode.as_char(), id.as_hex());
    for arg in args {
        let _ = write!(&mut message, ",{}", arg);
    }
    message.push_str("\r\n");
    message
}

/// Format status command (transmit/standby)
///
/// Command 0x69 controls radar power state:
/// - value=2: Transmit
/// - value=1: Standby
///
/// # Arguments
/// * `transmit` - true for transmit, false for standby
///
/// # Returns
/// Formatted command: `$S69,{1|2},0,0,60,300,0\r\n`
pub fn format_status_command(transmit: bool) -> String {
    let value = if transmit { 2 } else { 1 };
    // Args: status, 0, watchman_on_off, watchman_on_time, watchman_off_time, 0
    format_command(
        CommandMode::Set,
        CommandId::Status,
        &[value, 0, 0, 60, 300, 0],
    )
}

/// Format range command
///
/// # Arguments
/// * `range_meters` - Desired range in meters (e.g. 88896 for 48nm)
///
/// # Returns
/// Formatted command: `$S62,{wire_index},0,0\r\n`
/// The wire index is looked up from RANGE_TABLE based on the meters value.
pub fn format_range_command(range_meters: i32) -> String {
    let wire_index = meters_to_range_index(range_meters);
    format_command(CommandMode::Set, CommandId::Range, &[wire_index, 0, 0])
}

/// Furuno range index table (wire_index -> meters)
/// Verified via Wireshark captures from TimeZero ↔ DRS4D-NXT
/// Note: Wire indices are non-sequential (21 is min, 19 is out of order)
pub const RANGE_TABLE: [(i32, i32); 18] = [
    (21, 116),   // 1/16 nm = 116m (minimum range)
    (0, 231),    // 1/8 nm = 231m
    (1, 463),    // 1/4 nm = 463m
    (2, 926),    // 1/2 nm = 926m
    (3, 1389),   // 3/4 nm = 1389m
    (4, 1852),   // 1 nm = 1852m
    (5, 2778),   // 1.5 nm = 2778m
    (6, 3704),   // 2 nm = 3704m
    (7, 5556),   // 3 nm = 5556m
    (8, 7408),   // 4 nm = 7408m
    (9, 11112),  // 6 nm = 11112m
    (10, 14816), // 8 nm = 14816m
    (11, 22224), // 12 nm = 22224m
    (12, 29632), // 16 nm = 29632m
    (13, 44448), // 24 nm = 44448m
    (14, 59264), // 32 nm = 59264m
    (19, 66672), // 36 nm = 66672m (out of sequence!)
    (15, 88896), // 48 nm = 88896m (maximum range)
];

/// Convert wire index to meters
pub fn range_index_to_meters(wire_index: i32) -> Option<i32> {
    RANGE_TABLE
        .iter()
        .find(|(idx, _)| *idx == wire_index)
        .map(|(_, meters)| *meters)
}

/// Convert meters to wire index
/// Uses exact match lookup in the ordered RANGE_TABLE array.
/// The table is ordered by distance (index 0 = smallest, index 17 = largest).
pub fn meters_to_range_index(meters: i32) -> i32 {
    // RANGE_TABLE is ordered by distance, so we can do direct lookup
    for (wire_idx, m) in RANGE_TABLE.iter() {
        if *m == meters {
            return *wire_idx;
        }
    }
    // If no exact match, find the closest one that's >= requested meters
    for (wire_idx, m) in RANGE_TABLE.iter() {
        if *m >= meters {
            return *wire_idx;
        }
    }
    // Fallback to max range (48nm = wire index 15)
    15
}

/// Format gain command
///
/// # Arguments
/// * `value` - Gain value (0-100)
/// * `auto` - true for automatic gain control
///
/// # Returns
/// Formatted command: `$S63,{auto},{value},0,80,0\r\n`
/// Based on pcap: `$S63,0,50,0,80,0` (manual, value=50)
pub fn format_gain_command(value: i32, auto: bool) -> String {
    let auto_val = if auto { 1 } else { 0 };
    // From pcap: $S63,{auto},{value},0,80,0
    format_command(
        CommandMode::Set,
        CommandId::Gain,
        &[auto_val, value, 0, 80, 0],
    )
}

/// Format sea clutter command
///
/// # Arguments
/// * `value` - Sea clutter value (0-100)
/// * `auto` - true for automatic sea clutter control
///
/// # Returns
/// Formatted command: `$S64,{auto},{value},50,0,0,0\r\n`
/// Based on pcap: `$S64,{auto},{value},50,0,0,0`
pub fn format_sea_command(value: i32, auto: bool) -> String {
    let auto_val = if auto { 1 } else { 0 };
    format_command(
        CommandMode::Set,
        CommandId::Sea,
        &[auto_val, value, 50, 0, 0, 0],
    )
}

/// Format rain clutter command
///
/// # Arguments
/// * `value` - Rain clutter value (0-100)
/// * `auto` - true for automatic rain clutter control
///
/// # Returns
/// Formatted command: `$S65,{auto},{value},0,0,0,0\r\n`
/// Based on pcap: `$S65,{auto},{value},0,0,0,0`
pub fn format_rain_command(value: i32, auto: bool) -> String {
    let auto_val = if auto { 1 } else { 0 };
    format_command(
        CommandMode::Set,
        CommandId::Rain,
        &[auto_val, value, 0, 0, 0, 0],
    )
}

/// Format keep-alive (alive check) command
///
/// Should be sent every 5 seconds to maintain connection
///
/// # Returns
/// Formatted command: `$RE3\r\n`
pub fn format_keepalive() -> String {
    format_command(CommandMode::Request, CommandId::AliveCheck, &[])
}

/// Format request for all picture settings
///
/// # Returns
/// Formatted command: `$R66\r\n`
pub fn format_request_picture_all() -> String {
    format_command(CommandMode::Request, CommandId::CustomPictureAll, &[])
}

/// Format request for module/firmware information
///
/// # Returns
/// Formatted command: `$R96\r\n`
///
/// Response format: `$N96,{part1}-{ver1},{part2}-{ver2},...`
/// Example: `$N96,0359360-01.05,0359358-01.01,0359359-01.01,0359361-01.05,,,`
pub fn format_request_modules() -> String {
    format_command(CommandMode::Request, CommandId::Modules, &[])
}

/// Format request for operating time (total power-on hours)
///
/// # Returns
/// Formatted command: `$R8E,0\r\n`
///
/// Response format: `$N8E,{seconds}` where seconds is total power-on time
pub fn format_request_ontime() -> String {
    format_command(CommandMode::Request, CommandId::OnTime, &[0])
}

/// Format request for transmit time (total transmit hours)
///
/// # Returns
/// Formatted command: `$R8F,0\r\n`
///
/// Response format: `$N8F,{seconds}` where seconds is total transmit time
pub fn format_request_txtime() -> String {
    format_command(CommandMode::Request, CommandId::TxTime, &[0])
}

/// Format blind sector (no-transmit zone) command
///
/// # Arguments
/// * `s2_enable` - true to enable sector 2
/// * `s1_start` - Sector 1 start angle in degrees (0-359)
/// * `s1_width` - Sector 1 width in degrees (0 to disable)
/// * `s2_start` - Sector 2 start angle in degrees (0-359)
/// * `s2_width` - Sector 2 width in degrees
///
/// # Returns
/// Formatted command: `$S77,{s2_enable},{s1_start},{s1_width},{s2_start},{s2_width}\r\n`
pub fn format_blind_sector_command(
    s2_enable: bool,
    s1_start: i32,
    s1_width: i32,
    s2_start: i32,
    s2_width: i32,
) -> String {
    let s2_val = if s2_enable { 1 } else { 0 };
    format_command(
        CommandMode::Set,
        CommandId::BlindSector,
        &[s2_val, s1_start, s1_width, s2_start, s2_width],
    )
}

/// Format scan speed (antenna revolution) command
///
/// # Arguments
/// * `mode` - 0 for 24 RPM, 2 for Auto
///
/// # Returns
/// Formatted command: `$S89,{mode},0\r\n`
pub fn format_scan_speed_command(mode: i32) -> String {
    format_command(CommandMode::Set, CommandId::ScanSpeed, &[mode, 0])
}

/// Format noise reduction command
///
/// # Arguments
/// * `enabled` - true to enable noise reduction
///
/// # Returns
/// Formatted command: `$S67,0,3,{enabled},0\r\n`
pub fn format_noise_reduction_command(enabled: bool) -> String {
    let val = if enabled { 1 } else { 0 };
    // Feature 3 = Noise Reduction
    format_command(
        CommandMode::Set,
        CommandId::SignalProcessing,
        &[0, 3, val, 0],
    )
}

/// Format interference rejection command
///
/// # Arguments
/// * `enabled` - true to enable interference rejection
///
/// # Returns
/// Formatted command: `$S67,0,0,{enabled},0\r\n`
/// Note: enabled maps to 2 (not 1) per protocol spec
pub fn format_interference_rejection_command(enabled: bool) -> String {
    let val = if enabled { 2 } else { 0 };
    // Feature 0 = Interference Rejection
    format_command(
        CommandMode::Set,
        CommandId::SignalProcessing,
        &[0, 0, val, 0],
    )
}

/// Format RezBoost command
///
/// # Arguments
/// * `level` - 0=OFF, 1=Low, 2=Medium, 3=High
/// * `screen` - 0=Primary, 1=Secondary (dual scan)
///
/// # Returns
/// Formatted command: `$SEE,{level},{screen}\r\n`
pub fn format_rezboost_command(level: i32, screen: i32) -> String {
    format_command(CommandMode::Set, CommandId::RezBoost, &[level, screen])
}

/// Format Bird Mode command
///
/// # Arguments
/// * `level` - 0=OFF, 1=Low, 2=Medium, 3=High
/// * `screen` - 0=Primary, 1=Secondary (dual scan)
///
/// # Returns
/// Formatted command: `$SED,{level},{screen}\r\n`
pub fn format_bird_mode_command(level: i32, screen: i32) -> String {
    format_command(CommandMode::Set, CommandId::BirdMode, &[level, screen])
}

/// Format Target Analyzer command
///
/// # Arguments
/// * `enabled` - true to enable target analyzer
/// * `mode` - 0=Target, 1=Rain
/// * `screen` - 0=Primary, 1=Secondary (dual scan)
///
/// # Returns
/// Formatted command: `$SEF,{enabled},{mode},{screen}\r\n`
pub fn format_target_analyzer_command(enabled: bool, mode: i32, screen: i32) -> String {
    let val = if enabled { 1 } else { 0 };
    format_command(
        CommandMode::Set,
        CommandId::TargetAnalyzer,
        &[val, mode, screen],
    )
}

/// Format TX Channel command
///
/// # Arguments
/// * `channel` - 0=Auto, 1-3=Channel 1-3
///
/// # Returns
/// Formatted command: `$SEC,{channel}\r\n`
pub fn format_tx_channel_command(channel: i32) -> String {
    format_command(CommandMode::Set, CommandId::TxChannel, &[channel])
}

/// Format Auto Acquire (ARPA) command
///
/// # Arguments
/// * `enabled` - true to enable auto acquire by Doppler
///
/// # Returns
/// Formatted command: `$SF0,{enabled}\r\n`
pub fn format_auto_acquire_command(enabled: bool) -> String {
    let val = if enabled { 1 } else { 0 };
    format_command(CommandMode::Set, CommandId::AutoAcquire, &[val])
}

/// Format main bang suppression command
///
/// # Arguments
/// * `value` - 0-100 percentage (will be mapped to 0-255)
///
/// # Returns
/// Formatted command: `$S83,{value_255},0\r\n`
pub fn format_main_bang_command(percent: i32) -> String {
    // Map 0-100% to 0-255
    let value = (percent * 255) / 100;
    format_command(CommandMode::Set, CommandId::MainBangSize, &[value, 0])
}

/// Format heading alignment command
///
/// # Arguments
/// * `degrees_x10` - Heading offset in tenths of degrees (0-3599 for 0.0°-359.9°)
///
/// # Returns
/// Formatted command: `$S81,{degrees_x10},0\r\n`
pub fn format_heading_align_command(degrees_x10: i32) -> String {
    format_command(CommandMode::Set, CommandId::HeadingAlign, &[degrees_x10, 0])
}

/// Format antenna height command
///
/// # Arguments
/// * `meters` - Antenna height in meters
///
/// # Returns
/// Formatted command: `$S84,0,{meters},0\r\n`
///
/// Antenna height affects sea clutter calculations.
pub fn format_antenna_height_command(meters: i32) -> String {
    format_command(CommandMode::Set, CommandId::AntennaHeight, &[0, meters, 0])
}

// =============================================================================
// Response Parsing
// =============================================================================

/// Parse a Furuno response line
///
/// Response format: `${mode}{command_id},{args...}`
///
/// # Returns
/// Tuple of `(CommandMode, command_id, Vec<args>)` if valid
#[inline(never)]
pub fn parse_response(line: &str) -> Option<(CommandMode, u8, Vec<i32>)> {
    let line = line.trim();
    if !line.starts_with('$') || line.len() < 3 {
        return None;
    }

    let mode = match line.chars().nth(1)? {
        'S' => CommandMode::Set,
        'R' => CommandMode::Request,
        'N' => CommandMode::New,
        _ => return None,
    };

    // Parse command ID (hex, 1-2 chars)
    let rest = &line[2..];
    let comma_pos = rest.find(',').unwrap_or(rest.len());
    let cmd_id = u8::from_str_radix(&rest[..comma_pos], 16).ok()?;

    // Parse arguments
    let mut args = Vec::new();
    if comma_pos < rest.len() {
        for arg in rest[comma_pos + 1..].split(',') {
            if let Ok(val) = arg.trim().parse::<i32>() {
                args.push(val);
            }
        }
    }

    Some((mode, cmd_id, args))
}

/// Parse status response to get current radar state
///
/// Response: `$N69,{status},0,...`
/// - status=1: Standby
/// - status=2: Transmit
///
/// # Returns
/// true if transmitting, false if standby, None if invalid
#[inline(never)]
pub fn parse_status_response(line: &str) -> Option<bool> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::Status.as_hex() {
        return None;
    }
    args.first().map(|&status| status == 2)
}

/// Control value with auto/manual mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlValue {
    pub auto: bool,
    pub value: i32,
}

/// Parse gain response
///
/// Response: `$N63,{auto},{value},0,80,0`
/// - auto=0: Manual, auto=1: Auto
/// - value: 0-100
///
/// # Returns
/// ControlValue with auto mode and value
#[inline(never)]
pub fn parse_gain_response(line: &str) -> Option<ControlValue> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::Gain.as_hex() {
        return None;
    }
    if args.len() >= 2 {
        Some(ControlValue {
            auto: args[0] == 1,
            value: args[1],
        })
    } else {
        None
    }
}

/// Parse sea clutter response
///
/// Response: `$N64,{auto},{value},50,0,0,0`
/// - auto=0: Manual, auto=1: Auto
/// - value: 0-100
///
/// # Returns
/// ControlValue with auto mode and value
#[inline(never)]
pub fn parse_sea_response(line: &str) -> Option<ControlValue> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::Sea.as_hex() {
        return None;
    }
    if args.len() >= 2 {
        Some(ControlValue {
            auto: args[0] == 1,
            value: args[1],
        })
    } else {
        None
    }
}

/// Parse rain clutter response
///
/// Response: `$N65,{auto},{value},0,0,0,0`
/// - auto=0: Manual, auto=1: Auto
/// - value: 0-100
///
/// # Returns
/// ControlValue with auto mode and value
#[inline(never)]
pub fn parse_rain_response(line: &str) -> Option<ControlValue> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::Rain.as_hex() {
        return None;
    }
    if args.len() >= 2 {
        Some(ControlValue {
            auto: args[0] == 1,
            value: args[1],
        })
    } else {
        None
    }
}

/// Parse range response
///
/// Response: `$N62,{range_index},0,0`
/// - range_index: Index into range table
///
/// # Returns
/// Range index (use range_index_to_meters to convert)
#[inline(never)]
pub fn parse_range_response(line: &str) -> Option<i32> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::Range.as_hex() {
        return None;
    }
    args.first().copied()
}

// =============================================================================
// Request Command Formatters
// =============================================================================

/// Format request for current status (power state)
///
/// # Returns
/// Formatted command: `$R69\r\n`
pub fn format_request_status() -> String {
    format_command(CommandMode::Request, CommandId::Status, &[])
}

/// Format request for current gain settings
///
/// # Returns
/// Formatted command: `$R63\r\n`
pub fn format_request_gain() -> String {
    format_command(CommandMode::Request, CommandId::Gain, &[])
}

/// Format request for current sea clutter settings
///
/// # Returns
/// Formatted command: `$R64\r\n`
pub fn format_request_sea() -> String {
    format_command(CommandMode::Request, CommandId::Sea, &[])
}

/// Format request for current rain clutter settings
///
/// # Returns
/// Formatted command: `$R65\r\n`
pub fn format_request_rain() -> String {
    format_command(CommandMode::Request, CommandId::Rain, &[])
}

/// Format request for current range
///
/// # Returns
/// Formatted command: `$R62\r\n`
pub fn format_request_range() -> String {
    format_command(CommandMode::Request, CommandId::Range, &[])
}

/// Format request for signal processing settings (0x67)
///
/// This returns multiple $N67 responses, one per feature:
/// - Feature 0: Interference Rejection
/// - Feature 3: Noise Reduction
///
/// # Returns
/// Formatted command: `$R67\r\n`
pub fn format_request_signal_processing() -> String {
    format_command(CommandMode::Request, CommandId::SignalProcessing, &[])
}

/// Format request for noise reduction state (feature 3)
///
/// # Returns
/// Formatted command: `$R67,0,3\r\n`
pub fn format_request_noise_reduction() -> String {
    format_command(CommandMode::Request, CommandId::SignalProcessing, &[0, 3])
}

/// Format request for interference rejection state (feature 0)
///
/// # Returns
/// Formatted command: `$R67,0,0\r\n`
pub fn format_request_interference_rejection() -> String {
    format_command(CommandMode::Request, CommandId::SignalProcessing, &[0, 0])
}

/// Signal processing settings parsed from $N67 responses
#[derive(Debug, Clone, Default)]
pub struct SignalProcessingState {
    /// Interference rejection enabled (feature 0, value 2=ON)
    pub interference_rejection: bool,
    /// Noise reduction enabled (feature 3, value 1=ON)
    pub noise_reduction: bool,
}

/// Parse signal processing response
///
/// Response format varies:
/// - From SET echo: `$N67,0,{feature},{value},{screen}` (5 args)
/// - From REQUEST: `$N67,{feature},{value},{screen}` (4 args) - TBD, need to verify
///
/// - feature 0: Interference Rejection (0=OFF, 2=ON)
/// - feature 3: Noise Reduction (0=OFF, 1=ON)
///
/// # Returns
/// Tuple of (feature, value) if this is a $N67 response
#[inline(never)]
pub fn parse_signal_processing_response(line: &str) -> Option<(i32, i32)> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::SignalProcessing.as_hex() {
        return None;
    }
    // Two response formats based on argument count:
    // Format 1 (SET echo): $N67,0,{feature},{value},{screen} = 4 args
    // Format 2 (REQUEST response): $N67,{feature},{value},{screen} = 3 args
    //
    // We distinguish by arg count, NOT by first arg value.
    // This is important because feature=0 (interference rejection) would be
    // ambiguous if we checked args[0]==0.
    if args.len() >= 4 && args[0] == 0 {
        // Format 1: SET echo with leading 0
        Some((args[1], args[2]))
    } else if args.len() == 3 {
        // Format 2: REQUEST response - feature, value, screen
        Some((args[0], args[1]))
    } else if args.len() >= 2 {
        // Fallback for any other format
        Some((args[0], args[1]))
    } else {
        None
    }
}

/// Format request for RezBoost (beam sharpening) settings
///
/// # Returns
/// Formatted command: `$REE\r\n`
///
/// Response format: `$NEE,{level},{screen}` where level is 0=OFF, 1=Low, 2=Med, 3=High
pub fn format_request_rezboost() -> String {
    format_command(CommandMode::Request, CommandId::RezBoost, &[])
}

/// Parse RezBoost response
///
/// Response: `$NEE,{level},{screen}`
/// - level: 0=OFF, 1=Low, 2=Medium, 3=High
/// - screen: 0=Primary, 1=Secondary
///
/// # Returns
/// level value (0-3)
#[inline(never)]
pub fn parse_rezboost_response(line: &str) -> Option<i32> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::RezBoost.as_hex() {
        return None;
    }
    args.first().copied()
}

/// Format request for Bird Mode settings
///
/// # Returns
/// Formatted command: `$RED\r\n`
///
/// Response format: `$NED,{level},{screen}` where level is 0=OFF, 1=Low, 2=Med, 3=High
pub fn format_request_bird_mode() -> String {
    format_command(CommandMode::Request, CommandId::BirdMode, &[])
}

/// Parse Bird Mode response
///
/// Response: `$NED,{level},{screen}`
/// - level: 0=OFF, 1=Low, 2=Medium, 3=High
///
/// # Returns
/// level value (0-3)
#[inline(never)]
pub fn parse_bird_mode_response(line: &str) -> Option<i32> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::BirdMode.as_hex() {
        return None;
    }
    args.first().copied()
}

/// Format request for Target Analyzer (Doppler) settings
///
/// # Returns
/// Formatted command: `$REF\r\n`
///
/// Response format: `$NEF,{enabled},{mode},{screen}`
pub fn format_request_target_analyzer() -> String {
    format_command(CommandMode::Request, CommandId::TargetAnalyzer, &[])
}

/// Target Analyzer state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetAnalyzerState {
    /// Whether Target Analyzer is enabled
    pub enabled: bool,
    /// Mode: 0=Target, 1=Rain
    pub mode: i32,
}

/// Parse Target Analyzer response
///
/// Response: `$NEF,{enabled},{mode},{screen}`
/// - enabled: 0=OFF, 1=ON
/// - mode: 0=Target, 1=Rain
///
/// # Returns
/// TargetAnalyzerState with enabled and mode
#[inline(never)]
pub fn parse_target_analyzer_response(line: &str) -> Option<TargetAnalyzerState> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::TargetAnalyzer.as_hex() {
        return None;
    }
    if args.len() >= 2 {
        Some(TargetAnalyzerState {
            enabled: args[0] == 1,
            mode: args[1],
        })
    } else {
        None
    }
}

/// Format request for Scan Speed settings
///
/// # Returns
/// Formatted command: `$R89\r\n`
///
/// Response format: `$N89,{mode},{screen}` where mode is 0=24RPM, 2=Auto
pub fn format_request_scan_speed() -> String {
    format_command(CommandMode::Request, CommandId::ScanSpeed, &[])
}

/// Parse Scan Speed response
///
/// Response: `$N89,{mode},{screen}`
/// - mode: 0=24RPM, 2=Auto
///
/// # Returns
/// mode value (0 or 2)
#[inline(never)]
pub fn parse_scan_speed_response(line: &str) -> Option<i32> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::ScanSpeed.as_hex() {
        return None;
    }
    args.first().copied()
}

/// Format request for Main Bang Suppression settings
///
/// # Returns
/// Formatted command: `$R83\r\n`
///
/// Response format: `$N83,{value},{screen}` where value is 0-255
pub fn format_request_main_bang() -> String {
    format_command(CommandMode::Request, CommandId::MainBangSize, &[])
}

/// Parse Main Bang Suppression response
///
/// Response: `$N83,{value},{screen}`
/// - value: 0-255 (raw value, needs conversion to 0-100%)
///
/// # Returns
/// Percentage value (0-100)
#[inline(never)]
pub fn parse_main_bang_response(line: &str) -> Option<i32> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::MainBangSize.as_hex() {
        return None;
    }
    // Convert 0-255 to 0-100%
    args.first().map(|&val| (val * 100) / 255)
}

/// Format request for TX Channel settings
///
/// # Returns
/// Formatted command: `$REC\r\n`
///
/// Response format: `$NEC,{channel}` where channel is 0=Auto, 1-3=Channel 1-3
pub fn format_request_tx_channel() -> String {
    format_command(CommandMode::Request, CommandId::TxChannel, &[])
}

/// Parse TX Channel response
///
/// Response: `$NEC,{channel}`
/// - channel: 0=Auto, 1-3=Channel 1-3
///
/// # Returns
/// channel value (0-3)
#[inline(never)]
pub fn parse_tx_channel_response(line: &str) -> Option<i32> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::TxChannel.as_hex() {
        return None;
    }
    args.first().copied()
}

/// Format request for Blind Sector (no-transmit zones) settings
///
/// # Returns
/// Formatted command: `$R77\r\n`
///
/// Response format: `$N77,{s2_enable},{s1_start},{s1_width},{s2_start},{s2_width}`
pub fn format_request_blind_sector() -> String {
    format_command(CommandMode::Request, CommandId::BlindSector, &[])
}

/// Blind sector (no-transmit zones) state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlindSectorState {
    /// Sector 1 start angle in degrees (0-359)
    pub sector1_start: i32,
    /// Sector 1 width in degrees (0 = disabled)
    pub sector1_width: i32,
    /// Sector 2 start angle in degrees (0-359)
    pub sector2_start: i32,
    /// Sector 2 width in degrees (0 = disabled)
    pub sector2_width: i32,
}

impl BlindSectorState {
    /// Check if sector 1 is enabled (width > 0)
    pub fn sector1_enabled(&self) -> bool {
        self.sector1_width > 0
    }

    /// Check if sector 2 is enabled (width > 0)
    pub fn sector2_enabled(&self) -> bool {
        self.sector2_width > 0
    }

    /// Calculate end angle from start + width
    pub fn sector1_end(&self) -> i32 {
        (self.sector1_start + self.sector1_width) % 360
    }

    /// Calculate end angle from start + width
    pub fn sector2_end(&self) -> i32 {
        (self.sector2_start + self.sector2_width) % 360
    }
}

/// Parse Blind Sector response
///
/// Response: `$N77,{s2_enable},{s1_start},{s1_width},{s2_start},{s2_width}`
/// - s2_enable: 0=sector 2 off, 1=sector 2 on
/// - s1_start: Sector 1 start angle (0-359)
/// - s1_width: Sector 1 width (0 = disabled)
/// - s2_start: Sector 2 start angle (0-359)
/// - s2_width: Sector 2 width
///
/// # Returns
/// BlindSectorState with all zone parameters
#[inline(never)]
pub fn parse_blind_sector_response(line: &str) -> Option<BlindSectorState> {
    let (mode, cmd_id, args) = parse_response(line)?;
    if mode != CommandMode::New || cmd_id != CommandId::BlindSector.as_hex() {
        return None;
    }
    if args.len() >= 5 {
        Some(BlindSectorState {
            sector1_start: args[1],
            sector1_width: args[2],
            sector2_start: args[3],
            sector2_width: args[4],
        })
    } else {
        None
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_status_transmit() {
        let cmd = format_status_command(true);
        assert_eq!(cmd, "$S69,2,0,0,60,300,0\r\n");
    }

    #[test]
    fn test_format_status_standby() {
        let cmd = format_status_command(false);
        assert_eq!(cmd, "$S69,1,0,0,60,300,0\r\n");
    }

    #[test]
    fn test_format_range() {
        // 2778m = 1.5nm -> wire index 5
        let cmd = format_range_command(2778);
        assert_eq!(cmd, "$S62,5,0,0\r\n");

        // 88896m = 48nm -> wire index 15
        let cmd = format_range_command(88896);
        assert_eq!(cmd, "$S62,15,0,0\r\n");

        // 66672m = 36nm -> wire index 19
        let cmd = format_range_command(66672);
        assert_eq!(cmd, "$S62,19,0,0\r\n");
    }

    #[test]
    fn test_format_gain_manual() {
        let cmd = format_gain_command(75, false);
        assert_eq!(cmd, "$S63,0,75,0,80,0\r\n");
    }

    #[test]
    fn test_format_gain_auto() {
        let cmd = format_gain_command(50, true);
        assert_eq!(cmd, "$S63,1,50,0,80,0\r\n");
    }

    #[test]
    fn test_format_keepalive() {
        let cmd = format_keepalive();
        assert_eq!(cmd, "$RE3\r\n");
    }

    #[test]
    fn test_parse_login_response() {
        // Simulated response with port offset 0x0001 = 1
        let response: [u8; 12] = [
            0x09, 0x01, 0x00, 0x0c, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, // Port offset = 1
            0x00, 0x00,
        ];
        let port = parse_login_response(&response);
        assert_eq!(port, Some(10001)); // BASE_PORT + 1
    }

    #[test]
    fn test_parse_response() {
        let (mode, cmd_id, args) = parse_response("$N69,2,0,0,60,300,0").unwrap();
        assert_eq!(mode, CommandMode::New);
        assert_eq!(cmd_id, 0x69);
        assert_eq!(args, vec![2, 0, 0, 60, 300, 0]);
    }

    #[test]
    fn test_parse_status_response() {
        assert_eq!(parse_status_response("$N69,2,0,0,60,300,0"), Some(true));
        assert_eq!(parse_status_response("$N69,1,0,0,60,300,0"), Some(false));
        assert_eq!(parse_status_response("$N62,5,0,0"), None); // Wrong command
    }

    #[test]
    fn test_format_sea_manual() {
        let cmd = format_sea_command(60, false);
        assert_eq!(cmd, "$S64,0,60,50,0,0,0\r\n");
    }

    #[test]
    fn test_format_sea_auto() {
        let cmd = format_sea_command(50, true);
        assert_eq!(cmd, "$S64,1,50,50,0,0,0\r\n");
    }

    #[test]
    fn test_format_rain_manual() {
        let cmd = format_rain_command(30, false);
        assert_eq!(cmd, "$S65,0,30,0,0,0,0\r\n");
    }

    #[test]
    fn test_format_rain_auto() {
        let cmd = format_rain_command(25, true);
        assert_eq!(cmd, "$S65,1,25,0,0,0,0\r\n");
    }

    #[test]
    fn test_format_blind_sector() {
        // Sector 1 only (200°-300° = width 100°)
        let cmd = format_blind_sector_command(false, 200, 100, 0, 0);
        assert_eq!(cmd, "$S77,0,200,100,0,0\r\n");

        // Both sectors
        let cmd = format_blind_sector_command(true, 200, 100, 320, 60);
        assert_eq!(cmd, "$S77,1,200,100,320,60\r\n");

        // Disable all
        let cmd = format_blind_sector_command(false, 0, 0, 0, 0);
        assert_eq!(cmd, "$S77,0,0,0,0,0\r\n");
    }

    #[test]
    fn test_format_scan_speed() {
        let cmd = format_scan_speed_command(0); // 24 RPM
        assert_eq!(cmd, "$S89,0,0\r\n");

        let cmd = format_scan_speed_command(2); // Auto
        assert_eq!(cmd, "$S89,2,0\r\n");
    }

    #[test]
    fn test_format_noise_reduction() {
        let cmd = format_noise_reduction_command(true);
        assert_eq!(cmd, "$S67,0,3,1,0\r\n");

        let cmd = format_noise_reduction_command(false);
        assert_eq!(cmd, "$S67,0,3,0,0\r\n");
    }

    #[test]
    fn test_format_interference_rejection() {
        let cmd = format_interference_rejection_command(true);
        assert_eq!(cmd, "$S67,0,0,2,0\r\n"); // Note: enabled=2, not 1

        let cmd = format_interference_rejection_command(false);
        assert_eq!(cmd, "$S67,0,0,0,0\r\n");
    }

    #[test]
    fn test_format_rezboost() {
        let cmd = format_rezboost_command(0, 0); // OFF, primary
        assert_eq!(cmd, "$SEE,0,0\r\n");

        let cmd = format_rezboost_command(3, 1); // High, secondary
        assert_eq!(cmd, "$SEE,3,1\r\n");
    }

    #[test]
    fn test_format_bird_mode() {
        let cmd = format_bird_mode_command(0, 0); // OFF
        assert_eq!(cmd, "$SED,0,0\r\n");

        let cmd = format_bird_mode_command(2, 0); // Medium
        assert_eq!(cmd, "$SED,2,0\r\n");
    }

    #[test]
    fn test_format_target_analyzer() {
        let cmd = format_target_analyzer_command(false, 0, 0); // OFF
        assert_eq!(cmd, "$SEF,0,0,0\r\n");

        let cmd = format_target_analyzer_command(true, 0, 0); // Target mode
        assert_eq!(cmd, "$SEF,1,0,0\r\n");

        let cmd = format_target_analyzer_command(true, 1, 0); // Rain mode
        assert_eq!(cmd, "$SEF,1,1,0\r\n");
    }

    #[test]
    fn test_format_tx_channel() {
        let cmd = format_tx_channel_command(0); // Auto
        assert_eq!(cmd, "$SEC,0\r\n");

        let cmd = format_tx_channel_command(2); // Channel 2
        assert_eq!(cmd, "$SEC,2\r\n");
    }

    #[test]
    fn test_format_auto_acquire() {
        let cmd = format_auto_acquire_command(true);
        assert_eq!(cmd, "$SF0,1\r\n");

        let cmd = format_auto_acquire_command(false);
        assert_eq!(cmd, "$SF0,0\r\n");
    }

    #[test]
    fn test_format_main_bang() {
        let cmd = format_main_bang_command(0); // 0%
        assert_eq!(cmd, "$S83,0,0\r\n");

        let cmd = format_main_bang_command(50); // 50% = 127
        assert_eq!(cmd, "$S83,127,0\r\n");

        let cmd = format_main_bang_command(100); // 100% = 255
        assert_eq!(cmd, "$S83,255,0\r\n");
    }

    #[test]
    fn test_format_heading_align() {
        let cmd = format_heading_align_command(0); // 0.0°
        assert_eq!(cmd, "$S81,0,0\r\n");

        let cmd = format_heading_align_command(1800); // 180.0°
        assert_eq!(cmd, "$S81,1800,0\r\n");
    }

    #[test]
    fn test_format_antenna_height() {
        let cmd = format_antenna_height_command(5);
        assert_eq!(cmd, "$S84,0,5,0\r\n");

        let cmd = format_antenna_height_command(15);
        assert_eq!(cmd, "$S84,0,15,0\r\n");
    }

    // Tests for new response parsers and request formatters

    #[test]
    fn test_parse_gain_response() {
        // Manual mode, value 50
        let result = parse_gain_response("$N63,0,50,0,80,0").unwrap();
        assert!(!result.auto);
        assert_eq!(result.value, 50);

        // Auto mode, value 75
        let result = parse_gain_response("$N63,1,75,0,80,0").unwrap();
        assert!(result.auto);
        assert_eq!(result.value, 75);

        // Wrong command
        assert!(parse_gain_response("$N64,0,50,0,0,0,0").is_none());
    }

    #[test]
    fn test_parse_sea_response() {
        // Manual mode, value 60
        let result = parse_sea_response("$N64,0,60,50,0,0,0").unwrap();
        assert!(!result.auto);
        assert_eq!(result.value, 60);

        // Auto mode
        let result = parse_sea_response("$N64,1,50,50,0,0,0").unwrap();
        assert!(result.auto);
        assert_eq!(result.value, 50);
    }

    #[test]
    fn test_parse_rain_response() {
        // Manual mode, value 30
        let result = parse_rain_response("$N65,0,30,0,0,0,0").unwrap();
        assert!(!result.auto);
        assert_eq!(result.value, 30);

        // Auto mode
        let result = parse_rain_response("$N65,1,25,0,0,0,0").unwrap();
        assert!(result.auto);
        assert_eq!(result.value, 25);
    }

    #[test]
    fn test_parse_range_response() {
        // Range index 5 (1.5nm)
        let result = parse_range_response("$N62,5,0,0").unwrap();
        assert_eq!(result, 5);

        // Range index 21 (1/16nm)
        let result = parse_range_response("$N62,21,0,0").unwrap();
        assert_eq!(result, 21);
    }

    #[test]
    fn test_format_request_commands() {
        assert_eq!(format_request_status(), "$R69\r\n");
        assert_eq!(format_request_gain(), "$R63\r\n");
        assert_eq!(format_request_sea(), "$R64\r\n");
        assert_eq!(format_request_rain(), "$R65\r\n");
        assert_eq!(format_request_range(), "$R62\r\n");
    }

    #[test]
    fn test_parse_signal_processing_response() {
        // Format 1 (SET echo): $N67,0,{feature},{value},{screen}
        // Noise reduction ON: $N67,0,3,1,0
        let result = parse_signal_processing_response("$N67,0,3,1,0");
        assert_eq!(result, Some((3, 1))); // feature=3, value=1

        // Noise reduction OFF: $N67,0,3,0,0
        let result = parse_signal_processing_response("$N67,0,3,0,0");
        assert_eq!(result, Some((3, 0))); // feature=3, value=0

        // Interference rejection ON: $N67,0,0,2,0
        let result = parse_signal_processing_response("$N67,0,0,2,0");
        assert_eq!(result, Some((0, 2))); // feature=0, value=2

        // Interference rejection OFF: $N67,0,0,0,0
        let result = parse_signal_processing_response("$N67,0,0,0,0");
        assert_eq!(result, Some((0, 0))); // feature=0, value=0

        // Format 2 (REQUEST response): $N67,{feature},{value},{screen}
        // Noise reduction ON: $N67,3,1,0
        let result = parse_signal_processing_response("$N67,3,1,0");
        assert_eq!(result, Some((3, 1))); // feature=3, value=1

        // Noise reduction OFF: $N67,3,0,0
        let result = parse_signal_processing_response("$N67,3,0,0");
        assert_eq!(result, Some((3, 0))); // feature=3, value=0

        // Interference rejection ON: $N67,0,2,0 (3 args = Format 2)
        // Now correctly parsed by arg count, not by first arg value
        let result = parse_signal_processing_response("$N67,0,2,0");
        assert_eq!(result, Some((0, 2))); // feature=0, value=2

        // Interference rejection OFF: $N67,0,0,0 (3 args = Format 2)
        let result = parse_signal_processing_response("$N67,0,0,0");
        assert_eq!(result, Some((0, 0))); // feature=0, value=0

        // Wrong command
        assert!(parse_signal_processing_response("$N66,0,3,1,0").is_none());
    }

    #[test]
    fn test_format_request_extended_commands() {
        assert_eq!(format_request_rezboost(), "$REE\r\n");
        assert_eq!(format_request_bird_mode(), "$RED\r\n");
        assert_eq!(format_request_target_analyzer(), "$REF\r\n");
        assert_eq!(format_request_scan_speed(), "$R89\r\n");
        assert_eq!(format_request_main_bang(), "$R83\r\n");
        assert_eq!(format_request_tx_channel(), "$REC\r\n");
    }

    #[test]
    fn test_parse_rezboost_response() {
        // OFF
        assert_eq!(parse_rezboost_response("$NEE,0,0"), Some(0));
        // High
        assert_eq!(parse_rezboost_response("$NEE,3,0"), Some(3));
        // Wrong command
        assert!(parse_rezboost_response("$NED,0,0").is_none());
    }

    #[test]
    fn test_parse_bird_mode_response() {
        // OFF
        assert_eq!(parse_bird_mode_response("$NED,0,0"), Some(0));
        // Medium
        assert_eq!(parse_bird_mode_response("$NED,2,0"), Some(2));
    }

    #[test]
    fn test_parse_target_analyzer_response() {
        // OFF
        let result = parse_target_analyzer_response("$NEF,0,0,0").unwrap();
        assert!(!result.enabled);
        assert_eq!(result.mode, 0);

        // ON, Target mode
        let result = parse_target_analyzer_response("$NEF,1,0,0").unwrap();
        assert!(result.enabled);
        assert_eq!(result.mode, 0);

        // ON, Rain mode
        let result = parse_target_analyzer_response("$NEF,1,1,0").unwrap();
        assert!(result.enabled);
        assert_eq!(result.mode, 1);
    }

    #[test]
    fn test_parse_scan_speed_response() {
        // 24 RPM
        assert_eq!(parse_scan_speed_response("$N89,0,0"), Some(0));
        // Auto
        assert_eq!(parse_scan_speed_response("$N89,2,0"), Some(2));
    }

    #[test]
    fn test_parse_main_bang_response() {
        // 0%
        assert_eq!(parse_main_bang_response("$N83,0,0"), Some(0));
        // ~50%
        assert_eq!(parse_main_bang_response("$N83,127,0"), Some(49)); // 127*100/255 = 49
                                                                      // 100%
        assert_eq!(parse_main_bang_response("$N83,255,0"), Some(100));
    }

    #[test]
    fn test_parse_tx_channel_response() {
        // Auto
        assert_eq!(parse_tx_channel_response("$NEC,0"), Some(0));
        // Channel 2
        assert_eq!(parse_tx_channel_response("$NEC,2"), Some(2));
    }
}
