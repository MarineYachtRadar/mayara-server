//! Raymarine radar protocol — wire-level constants.
//!
//! Raymarine marine radars use two protocol families:
//!
//! - **Quantum** (FMCW solid-state): Q24, Q24C, Q24D, Cyclone series.
//!   Message IDs in the `0x2800xx` range, 250 spokes/revolution,
//!   up to 252 samples per spoke.
//!
//! - **RD** (magnetron): RD418/424 HD/D, Magnum, Open Array.
//!   Message IDs in the `0x0100xx` / `0x0188xx` range, 2048
//!   spokes/revolution, up to 1024 samples per spoke.
//!
//! Both protocols share the same multicast discovery on 224.0.0.1:5800.

#![allow(dead_code)]

// =============================================================================
// Discovery
// =============================================================================

/// Primary multicast discovery address (wired Ethernet).
pub const DISCOVERY_ADDRESS_WIRED: &str = "224.0.0.1:5800";

/// WiFi discovery address (Quantum WiFi radars).
pub const DISCOVERY_ADDRESS_WIFI: &str = "232.1.1.1:5800";

/// Beacon subtype for Quantum radars.
pub const BEACON_SUBTYPE_QUANTUM: u8 = 0x28;

/// Beacon subtype for RD radars.
pub const BEACON_SUBTYPE_RD: u8 = 0x01;

/// Beacon subtype for MFD identification.
pub const BEACON_SUBTYPE_MFD: u8 = 0x11;

/// Beacon subtype for W3 wireless adapter.
pub const BEACON_SUBTYPE_W3: u8 = 0x4d;

/// Model identification subtype in 56-byte beacon.
pub const BEACON_56_SUBTYPE_QUANTUM: u8 = 0x66;

// =============================================================================
// Quantum report message IDs (radar → MFD)
// =============================================================================

/// Quantum attributes/capabilities message.
pub const MSG_QUANTUM_ATTRIBUTES: u32 = 0x280001;

/// Quantum status report (controls state).
pub const MSG_QUANTUM_STATUS: u32 = 0x280002;

/// Quantum spoke (scan) data.
pub const MSG_QUANTUM_SPOKE: u32 = 0x280003;

/// Quantum radar mode report.
pub const MSG_QUANTUM_MODE: u32 = 0x280005;

/// Quantum WiFi signal strength.
pub const MSG_QUANTUM_SIGNAL: u32 = 0x280006;

/// Quantum feature flags (8 bytes: msg_id + u32 bitfield).
pub const MSG_QUANTUM_FEATURES: u32 = 0x280007;

/// Quantum parameters (per-range-channel tuning).
pub const MSG_QUANTUM_PARAMETERS: u32 = 0x280008;

/// Quantum Doppler status.
pub const MSG_QUANTUM_DOPPLER_STATUS: u32 = 0x280030;

// =============================================================================
// RD report message IDs (radar → MFD)
// =============================================================================

/// RD (analogue) status report.
pub const MSG_RD_STATUS: u32 = 0x010001;

/// RD HD status report.
pub const MSG_RD_STATUS_HD: u32 = 0x018801;

/// RD fixed capabilities report.
pub const MSG_RD_FIXED: u32 = 0x010002;

/// RD spoke (scan) data.
pub const MSG_RD_SPOKE: u32 = 0x010003;

/// RD serial number / info report.
pub const MSG_RD_INFO: u32 = 0x010006;

// =============================================================================
// Quantum command leads (first 2 bytes of command)
// =============================================================================

/// Quantum power on/off.
pub const CMD_Q_POWER: [u8; 2] = [0x10, 0x00];

/// Quantum set range.
pub const CMD_Q_RANGE: [u8; 2] = [0x01, 0x01];

/// Quantum gain mode (auto/manual).
pub const CMD_Q_GAIN_MODE: [u8; 2] = [0x01, 0x03];

/// Quantum gain value (manual).
pub const CMD_Q_GAIN_VALUE: [u8; 2] = [0x02, 0x83];

/// Quantum color gain mode.
pub const CMD_Q_COLOR_GAIN_MODE: [u8; 2] = [0x03, 0x03];

/// Quantum color gain value.
pub const CMD_Q_COLOR_GAIN_VALUE: [u8; 2] = [0x04, 0x03];

/// Quantum sea clutter mode.
pub const CMD_Q_SEA_MODE: [u8; 2] = [0x05, 0x03];

/// Quantum sea clutter value.
pub const CMD_Q_SEA_VALUE: [u8; 2] = [0x06, 0x03];

/// Quantum rain clutter enable.
pub const CMD_Q_RAIN_ENABLE: [u8; 2] = [0x0b, 0x03];

/// Quantum rain clutter value.
pub const CMD_Q_RAIN_VALUE: [u8; 2] = [0x0c, 0x03];

/// Quantum target expansion.
pub const CMD_Q_TARGET_EXPANSION: [u8; 2] = [0x0f, 0x03];

/// Quantum interference rejection.
pub const CMD_Q_INTERFERENCE_REJECTION: [u8; 2] = [0x11, 0x03];

/// Quantum operating mode (Harbor/Coastal/Offshore/Weather).
pub const CMD_Q_MODE: [u8; 2] = [0x14, 0x03];

/// Quantum Doppler mode (0x00=off, 0x03=on).
pub const CMD_Q_DOPPLER: [u8; 2] = [0x17, 0x03];

/// Quantum bearing alignment.
pub const CMD_Q_BEARING_ALIGNMENT: [u8; 2] = [0x01, 0x04];

/// Quantum main bang suppression.
pub const CMD_Q_MBS: [u8; 2] = [0x0a, 0x04];

/// Quantum sea clutter curve.
pub const CMD_Q_SEA_CURVE: [u8; 2] = [0x12, 0x03];

/// Quantum no-transmit sector 1.
pub const CMD_Q_BLANK_SECTOR_1: [u8; 2] = [0x05, 0x04];

/// Quantum no-transmit sector 2.
pub const CMD_Q_BLANK_SECTOR_2: [u8; 2] = [0x03, 0x04];

// =============================================================================
// Quantum heartbeat / keep-alive
// =============================================================================

/// 1-second keep-alive for Quantum (12 bytes, contains "Radar").
pub const HEARTBEAT_QUANTUM_1S: [u8; 12] = [
    0x00, 0x00, 0x28, 0x00, 0x52, 0x61, 0x64, 0x61, 0x72, 0x00, 0x00, 0x00,
];

/// 5-second extended keep-alive for Quantum (36 bytes).
pub const HEARTBEAT_QUANTUM_5S: [u8; 36] = [
    0x03, 0x89, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9e, 0x03, 0x00, 0x00, 0xb4, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// 1-second keep-alive for RD/E120 (12 bytes, contains "RADAR").
pub const HEARTBEAT_RD_1S: [u8; 12] = [
    0x00, 0x80, 0x01, 0x00, 0x52, 0x41, 0x44, 0x41, 0x52, 0x00, 0x00, 0x00,
];

/// 5-second extended keep-alive for RD/E120 (36 bytes).
pub const HEARTBEAT_RD_5S: [u8; 36] = [
    0x03, 0x89, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x68, 0x01, 0x00, 0x00, 0x9e, 0x03, 0x00, 0x00, 0xb4, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

// =============================================================================
// Feature flags (from 0x280007 Features message)
// =============================================================================

pub const FEATURE_ANALOGUE: u32 = 1 << 0;
pub const FEATURE_DIGITAL: u32 = 1 << 2;
pub const FEATURE_QUANTUM: u32 = 1 << 4;
pub const FEATURE_EDOME: u32 = 1 << 9;
pub const FEATURE_BIRD_MODE: u32 = 1 << 10;
pub const FEATURE_NO_DUAL_RANGE_RESTRICTIONS: u32 = 1 << 11;
pub const FEATURE_MARPA: u32 = 1 << 14;
pub const FEATURE_DUAL_RANGE_MARPA: u32 = 1 << 16;
pub const FEATURE_MARPA_BEYOND_12NM: u32 = 1 << 17;
pub const FEATURE_AUTO_RAIN: u32 = 1 << 18;
pub const FEATURE_DOPPLER: u32 = 1 << 19;
pub const FEATURE_DOPPLER_AUTO_ACQUIRE: u32 = 1 << 20;
pub const FEATURE_CYCLONE: u32 = 1 << 23;
pub const FEATURE_DOPPLER_BIRD_MODE: u32 = 1 << 25;

// =============================================================================
// Spoke geometry
// =============================================================================

/// Quantum: 250 spokes per revolution.
pub const QUANTUM_SPOKES_PER_REVOLUTION: u16 = 250;

/// RD: 2048 spokes per revolution.
pub const RD_SPOKES_PER_REVOLUTION: u16 = 2048;

/// Quantum: maximum 252 samples per spoke.
pub const QUANTUM_MAX_SPOKE_LEN: usize = 252;

/// RD HD: maximum 1024 samples per spoke.
pub const RD_HD_MAX_SPOKE_LEN: usize = 1024;

/// RD non-HD: 512 samples per spoke.
pub const RD_MAX_SPOKE_LEN: usize = 512;

/// RLE escape byte in spoke data.
pub const RLE_MARKER: u8 = 0x5c;

// =============================================================================
// Doppler pixel encoding
// =============================================================================

/// Spoke pixel value for Doppler receding targets.
pub const DOPPLER_RECEDING: u8 = 0xfe;

/// Spoke pixel value for Doppler approaching targets.
pub const DOPPLER_APPROACHING: u8 = 0xff;

// =============================================================================
// NavData message
// =============================================================================

/// NavData sub-ID (prepended to the 32-byte payload).
pub const NAVDATA_SUB_ID: u32 = 0x28000018;

/// NavData interval.
pub const NAVDATA_INTERVAL_MS: u64 = 100;
