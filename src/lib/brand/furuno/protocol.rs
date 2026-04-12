//! Furuno NavNet radar protocol — wire-level constants.
//!
//! Opcodes, multicast addresses, ports, packet structures, wire index tables,
//! and frame header bitmasks for the Furuno NavNet radar family.
//!
//! Supported models: DRS4W, DRS4D-NXT, DRS6A-NXT, DRS12A-NXT, DRS25A-NXT,
//! DRS, DRS4DL, DRS6A X-Class, FAR-21x7, FAR-14x7, FAR-14x6, FAR-15x3,
//! FAR-3000.
//!
//! ## Terminology
//!
//! - **Wire index**: Firmware range slot number sent in the `$S62` Range
//!   command. Non-sequential — see [`WIRE_INDEX_TABLE`].
//! - **Wire unit**: Distance unit on the wire: 0 = NM, 1 = km.
//! - **Command mode**: The single ASCII character following `$` in every
//!   command: S (Set), R (Request), N (New/notification).
//! - **Command ID**: Two hex-digit opcode after the mode character, e.g.
//!   `$S63` = Set Gain.
//! - **IMO echo format**: Spoke data encoding used by all models. Three
//!   compression modes (1–3) plus raw (mode 0).
//! - **Scale**: Number of spoke samples that map to the display range
//!   (header bytes 14–15). Samples beyond scale are outside the display range.

#![allow(dead_code)]

use enum_primitive_derive::Primitive;
use serde::Deserialize;
use std::fmt::{self, Display};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

// =============================================================================
// Spoke geometry
// =============================================================================

/// Total number of spokes per revolution (13-bit angle, 0–8191).
pub const SPOKES: usize = 8192;

/// Maximum spoke length in pixels delivered to the GUI.
///
/// Must be at least as large as any radar's native `sweep_len` (DRS4D-NXT
/// reports 884). Shorter native spokes are stretched to this size by
/// `stretch_spoke` so the inner/outer scale stays consistent. 1024 is a
/// round upper bound that accommodates all known Furuno models.
pub const SPOKE_LEN: usize = 1024;

/// Number of colors in the echo palette.
pub const PIXEL_VALUES: u8 = 64;

// =============================================================================
// Network — ports and addresses
// =============================================================================

/// Base port for all Furuno NavNet services (configurable in firmware, but
/// always 10000 in shipping products).
pub const BASE_PORT: u16 = 10000;

/// UDP beacon discovery port (`BASE_PORT + 10`).
pub const BEACON_PORT: u16 = BASE_PORT + 10;

/// UDP spoke echo data port (`BASE_PORT + 24`).
pub const DATA_PORT: u16 = BASE_PORT + 24;

/// Broadcast address for beacon discovery.
pub const BEACON_ADDRESS: SocketAddr = SocketAddr::new(
    IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)),
    BEACON_PORT,
);

/// Broadcast address for spoke echo data (used by DRS4W WiFi radar).
pub const DATA_BROADCAST_ADDRESS: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(172, 31, 255, 255), DATA_PORT);

/// Multicast address for spoke echo data (used by wired DRS/NXT/FAR models).
pub const SPOKE_DATA_MULTICAST_ADDRESS: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 2), DATA_PORT);

// =============================================================================
// Discovery and beacon packets
// =============================================================================

/// 32-byte packet announcing this software to the radar.
/// Contains embedded ASCII `"MAYARA"` at bytes 16–21.
pub const ANNOUNCE_MAYARA_PACKET: [u8; 32] = [
    0x1, 0x0, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0, 0x18, 0x1, 0x0, 0x0, 0x0, b'M', b'A',
    b'Y', b'A', b'R', b'A', 0x0, 0x0, 0x1, 0x1, 0x0, 0x2, 0x0, 0x1, 0x0, 0x12,
];

/// 16-byte beacon request packet. Byte 8 = `0x01` (beacon request type).
pub const REQUEST_BEACON_PACKET: [u8; 16] = [
    0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x08, 0x01, 0x00, 0x00,
    0x00,
];

/// 16-byte model request packet. Byte 8 = `0x14` (model request type).
pub const REQUEST_MODEL_PACKET: [u8; 16] = [
    0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x14, 0x01, 0x00, 0x08, 0x01, 0x00, 0x00,
    0x00,
];

/// Expected header bytes in the 32-byte beacon report (bytes 0–10).
pub const BEACON_REPORT_HEADER: [u8; 11] =
    [0x1, 0x0, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0];

/// Minimum beacon report size (= `size_of::<FurunoRadarReport>()`).
pub const BEACON_REPORT_LENGTH_MIN: usize = std::mem::size_of::<FurunoRadarReport>();

/// Fixed length of the 170-byte model report.
pub const MODEL_REPORT_LENGTH: usize = 170;

// =============================================================================
// Login protocol
// =============================================================================

/// TCP connect / read / write timeout for the COPYRIGHT login handshake.
pub const LOGIN_TIMEOUT: Duration = Duration::from_millis(500);

/// 56-byte login message sent via TCP to port 10010.
///
/// From `fnet.dll` function `login_via_copyright`. Byte 9 selects the service
/// (1 = Radar). The embedded ASCII payload starting at byte 12 reads:
/// `"COPYRIGHT (C) 2001 FURUNO ELECTRIC CO.,LTD. "`.
pub const LOGIN_MESSAGE: [u8; 56] = [
    //                                              v- byte 9: service ID (1=Radar)
    0x8, 0x1, 0x0, 0x38, 0x1, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0, 0x0, 0x43, 0x4f, 0x50, 0x59, 0x52,
    0x49, 0x47, 0x48, 0x54, 0x20, 0x28, 0x43, 0x29, 0x20, 0x32, 0x30, 0x30, 0x31, 0x20, 0x46,
    0x55, 0x52, 0x55, 0x4e, 0x4f, 0x20, 0x45, 0x4c, 0x45, 0x43, 0x54, 0x52, 0x49, 0x43, 0x20,
    0x43, 0x4f, 0x2e, 0x2c, 0x4c, 0x54, 0x44, 0x2e, 0x20,
];

/// Expected 8-byte reply header from the radar after sending [`LOGIN_MESSAGE`].
/// The 4 bytes following this header contain the big-endian port offset.
pub const LOGIN_EXPECTED_HEADER: [u8; 8] = [0x9, 0x1, 0x0, 0xc, 0x1, 0x0, 0x0, 0x0];

// =============================================================================
// Wire-format report structures
// =============================================================================

// DRS-4D NXT
// [01, 00, 00, 01, 00, 00, 00, 00, 00, 01, 00, 18, 01, 00, 00, 00, 52, 44, 30, 30, 33, 32, 31, 32, 01, 01, 00, 02, 00, 01, 00, 12] len 32
// [ .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   R   D   0   0   3   2   1   2   .   .   .   .   .   .   .   .]
//                                               ^__length           ^_name, always 8 long?
// FAR 2127
// [01, 00, 00, 01, 00, 00, 00, 00, 00, 01, 00, 1A, 01, 00, 00, 00, 52, 41, 44, 41, 52, 00, 00, 00, 01, 00, 00, 03, 00, 01, 00, 04, 00, 05] len 34
// [ .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   R   A   D   A   R   .   .   .   .   .   .   .   .   .   .   .   .   .]
//
// TimeZero
// [01, 00, 00, 01, 00, 00, 00, 00, 00, 01, 00, 1C, 01, 00, 00, 00, 4D, 46, 30, 30, 33, 31, 35, 30, 01, 01, 00, 04, 00, 0B, 00, 15, 00, 14, 00, 16] len 36
// [ .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   M   F   0   0   3   1   5   0   .   .   .   .   .   .   .   .   .   .   .   .]

/// 32-byte beacon report — radar serial/name identification.
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
pub struct FurunoRadarReport {
    pub _header: [u8; 11],
    pub length: u8,
    pub _filler2: [u8; 4],
    pub name: [u8; 8],
}

/// 170-byte model report — radar model name, firmware versions, serial number.
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
pub struct FurunoRadarModelReport {
    pub _filler1: [u8; 24],
    pub model: [u8; 32],
    pub _firmware_versions: [u8; 32],
    pub _firmware_version: [u8; 32],
    pub serial_no: [u8; 32],
    pub _filler2: [u8; 18],
}

// =============================================================================
// Radar model identification
// =============================================================================

/// All known Furuno radar models.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RadarModel {
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
    DRS4W,
    DRS12ANXT,
    DRS25ANXT,
}

impl Display for RadarModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s: &'static str = match self {
            RadarModel::Unknown => "Unknown",
            RadarModel::FAR21x7 => "FAR21x7",
            RadarModel::DRS => "DRS",
            RadarModel::FAR14x7 => "FAR14x7",
            RadarModel::DRS4DL => "DRS4DL",
            RadarModel::FAR3000 => "FAR3000",
            RadarModel::DRS4DNXT => "DRS4DNXT",
            RadarModel::DRS6ANXT => "DRS6ANXT",
            RadarModel::DRS6AXCLASS => "DRS6AXCLASS",
            RadarModel::FAR15x3 => "FAR15x3",
            RadarModel::FAR14x6 => "FAR14x6",
            RadarModel::DRS4W => "DRS4W",
            RadarModel::DRS12ANXT => "DRS12ANXT",
            RadarModel::DRS25ANXT => "DRS25ANXT",
        };

        write!(f, "{}", s)
    }
}

impl RadarModel {
    /// Map a 7-digit firmware part number from the `$N96` Modules response to a
    /// [`RadarModel`]. Returns [`RadarModel::Unknown`] for unrecognized codes.
    ///
    /// Source: `Fec.Wrapper.SensorProperty.GetRadarSensorType` (TimeZero).
    pub fn from_part_number(part: &str) -> RadarModel {
        match part {
            "0359235" => RadarModel::DRS,
            "0359255" => RadarModel::FAR14x7,
            "0359204" => RadarModel::FAR21x7,
            "0359321" => RadarModel::FAR14x7,
            "0359338" => RadarModel::DRS4DL,
            "0359367" => RadarModel::DRS4DL,
            "0359281" => RadarModel::FAR3000,
            "0359286" => RadarModel::FAR3000,
            "0359477" => RadarModel::FAR3000,
            "0359360" => RadarModel::DRS4DNXT,
            "0359421" => RadarModel::DRS6ANXT,
            "0359329" => RadarModel::DRS4W,
            "0359355" => RadarModel::DRS6AXCLASS,
            "0359344" => RadarModel::FAR15x3,
            "0359397" => RadarModel::FAR14x6,
            "0359560" => RadarModel::FAR21x7,
            _ => RadarModel::Unknown,
        }
    }
}

// =============================================================================
// Command mode
// =============================================================================

/// ASCII mode character prefixing every NavNet command.
pub enum CommandMode {
    /// `'S'` — Set (write a control value).
    Set,
    /// `'R'` — Request (read a control value).
    Request,
    /// `'N'` — New/Notification (radar → client).
    New,
    /// `'X'` — Reserved/extended (no arguments).
    X,
    /// `'E'` — Reserved/event (no arguments).
    E,
    /// `'O'` — Reserved/option (no arguments).
    O,
}

impl CommandMode {
    pub fn to_char(&self) -> char {
        match self {
            CommandMode::Set => 'S',
            CommandMode::Request => 'R',
            CommandMode::New => 'N',
            CommandMode::X => 'X',
            CommandMode::E => 'E',
            CommandMode::O => 'O',
        }
    }
}

impl From<u8> for CommandMode {
    fn from(item: u8) -> Self {
        match item {
            b'S' => CommandMode::Set,
            b'R' => CommandMode::Request,
            b'N' => CommandMode::New,
            b'X' => CommandMode::X,
            b'E' => CommandMode::E,
            b'O' => CommandMode::O,
            _ => CommandMode::New,
        }
    }
}

// =============================================================================
// Command IDs (opcodes)
// =============================================================================

/// Two-digit hex opcode following the mode character in `$<mode><hex>,...`.
#[derive(Primitive, PartialEq, Eq, Debug, Clone)]
pub enum CommandId {
    /// `0x60` — Connection control.
    Connect = 0x60,
    /// `0x61` — Display mode.
    DispMode = 0x61,
    /// `0x62` — Range selection: `$S62,<wire_index>,<wire_unit>,<drid>`.
    Range = 0x62,
    /// `0x63` — Gain control: `$S63,<auto>,<value>,<drid>,<auto_val>,0`.
    Gain = 0x63,
    /// `0x64` — Sea clutter: `$S64,<auto>,<value>,<auto_val>,<drid>,0,0`.
    Sea = 0x64,
    /// `0x65` — Rain clutter: `$S65,<mode>,<value>,0,<drid>,0,0`.
    Rain = 0x65,
    /// `0x66` — Custom picture (requested after every set).
    CustomPictureAll = 0x66,
    /// `0x67` — Signal processing (multi-purpose): feature 0 = IR, feature 3 = NR.
    SignalProcessing = 0x67,
    /// `0x68` — Pulse width.
    PulseWidth = 0x68,
    /// `0x69` — Power status / timed idle (watchman).
    Status = 0x69,
    /// `0x6D` — Unknown.
    U6D = 0x6D,
    /// `0x6E` — Antenna type (read-only, 7 params).
    AntennaType = 0x6E,

    /// `0x70` — Guard zone alarm status: `$N70,<count>,<status0>,<status1>`.
    GuardStatus = 0x70,

    /// `0x75` — Tuning: `$S75,<auto>,<value>,<drid>`.
    Tune = 0x75,
    /// `0x76` — Tune indicator feedback (read-only).
    TuneIndicator = 0x76,
    /// `0x77` — No-transmit sector (sector blanking).
    BlindSector = 0x77,
    /// `0x7D` — Radar alarm: `$N7D,<type>,<d1>,<d2>,<d3>`.
    /// Generic across all Furuno models; idle = `$N7D,0,0,0,0`.
    Alarm = 0x7D,

    /// `0x80` — Attenuation.
    Att = 0x80,
    /// `0x83` — Main bang suppression.
    MainBangSize = 0x83,
    /// `0x84` — Antenna height.
    AntennaHeight = 0x84,
    /// `0x85` — Near STC curve.
    NearSTC = 0x85,
    /// `0x86` — Middle STC curve.
    MiddleSTC = 0x86,
    /// `0x87` — Far STC curve.
    FarSTC = 0x87,
    /// `0x88` — Ring suppression.
    RingSuppression = 0x88,
    /// `0x89` — Scan speed (NXT: 0 = 24 RPM, 2 = Auto).
    ScanSpeed = 0x89,
    /// `0x8A` — Antenna switch.
    AntennaSwitch = 0x8A,
    /// `0x8D` — Antenna number.
    AntennaNo = 0x8D,
    /// `0x8E` — Operating hours (read-only).
    OnTime = 0x8E,
    /// `0x8F` — Transmit hours (read-only).
    TxTime = 0x8F,

    /// `0x96` — Firmware/model query.
    Modules = 0x96,
    /// `0x98` — Guard zone mode: `$S98,<mode>,<param>,<zoneIndex>`.
    GuardMode = 0x98,
    /// `0x99` — Guard zone fan parameters:
    /// `$S99,<zoneNo>,<startAngle>,<endAngle>,<innerRange>,<outerRange>`.
    GuardFan = 0x99,

    /// `0x9E` — Drift.
    Drift = 0x9E,
    /// `0xA3` — Trail mode.
    TrailMode = 0xA3,
    /// `0xAA` — Conning position.
    ConningPosition = 0xAA,
    /// `0xAC` — Wake-up count.
    WakeUpCount = 0xAC,
    /// `0xAF` — ARPA subsystem alarm/status bitmask: `$NAF,<bits>`.
    ArpaAlarm = 0xAF,

    /// `0xD2` — STC range.
    STCRange = 0xD2,
    /// `0xD3` — Custom memory.
    CustomMemory = 0xD3,
    /// `0xD4` — Build-up time.
    BuildUpTime = 0xD4,
    /// `0xD5` — Display unit information.
    DisplayUnitInformation = 0xD5,
    /// `0xE1` — Trail processing.
    TrailProcess = 0xE1,
    /// `0xE0` — Custom ATF settings.
    CustomATFSettings = 0xE0,
    /// `0xE3` — Alive check (keepalive ping).
    AliveCheck = 0xE3,
    /// `0xEA` — ATF settings (NXT, 6 params).
    ATFSettings = 0xEA,
    /// `0xED` — Bird mode (NXT: 0 = Off, 1 = Low, 2 = Med, 3 = High).
    BirdMode = 0xED,
    /// `0xEE` — Target Separation / RezBoost (beam sharpening).
    RezBoost = 0xEE,
    /// `0xEF` — Target Analyzer / Doppler mode.
    TargetAnalyzer = 0xEF,
    /// `0xF0` — Auto acquire.
    AutoAcquire = 0xF0,
    /// `0xF5` — NN3 hardware diagnostics (frequent, read-only).
    NN3Command = 0xF5,
    /// `0xFE` — Range select (24-arg form).
    RangeSelect = 0xFE,
}

// =============================================================================
// Wire range index tables and conversions
// =============================================================================

/// Wire unit value for nautical miles.
pub const WIRE_UNIT_NM: i32 = 0;

/// Wire unit value for kilometres.
pub const WIRE_UNIT_KM: i32 = 1;
// Wire unit 2 = SM (statute miles), 3 = Kyd (kilo-yards) — not yet implemented

/// Wire index → meters mapping (NM mode, wire unit 0).
///
/// Wire indices are **non-sequential**. The radar uses specific slot numbers
/// that do not correspond to a sorted range order. Verified via Wireshark
/// captures from TimeZero ↔ DRS4D-NXT.
pub const WIRE_INDEX_TABLE: [(i32, i32); 22] = [
    (21, 116),    // 1/16 nm = 116m - wire index 21!
    (0, 231),     // 1/8 nm = 231m
    (1, 463),     // 1/4 nm = 463m
    (2, 926),     // 1/2 nm = 926m
    (3, 1389),    // 3/4 nm = 1389m
    (4, 1852),    // 1 nm = 1852m
    (5, 2778),    // 1.5 nm = 2778m
    (6, 3704),    // 2 nm = 3704m
    (7, 5556),    // 3 nm = 5556m
    (8, 7408),    // 4 nm = 7408m
    (9, 11112),   // 6 nm = 11112m
    (10, 14816),  // 8 nm = 14816m
    (11, 22224),  // 12 nm = 22224m
    (12, 29632),  // 16 nm = 29632m
    (13, 44448),  // 24 nm = 44448m
    (14, 59264),  // 32 nm = 59264m
    (19, 66672),  // 36 nm = 66672m (OUT OF SEQUENCE!)
    (15, 88896),  // 48 nm = 88896m
    (20, 118528), // 64 nm = 118528m (OUT OF SEQUENCE!)
    (16, 133344), // 72 nm = 133344m
    (17, 177792), // 96 nm = 177792m
    (18, 222240), // 120 nm = 222240m
];

/// Wire index → meters mapping (km mode, wire unit 1).
/// Wire index 21 (0.0625 km) is **not** available in km mode.
pub const WIRE_INDEX_TABLE_KM: [(i32, i32); 21] = [
    (0, 125),     // 0.125 km
    (1, 250),     // 0.25 km
    (2, 500),     // 0.5 km
    (3, 750),     // 0.75 km
    (4, 1000),    // 1 km
    (5, 1500),    // 1.5 km
    (6, 2000),    // 2 km
    (7, 3000),    // 3 km
    (8, 4000),    // 4 km
    (9, 6000),    // 6 km
    (10, 8000),   // 8 km
    (11, 12000),  // 12 km
    (12, 16000),  // 16 km
    (13, 24000),  // 24 km
    (14, 32000),  // 32 km
    (19, 36000),  // 36 km (OUT OF SEQUENCE!)
    (15, 48000),  // 48 km
    (20, 64000),  // 64 km (OUT OF SEQUENCE!)
    (16, 72000),  // 72 km
    (17, 96000),  // 96 km
    (18, 120000), // 120 km
];

/// Convert meters to the nearest wire index (NM table).
pub fn meters_to_wire_index(meters: i32) -> i32 {
    lookup_wire_index(&WIRE_INDEX_TABLE, meters)
}

/// Convert meters to the nearest wire index (km table).
pub fn meters_to_wire_index_km(meters: i32) -> i32 {
    lookup_wire_index(&WIRE_INDEX_TABLE_KM, meters)
}

/// Convert meters to the nearest wire index for the given wire unit.
pub fn meters_to_wire_index_for_unit(meters: i32, wire_unit: i32) -> i32 {
    match wire_unit {
        WIRE_UNIT_KM => meters_to_wire_index_km(meters),
        _ => meters_to_wire_index(meters),
    }
}

fn lookup_wire_index(table: &[(i32, i32)], meters: i32) -> i32 {
    table
        .iter()
        .min_by_key(|(_, m)| (i64::from(*m) - i64::from(meters)).abs())
        .map(|(idx, _)| *idx)
        .unwrap_or(15)
}

/// Convert a wire index to meters (NM table).
pub fn wire_index_to_meters(wire_index: i32) -> Option<i32> {
    WIRE_INDEX_TABLE
        .iter()
        .find(|(idx, _)| *idx == wire_index)
        .map(|(_, meters)| *meters)
}

/// Convert a wire index to meters (km table).
pub fn wire_index_to_meters_km(wire_index: i32) -> Option<i32> {
    WIRE_INDEX_TABLE_KM
        .iter()
        .find(|(idx, _)| *idx == wire_index)
        .map(|(_, meters)| *meters)
}

/// Convert a wire index to meters for the given wire unit.
pub fn wire_index_to_meters_for_unit(wire_index: i32, wire_unit: i32) -> Option<i32> {
    match wire_unit {
        WIRE_UNIT_KM => wire_index_to_meters_km(wire_index),
        _ => wire_index_to_meters(wire_index),
    }
}

/// Determine the wire unit for a range value in meters.
/// Metric distances (km-based) use wire unit 1, nautical use wire unit 0.
pub fn wire_unit_for_meters(meters: i32) -> i32 {
    if WIRE_INDEX_TABLE_KM.iter().any(|(_, m)| *m == meters) {
        if crate::radar::range::Range::is_metric_distance(meters) {
            return WIRE_UNIT_KM;
        }
    }
    WIRE_UNIT_NM
}

// =============================================================================
// Spoke frame header — field offsets and bitmasks
// =============================================================================
//
// 16-byte IMO frame header layout:
//   [0]    packet_type (must be FRAME_MAGIC)
//   [1]    sequence_number
//   [2-3]  total_length (big-endian)
//   [4-7]  timestamp (little-endian u32)
//   [8]    spoke_data_len low byte
//   [9]    bit 0: spoke_data_len high; bits 1-7: spoke_count
//   [10]   sample_count low byte
//   [11]   bits 0-2: sample_count high; bits 3-4: encoding;
//          bit 5: heading_valid; bits 6-7: unknown
//   [12]   bits 0-5: range wire index; bits 6-7: range_status
//   [13]   range resolution metadata
//   [14]   scale low byte
//   [15]   bits 0-2: scale high; bit 3: flag;
//          bits 4-5: echo_type; bit 6: dual_range_id; bit 7: unknown

/// Byte 0 of every IMO echo frame must be this value.
pub const FRAME_MAGIC: u8 = 0x02;

/// Byte 9 bit 0: high bit of `spoke_data_len`.
pub const FRAME_SPOKE_DATA_LEN_HIGH_BIT: u8 = 0x01;

/// Byte 11 bits 0–2: high bits of `sweep_len` (sample_count).
pub const FRAME_SWEEP_LEN_HIGH_MASK: u8 = 0x07;

/// Byte 11 bits 3–4: encoding mode (0–3).
pub const FRAME_ENCODING_MASK: u8 = 0x18;

/// Right-shift for encoding mode extraction from byte 11.
pub const FRAME_ENCODING_SHIFT: u8 = 3;

/// Byte 11 bit 5: heading data present in per-spoke sub-header.
pub const FRAME_HEADING_VALID_BIT: u8 = 0x20;

/// Byte 12 bits 0–5: range wire index.
pub const FRAME_WIRE_INDEX_MASK: u8 = 0x3F;

/// Byte 15 bits 0–2: high bits of `scale`.
pub const FRAME_SCALE_HIGH_MASK: u8 = 0x07;

/// Byte 15 bit 6: dual range identifier (0 = Range A, 1 = Range B).
pub const FRAME_DUAL_RANGE_BIT: u8 = 0x40;

/// Per-spoke sub-header: bits 0–4 of angle/heading byte 1 or 3.
pub const SPOKE_ANGLE_HIGH_MASK: u8 = 0x1F;

// =============================================================================
// Spoke encoding constants
// =============================================================================

/// Encoding modes 1/2: a zero repeat count means 128.
pub const ENCODING_1_REPEAT_DEFAULT: usize = 0x80;

/// Encoding mode 3: a zero repeat count means 64.
pub const ENCODING_3_REPEAT_DEFAULT: usize = 0x40;

/// Bitmask for rounding consumed bytes up to 4-byte alignment:
/// `used = (used + 3) & SPOKE_ALIGNMENT_MASK`.
pub const SPOKE_ALIGNMENT_MASK: usize = !3;

// =============================================================================
// Echo processing
// =============================================================================

/// Software echo gain for low-power radars (DRS4W 2.2 kW, DRS).
/// Applied as a multiplier before the `>> 2` palette shift.
pub const ECHO_GAIN_LOW_POWER: u8 = 2;

/// Software echo gain for full-power radars (NXT, FAR).
pub const ECHO_GAIN_DEFAULT: u8 = 1;

// =============================================================================
// Guard zone constants
// =============================================================================

/// Guard mode value: zone disabled.
pub const GUARD_MODE_OFF: i32 = 0;

/// Guard mode value: fan (sector) zone.
pub const GUARD_MODE_FAN: i32 = 1;
