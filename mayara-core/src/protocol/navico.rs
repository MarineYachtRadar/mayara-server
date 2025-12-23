//! Navico radar protocol parsing (BR24, 3G, 4G, HALO)
//!
//! This module contains pure parsing functions for Navico radar packets.
//! No I/O operations - just `&[u8]` → `Result<T>` functions.
//!
//! # Supported Models
//!
//! - **BR24**: Original Broadband Radar
//! - **3G**: Third generation
//! - **4G**: Fourth generation with dual range capability
//! - **HALO**: High-definition series with Doppler support

use super::c_string;
use crate::error::ParseError;
use crate::radar::RadarDiscovery;
use crate::Brand;
use serde::Deserialize;

// =============================================================================
// Constants
// =============================================================================

/// Number of spokes per revolution for Navico radars
pub const SPOKES_PER_REVOLUTION: u16 = 2048;

/// Maximum spoke length in pixels
pub const MAX_SPOKE_LEN: u16 = 1024;

/// Raw spoke count (internal protocol uses 4096, actual is 2048)
pub const SPOKES_RAW: u16 = 4096;

/// Number of spokes per UDP frame
pub const SPOKES_PER_FRAME: usize = 32;

/// Bits per pixel (Navico uses 4-bit pixels, packed 2 per byte)
pub const BITS_PER_PIXEL: usize = 4;

/// Bytes per spoke data line
pub const SPOKE_DATA_BYTES: usize = MAX_SPOKE_LEN as usize / 2; // 512 bytes

/// BR24 beacon multicast address
pub const BR24_BEACON_ADDR: &str = "236.6.7.4";
pub const BR24_BEACON_PORT: u16 = 6768;

/// Gen3/Gen4/HALO beacon multicast address
pub const GEN3_BEACON_ADDR: &str = "236.6.7.5";
pub const GEN3_BEACON_PORT: u16 = 6878;

/// Info multicast address (for heading/navigation data)
pub const INFO_ADDR: &str = "239.238.55.73";
pub const INFO_PORT: u16 = 7527;

/// Speed multicast address A
pub const SPEED_ADDR_A: &str = "236.6.7.20";
pub const SPEED_PORT_A: u16 = 6690;

/// Speed multicast address B
pub const SPEED_ADDR_B: &str = "236.6.7.15";
pub const SPEED_PORT_B: u16 = 6005;

// =============================================================================
// Packet Definitions
// =============================================================================

/// Address request packet - send to discover Navico radars
pub const ADDRESS_REQUEST_PACKET: [u8; 2] = [0x01, 0xB1];

/// Beacon response header (first 2 bytes)
pub const BEACON_RESPONSE_HEADER: [u8; 2] = [0x01, 0xB2];

/// Report request command: causes radar to send Report 3
pub const REQUEST_03_REPORT: [u8; 2] = [0x04, 0xc2];

/// Report request command: causes radar to send Reports 02, 03, 04, 07 and 08
pub const REQUEST_MANY2_REPORT: [u8; 2] = [0x01, 0xc2];

/// Report request command: causes radar to send Report 4
pub const REQUEST_04_REPORT: [u8; 2] = [0x02, 0xc2];

/// Report request command: causes radar to send Report 2 and 8
pub const REQUEST_02_08_REPORT: [u8; 2] = [0x03, 0xc2];

/// Command to keep radar A active
pub const COMMAND_STAY_ON_A: [u8; 2] = [0xa0, 0xc1];

// =============================================================================
// Radar Models
// =============================================================================

/// Known Navico radar models
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Model {
    #[default]
    Unknown,
    BR24,
    Gen3,
    Gen4,
    HALO,
}

impl Model {
    pub fn as_str(&self) -> &'static str {
        match self {
            Model::Unknown => "Unknown",
            Model::BR24 => "BR24",
            Model::Gen3 => "3G",
            Model::Gen4 => "4G",
            Model::HALO => "HALO",
        }
    }

    /// Parse model from model byte in Report 03
    pub fn from_byte(model: u8) -> Self {
        match model {
            0x0e | 0x0f => Model::BR24, // 0x0e seen on older BR24
            0x08 => Model::Gen3,
            0x01 => Model::Gen4,
            0x00 => Model::HALO,
            _ => Model::Unknown,
        }
    }

    /// Parse model from string
    pub fn from_name(s: &str) -> Self {
        match s {
            "BR24" => Model::BR24,
            "3G" => Model::Gen3,
            "4G" => Model::Gen4,
            "HALO" => Model::HALO,
            _ => Model::Unknown,
        }
    }

    /// Returns true if this model supports Doppler
    pub fn has_doppler(&self) -> bool {
        matches!(self, Model::HALO)
    }

    /// Returns true if this model supports dual range
    pub fn has_dual_range(&self) -> bool {
        matches!(self, Model::Gen4 | Model::HALO)
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// =============================================================================
// Network Address Parsing
// =============================================================================

/// Network socket address (big endian) as found in Navico packets
///
/// This is like a SocketAddrV4 but with known layout for parsing
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct NetworkSocketAddrV4 {
    pub addr: [u8; 4],
    pub port: [u8; 2],
}

impl NetworkSocketAddrV4 {
    /// Get IP address as [u8; 4]
    pub fn ip(&self) -> [u8; 4] {
        self.addr
    }

    /// Get IP address as string
    pub fn ip_string(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.addr[0], self.addr[1], self.addr[2], self.addr[3]
        )
    }

    /// Get port number
    pub fn port(&self) -> u16 {
        u16::from_be_bytes(self.port)
    }

    /// Get as "ip:port" string
    pub fn as_string(&self) -> String {
        format!("{}:{}", self.ip_string(), self.port())
    }
}

// =============================================================================
// Beacon Packet Structures
// =============================================================================

/// Common beacon header for all Navico radars
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct BeaconHeader {
    pub id: u16,
    pub serial_no: [u8; 16], // ASCII serial number, zero terminated
    pub radar_addr: NetworkSocketAddrV4, // DHCP address of radar
    _filler1: [u8; 12],
    _addr1: NetworkSocketAddrV4,
    _filler2: [u8; 4],
    _addr2: NetworkSocketAddrV4,
    _filler3: [u8; 10],
    _addr3: NetworkSocketAddrV4,
    _filler4: [u8; 4],
    _addr4: NetworkSocketAddrV4,
}

/// Radar endpoint addresses within a beacon
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct BeaconRadar {
    _filler1: [u8; 10],
    pub data: NetworkSocketAddrV4, // Spoke data multicast address
    _filler2: [u8; 4],
    pub send: NetworkSocketAddrV4, // Command send address
    _filler3: [u8; 4],
    pub report: NetworkSocketAddrV4, // Report multicast address
}

/// Single-range beacon (3G, Halo 20, etc.)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct BeaconSingle {
    pub header: BeaconHeader,
    pub a: BeaconRadar,
}

/// Dual-range beacon (4G, HALO 20+, 24, 3, etc.)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct BeaconDual {
    pub header: BeaconHeader,
    pub a: BeaconRadar,
    pub b: BeaconRadar,
}

/// BR24 beacon (slightly different format)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct BR24Beacon {
    pub id: u16,
    pub serial_no: [u8; 16],
    pub radar_addr: NetworkSocketAddrV4,
    _filler1: [u8; 12],
    _addr1: NetworkSocketAddrV4,
    _filler2: [u8; 4],
    _addr2: NetworkSocketAddrV4,
    _filler3: [u8; 4],
    _addr3: NetworkSocketAddrV4,
    _filler4: [u8; 10],
    pub report: NetworkSocketAddrV4,
    _filler5: [u8; 4],
    pub send: NetworkSocketAddrV4,
    _filler6: [u8; 4],
    pub data: NetworkSocketAddrV4, // Note: different order than newer radars
}

// Sizes
pub const BEACON_BR24_SIZE: usize = std::mem::size_of::<BR24Beacon>();
pub const BEACON_SINGLE_SIZE: usize = std::mem::size_of::<BeaconSingle>();
pub const BEACON_DUAL_SIZE: usize = std::mem::size_of::<BeaconDual>();

// =============================================================================
// Spoke Data Structures
// =============================================================================

/// Frame header (8 bytes before spoke data)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct FrameHeader {
    pub frame_hdr: [u8; 8],
}

pub const FRAME_HEADER_SIZE: usize = std::mem::size_of::<FrameHeader>();

/// BR24/3G spoke header (24 bytes)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Br24SpokeHeader {
    pub header_len: u8,
    pub status: u8,
    pub scan_number: [u8; 2],
    pub mark: [u8; 4], // On BR24: always 0x00, 0x44, 0x0d, 0x0e
    pub angle: [u8; 2],
    pub heading: [u8; 2], // With RI-10/11 interface
    pub range: [u8; 4],
    _u01: [u8; 2],
    _u02: [u8; 2],
    _u03: [u8; 4],
}

/// 4G/HALO spoke header (24 bytes)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Br4gSpokeHeader {
    pub header_len: u8,
    pub status: u8,
    pub scan_number: [u8; 2],
    pub mark: [u8; 2],
    pub large_range: [u8; 2], // 4G and up
    pub angle: [u8; 2],
    pub heading: [u8; 2],     // With RI-10/11 interface
    pub small_range: [u8; 2], // Or -1
    pub rotation: [u8; 2],    // Or -1
    _u01: [u8; 4],
    _u02: [u8; 4],
}

pub const SPOKE_HEADER_SIZE: usize = std::mem::size_of::<Br4gSpokeHeader>();

/// Full spoke line (header + data)
pub const SPOKE_LINE_SIZE: usize = SPOKE_HEADER_SIZE + SPOKE_DATA_BYTES;

// =============================================================================
// Report Structures
// =============================================================================

/// Report 01 - Radar status (0x01 0xC4, 18 bytes)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report01 {
    pub what: u8,    // 0x01
    pub command: u8, // 0xC4
    pub status: u8,
    _u00: [u8; 15],
}

pub const REPORT_01_SIZE: usize = 18;

/// Report 02 - Controls status (0x02 0xC4, 99 bytes)
/// Includes guard zone data at offsets 54-88 (per protocol.md)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report02 {
    pub what: u8,                   // 0x02
    pub command: u8,                // 0xC4
    pub range: [u8; 4],             // 2..6
    _u00: [u8; 1],                  // 6
    pub mode: u8,                   // 7
    pub gain_auto: u8,              // 8
    _u01: [u8; 3],                  // 9..12
    pub gain: u8,                   // 12
    pub sea_auto: u8,               // 13 = 0=off, 1=harbor, 2=offshore
    _u02: [u8; 3],                  // 14..17
    pub sea: [u8; 4],               // 17..21
    _u03: u8,                       // 21
    pub rain: u8,                   // 22
    _u04: [u8; 11],                 // 23..34
    pub interference_rejection: u8, // 34
    _u05: [u8; 3],                  // 35..38
    pub target_expansion: u8,       // 38
    _u06: [u8; 3],                  // 39..42
    pub target_boost: u8,           // 42
    _u07: [u8; 11],                 // 43..54 unknown
    // Guard zone fields (offsets 54-88)
    pub guard_zone_sensitivity: u8, // 54 - shared by both zones (0-255)
    pub guard_zone_1_enabled: u8,   // 55
    pub guard_zone_2_enabled: u8,   // 56
    _u08: [u8; 4],                  // 57..61 unknown (zeros)
    pub guard_zone_1_inner_range: u8, // 61 - meters
    _u09: [u8; 3],                  // 62..65 unknown (zeros)
    pub guard_zone_1_outer_range: u8, // 65 - meters
    _u10: [u8; 3],                  // 66..69 unknown (zeros)
    pub guard_zone_1_bearing: [u8; 2], // 69..71 - deci-degrees (u16 LE)
    pub guard_zone_1_width: [u8; 2], // 71..73 - deci-degrees (u16 LE)
    _u11: [u8; 4],                  // 73..77 unknown (zeros)
    pub guard_zone_2_inner_range: u8, // 77 - meters
    _u12: [u8; 3],                  // 78..81 unknown (zeros)
    pub guard_zone_2_outer_range: u8, // 81 - meters
    _u13: [u8; 3],                  // 82..85 unknown (zeros)
    pub guard_zone_2_bearing: [u8; 2], // 85..87 - deci-degrees (u16 LE)
    pub guard_zone_2_width: [u8; 2], // 87..89 - deci-degrees (u16 LE)
    _u14: [u8; 10],                 // 89..99 unknown
}

pub const REPORT_02_SIZE: usize = 99;

/// Report 03 - Model info (0x03 0xC4, 129 bytes)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report03 {
    pub what: u8,                  // 0x03
    pub command: u8,               // 0xC4
    pub model: u8,                 // Model byte (0x00=HALO, 0x01=4G, 0x08=3G, 0x0E/0x0F=BR24)
    _u00: [u8; 31],                // 3..34
    pub hours: [u8; 4],            // 34..38 Operating hours (total power-on time)
    _u01: [u8; 4],                 // 38..42 Unknown (always 0x01)
    pub transmit_seconds: [u8; 4], // 42..46 Transmit seconds (total TX time)
    _u02: [u8; 12],                // 46..58 Unknown
    pub firmware_date: [u8; 32],   // 58..90 Wide chars (UTF-16)
    pub firmware_time: [u8; 32],   // 90..122 Wide chars (UTF-16)
    _u03: [u8; 7],                 // 122..129 Unknown
}

pub const REPORT_03_SIZE: usize = 129;

/// Report 04 - Installation settings (0x04 0xC4, 66 bytes)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report04 {
    pub what: u8,                   // 0x04
    pub command: u8,                // 0xC4
    _u00: [u8; 4],                  // 2..6
    pub bearing_alignment: [u8; 2], // 6..8
    _u01: [u8; 2],                  // 8..10
    pub antenna_height: [u8; 2],    // 10..12
    _u02: [u8; 7],                  // 12..19
    pub accent_light: u8,           // 19 (HALO only)
    _u03a: [u8; 32],                // 20..52 (split for serde array limit)
    _u03b: [u8; 14],                // 52..66
}

pub const REPORT_04_SIZE: usize = 66;

/// Sector blanking entry
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct SectorBlanking {
    pub enabled: u8,
    pub start_angle: [u8; 2],
    pub end_angle: [u8; 2],
}

/// Report 06 - Blanking/name (0x06 0xC4, 68 bytes - HALO 2006)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report06_68 {
    pub what: u8,                      // 0x06
    pub command: u8,                   // 0xC4
    _u00: [u8; 4],                     // 2..6
    pub name: [u8; 6],                 // 6..12
    _u01: [u8; 24],                    // 12..36
    pub blanking: [SectorBlanking; 4], // 36..56
    _u02: [u8; 12],                    // 56..68
}

/// Report 06 - Blanking/name (0x06 0xC4, 74 bytes - HALO 24 2023+)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report06_74 {
    pub what: u8,                      // 0x06
    pub command: u8,                   // 0xC4
    _u00: [u8; 4],                     // 2..6
    pub name: [u8; 6],                 // 6..12
    _u01: [u8; 30],                    // 12..42
    pub blanking: [SectorBlanking; 4], // 42..62
    _u02: [u8; 12],                    // 62..74
}

/// Report 08 - Advanced settings base (0x08 0xC4, 18 bytes)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report08Base {
    pub what: u8,                   // 0x08
    pub command: u8,                // 0xC4
    pub sea_state: u8,              // 2
    pub interference_rejection: u8, // 3
    pub scan_speed: u8,             // 4
    pub sls_auto: u8,               // 5 sidelobe suppression auto
    _field6: u8,                    // 6
    _field7: u8,                    // 7
    _field8: u8,                    // 8
    pub side_lobe_suppression: u8,  // 9
    _field10: [u8; 2],              // 10-11
    pub noise_rejection: u8,        // 12
    pub target_sep: u8,             // 13
    pub sea_clutter: u8,            // 14 (HALO)
    pub auto_sea_clutter: i8,       // 15 (HALO)
    _field16: u8,                   // 16
    _field17: u8,                   // 17
}

pub const REPORT_08_BASE_SIZE: usize = 18;

/// Report 08 extended - with Doppler (21 bytes)
#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct Report08Extended {
    pub base: Report08Base,
    pub doppler_state: u8,
    pub doppler_speed: [u8; 2], // Speed threshold in cm/s (0..1594)
}

pub const REPORT_08_EXTENDED_SIZE: usize = 21;

// =============================================================================
// Navigation Packet Structures
// =============================================================================

/// HALO heading packet (72 bytes)
#[derive(Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct HaloHeadingPacket {
    pub marker: [u8; 4],   // 'NKOE'
    pub preamble: [u8; 4], // 00 01 90 02
    pub counter: [u8; 2],  // Big-endian counter
    _u01: [u8; 26],
    _u02: [u8; 4],    // 12 f1 01 00
    pub now: [u8; 8], // Millis since 1970
    _u03: [u8; 8],
    _u04: [u8; 4],
    _u05: [u8; 4],
    _u06: [u8; 1],
    pub heading: [u8; 2], // Heading in 0.1 degrees
    _u07: [u8; 5],
}

/// HALO navigation packet (72 bytes)
#[derive(Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct HaloNavigationPacket {
    pub marker: [u8; 4],   // 'NKOE'
    pub preamble: [u8; 4], // 00 01 90 02
    pub counter: [u8; 2],  // Big-endian counter
    _u01: [u8; 26],
    _u02: [u8; 4],    // 02 f8 01 00
    pub now: [u8; 8], // Millis since 1970
    _u03: [u8; 18],
    pub cog: [u8; 2], // COG in 0.01 radians (0..63488)
    pub sog: [u8; 2], // SOG in 0.01 m/s
    _u04: [u8; 2],
}

/// HALO speed packet (23 bytes)
#[derive(Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct HaloSpeedPacket {
    pub marker: [u8; 6], // 01 d3 01 00 00 00
    pub sog: [u8; 2],    // Speed m/s
    _u00: [u8; 6],
    pub cog: [u8; 2], // COG
    _u01: [u8; 7],
}

impl HaloHeadingPacket {
    /// Parse a heading packet from raw bytes
    pub fn transmute(bytes: &[u8]) -> Result<Self, &'static str> {
        const SIZE: usize = core::mem::size_of::<HaloHeadingPacket>();
        if bytes.len() < SIZE {
            return Err("Buffer too small for HaloHeadingPacket");
        }
        let arr: [u8; SIZE] = bytes[..SIZE].try_into().map_err(|_| "Conversion failed")?;
        Ok(unsafe { core::mem::transmute(arr) })
    }

    /// Get heading in degrees (0.0 to 360.0)
    pub fn heading_degrees(&self) -> f64 {
        i16::from_le_bytes(self.heading) as f64 * 0.1
    }
}

impl HaloNavigationPacket {
    /// Parse a navigation packet from raw bytes
    pub fn transmute(bytes: &[u8]) -> Result<Self, &'static str> {
        const SIZE: usize = core::mem::size_of::<HaloNavigationPacket>();
        if bytes.len() < SIZE {
            return Err("Buffer too small for HaloNavigationPacket");
        }
        let arr: [u8; SIZE] = bytes[..SIZE].try_into().map_err(|_| "Conversion failed")?;
        Ok(unsafe { core::mem::transmute(arr) })
    }

    /// Get SOG in knots
    pub fn sog_knots(&self) -> f64 {
        u16::from_le_bytes(self.sog) as f64 * 0.01 * MS_TO_KN
    }

    /// Get COG in degrees (0.0 to 360.0)
    pub fn cog_degrees(&self) -> f64 {
        u16::from_le_bytes(self.cog) as f64 * 360.0 / 63488.0
    }
}

impl HaloSpeedPacket {
    /// Parse a speed packet from raw bytes
    pub fn transmute(bytes: &[u8]) -> Result<Self, &'static str> {
        const SIZE: usize = core::mem::size_of::<HaloSpeedPacket>();
        if bytes.len() < SIZE {
            return Err("Buffer too small for HaloSpeedPacket");
        }
        let arr: [u8; SIZE] = bytes[..SIZE].try_into().map_err(|_| "Conversion failed")?;
        Ok(unsafe { core::mem::transmute(arr) })
    }
}

// Conversion constant for speed
const MS_TO_KN: f64 = 1.943844;

// =============================================================================
// Doppler Mode
// =============================================================================

/// Doppler mode for HALO radars
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DopplerMode {
    #[default]
    None, // Doppler disabled
    Both,        // Show approaching and receding targets
    Approaching, // Show only approaching targets
}

impl DopplerMode {
    pub fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(DopplerMode::None),
            1 => Some(DopplerMode::Both),
            2 => Some(DopplerMode::Approaching),
            _ => None,
        }
    }

    pub fn as_byte(&self) -> u8 {
        match self {
            DopplerMode::None => 0,
            DopplerMode::Both => 1,
            DopplerMode::Approaching => 2,
        }
    }
}

// =============================================================================
// Radar Status
// =============================================================================

/// Radar status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Off = 0,
    Standby = 1,
    Transmit = 2,
    Preparing = 5,
}

impl Status {
    pub fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Status::Off),
            1 => Some(Status::Standby),
            2 => Some(Status::Transmit),
            5 => Some(Status::Preparing),
            _ => None,
        }
    }
}

// =============================================================================
// Parsed Data Structures
// =============================================================================

/// Parsed beacon result containing radar endpoint information
#[derive(Debug, Clone)]
pub struct ParsedBeacon {
    pub serial_no: String,
    pub radar_addr: String,
    pub is_dual_range: bool,
    pub is_br24: bool, // True for BR24/old 3G beacons (different spoke format)
    pub radars: Vec<ParsedRadarEndpoints>,
}

/// Endpoints for a single radar (A or B)
#[derive(Debug, Clone)]
pub struct ParsedRadarEndpoints {
    pub suffix: Option<String>, // "A" or "B" for dual-range, None for single
    pub data_addr: String,
    pub send_addr: String,
    pub report_addr: String,
}

/// Parsed spoke data
#[derive(Debug, Clone)]
pub struct ParsedSpoke {
    pub angle: u16,           // 0..2047
    pub heading: Option<u16>, // True heading if available
    pub range_meters: u32,
    pub data: Vec<u8>, // Pixel data (1024 bytes, unpacked from nibbles)
}

/// Parsed guard zone from Report 02
#[derive(Debug, Clone, Default)]
pub struct ParsedGuardZone {
    pub enabled: bool,
    pub inner_range_m: u8,    // meters
    pub outer_range_m: u8,    // meters
    pub bearing_decideg: u16, // center angle in deci-degrees
    pub width_decideg: u16,   // width in deci-degrees (3599 = full circle)
}

/// Parsed Report 02 (controls)
#[derive(Debug, Clone)]
pub struct ParsedControls {
    pub range: i32,
    pub mode: u8,
    pub gain: u8,
    pub gain_auto: bool,
    pub sea: i32,
    pub sea_auto: u8,
    pub rain: u8,
    pub interference_rejection: u8,
    pub target_expansion: u8,
    pub target_boost: u8,
    // Guard zones (parsed from offsets 54-88)
    pub guard_zone_sensitivity: u8, // 0-255, shared by both zones
    pub guard_zone_1: ParsedGuardZone,
    pub guard_zone_2: ParsedGuardZone,
}

/// Parsed Report 03 (model info)
#[derive(Debug, Clone)]
pub struct ParsedModelInfo {
    pub model: Model,
    pub model_byte: u8,
    pub operating_hours: u32,
    pub transmit_hours: f64,
    pub firmware_date: String,
    pub firmware_time: String,
}

/// Parsed Report 04 (installation settings)
#[derive(Debug, Clone)]
pub struct ParsedInstallation {
    pub bearing_alignment: i16,
    pub antenna_height: u16,
    pub accent_light: u8,
}

/// Parsed sector blanking entry
#[derive(Debug, Clone)]
pub struct ParsedSectorBlanking {
    pub enabled: bool,
    pub start_angle: i16,
    pub end_angle: i16,
}

/// Parsed Report 06 (blanking/name settings)
#[derive(Debug, Clone)]
pub struct ParsedBlanking {
    pub name: Option<String>,
    pub sectors: Vec<ParsedSectorBlanking>,
}

/// Parsed Report 08 (advanced settings)
#[derive(Debug, Clone)]
pub struct ParsedAdvancedSettings {
    pub sea_state: u8,
    pub local_interference_rejection: u8,
    pub scan_speed: u8,
    pub sidelobe_suppression_auto: bool,
    pub sidelobe_suppression: u8,
    pub noise_rejection: u8,
    pub target_separation: u8,
    pub sea_clutter: u8,
    pub auto_sea_clutter: i8,
    pub doppler_state: Option<u8>,
    pub doppler_speed: Option<u16>,
}

// =============================================================================
// Heading Parsing Utilities
// =============================================================================

const HEADING_TRUE_FLAG: u16 = 0x4000;
const HEADING_MASK: u16 = SPOKES_RAW - 1;

/// Check if heading value indicates true heading
pub fn is_heading_true(x: u16) -> bool {
    (x & HEADING_TRUE_FLAG) != 0
}

/// Check if heading value is valid
pub fn is_valid_heading(x: u16) -> bool {
    (x & !(HEADING_TRUE_FLAG | HEADING_MASK)) == 0
}

/// Extract heading value (returns None if invalid or not true heading)
pub fn extract_heading(x: u16) -> Option<u16> {
    if is_valid_heading(x) && is_heading_true(x) {
        Some(x & HEADING_MASK)
    } else {
        None
    }
}

// =============================================================================
// Parsing Functions
// =============================================================================

/// Check if packet is a Navico beacon response
pub fn is_beacon_response(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == BEACON_RESPONSE_HEADER[0] && data[1] == BEACON_RESPONSE_HEADER[1]
}

/// Check if packet is an address request (not a beacon response to process)
pub fn is_address_request(data: &[u8]) -> bool {
    data == ADDRESS_REQUEST_PACKET
}

/// Parse a beacon response packet
///
/// Returns radar discovery information. Works with BR24, 3G, 4G, and HALO.
/// For dual-range radars (4G, HALO), returns two discoveries (A and B ranges).
pub fn parse_beacon_response(
    data: &[u8],
    source_addr: &str,
) -> Result<Vec<RadarDiscovery>, ParseError> {
    if data.len() < 2 {
        return Err(ParseError::TooShort {
            expected: 2,
            actual: data.len(),
        });
    }

    // Check header
    if !is_beacon_response(data) {
        return Err(ParseError::InvalidHeader {
            expected: BEACON_RESPONSE_HEADER.to_vec(),
            actual: data[0..2].to_vec(),
        });
    }

    if is_address_request(data) {
        return Err(ParseError::InvalidPacket(
            "Address request, not beacon response".into(),
        ));
    }

    // Try parsing in order of size (largest first)
    if data.len() >= BEACON_DUAL_SIZE {
        return parse_beacon_dual(data, source_addr);
    } else if data.len() >= BEACON_SINGLE_SIZE {
        return parse_beacon_single(data, source_addr);
    } else if data.len() >= BEACON_BR24_SIZE {
        return parse_beacon_br24(data, source_addr);
    }

    Err(ParseError::TooShort {
        expected: BEACON_BR24_SIZE,
        actual: data.len(),
    })
}

fn parse_beacon_dual(data: &[u8], source_addr: &str) -> Result<Vec<RadarDiscovery>, ParseError> {
    let beacon: BeaconDual = bincode::deserialize(data)?;

    let serial_no = c_string(&beacon.header.serial_no).ok_or(ParseError::InvalidString)?;

    // Dual-range radars have two independent radar endpoints (A and B)
    Ok(vec![
        RadarDiscovery {
            brand: Brand::Navico,
            model: None, // Model comes from Report 03
            name: serial_no.clone(),
            address: source_addr.to_string(),
            data_port: beacon.a.data.port(),
            command_port: beacon.a.send.port(),
            spokes_per_revolution: SPOKES_PER_REVOLUTION,
            max_spoke_len: MAX_SPOKE_LEN,
            pixel_values: 16, // 4-bit pixels
            serial_number: None,
            nic_address: None, // Set by locator
            suffix: Some("A".into()),
            data_address: Some(beacon.a.data.as_string()),
            report_address: Some(beacon.a.report.as_string()),
            send_address: Some(beacon.a.send.as_string()),
        },
        RadarDiscovery {
            brand: Brand::Navico,
            model: None,
            name: serial_no,
            address: source_addr.to_string(),
            data_port: beacon.b.data.port(),
            command_port: beacon.b.send.port(),
            spokes_per_revolution: SPOKES_PER_REVOLUTION,
            max_spoke_len: MAX_SPOKE_LEN,
            pixel_values: 16,
            serial_number: None,
            nic_address: None,
            suffix: Some("B".into()),
            data_address: Some(beacon.b.data.as_string()),
            report_address: Some(beacon.b.report.as_string()),
            send_address: Some(beacon.b.send.as_string()),
        },
    ])
}

fn parse_beacon_single(data: &[u8], source_addr: &str) -> Result<Vec<RadarDiscovery>, ParseError> {
    let beacon: BeaconSingle = bincode::deserialize(data)?;

    let serial_no = c_string(&beacon.header.serial_no).ok_or(ParseError::InvalidString)?;

    Ok(vec![RadarDiscovery {
        brand: Brand::Navico,
        model: None,
        name: serial_no,
        address: source_addr.to_string(),
        data_port: beacon.a.data.port(),
        command_port: beacon.a.send.port(),
        spokes_per_revolution: SPOKES_PER_REVOLUTION,
        max_spoke_len: MAX_SPOKE_LEN,
        pixel_values: 16,
        serial_number: None,
        nic_address: None, // Set by locator
        suffix: None,
        data_address: Some(beacon.a.data.as_string()),
        report_address: Some(beacon.a.report.as_string()),
        send_address: Some(beacon.a.send.as_string()),
    }])
}

fn parse_beacon_br24(data: &[u8], source_addr: &str) -> Result<Vec<RadarDiscovery>, ParseError> {
    let beacon: BR24Beacon = bincode::deserialize(data)?;

    let serial_no = c_string(&beacon.serial_no).ok_or(ParseError::InvalidString)?;

    Ok(vec![RadarDiscovery {
        brand: Brand::Navico,
        model: Some("BR24".to_string()),
        name: serial_no,
        address: source_addr.to_string(),
        data_port: beacon.data.port(),
        command_port: beacon.send.port(),
        spokes_per_revolution: SPOKES_PER_REVOLUTION,
        max_spoke_len: MAX_SPOKE_LEN,
        pixel_values: 16,
        serial_number: None,
        nic_address: None, // Set by locator
        suffix: None,
        data_address: Some(beacon.data.as_string()),
        report_address: Some(beacon.report.as_string()),
        send_address: Some(beacon.send.as_string()),
    }])
}

/// Parse beacon into detailed endpoint information
pub fn parse_beacon_endpoints(data: &[u8]) -> Result<ParsedBeacon, ParseError> {
    if data.len() < 2 || !is_beacon_response(data) {
        return Err(ParseError::InvalidHeader {
            expected: BEACON_RESPONSE_HEADER.to_vec(),
            actual: if data.len() >= 2 {
                data[0..2].to_vec()
            } else {
                data.to_vec()
            },
        });
    }

    if data.len() >= BEACON_DUAL_SIZE {
        let beacon: BeaconDual = bincode::deserialize(data)?;
        let serial_no = c_string(&beacon.header.serial_no).ok_or(ParseError::InvalidString)?;

        Ok(ParsedBeacon {
            serial_no,
            radar_addr: beacon.header.radar_addr.as_string(),
            is_dual_range: true,
            is_br24: false,
            radars: vec![
                ParsedRadarEndpoints {
                    suffix: Some("A".into()),
                    data_addr: beacon.a.data.as_string(),
                    send_addr: beacon.a.send.as_string(),
                    report_addr: beacon.a.report.as_string(),
                },
                ParsedRadarEndpoints {
                    suffix: Some("B".into()),
                    data_addr: beacon.b.data.as_string(),
                    send_addr: beacon.b.send.as_string(),
                    report_addr: beacon.b.report.as_string(),
                },
            ],
        })
    } else if data.len() >= BEACON_SINGLE_SIZE {
        let beacon: BeaconSingle = bincode::deserialize(data)?;
        let serial_no = c_string(&beacon.header.serial_no).ok_or(ParseError::InvalidString)?;

        Ok(ParsedBeacon {
            serial_no,
            radar_addr: beacon.header.radar_addr.as_string(),
            is_dual_range: false,
            is_br24: false,
            radars: vec![ParsedRadarEndpoints {
                suffix: None,
                data_addr: beacon.a.data.as_string(),
                send_addr: beacon.a.send.as_string(),
                report_addr: beacon.a.report.as_string(),
            }],
        })
    } else if data.len() >= BEACON_BR24_SIZE {
        let beacon: BR24Beacon = bincode::deserialize(data)?;
        let serial_no = c_string(&beacon.serial_no).ok_or(ParseError::InvalidString)?;

        Ok(ParsedBeacon {
            serial_no,
            radar_addr: beacon.radar_addr.as_string(),
            is_dual_range: false,
            is_br24: true,
            radars: vec![ParsedRadarEndpoints {
                suffix: None,
                data_addr: beacon.data.as_string(),
                send_addr: beacon.send.as_string(),
                report_addr: beacon.report.as_string(),
            }],
        })
    } else {
        Err(ParseError::TooShort {
            expected: BEACON_BR24_SIZE,
            actual: data.len(),
        })
    }
}

/// Parse Report 01 (status)
pub fn parse_report_01(data: &[u8]) -> Result<Status, ParseError> {
    if data.len() < REPORT_01_SIZE {
        return Err(ParseError::TooShort {
            expected: REPORT_01_SIZE,
            actual: data.len(),
        });
    }

    let report: Report01 = bincode::deserialize(&data[..REPORT_01_SIZE])?;

    if report.what != 0x01 || report.command != 0xC4 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x01, 0xC4],
            actual: vec![report.what, report.command],
        });
    }

    Status::from_byte(report.status).ok_or(ParseError::InvalidPacket(format!(
        "Unknown status: {}",
        report.status
    )))
}

/// Parse Report 02 (controls)
pub fn parse_report_02(data: &[u8]) -> Result<ParsedControls, ParseError> {
    if data.len() < REPORT_02_SIZE {
        return Err(ParseError::TooShort {
            expected: REPORT_02_SIZE,
            actual: data.len(),
        });
    }

    let report: Report02 = bincode::deserialize(&data[..REPORT_02_SIZE])?;

    if report.what != 0x02 || report.command != 0xC4 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x02, 0xC4],
            actual: vec![report.what, report.command],
        });
    }

    Ok(ParsedControls {
        range: i32::from_le_bytes(report.range),
        mode: report.mode,
        gain: report.gain,
        gain_auto: report.gain_auto > 0,
        sea: i32::from_le_bytes(report.sea),
        sea_auto: report.sea_auto,
        rain: report.rain,
        interference_rejection: report.interference_rejection,
        target_expansion: report.target_expansion,
        target_boost: report.target_boost,
        // Guard zones
        guard_zone_sensitivity: report.guard_zone_sensitivity,
        guard_zone_1: ParsedGuardZone {
            enabled: report.guard_zone_1_enabled > 0,
            inner_range_m: report.guard_zone_1_inner_range,
            outer_range_m: report.guard_zone_1_outer_range,
            bearing_decideg: u16::from_le_bytes(report.guard_zone_1_bearing),
            width_decideg: u16::from_le_bytes(report.guard_zone_1_width),
        },
        guard_zone_2: ParsedGuardZone {
            enabled: report.guard_zone_2_enabled > 0,
            inner_range_m: report.guard_zone_2_inner_range,
            outer_range_m: report.guard_zone_2_outer_range,
            bearing_decideg: u16::from_le_bytes(report.guard_zone_2_bearing),
            width_decideg: u16::from_le_bytes(report.guard_zone_2_width),
        },
    })
}

/// Parse Report 03 (model info)
pub fn parse_report_03(data: &[u8]) -> Result<ParsedModelInfo, ParseError> {
    if data.len() < REPORT_03_SIZE {
        return Err(ParseError::TooShort {
            expected: REPORT_03_SIZE,
            actual: data.len(),
        });
    }

    let report: Report03 = bincode::deserialize(&data[..REPORT_03_SIZE])?;

    if report.what != 0x03 || report.command != 0xC4 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x03, 0xC4],
            actual: vec![report.what, report.command],
        });
    }

    // Parse wide string for firmware date/time
    let firmware_date = wide_string_to_string(&report.firmware_date);
    let firmware_time = wide_string_to_string(&report.firmware_time);

    // Transmit time is in seconds, convert to hours
    let transmit_seconds = u32::from_le_bytes(report.transmit_seconds);
    let transmit_hours = transmit_seconds as f64 / 3600.0;

    Ok(ParsedModelInfo {
        model: Model::from_byte(report.model),
        model_byte: report.model,
        operating_hours: u32::from_le_bytes(report.hours),
        transmit_hours,
        firmware_date,
        firmware_time,
    })
}

/// Parse Report 04 (installation settings)
pub fn parse_report_04(data: &[u8]) -> Result<ParsedInstallation, ParseError> {
    if data.len() < REPORT_04_SIZE {
        return Err(ParseError::TooShort {
            expected: REPORT_04_SIZE,
            actual: data.len(),
        });
    }

    let report: Report04 = bincode::deserialize(&data[..REPORT_04_SIZE])?;

    if report.what != 0x04 || report.command != 0xC4 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x04, 0xC4],
            actual: vec![report.what, report.command],
        });
    }

    Ok(ParsedInstallation {
        bearing_alignment: i16::from_le_bytes(report.bearing_alignment),
        antenna_height: u16::from_le_bytes(report.antenna_height),
        accent_light: report.accent_light,
    })
}

/// Parse Report 06 (blanking/name settings) - 68 byte variant (HALO 2006)
pub fn parse_report_06_68(data: &[u8]) -> Result<ParsedBlanking, ParseError> {
    const REPORT_06_68_SIZE: usize = 68;
    if data.len() < REPORT_06_68_SIZE {
        return Err(ParseError::TooShort {
            expected: REPORT_06_68_SIZE,
            actual: data.len(),
        });
    }

    let report: Report06_68 = bincode::deserialize(&data[..REPORT_06_68_SIZE])?;

    if report.what != 0x06 || report.command != 0xC4 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x06, 0xC4],
            actual: vec![report.what, report.command],
        });
    }

    let name = c_string(&report.name);
    let sectors = report
        .blanking
        .iter()
        .map(|b| ParsedSectorBlanking {
            enabled: b.enabled > 0,
            start_angle: i16::from_le_bytes(b.start_angle),
            end_angle: i16::from_le_bytes(b.end_angle),
        })
        .collect();

    Ok(ParsedBlanking { name, sectors })
}

/// Parse Report 06 (blanking/name settings) - 74 byte variant (HALO 24 2023+)
pub fn parse_report_06_74(data: &[u8]) -> Result<ParsedBlanking, ParseError> {
    const REPORT_06_74_SIZE: usize = 74;
    if data.len() < REPORT_06_74_SIZE {
        return Err(ParseError::TooShort {
            expected: REPORT_06_74_SIZE,
            actual: data.len(),
        });
    }

    let report: Report06_74 = bincode::deserialize(&data[..REPORT_06_74_SIZE])?;

    if report.what != 0x06 || report.command != 0xC4 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x06, 0xC4],
            actual: vec![report.what, report.command],
        });
    }

    let name = c_string(&report.name);
    let sectors = report
        .blanking
        .iter()
        .map(|b| ParsedSectorBlanking {
            enabled: b.enabled > 0,
            start_angle: i16::from_le_bytes(b.start_angle),
            end_angle: i16::from_le_bytes(b.end_angle),
        })
        .collect();

    Ok(ParsedBlanking { name, sectors })
}

/// Parse Report 08 (advanced settings)
///
/// Handles both 18-byte base version and 21-byte extended version with Doppler.
pub fn parse_report_08(data: &[u8]) -> Result<ParsedAdvancedSettings, ParseError> {
    if data.len() < REPORT_08_BASE_SIZE {
        return Err(ParseError::TooShort {
            expected: REPORT_08_BASE_SIZE,
            actual: data.len(),
        });
    }

    let report: Report08Base = bincode::deserialize(&data[..REPORT_08_BASE_SIZE])?;

    if report.what != 0x08 || report.command != 0xC4 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x08, 0xC4],
            actual: vec![report.what, report.command],
        });
    }

    // Check if we have the extended version with Doppler data
    let (doppler_state, doppler_speed) = if data.len() >= REPORT_08_EXTENDED_SIZE {
        let extended: Report08Extended = bincode::deserialize(&data[..REPORT_08_EXTENDED_SIZE])?;
        (
            Some(extended.doppler_state),
            Some(u16::from_le_bytes(extended.doppler_speed)),
        )
    } else {
        (None, None)
    };

    Ok(ParsedAdvancedSettings {
        sea_state: report.sea_state,
        local_interference_rejection: report.interference_rejection,
        scan_speed: report.scan_speed,
        sidelobe_suppression_auto: report.sls_auto > 0,
        sidelobe_suppression: report.side_lobe_suppression,
        noise_rejection: report.noise_rejection,
        target_separation: report.target_sep,
        sea_clutter: report.sea_clutter,
        auto_sea_clutter: report.auto_sea_clutter,
        doppler_state,
        doppler_speed,
    })
}

/// Parse spoke header (4G/HALO)
///
/// Range calculation uses: (large_range * small_range) / 512
/// This formula works for both 3G/4G and HALO models.
pub fn parse_4g_spoke_header(data: &[u8]) -> Result<(u32, u16, Option<u16>), ParseError> {
    if data.len() < SPOKE_HEADER_SIZE {
        return Err(ParseError::TooShort {
            expected: SPOKE_HEADER_SIZE,
            actual: data.len(),
        });
    }

    let header: Br4gSpokeHeader = bincode::deserialize(&data[..SPOKE_HEADER_SIZE])?;

    if header.header_len != SPOKE_HEADER_SIZE as u8 {
        return Err(ParseError::InvalidPacket(format!(
            "Invalid spoke header length: {} (expected {})",
            header.header_len, SPOKE_HEADER_SIZE
        )));
    }

    // Status must be 0x02 or 0x12
    if header.status != 0x02 && header.status != 0x12 {
        return Err(ParseError::InvalidPacket(format!(
            "Invalid spoke status: 0x{:02x}",
            header.status
        )));
    }

    let heading = u16::from_le_bytes(header.heading);
    let angle = u16::from_le_bytes(header.angle) / 2; // Convert from 4096 to 2048
    let large_range = u16::from_le_bytes(header.large_range);
    let small_range = u16::from_le_bytes(header.small_range);

    // Calculate range in meters
    let range = if large_range == 0x80 {
        // Short range mode (4G uses this for all ranges)
        if small_range == 0xffff {
            0
        } else {
            (small_range as u32) / 4
        }
    } else {
        // Standard range calculation for HALO
        ((large_range as u32) * (small_range as u32)) / 512
    };

    let heading = extract_heading(heading);

    Ok((range, angle, heading))
}

/// Parse spoke header (BR24)
pub fn parse_br24_spoke_header(data: &[u8]) -> Result<(u32, u16, Option<u16>), ParseError> {
    if data.len() < SPOKE_HEADER_SIZE {
        return Err(ParseError::TooShort {
            expected: SPOKE_HEADER_SIZE,
            actual: data.len(),
        });
    }

    let header: Br24SpokeHeader = bincode::deserialize(&data[..SPOKE_HEADER_SIZE])?;

    if header.header_len != SPOKE_HEADER_SIZE as u8 {
        return Err(ParseError::InvalidPacket(format!(
            "Invalid spoke header length: {}",
            header.header_len
        )));
    }

    if header.status != 0x02 && header.status != 0x12 {
        return Err(ParseError::InvalidPacket(format!(
            "Invalid spoke status: 0x{:02x}",
            header.status
        )));
    }

    let heading = u16::from_le_bytes(header.heading);
    let angle = u16::from_le_bytes(header.angle) / 2;

    // BR24 range calculation
    const BR24_RANGE_FACTOR: f64 = 10.0 / 1.414; // 10 m / sqrt(2)
    let raw_range = u32::from_le_bytes(header.range) & 0xffffff;
    let range = (raw_range as f64 * BR24_RANGE_FACTOR) as u32;

    let heading = extract_heading(heading);

    Ok((range, angle, heading))
}

/// Unpack spoke data from nibbles (4-bit pixels) to bytes
///
/// Input: 512 bytes (2 pixels per byte)
/// Output: 1024 bytes (1 pixel per byte, values 0..15)
pub fn unpack_spoke_data(packed: &[u8]) -> Vec<u8> {
    let mut unpacked = Vec::with_capacity(packed.len() * 2);

    for byte in packed {
        let low = byte & 0x0f;
        let high = (byte >> 4) & 0x0f;
        unpacked.push(low);
        unpacked.push(high);
    }

    unpacked
}

/// Unpack spoke data with Doppler interpretation
///
/// For HALO radars with Doppler enabled:
/// - 0x0F = approaching target (becomes `approaching_value`)
/// - 0x0E = receding target (becomes `receding_value`)
pub fn unpack_spoke_data_doppler(
    packed: &[u8],
    doppler_mode: DopplerMode,
    approaching_value: u8,
    receding_value: u8,
) -> Vec<u8> {
    let mut unpacked = Vec::with_capacity(packed.len() * 2);

    for byte in packed {
        let low = byte & 0x0f;
        let high = (byte >> 4) & 0x0f;

        let low_out = match doppler_mode {
            DopplerMode::None => low,
            DopplerMode::Both => match low {
                0x0f => approaching_value,
                0x0e => receding_value,
                _ => low,
            },
            DopplerMode::Approaching => match low {
                0x0f => approaching_value,
                _ => low,
            },
        };

        let high_out = match doppler_mode {
            DopplerMode::None => high,
            DopplerMode::Both => match high {
                0x0f => approaching_value,
                0x0e => receding_value,
                _ => high,
            },
            DopplerMode::Approaching => match high {
                0x0f => approaching_value,
                _ => high,
            },
        };

        unpacked.push(low_out);
        unpacked.push(high_out);
    }

    unpacked
}

// =============================================================================
// Command Generation
// =============================================================================

/// Create address request packet
pub fn create_address_request() -> &'static [u8] {
    &ADDRESS_REQUEST_PACKET
}

/// Generate status command (transmit/standby)
pub fn create_status_command(transmit: bool) -> Vec<u8> {
    let value = if transmit { 1u8 } else { 0u8 };
    vec![0x00, 0xc1, 0x01, 0x01, 0xc1, value]
}

/// Generate range command (range in decimeters)
pub fn create_range_command(decimeters: i32) -> Vec<u8> {
    let mut cmd = vec![0x03, 0xc1];
    cmd.extend_from_slice(&decimeters.to_le_bytes());
    cmd
}

/// Generate gain command
pub fn create_gain_command(value: u8, auto: bool) -> Vec<u8> {
    let auto = if auto { 1u32 } else { 0u32 };
    let mut cmd = vec![0x06, 0xc1, 0x00, 0x00, 0x00, 0x00];
    cmd.extend_from_slice(&auto.to_le_bytes());
    cmd.push(value);
    cmd
}

/// Generate rain clutter command
pub fn create_rain_command(value: u8) -> Vec<u8> {
    vec![0x06, 0xc1, 0x04, 0, 0, 0, 0, 0, 0, 0, value]
}

/// Generate interference rejection command
pub fn create_interference_rejection_command(level: u8) -> Vec<u8> {
    vec![0x08, 0xc1, level]
}

/// Generate scan speed command
pub fn create_scan_speed_command(speed: u8) -> Vec<u8> {
    vec![0x0f, 0xc1, speed]
}

/// Generate doppler command (HALO only)
pub fn create_doppler_command(mode: DopplerMode) -> Vec<u8> {
    vec![0x23, 0xc1, mode.as_byte()]
}

// =============================================================================
// Navigation Data Packet Formatting (send heading/SOG/COG to Navico radars)
// =============================================================================

/// Size of heading packet
pub const HEADING_PACKET_SIZE: usize = std::mem::size_of::<HaloHeadingPacket>();

/// Size of navigation packet
pub const NAVIGATION_PACKET_SIZE: usize = std::mem::size_of::<HaloNavigationPacket>();

/// Size of speed packet
pub const SPEED_PACKET_SIZE: usize = std::mem::size_of::<HaloSpeedPacket>();

/// Format a heading packet for Navico HALO radars
///
/// # Arguments
/// * `heading_deg` - Heading in degrees (0.0..360.0)
/// * `counter` - Packet counter (increments each transmission)
/// * `timestamp_ms` - Timestamp in milliseconds since Unix epoch
///
/// # Returns
/// 72-byte packet ready to send to INFO_ADDR:INFO_PORT
pub fn format_heading_packet(heading_deg: f64, counter: u16, timestamp_ms: i64) -> [u8; 72] {
    let heading = (heading_deg * 10.0) as i16;
    let now = timestamp_ms.to_le_bytes();

    let packet = HaloHeadingPacket {
        marker: [b'N', b'K', b'O', b'E'],
        preamble: [0, 1, 0x90, 0x02],
        counter: counter.to_be_bytes(),
        _u01: [0; 26],
        _u02: [0x12, 0xf1, 0x01, 0x00],
        now,
        _u03: [0, 0, 0, 2, 0, 0, 0, 0],
        _u04: [0; 4],
        _u05: [0; 4],
        _u06: [0xff],
        heading: heading.to_le_bytes(),
        _u07: [0; 5],
    };

    // Safe: struct is repr(C, packed) with known size
    unsafe { std::mem::transmute(packet) }
}

/// Format a navigation packet for Navico HALO radars (COG/SOG)
///
/// # Arguments
/// * `sog_ms` - Speed over ground in m/s
/// * `cog_deg` - Course over ground in degrees (0.0..360.0)
/// * `counter` - Packet counter (increments each transmission)
/// * `timestamp_ms` - Timestamp in milliseconds since Unix epoch
///
/// # Returns
/// 72-byte packet ready to send to INFO_ADDR:INFO_PORT
pub fn format_navigation_packet(
    sog_ms: f64,
    cog_deg: f64,
    counter: u16,
    timestamp_ms: i64,
) -> [u8; 72] {
    let sog = (sog_ms * 10.0) as i16; // 0.01 m/s units
    let cog = (cog_deg * (63488.0 / 360.0)) as i16; // 0.01 radians
    let now = timestamp_ms.to_le_bytes();

    let packet = HaloNavigationPacket {
        marker: [b'N', b'K', b'O', b'E'],
        preamble: [0, 1, 0x90, 0x02],
        counter: counter.to_be_bytes(),
        _u01: [0; 26],
        _u02: [0x02, 0xf8, 0x01, 0x00],
        now,
        _u03: [0; 18],
        cog: cog.to_le_bytes(),
        sog: sog.to_le_bytes(),
        _u04: [0xff, 0xff],
    };

    // Safe: struct is repr(C, packed) with known size
    unsafe { std::mem::transmute(packet) }
}

/// Format a speed packet for Navico HALO radars
///
/// # Arguments
/// * `sog_ms` - Speed over ground in m/s
/// * `cog_deg` - Course over ground in degrees (0.0..360.0)
///
/// # Returns
/// 23-byte packet ready to send to SPEED_ADDR_A:SPEED_PORT_A and SPEED_ADDR_B:SPEED_PORT_B
pub fn format_speed_packet(sog_ms: f64, cog_deg: f64) -> [u8; 23] {
    let sog = (sog_ms * 10.0) as u16;
    let cog = (cog_deg * 63488.0 / 360.0) as u16;

    let packet = HaloSpeedPacket {
        marker: [0x01, 0xd3, 0x01, 0x00, 0x00, 0x00],
        sog: sog.to_le_bytes(),
        _u00: [0x00, 0x00, 0x01, 0x00, 0x00, 0x00],
        cog: cog.to_le_bytes(),
        _u01: [0x00, 0x00, 0x01, 0x33, 0x00, 0x00, 0x00],
    };

    // Safe: struct is repr(C, packed) with known size
    unsafe { std::mem::transmute(packet) }
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Parse wide string (UTF-16LE) to UTF-8
fn wide_string_to_string(data: &[u8]) -> String {
    let u16_iter = data
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|&c| c != 0);

    String::from_utf16_lossy(&u16_iter.collect::<Vec<u16>>())
}

/// Check if this is a report packet (second byte is 0xC4 or 0xC6)
pub fn is_report(data: &[u8]) -> bool {
    data.len() >= 2 && (data[1] == 0xC4 || data[1] == 0xC6)
}

/// Get report type from packet
pub fn get_report_type(data: &[u8]) -> Option<u8> {
    if is_report(data) {
        Some(data[0])
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
    fn test_model_from_byte() {
        assert_eq!(Model::from_byte(0x00), Model::HALO);
        assert_eq!(Model::from_byte(0x01), Model::Gen4);
        assert_eq!(Model::from_byte(0x08), Model::Gen3);
        assert_eq!(Model::from_byte(0x0e), Model::BR24);
        assert_eq!(Model::from_byte(0x0f), Model::BR24);
        assert_eq!(Model::from_byte(0xFF), Model::Unknown);
    }

    #[test]
    fn test_model_has_doppler() {
        assert!(Model::HALO.has_doppler());
        assert!(!Model::Gen4.has_doppler());
        assert!(!Model::Gen3.has_doppler());
        assert!(!Model::BR24.has_doppler());
    }

    #[test]
    fn test_heading_extraction() {
        // True heading 1000 = 0x4000 | 1000 = 0x43E8
        assert_eq!(extract_heading(0x43E8), Some(1000));

        // Invalid (no true flag)
        assert_eq!(extract_heading(0x03E8), None);

        // Invalid (bad upper bits)
        assert_eq!(extract_heading(0x83E8), None);
    }

    #[test]
    fn test_unpack_spoke_data() {
        let packed = vec![0x12, 0x34, 0xAB];
        let unpacked = unpack_spoke_data(&packed);

        // 0x12 -> low=2, high=1
        // 0x34 -> low=4, high=3
        // 0xAB -> low=11, high=10
        assert_eq!(unpacked, vec![2, 1, 4, 3, 11, 10]);
    }

    #[test]
    fn test_unpack_spoke_data_doppler() {
        let packed = vec![0xEF, 0x12]; // low=F, high=E, then low=2, high=1

        // With Doppler::Both, 0xF->20 (approaching), 0xE->21 (receding)
        let unpacked = unpack_spoke_data_doppler(&packed, DopplerMode::Both, 20, 21);
        assert_eq!(unpacked, vec![20, 21, 2, 1]);

        // With Doppler::Approaching, only 0xF->20
        let unpacked = unpack_spoke_data_doppler(&packed, DopplerMode::Approaching, 20, 21);
        assert_eq!(unpacked, vec![20, 14, 2, 1]); // 0xE stays as 14

        // With Doppler::None, no conversion
        let unpacked = unpack_spoke_data_doppler(&packed, DopplerMode::None, 20, 21);
        assert_eq!(unpacked, vec![15, 14, 2, 1]);
    }

    #[test]
    fn test_is_beacon_response() {
        assert!(is_beacon_response(&[0x01, 0xB2, 0x00]));
        assert!(!is_beacon_response(&[0x01, 0xB1])); // Address request
        assert!(!is_beacon_response(&[0x00]));
    }

    #[test]
    fn test_doppler_mode() {
        assert_eq!(DopplerMode::from_byte(0), Some(DopplerMode::None));
        assert_eq!(DopplerMode::from_byte(1), Some(DopplerMode::Both));
        assert_eq!(DopplerMode::from_byte(2), Some(DopplerMode::Approaching));
        assert_eq!(DopplerMode::from_byte(3), None);

        assert_eq!(DopplerMode::None.as_byte(), 0);
        assert_eq!(DopplerMode::Both.as_byte(), 1);
        assert_eq!(DopplerMode::Approaching.as_byte(), 2);
    }

    #[test]
    fn test_status_from_byte() {
        assert_eq!(Status::from_byte(0), Some(Status::Off));
        assert_eq!(Status::from_byte(1), Some(Status::Standby));
        assert_eq!(Status::from_byte(2), Some(Status::Transmit));
        assert_eq!(Status::from_byte(5), Some(Status::Preparing));
        assert_eq!(Status::from_byte(3), None);
    }

    #[test]
    fn test_create_commands() {
        let status_cmd = create_status_command(true);
        assert_eq!(status_cmd[5], 1);

        let status_cmd = create_status_command(false);
        assert_eq!(status_cmd[5], 0);

        let range_cmd = create_range_command(10000);
        assert_eq!(&range_cmd[0..2], &[0x03, 0xc1]);

        let doppler_cmd = create_doppler_command(DopplerMode::Both);
        assert_eq!(doppler_cmd, vec![0x23, 0xc1, 1]);
    }

    #[test]
    fn test_beacon_sizes() {
        // Verify struct sizes match expected packet sizes
        assert!(BEACON_BR24_SIZE > 0);
        assert!(BEACON_SINGLE_SIZE > BEACON_BR24_SIZE);
        assert!(BEACON_DUAL_SIZE > BEACON_SINGLE_SIZE);
    }

    #[test]
    fn test_format_heading_packet() {
        let packet = format_heading_packet(90.0, 1, 1234567890000);
        assert_eq!(packet.len(), 72);
        assert_eq!(&packet[0..4], b"NKOE");
        // Heading 90.0 * 10 = 900 = 0x0384
        // heading field is at offset 65-66 (after marker[4], preamble[4], counter[2], _u01[26], _u02[4], now[8], _u03[8], _u04[4], _u05[4], _u06[1])
        assert_eq!(&packet[65..67], &900i16.to_le_bytes());
    }

    #[test]
    fn test_format_navigation_packet() {
        let packet = format_navigation_packet(5.0, 180.0, 2, 1234567890000);
        assert_eq!(packet.len(), 72);
        assert_eq!(&packet[0..4], b"NKOE");
    }

    #[test]
    fn test_format_speed_packet() {
        let packet = format_speed_packet(10.0, 45.0);
        assert_eq!(packet.len(), 23);
        assert_eq!(&packet[0..6], &[0x01, 0xd3, 0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_packet_sizes() {
        assert_eq!(HEADING_PACKET_SIZE, 72);
        assert_eq!(NAVIGATION_PACKET_SIZE, 72);
        assert_eq!(SPEED_PACKET_SIZE, 23);
    }

    #[test]
    fn test_parse_report_04() {
        // Report 04 packet: 0x04 0xC4 + data
        let mut data = vec![0x04, 0xC4];
        data.extend_from_slice(&[0; 4]); // _u00
        data.extend_from_slice(&(-50i16).to_le_bytes()); // bearing_alignment = -50
        data.extend_from_slice(&[0; 2]); // _u01
        data.extend_from_slice(&100u16.to_le_bytes()); // antenna_height = 100
        data.extend_from_slice(&[0; 7]); // _u02
        data.push(3); // accent_light = 3
        data.extend_from_slice(&[0; 46]); // _u03

        let result = parse_report_04(&data);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.bearing_alignment, -50);
        assert_eq!(parsed.antenna_height, 100);
        assert_eq!(parsed.accent_light, 3);
    }

    #[test]
    fn test_parse_report_08() {
        // Report 08 base packet: 0x08 0xC4 + data
        let data = vec![
            0x08, 0xC4, // what, command
            0x01, // sea_state = 1
            0x02, // interference_rejection = 2
            0x01, // scan_speed = 1
            0x01, // sls_auto = 1 (true)
            0x00, 0x00, 0x00, // fields 6-8
            0x50, // side_lobe_suppression = 80
            0x00, 0x00, // field10
            0x01, // noise_rejection = 1
            0x02, // target_sep = 2
            0x30, // sea_clutter = 48
            0x05, // auto_sea_clutter = 5
            0x00, 0x00, // fields 16-17
        ];

        let result = parse_report_08(&data);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.sea_state, 1);
        assert_eq!(parsed.local_interference_rejection, 2);
        assert_eq!(parsed.scan_speed, 1);
        assert!(parsed.sidelobe_suppression_auto);
        assert_eq!(parsed.sidelobe_suppression, 80);
        assert_eq!(parsed.noise_rejection, 1);
        assert_eq!(parsed.target_separation, 2);
        assert_eq!(parsed.sea_clutter, 48);
        assert_eq!(parsed.auto_sea_clutter, 5);
        assert!(parsed.doppler_state.is_none());
        assert!(parsed.doppler_speed.is_none());
    }

    #[test]
    fn test_parse_report_08_with_doppler() {
        // Report 08 extended packet with Doppler
        let mut data = vec![
            0x08,
            0xC4, // what, command
            0x01, // sea_state
            0x00, // interference_rejection
            0x02, // scan_speed
            0x00, // sls_auto
            0x00,
            0x00,
            0x00,
            0x40, // side_lobe_suppression
            0x00,
            0x00,
            0x01,         // noise_rejection
            0x01,         // target_sep
            0x20,         // sea_clutter
            0x03i8 as u8, // auto_sea_clutter = 3
            0x00,
            0x00,
            0x01, // doppler_state = 1 (Both)
        ];
        data.extend_from_slice(&500u16.to_le_bytes()); // doppler_speed = 500

        let result = parse_report_08(&data);
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.doppler_state, Some(1));
        assert_eq!(parsed.doppler_speed, Some(500));
    }
}
