//! Navico Radar Protocol (NRP) — wire-level constants.
//!
//! This module collects the opcodes, multicast addresses, ports, and packet
//! structure constants used to talk to Navico radars (BR24, 3G, 4G, HALO).
//!
//! Terminology used throughout:
//!
//! - **Opcode**: a 16-bit identifier sent little-endian as the first two bytes
//!   of every NRP packet. The high byte is the *category* and the low byte is
//!   the *sub-opcode*. For example, `0xC401` appears on the wire as `01 C4`,
//!   category `0xC4` (state report), sub-opcode `0x01` (StateMode).
//!
//! - **State report (category `0xC4`)**: radar → MFD. Reports radar state,
//!   configuration, installation parameters, features, and properties.
//!
//! - **Control command (category `0xC1`)**: MFD → radar. Sets operational
//!   parameters (power, range, gain, mode, antenna offsets, etc.).
//!
//! - **Query (category `0xC2`)**: MFD → radar. Requests a state report.
//!
//! - **Multi-device service (category `0xB1`/`0xB2`)**: used during discovery
//!   on multicast group `236.6.7.5:6878`.
//!
//! - **Beacon**: the radar's reply to a discovery query, advertising the
//!   multicast addresses and ports for spoke data, state, and commands.

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};

// =============================================================================
// Spoke geometry
// =============================================================================

/// Number of spokes per full antenna revolution (display space).
pub const SPOKES_PER_REVOLUTION: usize = 2048;

/// Raw spoke counter range: `[0..4096)`. The radar reports angles in this
/// double-resolution space but only every other value is populated, so after
/// dividing by 2 the result fits in `SPOKES_PER_REVOLUTION`.
pub const SPOKES_RAW: u16 = 4096;

/// Number of pixels in a single spoke (samples per radar line).
pub const SPOKE_PIXEL_LEN: usize = 1024;

/// Each pixel is a 4-bit nibble (values `0..16`).
pub const BITS_PER_PIXEL: usize = 4;

/// Number of pixels packed into a single wire byte.
pub const PIXELS_PER_BYTE: usize = 8 / BITS_PER_PIXEL;

/// Number of bytes in one spoke's pixel data on the wire.
pub const SPOKE_DATA_LENGTH: usize = SPOKE_PIXEL_LEN / PIXELS_PER_BYTE;

/// Number of spokes batched into a single radar data frame UDP packet.
pub const SPOKES_PER_FRAME: usize = 32;

// =============================================================================
// Discovery — Multi-device service
// =============================================================================

/// Multicast group used by Gen3+ radars (3G, 4G, HALO) for discovery.
pub const GEN3PLUS_DISCOVERY_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 5)), 6878);

/// Multicast group used by BR24 radars for discovery queries. BR24 radars
/// still respond on the Gen3+ discovery address (`236.6.7.5:6878`), but the
/// MFD must also probe this older group to find them.
pub const BR24_DISCOVERY_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 4)), 6768);

/// Discovery query packet sent by the MFD (multi-device service, opcode
/// `0xB101`, little-endian on the wire).
pub const DISCOVERY_QUERY_PACKET: [u8; 2] = [0x01, 0xB1];

/// Beacon response header bytes (opcode `0xB201`).
pub const BEACON_RESPONSE_HEADER: [u8; 2] = [0x01, 0xB2];

// =============================================================================
// Beacon service/subtype identifiers
// =============================================================================
//
// Each device group in the beacon has a `service_type` identifying its
// function, and contains service entries with `subtype` values for the
// individual data/command/report channels.

/// Service type for the primary radar services device group.
pub const RADAR_SERVICE_TYPE: u16 = 0x0010;

/// Service entry subtype: spoke data stream (multicast RX).
pub const SPOKE_DATA_SUBTYPE: u16 = 0x0010;

/// Service entry subtype: control commands (multicast TX to radar).
pub const COMMAND_SUBTYPE: u16 = 0x0011;

/// Service entry subtype: state/report channel (multicast RX from radar).
pub const REPORT_SUBTYPE: u16 = 0x0012;

// =============================================================================
// HALO heading / navigation / speed multicast addresses
// =============================================================================

/// Multicast address where the MFD sends heading and navigation packets to
/// HALO radars (NKOE format, 72-byte payload). The MFD must keep sending
/// these packets while the radar is transmitting so the radar can map
/// Doppler returns and tracked targets to ground-relative coordinates.
pub const HALO_HEADING_INFO_ADDRESS: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(239, 238, 55, 73), 7527);

/// Primary multicast address where the MFD sends a 23-byte speed packet
/// (`01 d3 01 00 00 00`-prefixed) to HALO radars.
pub const HALO_SPEED_ADDRESS_A: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 20), 6690);

/// Alternate multicast address where the MFD also sends the speed packet.
/// radar_pi sends an identical copy to both addresses.
pub const HALO_SPEED_ADDRESS_B: SocketAddrV4 =
    SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 15), 6005);

// =============================================================================
// NRP opcode category bytes
// =============================================================================

/// Category byte for MFD → radar control commands (`0xC1xx`).
pub const CATEGORY_CONTROL: u8 = 0xC1;

/// Category byte for MFD → radar query commands (`0xC2xx`).
pub const CATEGORY_QUERY: u8 = 0xC2;

/// Category byte for radar → MFD state reports (`0xC4xx`).
pub const CATEGORY_STATE: u8 = 0xC4;

// =============================================================================
// State report sub-opcodes (radar → MFD, category 0xC4)
// =============================================================================

/// `0xC401` — Power/transmit state. 18 bytes. All models.
pub const STATE_MODE: u8 = 0x01;

/// `0xC402` — Range, gain, sea clutter, rain clutter, mode. 99 bytes. All models.
pub const STATE_SETUP: u8 = 0x02;

/// `0xC403` — Scanner type, operating hours, firmware version. 129 bytes. All models.
pub const STATE_PROPERTIES: u8 = 0x03;

/// `0xC404` — Bearing alignment, antenna height, accent light. 66 bytes. All models.
pub const STATE_CONFIG: u8 = 0x04;

/// `0xC405` — BR24-only extended status block. 564 bytes.
pub const STATE_BR24_EXTENDED: u8 = 0x05;

/// `0xC406` — Sector blanking, antenna offsets, transceiver name. 68 or 74 bytes. HALO only.
pub const STATE_INSTALLATION: u8 = 0x06;

/// `0xC407` — Extended properties. Variable length (BR24: 780, 4G: 188, HALO: varies).
pub const STATE_PROPERTIES_EXTENDED: u8 = 0x07;

/// `0xC408` — Sea state, scan speed, sidelobe, doppler. 18/21/22/32 bytes.
pub const STATE_SETUP_EXTENDED: u8 = 0x08;

/// `0xC409` — TLV-encoded feature/capability advertisement. Variable length. HALO only.
pub const STATE_FEATURES: u8 = 0x09;

/// `0xC40A` — Additional HALO state. Variable length.
pub const STATE_ADDITIONAL: u8 = 0x0A;

// =============================================================================
// 0xC409 TLV feature type IDs
//
// Each TLV entry in the StateDataBlock is: [type:u8][reserved:u8][length:u8][payload...]
// =============================================================================

/// TLV type IDs for the 0xC409 StateDataBlock.
pub mod tlv {
    /// Bitmask of supported operating modes.
    /// 4 bytes LE: bit0=custom, 1=harbor, 2=offshore, 3=weather, 4=bird, 5=doppler, 6=buoy.
    pub const SUPPORTED_USE_MODES: u8 = 2;
    /// Interference rejection level count. 5 bytes: `[count:u8][_:3B][flags:u8]`.
    pub const INTERFERENCE_REJECT: u8 = 3;
    /// Noise rejection level count. 5 bytes.
    pub const NOISE_REJECT: u8 = 4;
    /// Target boost (expansion) level count. 5 bytes.
    pub const TARGET_BOOST: u8 = 5;
    /// STC curve level count. 5 bytes.
    pub const STC_CURVE: u8 = 6;
    /// Beam sharpening (target separation) level count. 5 bytes.
    pub const BEAM_SHARPENING: u8 = 7;
    /// Fast scan (scan speed) level count. 5 bytes.
    pub const FAST_SCAN: u8 = 8;
    /// Sidelobe gain min/max range. 4 bytes: `[min:u8][_:u8][max:u8][_:u8]`.
    pub const SIDELOBE_GAIN_RANGE: u8 = 9;
    /// Supported antenna types. Variable: dome=`00`, array=`[count:u8][size_mm:u16 LE]...`.
    pub const SUPPORTED_ANTENNAS: u8 = 10;
    /// Instrumented range min/max in decimeters. 8 bytes: `[min:u32 LE][max:u32 LE]`.
    pub const INSTRUMENTED_RANGE: u8 = 11;
    /// Local interference rejection level count. 5 bytes.
    pub const LOCAL_INTERFERENCE_REJECT: u8 = 12;

    /// Mode bitmask bit positions.
    pub const MODE_CUSTOM: u32 = 1 << 0;
    pub const MODE_HARBOR: u32 = 1 << 1;
    pub const MODE_OFFSHORE: u32 = 1 << 2;
    pub const MODE_WEATHER: u32 = 1 << 3;
    pub const MODE_BIRD: u32 = 1 << 4;
    pub const MODE_DOPPLER: u32 = 1 << 5;
    pub const MODE_BUOY: u32 = 1 << 6;
}

// =============================================================================
// Control command sub-opcodes (MFD → radar, category 0xC1)
// =============================================================================

/// `0xC100` — Radar power. Payload: `01` byte.
pub const CMD_POWER_ON: u8 = 0x00;

/// `0xC101` — Transmit enable. Payload: `0` (standby) or `1` (transmit).
pub const CMD_TRANSMIT: u8 = 0x01;

/// `0xC103` — Range in decimeters, i32 LE.
pub const CMD_RANGE: u8 = 0x03;

/// `0xC105` — Bearing alignment, i16 LE, tenths of a degree.
pub const CMD_BEARING_ALIGNMENT: u8 = 0x05;

/// `0xC106` — Generic gain-style multi-variant command (gain, sea, rain,
/// sidelobe suppression). Payload starts with a variant byte.
pub const CMD_GAIN_VARIANT: u8 = 0x06;

/// `0xC108` — Interference rejection level.
pub const CMD_INTERFERENCE_REJECTION: u8 = 0x08;

/// `0xC109` — Target expansion level (non-HALO).
pub const CMD_TARGET_EXPANSION: u8 = 0x09;

/// `0xC10A` — Target boost level.
pub const CMD_TARGET_BOOST: u8 = 0x0A;

/// `0xC10B` — Sea state level.
pub const CMD_SEA_STATE: u8 = 0x0B;

/// `0xC10D` — No-transmit sector enable (step 1 of a 2-part command).
pub const CMD_NOTRANSMIT_ENABLE: u8 = 0x0D;

/// `0xC10E` — Local interference rejection level.
pub const CMD_LOCAL_INTERFERENCE_REJECTION: u8 = 0x0E;

/// `0xC10F` — Scan speed level.
pub const CMD_SCAN_SPEED: u8 = 0x0F;

/// `0xC110` — Use mode. Payload: 2-byte `tUseMode` (mode + variant).
pub const CMD_USE_MODE: u8 = 0x10;

/// `0xC111` — HALO-specific sea clutter with auto offset.
pub const CMD_HALO_SEA: u8 = 0x11;

/// `0xC112` — HALO-specific target expansion.
pub const CMD_HALO_TARGET_EXPANSION: u8 = 0x12;

/// `0xC121` — Noise rejection level.
pub const CMD_NOISE_REJECTION: u8 = 0x21;

/// `0xC122` — Target separation level.
pub const CMD_TARGET_SEPARATION: u8 = 0x22;

/// `0xC123` — Doppler mode.
pub const CMD_DOPPLER: u8 = 0x23;

/// `0xC124` — Doppler speed threshold, u16 LE in cm/s (range 0..=1594).
pub const CMD_DOPPLER_SPEED_THRESHOLD: u8 = 0x24;

/// `0xC130` — Installation setting (antenna height, antenna offsets, etc).
/// Payload: 4-byte tag followed by setting-specific data.
pub const CMD_INSTALLATION: u8 = 0x30;

/// `0xC131` — Accent light level.
pub const CMD_ACCENT_LIGHT: u8 = 0x31;

/// `0xC1A0` — "Stay on scanner A" ping (keeps the MFD as the active client).
pub const CMD_STAY_ON_A: u8 = 0xA0;

/// `0xC1C0` — No-transmit sector range (step 2 of a 2-part command).
pub const CMD_NOTRANSMIT_SECTOR: u8 = 0xC0;

// =============================================================================
// Installation command (0xC130) tags
// =============================================================================

/// Set antenna height. Payload: i32 LE millimetres.
pub const INSTALL_TAG_ANTENNA_HEIGHT: u8 = 0x01;

/// Set antenna offset from GPS position. Payload: i32 LE ahead mm + i32 LE starboard mm.
pub const INSTALL_TAG_ANTENNA_OFFSET: u8 = 0x04;

// =============================================================================
// Query sub-opcodes (MFD → radar, category 0xC2)
// =============================================================================

/// `0xC201` — Request a batch of state reports (StateSetup, StateConfig,
/// StateFeatures, StatePropertiesExtended, StateInstallation).
pub const QUERY_REPORTS_BATCH: u8 = 0x01;

/// `0xC202` — Request StateFeatures.
pub const QUERY_STATE_CONFIG: u8 = 0x02;

/// `0xC203` — Request StateSetup and StateInstallation.
pub const QUERY_SETUP_AND_INSTALLATION: u8 = 0x03;

/// `0xC204` — Request StateConfig (model info and firmware).
pub const QUERY_STATE_PROPERTIES: u8 = 0x04;

// =============================================================================
// Packet constructors — common discovery / query byte sequences
// =============================================================================

/// Request a StateConfig report (`0xC204`).
pub const REQUEST_STATE_PROPERTIES: [u8; 2] = [QUERY_STATE_PROPERTIES, CATEGORY_QUERY];

/// Request a batch of state reports (`0xC201`).
pub const REQUEST_STATE_BATCH: [u8; 2] = [QUERY_REPORTS_BATCH, CATEGORY_QUERY];

/// "Stay on scanner A" ping (`0xC1A0`).
pub const COMMAND_STAY_ON_A: [u8; 2] = [CMD_STAY_ON_A, CATEGORY_CONTROL];

/// Spoke/radar line status bytes that indicate a valid spoke payload.
/// Different HALO firmware revisions use different upper bits, so we accept
/// any of these as "valid spoke data":
///
/// - `0x02` — BR24 valid spoke (from the 2011 Dabrowski paper)
/// - `0x12` — observed on 4G and newer
/// - `0xC2` — observed on HALO 20+
pub const VALID_SPOKE_STATUSES: [u8; 3] = [0x02, 0x12, 0xC2];
