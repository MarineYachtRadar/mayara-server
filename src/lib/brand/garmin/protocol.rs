//! Garmin Marine Network (GMN) radar protocol — wire-level constants.
//!
//! This module collects the message IDs, multicast addresses, ports, and
//! packet structure constants used to talk to Garmin marine radars over the
//! Garmin Marine Network. Garmin's radar protocol comes in two flavours:
//!
//! - **Legacy HD** (GMR 18 HD, GMR 24 HD, ...) uses message IDs in the
//!   `0x02xx` range and packs spoke samples as 1-bit binary values.
//! - **Enhanced** (GMR xHD, xHD2, Fantom, Fantom Pro) uses message IDs
//!   in the `0x09xx` range and 8-bit grayscale samples.
//!
//! Both protocols share the same 8-byte header format and the same
//! `239.254.2.0` multicast group, just on different UDP ports for spoke data.
//!
//! Sources:
//! - `research/garmin/enhanced-radar-protocol.md`
//! - `research/garmin/legacy-radar-protocol.md`
//! - `research/garmin/discovery-handshake.md`
//! - `research/garmin/feature-detection.md`
//! - `research/garmin/radar-detection.md`

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

// =============================================================================
// Network — multicast groups, ports, subnet
// =============================================================================

/// Garmin radars live on the `172.16.0.0/12` subnet (netmask `255.240.0.0`).
/// The radar in `garmin_xhd.pcap` advertises itself as `172.16.2.0` (yes, .0
/// is a valid host address — Garmin uses /16 subnets).
pub(crate) const GARMIN_NETMASK: Ipv4Addr = Ipv4Addr::new(255, 240, 0, 0);

/// CDM (Common Device Model) heartbeat multicast group. The radar — and every
/// other Garmin marine device — broadcasts a 34-byte `0x038e` "V2 heartbeat"
/// here every 5 seconds, advertising its product ID and service IDs.
pub(crate) const CDM_HEARTBEAT_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 254, 2, 2)), 50050);

/// Settings/status report multicast group. The radar broadcasts ~80
/// individual report messages per second here, one setting per packet.
pub(crate) const REPORT_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 254, 2, 0)), REPORT_PORT);

/// Spoke (sweep) data multicast group used by enhanced-protocol radars. Legacy
/// HD radars send spoke data on `REPORT_ADDRESS` (port 50100) instead.
pub(crate) const DATA_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 254, 2, 0)), DATA_PORT);

/// UDP port for the CDM heartbeat group (`0xC382`).
pub(crate) const CDM_HEARTBEAT_PORT: u16 = 50050;

/// UDP port for the radar settings/status report stream (`0xC3B4`).
pub(crate) const REPORT_PORT: u16 = 50100;

/// UDP port for unicast control commands sent from the MFD to the radar
/// (`0xC3B5`). Commands are sent as UDP datagrams to `<radar_ip>:50101`.
pub(crate) const COMMAND_PORT: u16 = 50101;

/// UDP port for the spoke (sweep) data stream (`0xC3B6`).
pub(crate) const DATA_PORT: u16 = 50102;

// =============================================================================
// Packet header
// =============================================================================

/// Length of the GMN packet header in bytes. Every Garmin radar packet starts
/// with: `[u32 LE packet_type][u32 LE payload_len]`.
pub(crate) const GMN_HEADER_LEN: usize = 8;

// =============================================================================
// Spoke geometry — HD (legacy)
// =============================================================================

/// HD radars use 720 spokes per revolution (0.5° resolution).
pub(crate) const HD_SPOKES_PER_REVOLUTION: usize = 720;

/// HD radars pack 1-bit binary samples, up to 2016 samples per spoke
/// (252 bytes × 8 bits).
pub(crate) const HD_MAX_SPOKE_LEN: usize = 2016;

/// HD spoke packet header is 52 bytes; samples follow at offset +52.
pub(crate) const HD_SPOKE_HEADER_SIZE: usize = 52;

/// HD packs **4 spokes per UDP packet** (the spoke index is `angle*2 + i`
/// for `i ∈ 0..4`).
pub(crate) const HD_SPOKES_PER_PACKET: usize = 4;

/// HD has 1-bit binary data, expanded to 0 or 255 by the unpacker.
/// `pixel_values` is the number of distinct intensity levels in the rendered
/// spoke data — for HD that's just on/off.
pub(crate) const HD_PIXEL_VALUES: u8 = 2;

// =============================================================================
// Spoke geometry — enhanced protocol
// =============================================================================

/// enhanced-protocol radars use 1440 spokes per revolution (0.25° resolution).
pub(crate) const SPOKES_PER_REVOLUTION: usize = 1440;

/// enhanced-protocol radars use 8-bit grayscale samples, up to ~705 samples per spoke
/// (from `radar_pi`'s `GARMIN_MAX_SPOKE_LEN`).
pub(crate) const MAX_SPOKE_LEN: usize = 705;

/// Enhanced spoke packet header is 36 bytes; samples follow at offset +36.
/// See `enhanced-radar-protocol.md` for the per-field layout.
pub(crate) const SPOKE_HEADER_SIZE: usize = 36;

/// Wire offset of the range indicator byte in the spoke header.
/// `0` = Range A, `1` = Range B. Corresponds to `payload[0x10]`
/// = `wire_data[24]`.
pub(crate) const SPOKE_RANGE_INDICATOR_OFFSET: usize = 24;

/// Enhanced protocol encodes spoke angles in **1/8 degree units**, so the wire angle ranges
/// `0..11520` (= 1440 × 8). Divide by 8 to get the spoke index in `0..1440`.
pub(crate) const ANGLE_UNITS_PER_SPOKE: u16 = 8;

/// Enhanced protocol has 8-bit samples; mayara halves them to make room for legend / marker
/// pixels in the rendered output (matching the Raymarine convention).
pub(crate) const PIXEL_VALUES: u8 = 128;

/// First sample byte value in the Fantom MotionScope "approaching" band.
/// Wire values 0xF0–0xF7 (240–247) encode approaching targets with 8
/// intensity sub-levels.
pub(crate) const DOPPLER_APPROACHING_START: u8 = 0xF0;

/// First sample byte value in the "receding" band (0xF8–0xFF, 248–255).
pub(crate) const DOPPLER_RECEDING_START: u8 = 0xF8;

/// Number of intensity sub-levels per Doppler direction on the wire (8),
/// and per direction in the legend after the ÷2 halving (4).
pub(crate) const DOPPLER_LEVELS_PER_DIRECTION: u8 = 4;

// =============================================================================
// Encoding conventions
// =============================================================================

/// Most enhanced-protocol angle fields (bearing alignment, no-TX zone start/stop, park
/// position) are encoded as `int32 LE` in **degrees × 32**.
pub(crate) const DEGREE_SCALE: i32 = 32;

/// Enhanced-protocol gain, sea, and rain levels are reported and accepted as `uint16 LE`
/// in `0..=10000`, i.e. percent × 100.
pub(crate) const GAIN_SCALE: u16 = 100;

// =============================================================================
// CDM discovery (`0x038e` heartbeat)
// =============================================================================

/// `0x038e` — CDM "V2 heartbeat" announcement. 26-byte payload sent to
/// `239.254.2.2:50050` every 5 seconds. Carries `product_id`, service IDs,
/// and an uptime/sequence counter.
pub(crate) const MSG_CDM_HEARTBEAT: u32 = 0x038e;

/// Offset (within the heartbeat payload, after the 8-byte GMN header) of the
/// `version_marker` byte. Value `2` means "V2 heartbeat".
pub(crate) const CDM_OFFSET_VERSION_MARKER: usize = 0;

/// Offset of the `product_id` u16 within the heartbeat payload.
pub(crate) const CDM_OFFSET_PRODUCT_ID: usize = 2;

/// Offset of the `simulator_mode` byte within the heartbeat payload.
pub(crate) const CDM_OFFSET_SIMULATOR_MODE: usize = 4;

/// Offset of the `product_subtype` byte within the heartbeat payload.
pub(crate) const CDM_OFFSET_PRODUCT_SUBTYPE: usize = 5;

// =============================================================================
// Legacy HD message IDs (0x02xx)
// =============================================================================

// --- Status / report messages (radar → MFD) -------------------------------------

/// `0x02A3` — HD spoke data (short form), 4 spokes packed per packet.
pub(crate) const MSG_HD_SPOKE: u32 = 0x02a3;

/// `0x02A4` — HD AFC status update.
pub(crate) const MSG_HD_AFC_STATUS: u32 = 0x02a4;

/// `0x02A5` — HD radar state (state code, warmup countdown, range, gain,
/// sea/rain clutter, bearing, crosstalk, scan speed). Sent periodically.
pub(crate) const MSG_HD_STATE: u32 = 0x02a5;

/// `0x02A6` — HD scanner identification / version packet.
pub(crate) const MSG_HD_SCANNER_ID: u32 = 0x02a6;

/// `0x02A7` — HD composite settings report (~88 bytes). Triggered by
/// sending `MSG_HD_REQUEST_SETTINGS` (`0x02EE`).
pub(crate) const MSG_HD_SETTINGS: u32 = 0x02a7;

/// `0x02AA` — HD cumulative transmit time (seconds).
pub(crate) const MSG_HD_TRANSMIT_TIME: u32 = 0x02aa;

/// `0x02AB` — HD rotation speed (RPM × 100).
pub(crate) const MSG_HD_ROTATION_SPEED: u32 = 0x02ab;

/// `0x02AC` — HD system temperature (raw × 25).
pub(crate) const MSG_HD_SYSTEM_TEMP: u32 = 0x02ac;

/// `0x02AD` — HD antenna size report.
pub(crate) const MSG_HD_ANTENNA_SIZE: u32 = 0x02ad;

/// `0x02AE` — HD capability report (determines whether the radar uses 720/1‑bit
/// or 1440/8‑bit spoke data).
pub(crate) const MSG_HD_CAPABILITY: u32 = 0x02ae;

// --- Command messages (MFD → radar) --------------------------------------------

/// `0x02B2` — Set transmit mode. Payload: `[u16 LE]` `1=standby, 2=transmit`.
pub(crate) const CMD_HD_SET_TRANSMIT: u32 = 0x02b2;

/// `0x02B3` — Set range A (single-range mode). Payload: `[i32 LE meters - 1]`.
/// **Note the off-by-one**: HD wire encodes `meters - 1`, Enhanced protocol encodes `meters`
/// directly.
pub(crate) const CMD_HD_SET_RANGE_A: u32 = 0x02b3;

/// `0x02B4` — Set range A gain. Payload: `[u8 gain][u8 auto_flag]`.
pub(crate) const CMD_HD_SET_GAIN: u32 = 0x02b4;

/// `0x02B5` — Set range A sea clutter. Payload:
/// `[u32 LE gain][u32 LE auto_flag]` (8-byte body).
pub(crate) const CMD_HD_SET_SEA: u32 = 0x02b5;

/// `0x02B6` — Set range A rain clutter gain. Payload: `[u32 LE gain]`.
pub(crate) const CMD_HD_SET_RAIN: u32 = 0x02b6;

/// `0x02B7` — Set front-of-boat / bearing alignment. Payload: `[i16 LE degrees]`.
pub(crate) const CMD_HD_SET_BEARING_ALIGNMENT: u32 = 0x02b7;

/// `0x02B9` — Set rotation speed mode (0=normal, 1=slow).
pub(crate) const CMD_HD_SET_RPM_MODE: u32 = 0x02b9;

/// `0x02BE` — Set dither mode (interference rejection).
pub(crate) const CMD_HD_SET_DITHER: u32 = 0x02be;

/// `0x02EE` — Request settings report (triggers a `MSG_HD_SETTINGS` reply).
pub(crate) const CMD_HD_REQUEST_SETTINGS: u32 = 0x02ee;

/// `0x02FC` — Set FTC gain (single-range). Payload: `[u8 gain][u8 ftc_mode]`.
pub(crate) const CMD_HD_SET_FTC: u32 = 0x02fc;

// =============================================================================
// Enhanced message IDs (0x09xx)
// =============================================================================
//
// In the enhanced protocol, the same message ID is used both for the MFD to
// **set** a value and for the radar to **report** the current state of that
// value. The radar continuously broadcasts its full state on the report
// multicast, one setting per packet.

// --- Operational settings ------------------------------------------------------

/// `0x0911` — Scan type (1 = single range).
pub(crate) const MSG_SCAN_TYPE: u32 = 0x0911;

/// `0x0916` — RPM mode (0=normal, 1=slow). Payload: `[u8]`.
/// On the wire mayara sends this as `value × 2` per radar_pi.
pub(crate) const MSG_RPM_MODE: u32 = 0x0916;

/// `0x0917` — Total spoke count (typically 2400).
pub(crate) const MSG_SPOKE_TOTAL: u32 = 0x0917;

/// `0x0918` — Current transmit mode (companion to `0x0919`).
pub(crate) const MSG_TRANSMIT_MODE_CURRENT: u32 = 0x0918;

/// `0x0919` — Set transmit mode (0=standby, 1=transmit). Payload: `[u8]`.
pub(crate) const MSG_TRANSMIT_MODE: u32 = 0x0919;

/// `0x091B` — Dither / interference rejection mode. Payload: `[u8]`.
pub(crate) const MSG_DITHER_MODE: u32 = 0x091b;

/// `0x091C` — Range mode (0=single, 1=dual).
pub(crate) const MSG_RANGE_MODE: u32 = 0x091c;

/// `0x091D` — Range A radar mode / auto-gain level (0=auto low, 1=auto high).
pub(crate) const MSG_RANGE_A_AUTO_LEVEL: u32 = 0x091d;

/// `0x091E` — Set range A in **meters** (uint32 LE). Unlike legacy HD this is a
/// direct value, no -1 offset.
pub(crate) const MSG_RANGE_A: u32 = 0x091e;

/// `0x091F` — Range B in meters (dual-range mode).
pub(crate) const MSG_RANGE_B: u32 = 0x091f;

/// `0x0920` — AFC mode (0=manual, 1=auto).
pub(crate) const MSG_AFC_MODE: u32 = 0x0920;

/// `0x0921` — AFC setting (uint16 LE).
pub(crate) const MSG_AFC_SETTING: u32 = 0x0921;

/// `0x0922` — AFC coarse value (uint16 LE).
pub(crate) const MSG_AFC_COARSE: u32 = 0x0922;

/// `0x0924` — Range A gain mode (0=manual, 2=auto).
pub(crate) const MSG_RANGE_A_GAIN_MODE: u32 = 0x0924;

/// `0x0925` — Range A gain level (uint16 LE, 0..10000 = 0..100% × 100).
pub(crate) const MSG_RANGE_A_GAIN: u32 = 0x0925;

/// `0x0926` — Range B gain mode (dual-range).
pub(crate) const MSG_RANGE_B_GAIN_MODE: u32 = 0x0926;

/// `0x0927` — Range B gain level (dual-range).
pub(crate) const MSG_RANGE_B_GAIN: u32 = 0x0927;

/// `0x092F` — AFC tuning mode / trigger.
pub(crate) const MSG_AFC_TUNING_MODE: u32 = 0x092f;

/// `0x0930` — Front-of-boat / bearing alignment. Payload: `[i32 LE deg × 32]`.
pub(crate) const MSG_BEARING_ALIGNMENT: u32 = 0x0930;

/// `0x0931` — Park position. Payload: `[i32 LE deg × 32]`.
pub(crate) const MSG_PARK_POSITION: u32 = 0x0931;

/// `0x0932` — Noise blanker / crosstalk rejection mode.
pub(crate) const MSG_NOISE_BLANKER: u32 = 0x0932;

/// `0x0933` — Range A rain filter control mode (0=off, 1=on).
pub(crate) const MSG_RANGE_A_RAIN_MODE: u32 = 0x0933;

/// `0x0934` — Range A rain filter gain (uint16 LE, gain × 100).
pub(crate) const MSG_RANGE_A_RAIN_GAIN: u32 = 0x0934;

/// `0x0936` — Range B rain filter control mode (dual-range).
pub(crate) const MSG_RANGE_B_RAIN_MODE: u32 = 0x0936;

/// `0x0937` — Range B rain filter gain (dual-range).
pub(crate) const MSG_RANGE_B_RAIN_GAIN: u32 = 0x0937;

/// `0x0939` — Range A sea clutter mode (0=off, 1=manual, 2=auto).
pub(crate) const MSG_RANGE_A_SEA_MODE: u32 = 0x0939;

/// `0x093A` — Range A sea clutter gain (uint16 LE, gain × 100).
pub(crate) const MSG_RANGE_A_SEA_GAIN: u32 = 0x093a;

/// `0x093B` — Range A sea state (0=calm, 1=moderate, 2=rough).
pub(crate) const MSG_RANGE_A_SEA_STATE: u32 = 0x093b;

/// `0x093C` — Range B sea clutter mode (dual-range).
pub(crate) const MSG_RANGE_B_SEA_MODE: u32 = 0x093c;

/// `0x093D` — Range B sea clutter gain (dual-range).
pub(crate) const MSG_RANGE_B_SEA_GAIN: u32 = 0x093d;

/// `0x093E` — Range B sea state (dual-range).
pub(crate) const MSG_RANGE_B_SEA_STATE: u32 = 0x093e;

/// `0x093F` — No-transmit zone 1 mode (0=off, 1=on).
pub(crate) const MSG_NO_TX_ZONE_1_MODE: u32 = 0x093f;

/// `0x0940` — No-transmit zone 1 start angle. Payload: `[i32 LE deg × 32]`.
pub(crate) const MSG_NO_TX_ZONE_1_START: u32 = 0x0940;

/// `0x0941` — No-transmit zone 1 stop angle. Payload: `[i32 LE deg × 32]`.
pub(crate) const MSG_NO_TX_ZONE_1_STOP: u32 = 0x0941;

/// `0x0942` — Sentry / timed transmit mode (0=off, 1=on).
pub(crate) const MSG_SENTRY_MODE: u32 = 0x0942;

/// `0x0943` — Sentry standby time in seconds (uint16 LE).
pub(crate) const MSG_SENTRY_STANDBY_TIME: u32 = 0x0943;

/// `0x0944` — Sentry transmit time in seconds (uint16 LE).
pub(crate) const MSG_SENTRY_TRANSMIT_TIME: u32 = 0x0944;

/// `0x0950` — Antenna size (uint16 LE).
pub(crate) const MSG_ANTENNA_SIZE: u32 = 0x0950;

/// `0x0956` — Range B radar mode / auto-gain level (dual-range).
pub(crate) const MSG_RANGE_B_RADAR_MODE: u32 = 0x0956;

/// `0x0966` — Transmit channel mode. Payload: `[u8]`.
/// `0` = manual, `1` = auto. Fantom Pro / solid-state only.
/// Gated on capability bit `0xb2`.
pub(crate) const MSG_TRANSMIT_CHANNEL_MODE: u32 = 0x0966;

/// `0x0967` — Transmit channel select (uint16 LE, 1-based channel number).
pub(crate) const MSG_TRANSMIT_CHANNEL_SELECT: u32 = 0x0967;

/// `0x09B6` — Transmit channel max (uint16 LE). Broadcast once at init.
pub(crate) const MSG_TRANSMIT_CHANNEL_MAX: u32 = 0x09b6;

/// `0x096A` — No-transmit zone 2 mode (0=off, 1=on). Fantom Pro and
/// later — only present on radars that report capability bit
/// `cap::NO_TX_ZONE_2_MODE` (0xbf) in `0x09B1`.
pub(crate) const MSG_NO_TX_ZONE_2_MODE: u32 = 0x096a;

/// `0x096B` — No-transmit zone 2 start angle. Payload: `[i32 LE deg × 32]`.
pub(crate) const MSG_NO_TX_ZONE_2_START: u32 = 0x096b;

/// `0x096C` — No-transmit zone 2 stop angle. Payload: `[i32 LE deg × 32]`.
pub(crate) const MSG_NO_TX_ZONE_2_STOP: u32 = 0x096c;

/// `0x0960` — Range A scan mode / MotionScope (Doppler). Payload: `[u8]`.
/// `0` = off, `1` = approaching only, `2` = approaching + receding.
/// Gated on capability bit `0xa3`.
pub(crate) const MSG_RANGE_A_DOPPLER_MODE: u32 = 0x0960;

/// `0x0961` — Range B scan mode (dual-range Doppler).
pub(crate) const MSG_RANGE_B_DOPPLER_MODE: u32 = 0x0961;

/// `0x0962` — Range A pulse expansion mode (0=off, 1=on). xHD2+.
/// Gated on capability bit `0xab`.
pub(crate) const MSG_RANGE_A_PULSE_EXPANSION: u32 = 0x0962;

/// `0x0963` — Range B pulse expansion mode (dual-range).
pub(crate) const MSG_RANGE_B_PULSE_EXPANSION: u32 = 0x0963;

/// `0x0968` — Range A target size mode. xHD2/Fantom.
/// Gated on capability bit `0xbd`.
pub(crate) const MSG_RANGE_A_TARGET_SIZE: u32 = 0x0968;

/// `0x0969` — Range B target size mode (dual-range).
pub(crate) const MSG_RANGE_B_TARGET_SIZE: u32 = 0x0969;

/// `0x096D` — Range A Doppler sensitivity (uint16 LE).
/// Gated on capability bit `0xc3`. Units uncertain (likely percent).
pub(crate) const MSG_RANGE_A_DOPPLER_SENSITIVITY: u32 = 0x096d;

/// `0x096E` — Range B Doppler sensitivity (uint16 LE).
pub(crate) const MSG_RANGE_B_DOPPLER_SENSITIVITY: u32 = 0x096e;

/// `0x0970` — Range A scan average mode (0=off, 1=on). xHD3/Fantom Pro.
/// Gated on capability bit `0xca`.
pub(crate) const MSG_RANGE_A_SCAN_AVERAGE_MODE: u32 = 0x0970;

/// `0x0971` — Range B scan average mode (dual-range).
pub(crate) const MSG_RANGE_B_SCAN_AVERAGE_MODE: u32 = 0x0971;

/// `0x0972` — Range A scan average sensitivity (uint16 LE).
pub(crate) const MSG_RANGE_A_SCAN_AVERAGE_SENSITIVITY: u32 = 0x0972;

/// `0x0973` — Range B scan average sensitivity (uint16 LE).
pub(crate) const MSG_RANGE_B_SCAN_AVERAGE_SENSITIVITY: u32 = 0x0973;

// --- State / runtime / hardware health ----------------------------------------

/// `0x0992` — Scanner state. Payload: `[u8]`. See `STATE_*` constants.
pub(crate) const MSG_SCANNER_STATE: u32 = 0x0992;

/// `0x0993` — Milliseconds until the next state change.
pub(crate) const MSG_STATE_CHANGE: u32 = 0x0993;

/// `0x0998` — Spoke (sweep) data. One spoke per packet on port 50102.
pub(crate) const MSG_SPOKE: u32 = 0x0998;

/// `0x099A` — AFC tuning progress (0..100%).
pub(crate) const MSG_AFC_PROGRESS: u32 = 0x099a;

/// `0x099B` — Error message. Body has a 64-byte ASCII info string at +16.
pub(crate) const MSG_ERROR_MESSAGE: u32 = 0x099b;

/// `0x099F` — Maximum range in meters.
pub(crate) const MSG_MAX_RANGE: u32 = 0x099f;

/// `0x09A2` — Transmit power.
pub(crate) const MSG_TRANSMIT_POWER: u32 = 0x09a2;

/// `0x09A3` — Input voltage (uint16 LE).
pub(crate) const MSG_INPUT_VOLTAGE: u32 = 0x09a3;

/// `0x09A4` — Heater voltage (uint16 LE).
pub(crate) const MSG_HEATER_VOLTAGE: u32 = 0x09a4;

/// `0x09A6` — High voltage (uint16 LE).
pub(crate) const MSG_HIGH_VOLTAGE: u32 = 0x09a6;

/// `0x09A7` — Transmit current (uint16 LE).
pub(crate) const MSG_TRANSMIT_CURRENT: u32 = 0x09a7;

/// `0x09A8` — System temperature (uint16 LE).
pub(crate) const MSG_SYSTEM_TEMPERATURE: u32 = 0x09a8;

/// `0x09AA` — Cumulative operation time in seconds.
pub(crate) const MSG_OPERATION_TIME: u32 = 0x09aa;

/// `0x09AB` — Cumulative modulator time in seconds.
pub(crate) const MSG_MODULATOR_TIME: u32 = 0x09ab;

/// `0x09AC` — Cumulative transmit time in seconds.
pub(crate) const MSG_TRANSMIT_TIME: u32 = 0x09ac;

/// `0x09B1` — Capability bitmap. 48-byte payload containing 5 × u64 capability
/// words at offsets +0x08..+0x28. See `feature-detection.md` for the bit map.
pub(crate) const MSG_CAPABILITY: u32 = 0x09b1;

/// `0x09B2` — Range table. 72-byte payload listing the supported ranges in
/// meters. Layout: 4-byte `[version=1, length=72]`, then `[u32 count]`,
/// then `count` × `u32 LE meters`.
pub(crate) const MSG_RANGE_TABLE: u32 = 0x09b2;

/// `0x09B7` — Default Doppler sensitivity (uint16 LE). Broadcast once at
/// init as part of the capability stream. Used by the "Restore Defaults"
/// code path; no `Radar_Manager_Command_*` wrapper.
pub(crate) const MSG_DEFAULT_DOPPLER_SENSITIVITY: u32 = 0x09b7;

// =============================================================================
// Scanner state values (msg `0x0992`)
// =============================================================================

/// Warmup — transmitter is heating up.
pub(crate) const STATE_WARMING_UP: u32 = 2;

/// Standby — radar ready but not transmitting.
pub(crate) const STATE_STANDBY: u32 = 3;

/// Spinning up — antenna accelerating before transmit.
pub(crate) const STATE_SPINNING_UP: u32 = 4;

/// Transmitting — actively scanning.
pub(crate) const STATE_TRANSMIT: u32 = 5;

/// Stopping — shutdown requested.
pub(crate) const STATE_STOPPING: u32 = 6;

/// Spinning down — antenna decelerating.
pub(crate) const STATE_SPINNING_DOWN: u32 = 7;

/// Starting — initial startup.
pub(crate) const STATE_STARTING: u32 = 10;

// =============================================================================
// HD scanner state values (msg `0x02A5`)
// =============================================================================

/// HD warming up.
pub(crate) const HD_STATE_WARMING_UP: u16 = 1;

/// HD standby.
pub(crate) const HD_STATE_STANDBY: u16 = 3;

/// HD transmitting.
pub(crate) const HD_STATE_TRANSMIT: u16 = 4;

/// HD spinning up.
pub(crate) const HD_STATE_SPINNING_UP: u16 = 5;
