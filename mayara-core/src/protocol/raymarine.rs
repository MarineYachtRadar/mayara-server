//! Raymarine radar protocol parsing (Quantum, RD series)
//!
//! This module contains pure parsing functions for Raymarine radar packets.
//! No I/O operations - just `&[u8]` → `Result<T>` functions.
//!
//! # Supported Models
//!
//! - **RD series**: RD418HD, RD424HD, RD418D, RD424D
//! - **Open Array HD/SHD**: 4kW and 12kW variants
//! - **Magnum**: 4kW and 12kW
//! - **Quantum**: Q24, Q24C, Q24D (with Doppler)
//! - **Cyclone/Cyclone Pro**: Next-gen solid state

use super::c_string;
use crate::error::ParseError;
use crate::radar::RadarDiscovery;
use crate::Brand;
use serde::Deserialize;

// =============================================================================
// Constants
// =============================================================================

/// Number of spokes per revolution for RD series
pub const RD_SPOKES_PER_REVOLUTION: u16 = 2048;

/// Maximum spoke length for RD series
pub const RD_SPOKE_LEN: u16 = 1024;

/// Number of spokes per revolution for Quantum
pub const QUANTUM_SPOKES_PER_REVOLUTION: u16 = 250;

/// Maximum spoke length for Quantum
pub const QUANTUM_SPOKE_LEN: u16 = 252;

/// Pixel values for non-HD radars (4-bit)
pub const NON_HD_PIXEL_VALUES: u8 = 16;

/// Raw pixel values for HD radars (8-bit)
pub const HD_PIXEL_VALUES_RAW: u16 = 256;

/// Pixel values for HD radars (7-bit, last bit reserved)
pub const HD_PIXEL_VALUES: u8 = 128;

/// Beacon multicast address for classic Raymarine
pub const BEACON_ADDR: &str = "224.0.0.1";
pub const BEACON_PORT: u16 = 5800;

/// Quantum WiFi multicast address
pub const QUANTUM_WIFI_ADDR: &str = "232.1.1.1";

// =============================================================================
// Radar Models
// =============================================================================

/// Base model category
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaseModel {
    #[default]
    RD, // Analog radars: RD, HD, SHD, Magnum
    Quantum, // Solid-state: Quantum, Cyclone
}

impl BaseModel {
    pub fn as_str(&self) -> &'static str {
        match self {
            BaseModel::RD => "RD",
            BaseModel::Quantum => "Quantum",
        }
    }
}

impl std::fmt::Display for BaseModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Detailed radar model information
#[derive(Debug, Clone)]
pub struct Model {
    pub base: BaseModel,
    pub hd: bool, // HD = 256 bits per pixel
    pub spokes_per_revolution: u16,
    pub max_spoke_len: u16,
    pub doppler: bool,
    pub name: &'static str,
    pub part_number: &'static str,
}

impl Model {
    /// Parse model from E-series part number
    pub fn from_part_number(part: &str) -> Option<Self> {
        let (base, hd, spokes_per_revolution, max_spoke_len, doppler, name, part_number) =
            match part {
                // Quantum models
                "E70210" => (
                    BaseModel::Quantum,
                    true,
                    QUANTUM_SPOKES_PER_REVOLUTION,
                    QUANTUM_SPOKE_LEN,
                    false,
                    "Quantum Q24",
                    "E70210",
                ),
                "E70344" => (
                    BaseModel::Quantum,
                    true,
                    QUANTUM_SPOKES_PER_REVOLUTION,
                    QUANTUM_SPOKE_LEN,
                    false,
                    "Quantum Q24C",
                    "E70344",
                ),
                "E70498" => (
                    BaseModel::Quantum,
                    true,
                    QUANTUM_SPOKES_PER_REVOLUTION,
                    QUANTUM_SPOKE_LEN,
                    true,
                    "Quantum Q24D",
                    "E70498",
                ),

                // Cyclone models
                "E70620" => (
                    BaseModel::Quantum,
                    true,
                    QUANTUM_SPOKES_PER_REVOLUTION,
                    QUANTUM_SPOKE_LEN,
                    true,
                    "Cyclone",
                    "E70620",
                ),
                "E70621" => (
                    BaseModel::Quantum,
                    true,
                    QUANTUM_SPOKES_PER_REVOLUTION,
                    QUANTUM_SPOKE_LEN,
                    true,
                    "Cyclone Pro",
                    "E70621",
                ),

                // Magnum models
                "E70484" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "Magnum 4kW",
                    "E70484",
                ),
                "E70487" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "Magnum 12kW",
                    "E70487",
                ),

                // Open Array HD models
                "E52069" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "Open Array HD 4kW",
                    "E52069",
                ),
                "E92160" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "Open Array HD 12kW",
                    "E92160",
                ),

                // Open Array SHD models
                "E52081" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "Open Array SHD 4kW",
                    "E52081",
                ),
                "E52082" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "Open Array SHD 12kW",
                    "E52082",
                ),

                // RD HD models
                "E92142" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "RD418HD",
                    "E92142",
                ),
                "E92143" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    RD_SPOKE_LEN,
                    false,
                    "RD424HD",
                    "E92143",
                ),

                // RD D models
                "E92130" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    512,
                    false,
                    "RD418D",
                    "E92130",
                ),
                "E92132" => (
                    BaseModel::RD,
                    true,
                    RD_SPOKES_PER_REVOLUTION,
                    512,
                    false,
                    "RD424D",
                    "E92132",
                ),

                _ => return None,
            };

        Some(Model {
            base,
            hd,
            spokes_per_revolution,
            max_spoke_len,
            doppler,
            name,
            part_number,
        })
    }

    /// Get pixel values count based on HD capability
    pub fn pixel_values(&self) -> u8 {
        if self.hd {
            HD_PIXEL_VALUES
        } else {
            NON_HD_PIXEL_VALUES
        }
    }
}

// =============================================================================
// Network Address Parsing
// =============================================================================

/// Network socket address (little endian) as found in Raymarine packets
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct LittleEndianSocketAddrV4 {
    pub addr: [u8; 4],
    pub port: [u8; 2],
}

impl LittleEndianSocketAddrV4 {
    /// Get IP address as [u8; 4] (raw bytes in network order)
    pub fn ip(&self) -> [u8; 4] {
        // Convert from little-endian to network byte order
        let le_val = u32::from_le_bytes(self.addr);
        le_val.to_be_bytes()
    }

    /// Get IP address as string
    ///
    /// Raymarine stores IP addresses as little-endian u32, so we need
    /// to interpret them correctly.
    pub fn ip_string(&self) -> String {
        // Read as little-endian u32, then extract octets
        let ip_val = u32::from_le_bytes(self.addr);
        let a = (ip_val >> 24) & 0xff;
        let b = (ip_val >> 16) & 0xff;
        let c = (ip_val >> 8) & 0xff;
        let d = ip_val & 0xff;
        format!("{}.{}.{}.{}", a, b, c, d)
    }

    /// Get port number
    pub fn port(&self) -> u16 {
        u16::from_le_bytes(self.port)
    }

    /// Get as "ip:port" string
    pub fn as_string(&self) -> String {
        format!("{}:{}", self.ip_string(), self.port())
    }
}

// =============================================================================
// Beacon Packet Structures
// =============================================================================

/// 36-byte beacon containing radar endpoints
///
/// This beacon is sent after a 56-byte beacon with matching link_id.
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct Beacon36 {
    pub beacon_type: [u8; 4],              // 0: always 0x00000000
    pub link_id: [u8; 4],                  // 4: identifies radar instance
    pub subtype: [u8; 4],                  // 8: 0x28 for Quantum, 0x01 for RD
    _field5: [u8; 4],                      // 12
    _field6: [u8; 4],                      // 16
    pub report: LittleEndianSocketAddrV4,  // 20: report/data address
    _align1: [u8; 2],                      // 26
    pub command: LittleEndianSocketAddrV4, // 28: command address
    _align2: [u8; 2],                      // 34
}

pub const BEACON_36_SIZE: usize = std::mem::size_of::<Beacon36>();

/// 56-byte beacon containing radar identification
///
/// This beacon identifies the radar model. A 36-byte beacon
/// with matching link_id follows with endpoint addresses.
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct Beacon56 {
    pub beacon_type: [u8; 4], // 0: always 0x00000001
    pub subtype: [u8; 4],     // 4: 0x66 for Quantum, 0x01 for RD
    pub link_id: [u8; 4],     // 8: identifies radar instance
    _field4: [u8; 4],         // 12
    _field5: [u8; 4],         // 16
    pub model_name: [u8; 32], // 20: e.g. "QuantumRadar" when subtype = 0x66
    _field7: [u8; 4],         // 52
}

pub const BEACON_56_SIZE: usize = std::mem::size_of::<Beacon56>();

// =============================================================================
// Parsed Data Structures
// =============================================================================

/// Link ID type (32-bit identifier for radar instance)
pub type LinkId = u32;

/// Result of parsing a 56-byte beacon
#[derive(Debug, Clone)]
pub struct ParsedBeacon56 {
    pub beacon_type: u32,
    pub subtype: u32,
    pub link_id: LinkId,
    pub model_name: Option<String>,
    pub base_model: BaseModel,
}

/// Result of parsing a 36-byte beacon
#[derive(Debug, Clone)]
pub struct ParsedBeacon36 {
    pub beacon_type: u32,
    pub link_id: LinkId,
    pub subtype: u32,
    pub report_addr: String,
    pub command_addr: String,
}

/// Combined beacon result for radar discovery
#[derive(Debug, Clone)]
pub struct ParsedRadarBeacon {
    pub link_id: LinkId,
    pub model_name: Option<String>,
    pub base_model: BaseModel,
    pub report_addr: String,
    pub command_addr: String,
}

// =============================================================================
// Beacon Type Constants
// =============================================================================

/// Beacon type for 36-byte beacons
pub const BEACON_TYPE_36: u32 = 0x00000000;

/// Beacon type for 56-byte beacons
pub const BEACON_TYPE_56: u32 = 0x00000001;

/// Subtype for Quantum in 56-byte beacon
pub const SUBTYPE_QUANTUM_56: u32 = 0x66;

/// Subtype for RD in 56-byte beacon
pub const SUBTYPE_RD_56: u32 = 0x01;

/// Subtype for Quantum in 36-byte beacon
pub const SUBTYPE_QUANTUM_36: u32 = 0x28;

/// Subtype for RD in 36-byte beacon
pub const SUBTYPE_RD_36: u32 = 0x01;

/// Subtype for wireless Quantum
pub const SUBTYPE_WIRELESS: u32 = 0x4d;

/// Subtype for MFD request (ignore)
pub const SUBTYPE_MFD_REQUEST: u32 = 0x11;

// =============================================================================
// Parsing Functions
// =============================================================================

/// Check if packet is a 36-byte beacon
pub fn is_beacon_36(data: &[u8]) -> bool {
    data.len() == BEACON_36_SIZE
}

/// Check if packet is a 56-byte beacon
pub fn is_beacon_56(data: &[u8]) -> bool {
    data.len() == BEACON_56_SIZE
}

/// Parse a 56-byte beacon (identification)
pub fn parse_beacon_56(data: &[u8]) -> Result<ParsedBeacon56, ParseError> {
    if data.len() < BEACON_56_SIZE {
        return Err(ParseError::TooShort {
            expected: BEACON_56_SIZE,
            actual: data.len(),
        });
    }

    let beacon: Beacon56 = bincode::deserialize(&data[..BEACON_56_SIZE])?;

    let beacon_type = u32::from_le_bytes(beacon.beacon_type);
    if beacon_type != BEACON_TYPE_56 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x01, 0x00, 0x00, 0x00],
            actual: beacon.beacon_type.to_vec(),
        });
    }

    let subtype = u32::from_le_bytes(beacon.subtype);
    let link_id = u32::from_le_bytes(beacon.link_id);

    let (model_name, base_model) = match subtype {
        SUBTYPE_QUANTUM_56 => {
            let name = c_string(&beacon.model_name);
            (name, BaseModel::Quantum)
        }
        SUBTYPE_RD_56 => (Some("RD/HD/Eseries".to_string()), BaseModel::RD),
        SUBTYPE_WIRELESS => {
            // Wireless variant (Quantum_W3)
            let name = c_string(&beacon.model_name);
            (name, BaseModel::Quantum)
        }
        SUBTYPE_MFD_REQUEST => {
            // Request from MFD, ignore
            return Err(ParseError::InvalidPacket("MFD request beacon".into()));
        }
        _ => {
            return Err(ParseError::InvalidPacket(format!(
                "Unknown 56-byte beacon subtype: 0x{:02x}",
                subtype
            )));
        }
    };

    Ok(ParsedBeacon56 {
        beacon_type,
        subtype,
        link_id,
        model_name,
        base_model,
    })
}

/// Parse a 36-byte beacon (endpoints)
pub fn parse_beacon_36(data: &[u8]) -> Result<ParsedBeacon36, ParseError> {
    if data.len() < BEACON_36_SIZE {
        return Err(ParseError::TooShort {
            expected: BEACON_36_SIZE,
            actual: data.len(),
        });
    }

    let beacon: Beacon36 = bincode::deserialize(&data[..BEACON_36_SIZE])?;

    let beacon_type = u32::from_le_bytes(beacon.beacon_type);
    if beacon_type != BEACON_TYPE_36 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x00, 0x00, 0x00, 0x00],
            actual: beacon.beacon_type.to_vec(),
        });
    }

    let link_id = u32::from_le_bytes(beacon.link_id);
    let subtype = u32::from_le_bytes(beacon.subtype);

    Ok(ParsedBeacon36 {
        beacon_type,
        link_id,
        subtype,
        report_addr: beacon.report.as_string(),
        command_addr: beacon.command.as_string(),
    })
}

/// Parse beacon response and create RadarDiscovery
///
/// Raymarine uses a two-beacon discovery:
/// 1. First, a 56-byte beacon identifies the radar (link_id, model)
/// 2. Then, a 36-byte beacon provides endpoints (same link_id)
///
/// This function handles either beacon type.
pub fn parse_beacon_response(data: &[u8], source_addr: &str) -> Result<RadarDiscovery, ParseError> {
    if data.len() < 36 {
        return Err(ParseError::TooShort {
            expected: 36,
            actual: data.len(),
        });
    }

    // Try 56-byte beacon first
    if data.len() >= BEACON_56_SIZE {
        let beacon = parse_beacon_56(data)?;

        let (spokes, spoke_len, pixels) = match beacon.base_model {
            BaseModel::Quantum => (
                QUANTUM_SPOKES_PER_REVOLUTION,
                QUANTUM_SPOKE_LEN,
                HD_PIXEL_VALUES,
            ),
            BaseModel::RD => (RD_SPOKES_PER_REVOLUTION, RD_SPOKE_LEN, NON_HD_PIXEL_VALUES),
        };

        return Ok(RadarDiscovery {
            brand: Brand::Raymarine,
            model: beacon.model_name,
            name: format!("{:08X}", beacon.link_id),
            address: source_addr.to_string(),
            data_port: 0, // Will come from 36-byte beacon
            command_port: 0,
            spokes_per_revolution: spokes,
            max_spoke_len: spoke_len,
            pixel_values: pixels,
            serial_number: None,
            nic_address: None, // Set by locator
            suffix: None,
            data_address: None,
            report_address: None,
            send_address: None,
        });
    }

    // Try 36-byte beacon
    if data.len() >= BEACON_36_SIZE {
        let beacon = parse_beacon_36(data)?;

        let base_model = match beacon.subtype {
            SUBTYPE_QUANTUM_36 => BaseModel::Quantum,
            SUBTYPE_RD_36 => BaseModel::RD,
            _ => BaseModel::RD, // Default to RD for unknown subtypes
        };

        let (spokes, spoke_len, pixels) = match base_model {
            BaseModel::Quantum => (
                QUANTUM_SPOKES_PER_REVOLUTION,
                QUANTUM_SPOKE_LEN,
                HD_PIXEL_VALUES,
            ),
            BaseModel::RD => (RD_SPOKES_PER_REVOLUTION, RD_SPOKE_LEN, NON_HD_PIXEL_VALUES),
        };

        // Parse port from report address
        let data_port = beacon
            .report_addr
            .rsplit(':')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let command_port = beacon
            .command_addr
            .rsplit(':')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        return Ok(RadarDiscovery {
            brand: Brand::Raymarine,
            model: None,
            name: format!("{:08X}", beacon.link_id),
            address: source_addr.to_string(),
            data_port,
            command_port,
            spokes_per_revolution: spokes,
            max_spoke_len: spoke_len,
            pixel_values: pixels,
            serial_number: None,
            nic_address: None, // Set by locator
            suffix: None,
            data_address: None,
            report_address: None,
            send_address: None,
        });
    }

    Err(ParseError::TooShort {
        expected: 36,
        actual: data.len(),
    })
}

/// Validate 36-byte beacon subtype for known model
pub fn is_valid_beacon_36_subtype(subtype: u32, base_model: BaseModel) -> bool {
    match base_model {
        BaseModel::Quantum => subtype == SUBTYPE_QUANTUM_36,
        BaseModel::RD => matches!(subtype, SUBTYPE_RD_36 | 8 | 21 | 26 | 27 | 30 | 35),
    }
}

// =============================================================================
// Quantum Frame Parsing
// =============================================================================

/// Quantum frame header (20 bytes)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct QuantumFrameHeader {
    pub frame_type: u32, // 0x00280003
    pub seq_num: u16,
    pub something_1: u16,       // 0x0101
    pub scan_len: u16,          // 0x002b
    pub num_spokes: u16,        // 0x00fa
    pub something_3: u16,       // 0x0008
    pub returns_per_range: u16, // number of radar returns per range from the status
    pub azimuth: u16,
    pub data_len: u16, // length of the rest of the data
}

pub const QUANTUM_FRAME_HEADER_SIZE: usize = std::mem::size_of::<QuantumFrameHeader>();

/// Parsed Quantum frame header
#[derive(Debug, Clone)]
pub struct ParsedQuantumFrame {
    pub seq_num: u16,
    pub scan_len: u16,
    pub num_spokes: u16,
    pub returns_per_range: u16,
    pub azimuth: u16,
    pub data_len: u16,
}

/// Parse Quantum frame header
pub fn parse_quantum_frame_header(data: &[u8]) -> Result<ParsedQuantumFrame, ParseError> {
    if data.len() < QUANTUM_FRAME_HEADER_SIZE {
        return Err(ParseError::TooShort {
            expected: QUANTUM_FRAME_HEADER_SIZE,
            actual: data.len(),
        });
    }

    let header: QuantumFrameHeader = bincode::deserialize(&data[..QUANTUM_FRAME_HEADER_SIZE])?;

    Ok(ParsedQuantumFrame {
        seq_num: header.seq_num,
        scan_len: u16::from_le_bytes(header.scan_len.to_le_bytes()),
        num_spokes: u16::from_le_bytes(header.num_spokes.to_le_bytes()),
        returns_per_range: u16::from_le_bytes(header.returns_per_range.to_le_bytes()),
        azimuth: u16::from_le_bytes(header.azimuth.to_le_bytes()),
        data_len: u16::from_le_bytes(header.data_len.to_le_bytes()),
    })
}

// =============================================================================
// Quantum Status Report
// =============================================================================

/// Controls per mode for Quantum
#[derive(Debug, Clone, Copy)]
pub struct QuantumControlsPerMode {
    pub gain_auto: bool,
    pub gain: u8,
    pub color_gain_auto: bool,
    pub color_gain: u8,
    pub sea_auto: bool,
    pub sea: u8,
    pub rain_enabled: bool,
    pub rain: u8,
}

/// Parsed Quantum status report
#[derive(Debug, Clone)]
pub struct ParsedQuantumStatus {
    pub status: u8,
    pub bearing_offset: i16,
    pub interference_rejection: u8,
    pub range_index: u8,
    pub mode: u8,
    pub controls: [QuantumControlsPerMode; 4],
    pub target_expansion: u8,
    pub mbs_enabled: bool,
    pub ranges: Vec<u32>,
}

/// Parse Quantum status report (0x00280002)
pub fn parse_quantum_status(data: &[u8]) -> Result<ParsedQuantumStatus, ParseError> {
    const MIN_SIZE: usize = 228; // Minimum size for status report with ranges
    if data.len() < MIN_SIZE {
        return Err(ParseError::TooShort {
            expected: MIN_SIZE,
            actual: data.len(),
        });
    }

    let status = data[4];
    let bearing_offset = i16::from_le_bytes([data[14], data[15]]);
    let interference_rejection = data[17];
    let range_index = data[20];
    let mode = data[21];

    // Parse controls for 4 modes (each 8 bytes starting at offset 22)
    let mut controls = [QuantumControlsPerMode {
        gain_auto: false,
        gain: 0,
        color_gain_auto: false,
        color_gain: 0,
        sea_auto: false,
        sea: 0,
        rain_enabled: false,
        rain: 0,
    }; 4];

    for i in 0..4 {
        let base = 22 + i * 8;
        controls[i] = QuantumControlsPerMode {
            gain_auto: data[base] > 0,
            gain: data[base + 1],
            color_gain_auto: data[base + 2] > 0,
            color_gain: data[base + 3],
            sea_auto: data[base + 4] > 0,
            sea: data[base + 5],
            rain_enabled: data[base + 6] > 0,
            rain: data[base + 7],
        };
    }

    let target_expansion = data[54];
    let mbs_enabled = data[59] > 0;

    // Parse ranges (20 u32 values starting at offset 148)
    let mut ranges = Vec::with_capacity(20);
    for i in 0..20 {
        let offset = 148 + i * 4;
        if offset + 4 <= data.len() {
            let range = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            ranges.push(range);
        }
    }

    Ok(ParsedQuantumStatus {
        status,
        bearing_offset,
        interference_rejection,
        range_index,
        mode,
        controls,
        target_expansion,
        mbs_enabled,
        ranges,
    })
}

// =============================================================================
// RD Frame Parsing
// =============================================================================

/// RD frame header (32 bytes)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct RdFrameHeader {
    pub field01: u32, // 0x00010003
    pub zero_1: u32,
    pub fieldx_1: u32,    // 0x0000001c
    pub nspokes: u32,     // 0x00000008 - usually but changes
    pub spoke_count: u32, // 0x00000000 in regular, counting in HD
    pub zero_3: u32,
    pub fieldx_3: u32, // 0x00000001
    pub fieldx_4: u32, // 0x00000000 or 0xffffffff in regular, 0x400 in HD
}

pub const RD_FRAME_HEADER_SIZE: usize = std::mem::size_of::<RdFrameHeader>();

/// Parsed RD frame header
#[derive(Debug, Clone)]
pub struct ParsedRdFrame {
    pub nspokes: u32,
    pub is_hd: bool,
}

/// Parse RD frame header
pub fn parse_rd_frame_header(data: &[u8]) -> Result<ParsedRdFrame, ParseError> {
    if data.len() < RD_FRAME_HEADER_SIZE {
        return Err(ParseError::TooShort {
            expected: RD_FRAME_HEADER_SIZE,
            actual: data.len(),
        });
    }

    let header: RdFrameHeader = bincode::deserialize(&data[..RD_FRAME_HEADER_SIZE])?;

    // Validate frame header
    if header.field01 != 0x00010003
        || header.fieldx_1 != 0x0000001c
        || header.fieldx_3 != 0x00000001
    {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x03, 0x00, 0x01, 0x00],
            actual: vec![(header.field01 & 0xFF) as u8],
        });
    }

    let is_hd = header.fieldx_4 == 0x400;

    Ok(ParsedRdFrame {
        nspokes: header.nspokes,
        is_hd,
    })
}

// =============================================================================
// RD Status Report
// =============================================================================

/// Parsed RD status report
#[derive(Debug, Clone)]
pub struct ParsedRdStatus {
    pub ranges: Vec<u32>,
    pub status: u8,
    pub warmup_time: u8,
    pub signal_strength: u8,
    pub range_id: u8,
    pub auto_gain: bool,
    pub gain: u32,
    pub auto_sea: u8,
    pub sea: u8,
    pub rain_enabled: bool,
    pub rain: u8,
    pub ftc_enabled: bool,
    pub ftc: u8,
    pub auto_tune: bool,
    pub tune: u8,
    pub bearing_offset: i16,
    pub interference_rejection: u8,
    pub target_expansion: u8,
    pub mbs_enabled: bool,
    pub is_hd: bool,
}

/// Parse RD status report (0x010001 or 0x018801)
pub fn parse_rd_status(data: &[u8]) -> Result<ParsedRdStatus, ParseError> {
    const MIN_SIZE: usize = 250; // Minimum size for RD status report
    if data.len() < MIN_SIZE {
        return Err(ParseError::TooShort {
            expected: MIN_SIZE,
            actual: data.len(),
        });
    }

    let field01 = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if field01 != 0x010001 && field01 != 0x018801 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x01, 0x00, 0x01, 0x00],
            actual: vec![data[0], data[1], data[2], data[3]],
        });
    }

    let is_hd = field01 == 0x018801;

    // Parse ranges (11 u32 values starting at offset 4)
    let mut ranges = Vec::with_capacity(11);
    for i in 0..11 {
        let offset = 4 + i * 4;
        let range = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        ranges.push(range);
    }

    let status = data[180];
    let warmup_time = data[184];
    let signal_strength = data[185];
    let range_id = data[193];
    let auto_gain = data[196] > 0;
    let gain = u32::from_le_bytes([data[200], data[201], data[202], data[203]]);
    let auto_sea = data[204];
    let sea = data[208];
    let rain_enabled = data[209] > 0;
    let rain = data[213];
    let ftc_enabled = data[214] > 0;
    let ftc = data[218];
    let auto_tune = data[219] > 0;
    let tune = data[223];
    let bearing_offset = i16::from_le_bytes([data[224], data[225]]);
    let interference_rejection = data[226];
    let target_expansion = data[230];
    let mbs_enabled = data[244] > 0;

    Ok(ParsedRdStatus {
        ranges,
        status,
        warmup_time,
        signal_strength,
        range_id,
        auto_gain,
        gain,
        auto_sea,
        sea,
        rain_enabled,
        rain,
        ftc_enabled,
        ftc,
        auto_tune,
        tune,
        bearing_offset,
        interference_rejection,
        target_expansion,
        mbs_enabled,
        is_hd,
    })
}

// =============================================================================
// Spoke Data Decompression
// =============================================================================

/// Decompress Quantum spoke data using RLE (0x5c escape byte)
pub fn decompress_quantum_spoke(
    data: &[u8],
    doppler_lookup: &[u8; 256],
    returns_per_line: usize,
) -> Vec<u8> {
    let mut unpacked = Vec::with_capacity(1024);
    let mut offset = 0;

    while offset < data.len() {
        if data[offset] != 0x5c {
            let pixel = data[offset] as usize;
            unpacked.push(doppler_lookup[pixel]);
            offset += 1;
        } else if offset + 2 < data.len() {
            let count = data[offset + 1] as usize;
            let pixel = data[offset + 2] as usize;
            let value = doppler_lookup[pixel];
            for _ in 0..count {
                unpacked.push(value);
            }
            offset += 3;
        } else {
            break;
        }
    }

    unpacked.truncate(returns_per_line);
    unpacked
}

/// Decompress RD spoke data using RLE (0x5c escape byte)
///
/// HD mode: single byte per pixel, shift right by 1
/// Non-HD mode: two pixels per byte (low and high nibbles)
pub fn decompress_rd_spoke(data: &[u8], is_hd: bool, returns_per_line: usize) -> Vec<u8> {
    let mut unpacked = Vec::with_capacity(returns_per_line);
    let mut offset = 0;

    while offset < data.len() {
        if is_hd {
            if data[offset] != 0x5c {
                unpacked.push(data[offset] >> 1);
                offset += 1;
            } else if offset + 2 < data.len() {
                let count = data[offset + 1] as usize;
                let value = data[offset + 2] >> 1;
                for _ in 0..count {
                    unpacked.push(value);
                }
                offset += 3;
            } else {
                break;
            }
        } else {
            // Non-HD: 2 pixels per byte
            if data[offset] != 0x5c {
                unpacked.push(data[offset] & 0x0f);
                unpacked.push(data[offset] >> 4);
                offset += 1;
            } else if offset + 2 < data.len() {
                let count = data[offset + 1] as usize;
                let value = data[offset + 2];
                for _ in 0..count {
                    unpacked.push(value & 0x0f);
                    unpacked.push(value >> 4);
                }
                offset += 3;
            } else {
                break;
            }
        }
    }

    unpacked.truncate(returns_per_line);
    unpacked
}

// =============================================================================
// MFD Beacon
// =============================================================================

/// MFD beacon packet (56 bytes)
/// This is sent by MFDs to discover radars
pub const MFD_BEACON: [u8; 56] = [
    0x01, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x38, 0x8c, 0x81, 0xd4, 0x6a, 0x01, 0x0e, 0x83,
    0x6c, 0x03, 0x12, 0xc6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00,
];

/// Create the MFD beacon request
pub fn create_mfd_beacon() -> &'static [u8] {
    &MFD_BEACON
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Real Quantum beacon data from pcap
    const QUANTUM_BEACON_36: [u8; 36] = [
        0x0, 0x0, 0x0, 0x0, 0x58, 0x6b, 0x80, 0xd6, 0x28, 0x0, 0x0, 0x0, 0x3, 0x0, 0x64, 0x0, 0x6,
        0x8, 0x10, 0x0, 0x1, 0xf3, 0x1, 0xe8, 0xe, 0xa, 0x11, 0x0, 0xd6, 0x6, 0x12, 0xc6, 0xf, 0xa,
        0x36, 0x0,
    ];

    const QUANTUM_BEACON_56: [u8; 56] = [
        0x1, 0x0, 0x0, 0x0, 0x66, 0x0, 0x0, 0x0, 0x58, 0x6b, 0x80, 0xd6, 0xf3, 0x0, 0x0, 0x0, 0xf3,
        0x0, 0xa8, 0xc0, 0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, 0x52, 0x61, 0x64, 0x61, 0x72,
        0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
        0x0, 0x0, 0x2, 0x0, 0x0, 0x0,
    ];

    // RD series beacon data
    const RD_BEACON_36: [u8; 36] = [
        0x0, 0x0, 0x0, 0x0, // message_type
        0xb1, 0x69, 0xc2, 0xb2, // link_id
        0x1, 0x0, 0x0, 0x0, // sub_type 1
        0x1, 0x0, 0x1e, 0x0, 0xb, 0x8, 0x10, 0x0, 231, 69, 29, 224, 0x6, 0xa, 0x0,
        0x0, // 224.29.69.231:2566
        47, 234, 0, 10, 11, 8, 0, 0, // 10.0.234.47:2059
    ];

    const RD_BEACON_56: [u8; 56] = [
        0x1, 0x0, 0x0, 0x0, // message_type
        0x1, 0x0, 0x0, 0x0, // sub_type
        0xb1, 0x69, 0xc2, 0xb2, // link_id
        0xb, 0x2, 0x0, 0x0, 0x2f, 0xea, 0x0, 0xa, 0x0, 0x31, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x33,
        0xcc, 0x33, 0x4e, 0x37, 0xcc, 0x27, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x30, 0xcc,
        0x13, 0xc8, 0x33, 0xcc, 0x13, 0xcc, 0x33, 0xc0, 0x13, 0x2, 0x0, 0x1, 0x0,
    ];

    #[test]
    fn test_parse_quantum_beacon_56() {
        let result = parse_beacon_56(&QUANTUM_BEACON_56);
        assert!(result.is_ok());

        let beacon = result.unwrap();
        assert_eq!(beacon.beacon_type, BEACON_TYPE_56);
        assert_eq!(beacon.subtype, SUBTYPE_QUANTUM_56);
        assert_eq!(beacon.link_id, 0xd6806b58);
        assert_eq!(beacon.model_name, Some("QuantumRadar".to_string()));
        assert_eq!(beacon.base_model, BaseModel::Quantum);
    }

    #[test]
    fn test_parse_quantum_beacon_36() {
        let result = parse_beacon_36(&QUANTUM_BEACON_36);
        assert!(result.is_ok());

        let beacon = result.unwrap();
        assert_eq!(beacon.beacon_type, BEACON_TYPE_36);
        assert_eq!(beacon.link_id, 0xd6806b58);
        assert_eq!(beacon.subtype, SUBTYPE_QUANTUM_36);
        // Report address: 232.1.243.1:2574 (0xe, 0xa = 2574)
        assert!(beacon.report_addr.starts_with("232.1.243.1:"));
        // Command address: 198.18.6.214:2575
        assert!(beacon.command_addr.starts_with("198.18.6.214:"));
    }

    #[test]
    fn test_parse_rd_beacon_56() {
        let result = parse_beacon_56(&RD_BEACON_56);
        assert!(result.is_ok());

        let beacon = result.unwrap();
        assert_eq!(beacon.beacon_type, BEACON_TYPE_56);
        assert_eq!(beacon.subtype, SUBTYPE_RD_56);
        assert_eq!(beacon.link_id, 0xb2c269b1);
        assert_eq!(beacon.base_model, BaseModel::RD);
    }

    #[test]
    fn test_parse_rd_beacon_36() {
        let result = parse_beacon_36(&RD_BEACON_36);
        assert!(result.is_ok());

        let beacon = result.unwrap();
        assert_eq!(beacon.beacon_type, BEACON_TYPE_36);
        assert_eq!(beacon.link_id, 0xb2c269b1);
        assert_eq!(beacon.subtype, SUBTYPE_RD_36);
    }

    #[test]
    fn test_is_beacon_types() {
        assert!(is_beacon_36(&QUANTUM_BEACON_36));
        assert!(!is_beacon_56(&QUANTUM_BEACON_36));

        assert!(is_beacon_56(&QUANTUM_BEACON_56));
        assert!(!is_beacon_36(&QUANTUM_BEACON_56));
    }

    #[test]
    fn test_model_from_part_number() {
        let quantum = Model::from_part_number("E70210");
        assert!(quantum.is_some());
        let quantum = quantum.unwrap();
        assert_eq!(quantum.name, "Quantum Q24");
        assert_eq!(quantum.base, BaseModel::Quantum);
        assert!(quantum.hd);
        assert!(!quantum.doppler);

        let quantum_d = Model::from_part_number("E70498");
        assert!(quantum_d.is_some());
        let quantum_d = quantum_d.unwrap();
        assert_eq!(quantum_d.name, "Quantum Q24D");
        assert!(quantum_d.doppler);

        let rd418hd = Model::from_part_number("E92142");
        assert!(rd418hd.is_some());
        let rd418hd = rd418hd.unwrap();
        assert_eq!(rd418hd.name, "RD418HD");
        assert_eq!(rd418hd.base, BaseModel::RD);

        let unknown = Model::from_part_number("EXXXXX");
        assert!(unknown.is_none());
    }

    #[test]
    fn test_base_model_display() {
        assert_eq!(BaseModel::RD.to_string(), "RD");
        assert_eq!(BaseModel::Quantum.to_string(), "Quantum");
    }

    #[test]
    fn test_parse_short_packet() {
        let result = parse_beacon_56(&[0u8; 10]);
        assert!(matches!(result, Err(ParseError::TooShort { .. })));

        let result = parse_beacon_36(&[0u8; 10]);
        assert!(matches!(result, Err(ParseError::TooShort { .. })));
    }
}
