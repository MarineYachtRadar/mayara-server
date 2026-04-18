//! Koden RADARpc radar protocol — wire-level constants.
//!
//! The protocol is shared by all Koden Ethernet control boxes: MDS-5R,
//! MDS-6R, and MDS-11R.
//!
//! ## Transport
//!
//! All communication uses **UDP port 10001** — discovery, control, keep-alive,
//! and spoke image data share the same port.
//!
//! ## Packet framing
//!
//! Three packet types, distinguished by the first byte:
//!
//! - `&` (0x26) — control commands and responses (warmup, power, error)
//! - `#` (0x23) — status/setting responses
//! - `{{{{` (4 × 0x7B) — image data (spoke frames)
//!
//! Control and status packets are terminated by `\r` (0x0D).

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

// =============================================================================
// Spoke geometry
// =============================================================================

/// Koden radars report spokes with angles relative to total_spokes_per_revolution
/// (typically 4096). We normalise to this internal count.
pub(crate) const SPOKES: usize = 4096;

/// Maximum spoke length in pixels. Koden sends 240, 480, or 960
/// samples per spoke depending on range; spokes are passed at their
/// native resolution without stretching.
pub(crate) const SPOKE_LEN: usize = 960;

/// Number of echo intensity levels. Koden sends raw 8-bit values, but we scale to 128
/// to fit into our standard spoke format and preserve headroom for other pixel values.
pub(crate) const PIXEL_VALUES: u8 = 128;

// =============================================================================
// Network
// =============================================================================

/// UDP port for all Koden radar communication.
pub(crate) const RADAR_PORT: u16 = 10001;

/// Broadcast address for radar discovery.
/// We listen on 0.0.0.0:10001 and broadcast to 255.255.255.255:10001.
pub(crate) const BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), RADAR_PORT);

// =============================================================================
// Timing
// =============================================================================

/// Keep-alive interval in seconds.
pub(crate) const KEEPALIVE_INTERVAL_SECS: u64 = 10;

/// Radar is declared lost after this many seconds of silence.
pub(crate) const RADAR_LOST_TIMEOUT_SECS: u64 = 15;

// =============================================================================
// Packet start/end markers
// =============================================================================

pub(crate) const CONTROL_PREFIX: u8 = b'&'; // 0x26
pub(crate) const STATUS_PREFIX: u8 = b'#'; // 0x23
pub(crate) const IMAGE_MARKER: [u8; 4] = [b'{', b'{', b'{', b'{'];
pub(crate) const PACKET_END: u8 = b'\r'; // 0x0D

// =============================================================================
// Boolean values on the wire
// =============================================================================

pub(crate) const WIRE_FALSE: u8 = 0x00;
pub(crate) const WIRE_TRUE: u8 = 0x11;
pub(crate) const WIRE_AUTO: u8 = 0x22;

// =============================================================================
// Control response types (byte 1 of `&` packets)
// =============================================================================

pub(crate) const RESP_WARMUP: u8 = b'a';
pub(crate) const RESP_ERROR: u8 = b'e';
pub(crate) const RESP_POWER: u8 = b'p';

/// Warmup-complete sentinel (byte 2 of `&a` packet).
pub(crate) const WARMUP_COMPLETE: u8 = 0x11;

// =============================================================================
// Status command IDs (byte 1 of `#` packets, sent TO and FROM radar)
// =============================================================================

pub(crate) const CMD_HEADING_LINE: u8 = 0x30;
pub(crate) const CMD_TUNING_MODE: u8 = 0x41;
pub(crate) const CMD_TRIGGER_DELAY: u8 = 0x44;
pub(crate) const CMD_TARGET_EXPANSION: u8 = 0x45;
pub(crate) const CMD_FTC: u8 = 0x46;
pub(crate) const CMD_GAIN: u8 = 0x47;
pub(crate) const CMD_INTERFERENCE_REJECTION: u8 = 0x49;
pub(crate) const CMD_MODEL_INFO: u8 = 0x4E;
pub(crate) const CMD_STC: u8 = 0x53;
pub(crate) const CMD_FINE_TUNING: u8 = 0x54;
pub(crate) const CMD_COARSE_TUNING: u8 = 0x55;
pub(crate) const CMD_TUNING_METER: u8 = 0x63;
pub(crate) const CMD_WRITE_IP: u8 = 0x64;
pub(crate) const CMD_SYSTEM_ERROR: u8 = 0x65;
pub(crate) const CMD_AUTO_GAIN_MODE: u8 = 0x67;
pub(crate) const CMD_TRANSFER_MODE: u8 = 0x69;
pub(crate) const CMD_MODEL_CODE: u8 = 0x72;
pub(crate) const CMD_TRANSMISSION_MODE: u8 = 0x74;

// =============================================================================
// Model code → model name mapping
// =============================================================================

/// Map a model code byte (from CMD_MODEL_CODE response) to a model name.
///
/// Map a model code byte to a human-readable model name.
pub(crate) fn model_name(code: u8) -> &'static str {
    match code {
        0 => "MDS-50R 2kW Dome",
        1 => "MDS-51R 4kW Dome",
        2 => "MDS-52R 4kW Open Array",
        3 => "MDS-61R 6kW Open Array",
        4 => "MDS-62R 12kW Open Array",
        5 => "MDS-63R 25kW Open Array",
        6 => "MDS-1R/8R 2kW Dome",
        10 => "MDS-10R 4kW Open Array",
        14 => "MDS-9R 4kW Dome",
        15 => "MDS-5R Interface",
        _ => "Unknown",
    }
}
pub(crate) const CMD_AUTO_STC_MODE: u8 = 0x80;
pub(crate) const CMD_AUTO_GAIN_PRESET: u8 = 0x81;
pub(crate) const CMD_MANUAL_GAIN_PRESET: u8 = 0x82;
pub(crate) const CMD_AUTO_STC_PRESET: u8 = 0x83;
pub(crate) const CMD_MANUAL_STC_PRESET: u8 = 0x84;
pub(crate) const CMD_AUTO_TUNE_PRESET: u8 = 0x85;
pub(crate) const CMD_MANUAL_TUNE_PRESET: u8 = 0x86;
pub(crate) const CMD_HARBOR_STC_PRESET: u8 = 0x87;
pub(crate) const CMD_STC_CURVE_PRESET: u8 = 0x88;
pub(crate) const CMD_INFO_BLOCK: u8 = 0x9A;
pub(crate) const CMD_BLANKING_SECTOR: u8 = 0x9C;
pub(crate) const CMD_PULSE_LENGTH: u8 = 0xA4;
pub(crate) const CMD_ANTENNA_SPEED: u8 = 0xA5;
pub(crate) const CMD_MAC_ADDRESS: u8 = 0xA7;
pub(crate) const CMD_SET_IP: u8 = 0xA8;
pub(crate) const CMD_RECEIVED_IP: u8 = 0xA9;
pub(crate) const CMD_PARK_ANGLE: u8 = 0xAA;
pub(crate) const CMD_KEEPALIVE_ACK: u8 = 0xAB;
pub(crate) const CMD_DEMO_MODE: u8 = 0xAC;
pub(crate) const CMD_TRANSFER_SIZE: u8 = 0xAD;
pub(crate) const CMD_BROADCAST_SETTING: u8 = 0xFF;

// =============================================================================
// Startup packet sequence
// =============================================================================

/// Information request sent after radar responds to discovery.
/// Five 4-byte sub-commands packed into one 20-byte send.
pub(crate) const STARTUP_REQUEST: [u8; 20] = [
    0x26, 0xFF, 0x11, 0x0D, // Broadcast setting = 0x11
    0x26, 0x9A, 0x11, 0x0D, // Request info block
    0x26, 0xA7, 0x11, 0x0D, // Request MAC address
    0x24, 0x4E, 0x11, 0x0D, // Request model/serial info
    0x26, 0x72, 0xFF, 0x0D, // Request model code
];

/// Keep-alive packet (4 bytes).
pub(crate) const KEEPALIVE_PACKET: [u8; 4] = [0x26, 0x69, 0x22, 0x0D];

// =============================================================================
// Image header offsets (from start of `{{{{` packet)
// =============================================================================

pub(crate) const IMG_TRANSFER_TYPE: usize = 0x0E;
pub(crate) const IMG_STATUS_CHANGED: usize = 0x0F;
pub(crate) const IMG_RANGE_INDEX: usize = 0x10;
pub(crate) const IMG_RANGE_INDEX_2: usize = 0x11;
pub(crate) const IMG_NUM_SPOKES: usize = 0x25;
pub(crate) const IMG_SAMPLES_PER_SPOKE_LO: usize = 0x26;
pub(crate) const IMG_SAMPLES_PER_SPOKE_HI: usize = 0x27;
pub(crate) const IMG_TOTAL_SPOKES_LO: usize = 0x28;
pub(crate) const IMG_TOTAL_SPOKES_HI: usize = 0x29;
pub(crate) const IMG_START_ANGLE_LO: usize = 0x2A;
pub(crate) const IMG_START_ANGLE_HI: usize = 0x2B;
pub(crate) const IMG_SPOKE_DATA: usize = 0x2C;

/// Offset of spoke data for 'R' (rotated) transfer type.
pub(crate) const IMG_SPOKE_DATA_ROTATED: usize = 0xA8;

/// Transfer type values.
pub(crate) const TRANSFER_NORMAL: u8 = 2;
pub(crate) const TRANSFER_ROTATED: u8 = 0x52; // 'R'

/// Minimum image packet size (header only, no spoke data).
pub(crate) const IMG_MIN_SIZE: usize = 0x2C;

// =============================================================================
// Range table
// =============================================================================

/// Range table: index → nautical miles.
/// 20 entries mapping range index to nautical miles.
pub(crate) const RANGE_TABLE_NM: [f64; 20] = [
    0.125, 0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0, 36.0, 48.0,
    64.0, 72.0, 96.0,
];

const NM_TO_METERS: f64 = 1852.0;

/// Convert a range index to meters.
pub(crate) fn range_index_to_meters(index: u8) -> f64 {
    let idx = index as usize;
    if idx < RANGE_TABLE_NM.len() {
        RANGE_TABLE_NM[idx] * NM_TO_METERS
    } else {
        // Fallback for unknown indices
        RANGE_TABLE_NM[RANGE_TABLE_NM.len() - 1] * NM_TO_METERS
    }
}

// =============================================================================
// Spoke sample count mapping
// =============================================================================

/// Map the wire samples_per_spoke to the actual pixel count.
pub(crate) fn actual_spoke_pixels(wire_samples: u16) -> u16 {
    match wire_samples {
        0x100 => 0xF0,  // 256 → 240
        0x200 => 0x1E0, // 512 → 480
        0x400 => 0x3C0, // 1024 → 960
        _ => 0,
    }
}

pub(crate) const KODEN_ANGLE_SCALE: f64 = 1024.0 / 360.0;

/// Koden wire angle (0–1023) to radians (0–2π).
pub(crate) fn koden_angle_to_radians(angle: u16) -> f64 {
    (angle as f64) * std::f64::consts::TAU / 1024.0
}

/// Radians to Koden wire angle (0–1023, unsigned).
/// Accepts any input range; the result is normalized to 0–1023.
pub(crate) fn radians_to_koden_angle(rad: f64) -> u16 {
    let normalized = rad.rem_euclid(std::f64::consts::TAU);
    (normalized * 1024.0 / std::f64::consts::TAU).round() as u16 % 1024
}
