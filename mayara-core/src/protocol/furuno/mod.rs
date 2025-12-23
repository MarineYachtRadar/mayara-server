//! Furuno radar protocol parsing
//!
//! This module contains pure parsing functions for Furuno radar packets.
//! No I/O operations - just `&[u8]` → `Result<T>` functions.

pub mod command;
pub mod dispatch;
pub mod report;

use super::c_string;
use crate::error::ParseError;
use crate::radar::RadarDiscovery;
use crate::Brand;
use serde::Deserialize;

// =============================================================================
// Constants
// =============================================================================

/// Number of spokes per revolution for Furuno radars
pub const SPOKES_PER_REVOLUTION: u16 = 8192;

/// Maximum spoke length in pixels
pub const MAX_SPOKE_LEN: u16 = 884;

/// Base port for Furuno radar communication
pub const BASE_PORT: u16 = 10000;

/// Port for beacon discovery (broadcast)
pub const BEACON_PORT: u16 = BASE_PORT + 10;

/// Port for spoke data (multicast)
pub const DATA_PORT: u16 = BASE_PORT + 24;

/// Broadcast address for Furuno radars
pub const BROADCAST_ADDR: &str = "172.31.255.255";

/// Multicast address for spoke data
pub const DATA_MULTICAST_ADDR: &str = "239.255.0.2";

// =============================================================================
// Network Requirements
// =============================================================================

/// Required IP range for Furuno DRS radars (172.31.x.x/16)
/// The radar has a hardcoded IP and requires the host to be in this range.
pub const REQUIRED_IP_PREFIX: [u8; 2] = [172, 31];

/// Check if an IP address is in the Furuno-required range (172.31.x.x)
///
/// # Example
/// ```
/// use mayara_core::protocol::furuno::is_valid_furuno_ip;
/// assert!(is_valid_furuno_ip("172.31.3.10"));
/// assert!(!is_valid_furuno_ip("192.168.1.1"));
/// ```
pub fn is_valid_furuno_ip(ip: &str) -> bool {
    ip.split('.')
        .take(2)
        .map(|s| s.parse::<u8>().unwrap_or(0))
        .collect::<Vec<_>>()
        == REQUIRED_IP_PREFIX
}

/// Get a human-readable description of Furuno network requirements
pub fn network_requirement_message() -> &'static str {
    "Furuno DRS radars require the host to have an IP address in the 172.31.x.x range. \
     Configure your network interface with an IP like 172.31.3.x/16 on the interface \
     connected to the radar network."
}

// =============================================================================
// Packet Definitions
// =============================================================================

/// Beacon request packet - send this to discover Furuno radars
pub const REQUEST_BEACON_PACKET: [u8; 16] = [
    0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x08, 0x01, 0x00, 0x00, 0x00,
];

/// Model info request packet
pub const REQUEST_MODEL_PACKET: [u8; 16] = [
    0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x14, 0x01, 0x00, 0x08, 0x01, 0x00, 0x00, 0x00,
];

/// Announce presence packet
pub const ANNOUNCE_PACKET: [u8; 32] = [
    0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x01, 0x00, 0x00, 0x00,
    b'M', b'A', b'Y', b'A', b'R', b'A', 0x00, 0x00, 0x01, 0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x12,
];

/// Expected header for radar beacon response
pub const BEACON_RESPONSE_HEADER: [u8; 11] = [
    0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
];

// =============================================================================
// Radar Models
// =============================================================================

/// Known Furuno radar models
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Unknown,
    FAR21x7,
    DRS,
    FAR14x7,
    DRS4DL,
    FAR3000,
    DRS4DNXT,
    DRS6ANXT,
    DRS6AXCLASS,
    FAR15x3,
    FAR14x6,
    DRS12ANXT,
    DRS25ANXT,
}

impl Model {
    pub fn as_str(&self) -> &'static str {
        match self {
            Model::Unknown => "Unknown",
            Model::FAR21x7 => "FAR21x7",
            Model::DRS => "DRS",
            Model::FAR14x7 => "FAR14x7",
            Model::DRS4DL => "DRS4DL",
            Model::FAR3000 => "FAR3000",
            Model::DRS4DNXT => "DRS4D-NXT",
            Model::DRS6ANXT => "DRS6A-NXT",
            Model::DRS6AXCLASS => "DRS6AXCLASS",
            Model::FAR15x3 => "FAR15x3",
            Model::FAR14x6 => "FAR14x6",
            Model::DRS12ANXT => "DRS12A-NXT",
            Model::DRS25ANXT => "DRS25A-NXT",
        }
    }

    /// Parse model from name string (as returned by `as_str()` or from radar)
    pub fn from_name(s: &str) -> Self {
        match s {
            "DRS4D-NXT" => Model::DRS4DNXT,
            "DRS6A-NXT" => Model::DRS6ANXT,
            "DRS12A-NXT" => Model::DRS12ANXT,
            "DRS25A-NXT" => Model::DRS25ANXT,
            "DRS6A-XCLASS" | "DRS6AXCLASS" => Model::DRS6AXCLASS,
            "FAR-21x7" | "FAR21x7" => Model::FAR21x7,
            "FAR-14x7" | "FAR14x7" => Model::FAR14x7,
            "FAR-3000" | "FAR3000" => Model::FAR3000,
            "FAR-15x3" | "FAR15x3" => Model::FAR15x3,
            "FAR-14x6" | "FAR14x6" => Model::FAR14x6,
            "DRS4DL" => Model::DRS4DL,
            "DRS" => Model::DRS,
            _ => Model::Unknown,
        }
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// =============================================================================
// Packet Structures
// =============================================================================

/// Beacon response structure (32 bytes minimum)
///
/// Example packet from DRS4D-NXT:
/// ```text
/// [01 00 00 01 00 00 00 00 00 01 00 18 01 00 00 00 52 44 30 30 33 32 31 32 ...]
///                                ^__length           ^_name ("RD003212")
/// ```
#[derive(Deserialize, Debug, Clone)]
#[repr(C, packed)]
pub struct BeaconResponse {
    _header: [u8; 11],
    pub length: u8,
    _filler: [u8; 4],
    pub name: [u8; 8],
}

/// Model report structure (170 bytes)
///
/// Layout based on Wireshark analysis:
/// - Offset 0x00: Header/filler (48 bytes)
/// - Offset 0x30 (48): Device name/model (32 bytes, null-terminated)
/// - Offset 0x50 (80): Firmware versions (32 bytes)
/// - Offset 0x70 (96): Firmware version (32 bytes)
/// - Offset 0x90 (144): Serial number (26 bytes)
///
/// Total: 170 bytes
#[derive(Deserialize, Debug, Clone)]
#[repr(C, packed)]
pub struct ModelReport {
    _filler1a: [u8; 24],
    _filler1b: [u8; 24],
    pub model: [u8; 32],
    pub firmware_versions: [u8; 32],
    pub firmware_version: [u8; 32],
    pub serial_no: [u8; 26],
}

// =============================================================================
// Parsing Functions
// =============================================================================

/// Check if a packet is a valid Furuno beacon response
pub fn is_beacon_response(data: &[u8]) -> bool {
    data.len() >= 32 && data[0..11] == BEACON_RESPONSE_HEADER && data[16] == b'R'
}

/// Check if a packet is a model report (170 bytes)
pub fn is_model_report(data: &[u8]) -> bool {
    data.len() == 170
}

/// Parse a beacon response packet
///
/// # Arguments
/// * `data` - Raw packet bytes (at least 32 bytes)
/// * `source_addr` - Source IP address as string (for RadarDiscovery)
///
/// # Returns
/// * `Ok(RadarDiscovery)` with parsed radar information
/// * `Err(ParseError)` if packet is invalid
pub fn parse_beacon_response(data: &[u8], source_addr: &str) -> Result<RadarDiscovery, ParseError> {
    // Check minimum length
    if data.len() < 32 {
        return Err(ParseError::TooShort {
            expected: 32,
            actual: data.len(),
        });
    }

    // Check header
    if data[0..11] != BEACON_RESPONSE_HEADER {
        return Err(ParseError::InvalidHeader {
            expected: BEACON_RESPONSE_HEADER.to_vec(),
            actual: data[0..11].to_vec(),
        });
    }

    // Check for 'R' marker at position 16
    if data[16] != b'R' {
        return Err(ParseError::InvalidHeader {
            expected: vec![b'R'],
            actual: vec![data[16]],
        });
    }

    // Deserialize the structure
    let response: BeaconResponse = bincode::deserialize(data)?;

    // Check length field matches packet
    let expected_len = response.length as usize + 8;
    if expected_len != data.len() {
        return Err(ParseError::LengthMismatch {
            header_len: expected_len,
            actual_len: data.len(),
        });
    }

    // Extract name
    let name = c_string(&response.name).ok_or(ParseError::InvalidString)?;

    Ok(RadarDiscovery {
        brand: Brand::Furuno,
        model: None, // Model comes from UDP model report
        name,
        address: source_addr.to_string(),
        data_port: DATA_PORT,
        command_port: 0, // Set after TCP login
        spokes_per_revolution: SPOKES_PER_REVOLUTION,
        max_spoke_len: MAX_SPOKE_LEN,
        pixel_values: 64,
        serial_number: None,
        nic_address: None, // Set by locator
        suffix: None,
        data_address: None,
        report_address: None,
        send_address: None,
    })
}

/// Parse a model report packet (170 bytes)
///
/// # Returns
/// * `Ok((model, serial_no))` - Model name and serial number
/// * `Err(ParseError)` if packet is invalid
pub fn parse_model_report(data: &[u8]) -> Result<(Option<String>, Option<String>), ParseError> {
    if data.len() != 170 {
        return Err(ParseError::TooShort {
            expected: 170,
            actual: data.len(),
        });
    }

    let report: ModelReport = bincode::deserialize(data)?;

    let model = c_string(&report.model);
    let serial_no = c_string(&report.serial_no);

    Ok((model, serial_no))
}

/// Create the beacon request packet
pub fn create_beacon_request() -> &'static [u8] {
    &REQUEST_BEACON_PACKET
}

/// Create the model request packet
pub fn create_model_request() -> &'static [u8] {
    &REQUEST_MODEL_PACKET
}

/// Create the announce packet
pub fn create_announce_packet() -> &'static [u8] {
    &ANNOUNCE_PACKET
}

// =============================================================================
// Spoke Data Parsing
// =============================================================================

/// Spoke frame metadata parsed from header
#[derive(Debug, Clone)]
pub struct SpokeFrameHeader {
    /// Number of spokes in this frame
    pub sweep_count: u32,
    /// Length of each decoded spoke in pixels
    pub sweep_len: u32,
    /// Encoding type (0-3)
    pub encoding: u8,
    /// Whether heading is included
    pub have_heading: bool,
    /// Range index for lookup
    pub range_index: u8,
}

/// A single parsed spoke
#[derive(Debug, Clone)]
pub struct ParsedSpoke {
    /// Angle in radar units [0..8192)
    pub angle: u16,
    /// Heading in radar units (if available)
    pub heading: Option<u16>,
    /// Decoded pixel data (0-63 values, shifted right 2 bits from 0-255)
    pub data: Vec<u8>,
}

/// Check if data is a valid Furuno spoke frame
pub fn is_spoke_frame(data: &[u8]) -> bool {
    data.len() >= 16 && data[0] == 0x02
}

/// Parse spoke frame header (16 bytes)
///
/// Header format (from radar.dll reverse engineering):
/// - Byte 0: 0x02 (always)
/// - Byte 8-9: range value (v1 = (data\[8\] + (data\[9\] & 0x01) * 256) * 4 + 4)
/// - Byte 9: sweep_count = data\[9\] >> 1
/// - Byte 10-11: sweep_len = ((data\[11\] & 0x07) << 8) | data\[10\]
/// - Byte 11: encoding = (data\[11\] & 0x18) >> 3
/// - Byte 12: range_index
/// - Byte 15: have_heading = (data\[15\] & 0x30) >> 3
pub fn parse_spoke_header(data: &[u8]) -> Result<SpokeFrameHeader, ParseError> {
    if data.len() < 16 {
        return Err(ParseError::TooShort {
            expected: 16,
            actual: data.len(),
        });
    }

    if data[0] != 0x02 {
        return Err(ParseError::InvalidHeader {
            expected: vec![0x02],
            actual: vec![data[0]],
        });
    }

    let sweep_count = (data[9] >> 1) as u32;
    let sweep_len = ((data[11] & 0x07) as u32) << 8 | data[10] as u32;
    let encoding = (data[11] & 0x18) >> 3;
    let range_index = data[12];
    let have_heading = ((data[15] & 0x30) >> 3) != 0;

    Ok(SpokeFrameHeader {
        sweep_count,
        sweep_len,
        encoding,
        have_heading,
        range_index,
    })
}

/// Parse all spokes from a frame
///
/// Returns parsed spokes and updates the previous spoke buffer for delta encoding.
pub fn parse_spoke_frame(
    data: &[u8],
    prev_spoke: &mut Vec<u8>,
) -> Result<Vec<ParsedSpoke>, ParseError> {
    let header = parse_spoke_header(data)?;
    let mut spokes = Vec::with_capacity(header.sweep_count as usize);
    let sweep_len = header.sweep_len as usize;

    let mut offset = 16; // Skip header

    for sweep_idx in 0..header.sweep_count {
        if data.len() < offset + 4 {
            break; // Insufficient data for spoke header
        }

        // Read spoke angle and heading (4 bytes)
        let angle = (data[offset + 1] as u16) << 8 | data[offset] as u16;
        let heading_raw = (data[offset + 3] as u16) << 8 | data[offset + 2] as u16;
        offset += 4;

        // Decode spoke data based on encoding
        let (decoded, used) = match header.encoding {
            0 => decode_encoding_0(&data[offset..], sweep_len),
            1 => decode_encoding_1(&data[offset..], sweep_len),
            2 => {
                if sweep_idx == 0 {
                    decode_encoding_1(&data[offset..], sweep_len)
                } else {
                    decode_encoding_2(&data[offset..], prev_spoke, sweep_len)
                }
            }
            3 => decode_encoding_3(&data[offset..], prev_spoke, sweep_len),
            _ => (Vec::new(), 0),
        };
        offset += used;

        // Convert to 6-bit values (0-63) as expected by webapp
        let mut spoke_data = Vec::with_capacity(decoded.len());
        for b in &decoded {
            spoke_data.push(b >> 2);
        }

        let heading = if header.have_heading {
            Some(heading_raw)
        } else {
            None
        };

        spokes.push(ParsedSpoke {
            angle,
            heading,
            data: spoke_data,
        });

        // Update previous spoke for delta encoding
        *prev_spoke = decoded;
    }

    Ok(spokes)
}

/// Decode encoding 0 - raw data (no compression)
fn decode_encoding_0(data: &[u8], sweep_len: usize) -> (Vec<u8>, usize) {
    let len = sweep_len.min(data.len());
    (data[..len].to_vec(), len)
}

/// Decode encoding 1 - RLE with strength values
fn decode_encoding_1(data: &[u8], sweep_len: usize) -> (Vec<u8>, usize) {
    let mut spoke = Vec::with_capacity(sweep_len);
    let mut used = 0;
    let mut strength: u8 = 0;

    while spoke.len() < sweep_len && used < data.len() {
        if data[used] & 0x01 == 0 {
            // New strength value
            strength = data[used];
            spoke.push(strength);
        } else {
            // Repeat count
            let mut repeat = data[used] >> 1;
            if repeat == 0 {
                repeat = 0x80;
            }
            for _ in 0..repeat {
                if spoke.len() >= sweep_len {
                    break;
                }
                spoke.push(strength);
            }
        }
        used += 1;
    }

    // Round up to int32 boundary
    used = (used + 3) & !3;
    (spoke, used)
}

/// Decode encoding 2 - RLE with previous spoke reference
fn decode_encoding_2(data: &[u8], prev_spoke: &[u8], sweep_len: usize) -> (Vec<u8>, usize) {
    let mut spoke = Vec::with_capacity(sweep_len);
    let mut used = 0;

    while spoke.len() < sweep_len && used < data.len() {
        if data[used] & 0x01 == 0 {
            // New strength value
            spoke.push(data[used]);
        } else {
            // Repeat from previous spoke
            let mut repeat = data[used] >> 1;
            if repeat == 0 {
                repeat = 0x80;
            }
            for _ in 0..repeat {
                if spoke.len() >= sweep_len {
                    break;
                }
                let i = spoke.len();
                let strength = prev_spoke.get(i).copied().unwrap_or(0);
                spoke.push(strength);
            }
        }
        used += 1;
    }

    used = (used + 3) & !3;
    (spoke, used)
}

/// Decode encoding 3 - Combined RLE with strength and previous spoke reference
fn decode_encoding_3(data: &[u8], prev_spoke: &[u8], sweep_len: usize) -> (Vec<u8>, usize) {
    let mut spoke = Vec::with_capacity(sweep_len);
    let mut used = 0;
    let mut strength: u8 = 0;

    while spoke.len() < sweep_len && used < data.len() {
        if data[used] & 0x03 == 0 {
            // New strength value
            strength = data[used];
            spoke.push(strength);
        } else {
            let mut repeat = data[used] >> 2;
            if repeat == 0 {
                repeat = 0x40;
            }

            if data[used] & 0x01 == 0 {
                // Repeat from previous spoke
                for _ in 0..repeat {
                    if spoke.len() >= sweep_len {
                        break;
                    }
                    let i = spoke.len();
                    strength = prev_spoke.get(i).copied().unwrap_or(0);
                    spoke.push(strength);
                }
            } else {
                // Repeat current strength
                for _ in 0..repeat {
                    if spoke.len() >= sweep_len {
                        break;
                    }
                    spoke.push(strength);
                }
            }
        }
        used += 1;
    }

    used = (used + 3) & !3;
    (spoke, used)
}

/// Standard Furuno DRS-NXT range table (in meters)
/// Index 0-21 correspond to Furuno DRS range_index values
/// Note: Furuno uses non-sequential indexing!
/// Index 21 = minimum (1/16 nm), Index 15 = maximum (48 nm), Index 19 = 36 nm (out of sequence)
/// 1 nautical mile = 1852 meters
pub const RANGE_TABLE: [u32; 24] = [
    231,   // 0: 1/8 nm
    463,   // 1: 1/4 nm
    926,   // 2: 1/2 nm
    1389,  // 3: 3/4 nm
    1852,  // 4: 1 nm
    2778,  // 5: 1.5 nm
    3704,  // 6: 2 nm
    5556,  // 7: 3 nm
    7408,  // 8: 4 nm
    11112, // 9: 6 nm
    14816, // 10: 8 nm
    22224, // 11: 12 nm
    29632, // 12: 16 nm
    44448, // 13: 24 nm
    59264, // 14: 32 nm
    88896, // 15: 48 nm (maximum)
    0,     // 16: unused
    0,     // 17: unused
    0,     // 18: unused
    66672, // 19: 36 nm (out of sequence!)
    0,     // 20: unused
    116,   // 21: 1/16 nm (minimum)
    0,     // 22: unused
    0,     // 23: unused
];

/// Get range in meters from range index
pub fn get_range_meters(range_index: u8) -> u32 {
    RANGE_TABLE.get(range_index as usize).copied().unwrap_or(0)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Real packet from DRS4D-NXT
    const SAMPLE_BEACON: [u8; 32] = [
        0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x01, 0x00, 0x00,
        0x00, 0x52, 0x44, 0x30, 0x30, 0x33, 0x32, 0x31, 0x32, // "RD003212"
        0x01, 0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x12,
    ];

    #[test]
    fn test_is_beacon_response() {
        assert!(is_beacon_response(&SAMPLE_BEACON));
        assert!(!is_beacon_response(&[0u8; 16]));
    }

    #[test]
    fn test_parse_beacon_response() {
        let result = parse_beacon_response(&SAMPLE_BEACON, "172.31.6.1");
        assert!(result.is_ok());

        let discovery = result.unwrap();
        assert_eq!(discovery.brand, Brand::Furuno);
        assert_eq!(discovery.name, "RD003212");
        assert_eq!(discovery.spokes_per_revolution, 8192);
        assert_eq!(discovery.max_spoke_len, 884);
    }

    #[test]
    fn test_parse_short_packet() {
        let result = parse_beacon_response(&[0u8; 16], "172.31.6.1");
        assert!(matches!(result, Err(ParseError::TooShort { .. })));
    }

    #[test]
    fn test_parse_invalid_header() {
        let mut bad_packet = SAMPLE_BEACON;
        bad_packet[0] = 0xFF;

        let result = parse_beacon_response(&bad_packet, "172.31.6.1");
        assert!(matches!(result, Err(ParseError::InvalidHeader { .. })));
    }

    #[test]
    fn test_is_valid_furuno_ip() {
        // Valid Furuno IPs
        assert!(is_valid_furuno_ip("172.31.3.10"));
        assert!(is_valid_furuno_ip("172.31.0.1"));
        assert!(is_valid_furuno_ip("172.31.255.255"));

        // Invalid IPs
        assert!(!is_valid_furuno_ip("192.168.1.1"));
        assert!(!is_valid_furuno_ip("10.0.0.1"));
        assert!(!is_valid_furuno_ip("172.30.1.1"));
        assert!(!is_valid_furuno_ip("172.32.1.1"));
        assert!(!is_valid_furuno_ip("invalid"));
    }
}
