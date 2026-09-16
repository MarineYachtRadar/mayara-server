use anyhow::{Error, bail};
use deku::DekuRead;
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::GarminRadarType;
use super::capabilities::GarminCapabilities;
use super::command::Command;
use super::protocol::*;
use super::range_table;
use crate::Cli;
use crate::brand::CommandSender;
use crate::network;
use crate::radar::settings::{ControlId, ControlValue};
use crate::radar::spoke::GenericSpoke;
use crate::radar::{
    BYTE_LOOKUP_LENGTH, CommonRadar, DUAL_RANGE_A, DUAL_RANGE_B, DopplerMode, Legend, Power,
    RadarError, RadarInfo, SharedRadars, transmit_claim_after_report, transmit_claim_after_request,
};
use crate::replay::RadarSocket;
use crate::util::{c_string, decode_head};
use serde_json::Value;

/// How often the receiver checks whether the radar should stand down; the
/// watchdog that decides it ticks at the same rate.
const STAND_DOWN_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// A scalar setting report: the 8-byte GMN header followed by the value.
///
/// The radar sends the same logical setting as one, two or four bytes, and it
/// is the header's length field that says which — not the opcode. A packet
/// whose payload is shorter than the length it declares fails to decode
/// rather than reading past the end of the datagram.
#[derive(DekuRead, Debug, PartialEq)]
#[deku(endian = "little")]
struct ScalarReport {
    header: GmnHeader,
    #[deku(ctx = "header.payload_len")]
    value: ScalarValue,
}

#[derive(DekuRead, Debug, PartialEq)]
#[deku(
    ctx = "endian: deku::ctx::Endian, payload_len: u32",
    endian = "endian",
    id = "payload_len"
)]
enum ScalarValue {
    #[deku(id = "1")]
    U8(u8),
    #[deku(id = "2")]
    U16(u16),
    #[deku(id = "4")]
    U32(u32),
    /// A width we have no layout for. Reported as zero, as it always has been.
    #[deku(id_pat = "_")]
    Unknown,
}

impl ScalarValue {
    fn as_u32(&self) -> u32 {
        match *self {
            ScalarValue::U8(v) => v as u32,
            ScalarValue::U16(v) => v as u32,
            ScalarValue::U32(v) => v,
            ScalarValue::Unknown => 0,
        }
    }
}

/// The 36-byte header of an enhanced-protocol spoke; the samples follow it.
///
/// `scan_length` and `scan_length_i` repeat the sample count the radar is
/// about to send, and nothing explains why there are three of them; only
/// `scan_length_bytes` is used.
#[derive(DekuRead, Debug, PartialEq)]
#[deku(endian = "little")]
struct EnhancedSpokeHeader {
    _packet_type: u32,      //  0..4
    _payload_len: u32,      //  4..8
    _fill_1: [u8; 2],       //  8..10
    _scan_length: u16,      // 10..12
    angle: u16,             // 12..14, eighths of a degree
    _fill_2: [u8; 2],       // 14..16
    range_meters: u32,      // 16..20
    _display_meters: u32,   // 20..24
    range_indicator: u8,    // 24, 1 selects Range B on a dual-range radar
    _fill_3: u8,            // 25
    scan_length_bytes: u16, // 26..28
    _fills_4: [u8; 2],      // 28..30
    _scan_length_i: u32,    // 30..34
    _fills_5: [u8; 2],      // 34..36
}

/// The 52-byte header of an HD spoke packet, which carries
/// [`HD_SPOKES_PER_PACKET`] spokes' worth of 1-bit samples after it.
#[derive(DekuRead, Debug, PartialEq)]
#[deku(endian = "little")]
struct HdSpokeHeader {
    _packet_type: u32, //  0..4
    _payload_len: u32, //  4..8
    angle: u16,        //  8..10
    scan_length: u16,  // 10..12
    _u00: [u8; 4],     // 12..16
    range_meters: u32, // 16..20, one less than the range it means
    _u01: [u8; 32],    // 20..52
}

/// The 48-byte HD state report (`0x02A5`). The bytes this leaves unread are
/// undocumented rather than known to be empty.
#[derive(DekuRead, Debug, PartialEq)]
#[deku(endian = "little")]
struct HdStatusReport {
    _packet_type: u32,      //  0..4
    _payload_len: u32,      //  4..8
    scanner_state: u16,     //  8..10
    warmup: u16,            // 10..12
    range_meters: u32,      // 12..16, one less than the range it means
    gain_level: u8,         // 16
    gain_mode: u8,          // 17
    _u00: [u8; 2],          // 18..20
    sea_clutter_level: u8,  // 20
    sea_clutter_mode: u8,   // 21
    _u01: [u8; 2],          // 22..24
    rain_clutter_level: u8, // 24
    _u02: [u8; 3],          // 25..28
    dome_offset: i16,       // 28..30
    _u03: u8,               // 30
    crosstalk_onoff: u8,    // 31
    _u04: [u8; 8],          // 32..40
    dome_speed: u8,         // 40
    _u05: [u8; 7],          // 41..48
}

/// Lookup table for converting raw wire pixel values to legend indices.
/// For xHD, values are halved to make room for special legend entries.
type WireToLegendTable = [u8; BYTE_LOOKUP_LENGTH];

fn wire_to_legend(legend: &Legend, is_xhd: bool, doppler: bool) -> WireToLegendTable {
    let mut lookup = [0u8; BYTE_LOOKUP_LENGTH];

    if is_xhd {
        if doppler {
            // Fantom with MotionScope: the 256 wire values are split:
            //   0x00–0xEF (0–239)   → normal intensity, halved to 0–119
            //   0xF0–0xF7 (240–247) → approaching, 4 legend indices
            //   0xF8–0xFF (248–255) → receding, 4 legend indices
            let (appr_start, appr_count) = legend.doppler_approaching.unwrap_or((0, 0));
            let (recv_start, recv_count) = legend.doppler_receding.unwrap_or((0, 0));
            for (j, slot) in lookup.iter_mut().enumerate() {
                let jb = j as u8;
                *slot = if jb >= DOPPLER_RECEDING_START {
                    // Receding band: map 8 wire sub-levels to `recv_count`
                    // legend entries via integer division.
                    let sub = jb - DOPPLER_RECEDING_START;
                    let idx = if recv_count > 0 {
                        sub * recv_count / 8
                    } else {
                        0
                    };
                    recv_start + idx
                } else if jb >= DOPPLER_APPROACHING_START {
                    // Approaching band: 8 wire sub-levels → `appr_count` entries.
                    let sub = jb - DOPPLER_APPROACHING_START;
                    let idx = if appr_count > 0 {
                        sub * appr_count / 8
                    } else {
                        0
                    };
                    appr_start + idx
                } else {
                    // Normal intensity, halved.
                    jb / 2
                };
            }
        } else {
            // without Doppler: divide by 2 to make room for legend values
            for (j, slot) in lookup.iter_mut().enumerate() {
                *slot = (j / 2) as u8;
            }
        }
    } else {
        // HD: binary data, no transformation needed
        for (j, slot) in lookup.iter_mut().enumerate() {
            *slot = j as u8;
        }
    }

    lookup
}

/// Per-range mutable state. In single-range mode there's one of these;
/// in dual-range mode there's one for Range A and one for Range B.
struct RangeState {
    range_meters: u32,
    doppler: DopplerMode,
    gain_level: u32,
    gain_auto: bool,
    wire_to_legend: WireToLegendTable,
}

pub(crate) struct GarminReportReceiver {
    common: CommonRadar,
    common_b: Option<CommonRadar>,
    command_sender_b: Option<Command>,
    radar_type: GarminRadarType,
    report_socket: Option<RadarSocket>,
    data_socket: Option<RadarSocket>,
    command_sender: Option<Command>,
    reported_unknown: HashMap<u32, bool>,
    /// The dual-range id a client asked to Transmit through mayara,
    /// while that transmit is still ours to stand down. A Garmin radar keeps
    /// transmitting until a client tells it to stop and does nothing on losing
    /// its CDM peers but stop broadcasting spokes (research/garmin/
    /// gmr-xhd-firmware.md), so standing down means sending Standby ourselves,
    /// and only for a transmit that was ours: an MFD's transmit is never
    /// touched. Both ranges share the scanner, so this is one fact for it.
    transmit_is_ours: Option<i32>,

    range_a: RangeState,
    range_b: Option<RangeState>,

    /// Capability bitmap for the connected radar.
    capabilities: GarminCapabilities,
    capabilities_seen: bool,

    /// Whether the radar's broadcast range table has been applied.
    range_table_seen: bool,

    // No-transmit sector state (antenna-level, not per-range).
    no_tx_1: PendingNoTxSector,
    no_tx_2: PendingNoTxSector,
}

/// Per-zone aggregation state for the no-transmit sector messages.
#[derive(Default)]
struct PendingNoTxSector {
    enabled: Option<bool>,
    start: Option<f64>,
    end: Option<f64>,
}

/// Selector for the two no-transmit zones. Zone 1 (`0x093f..0x0941`)
/// is supported by every xHD; zone 2 (`0x096a..0x096c`) only by Fantom
/// Pro and other multi-zone radars that advertise capability bit
/// `cap::NO_TX_ZONE_2_MODE` in `0x09B1`.
#[derive(Copy, Clone, Debug)]
enum NoTxZone {
    One,
    Two,
}

impl NoTxZone {
    fn number(self) -> u8 {
        match self {
            NoTxZone::One => 1,
            NoTxZone::Two => 2,
        }
    }

    fn control_id(self) -> ControlId {
        match self {
            NoTxZone::One => ControlId::NoTransmitSector1,
            NoTxZone::Two => ControlId::NoTransmitSector2,
        }
    }
}

impl GarminReportReceiver {
    pub(crate) fn new(args: &Cli, info: RadarInfo, radars: SharedRadars) -> GarminReportReceiver {
        let key = info.key();

        let replay = args.is_replay();
        log::debug!(
            "{}: Creating GarminReportReceiver with args {:?}",
            key,
            args
        );

        // Detect radar type from spoke count
        let radar_type = if info.spokes_per_revolution > 720 {
            GarminRadarType::XHD
        } else {
            GarminRadarType::HD
        };

        let command_sender = Some(Command::new(radar_type, info.send_command_addr));

        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();

        let wire_to_legend = wire_to_legend(
            &info.get_legend(),
            radar_type == GarminRadarType::XHD,
            false,
        );

        let mut common =
            CommonRadar::new(args, key, info, radars, control_update_rx, replay, blob_tx);
        // Coalesce ~1/32 of a revolution of spokes per broadcast.
        // Garmin Fantom is 1 spoke / UDP and xHD is 4 spokes / UDP, so
        // batching cuts compression / WebSocket-framing cycles by a
        // factor matching `spokes_per_revolution.div_ceil(32)`.
        let target = common.info.spokes_per_revolution.div_ceil(32) as usize;
        common.set_spoke_batch_threshold(target);

        let capabilities = match radar_type {
            GarminRadarType::HD => GarminCapabilities::for_legacy_hd(),
            _ => GarminCapabilities::empty(),
        };

        GarminReportReceiver {
            common,
            common_b: None,
            command_sender_b: None,
            radar_type,
            report_socket: None,
            data_socket: None,
            command_sender,
            reported_unknown: HashMap::new(),
            transmit_is_ours: None,
            range_a: RangeState {
                range_meters: 0,
                doppler: DopplerMode::None,
                gain_level: 0,
                gain_auto: false,
                wire_to_legend,
            },
            range_b: None,
            capabilities,
            capabilities_seen: matches!(radar_type, GarminRadarType::HD),
            range_table_seen: false,
            no_tx_1: PendingNoTxSector::default(),
            no_tx_2: PendingNoTxSector::default(),
        }
    }

    /// Attach a Range B receiver for dual-range mode. Called by the
    /// locator after constructing the second RadarInfo.
    pub(crate) fn set_range_b(&mut self, args: &Cli, info: RadarInfo, radars: SharedRadars) {
        let key = info.key();
        let replay = args.is_replay();
        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();
        let wire_to_legend = wire_to_legend(
            &info.get_legend(),
            self.radar_type == GarminRadarType::XHD,
            false,
        );
        let command_sender_b = Some(Command::new_range_b(
            self.radar_type,
            info.send_command_addr,
        ));

        let mut common_b =
            CommonRadar::new(args, key, info, radars, control_update_rx, replay, blob_tx);
        let target = common_b.info.spokes_per_revolution.div_ceil(32) as usize;
        common_b.set_spoke_batch_threshold(target);
        self.common_b = Some(common_b);
        self.command_sender_b = command_sender_b;
        self.range_b = Some(RangeState {
            range_meters: 0,
            doppler: DopplerMode::None,
            gain_level: 0,
            gain_auto: false,
            wire_to_legend,
        });
    }

    async fn start_sockets(&mut self) -> io::Result<()> {
        // Report socket (239.254.2.0:50100)
        match network::create_udp_listen(
            &self.common.info.report_addr,
            &self.common.info.nic_addr,
            network::SocketType::Multicast,
        ) {
            Ok(socket) => {
                self.report_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for reports",
                    self.common.key,
                    self.common.info.report_addr,
                    self.common.info.nic_addr
                );
            }
            Err(e) => {
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    self.common.info.report_addr,
                    self.common.info.nic_addr,
                    e
                );
                return Err(e);
            }
        }

        // uses a separate data socket
        if self.radar_type == GarminRadarType::XHD {
            match network::create_udp_listen(
                &self.common.info.spoke_data_addr,
                &self.common.info.nic_addr,
                network::SocketType::Multicast,
            ) {
                Ok(socket) => {
                    self.data_socket = Some(socket);
                    log::debug!(
                        "{}: {} via {}: listening for data",
                        self.common.key,
                        self.common.info.spoke_data_addr,
                        self.common.info.nic_addr
                    );
                }
                Err(e) => {
                    log::debug!(
                        "{}: {} via {}: create data multicast failed: {}",
                        self.common.key,
                        self.common.info.spoke_data_addr,
                        self.common.info.nic_addr,
                        e
                    );
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!(
            "{}: listening for reports (type={}, report_socket={}, data_socket={})",
            self.common.key,
            self.radar_type,
            self.report_socket.is_some(),
            self.data_socket.is_some()
        );
        let mut report_buf = Vec::with_capacity(10000);
        let mut data_buf = Vec::with_capacity(10000);
        let mut stand_down_check = Instant::now() + STAND_DOWN_CHECK_INTERVAL;

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
                },
                _ = sleep_until(stand_down_check) => {
                    stand_down_check = Instant::now() + STAND_DOWN_CHECK_INTERVAL;
                    if self.common.info.stand_down() {
                        self.stand_down_our_transmit().await;
                    }
                },
                r = async {
                    if let Some(sock) = self.report_socket.as_mut() {
                        sock.recv_buf_from(&mut report_buf).await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match r {
                        Ok((_len, _addr)) => {
                            self.common.info.mark_input();
                            if let Err(e) = self.process_report(&report_buf) {
                                log::error!("{}: {}", self.common.key, e);
                            }
                            report_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },
                r = async {
                    if let Some(sock) = self.data_socket.as_mut() {
                        sock.recv_buf_from(&mut data_buf).await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    match r {
                        Ok((_len, _addr)) => {
                            if let Err(e) = self.process_data(&data_buf) {
                                log::error!("{}: {}", self.common.key, e);
                            }
                            data_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },
                r = self.common.control_update_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            let claim = transmit_claim_after_request(self.transmit_is_ours, DUAL_RANGE_A, &cv.control_value);
                            if self.common.process_control_update(cv, &mut self.command_sender).await.is_ok() {
                                self.transmit_is_ours = claim;
                            }
                        },
                    }
                },
                Some(r) = conditional_recv(&mut self.common_b) => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            let claim = transmit_claim_after_request(self.transmit_is_ours, DUAL_RANGE_B, &cv.control_value);
                            if let Some(ref mut cb) = self.common_b
                                && cb.process_control_update(cv, &mut self.command_sender_b).await.is_ok() {
                                self.transmit_is_ours = claim;
                            }
                        },
                    }
                }
            }
        }
    }

    pub(super) async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), RadarError> {
        loop {
            if let Err(e) = self.start_sockets().await {
                log::warn!("{}: Failed to start sockets: {}", self.common.key, e);
                sleep(Duration::from_millis(1000)).await;
                continue;
            }

            match self.socket_loop(subsys).await {
                Err(RadarError::Shutdown) => {
                    return Ok(());
                }
                _ => {
                    self.report_socket = None;
                    self.data_socket = None;
                }
            }

            sleep(Duration::from_millis(1000)).await;
        }
    }

    fn process_report(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < GMN_HEADER_LEN {
            bail!("Report too short: {} bytes", data.len());
        }

        let header: GmnHeader = decode_head(data)?;

        log::trace!(
            "{}: Report packet_type={:04X} len={}",
            self.common.key,
            header.packet_type,
            header.payload_len
        );

        match header.packet_type {
            // HD spoke data (on same port as reports)
            MSG_HD_SPOKE if self.radar_type == GarminRadarType::HD => {
                self.process_hd_spoke(data)?;
            }
            MSG_HD_STATE => self.process_hd_status(data)?,
            MSG_HD_SETTINGS => {
                log::trace!("{}: HD settings packet len={}", self.common.key, data.len());
            }
            // status reports
            MSG_RPM_MODE => self.process_scan_speed(data)?,
            // 0x0918 (current transmit mode) and 0x0919 (set transmit mode)
            // both report the radar's transmit state. Treat them
            // identically — the radar broadcasts both, and the MFD
            // pulls the same handler off either ID.
            MSG_TRANSMIT_MODE | MSG_TRANSMIT_MODE_CURRENT => self.process_transmit_state(data)?,
            MSG_DITHER_MODE => self.process_dither_mode(data)?,
            MSG_RANGE_MODE => self.process_range_mode(data)?,
            MSG_RANGE_A => self.process_range(data)?,
            MSG_AFC_MODE => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: AFC mode: {}", self.common.key, v);
                // 0=manual, 1=auto → map to Tune auto flag
                self.common
                    .set_value_auto(&ControlId::Tune, 0.0, if v == 1 { 1 } else { 0 });
            }
            MSG_AFC_SETTING => self.process_afc_setting(data)?,
            MSG_AFC_COARSE => self.process_afc_coarse(data)?,
            MSG_AFC_TUNING_MODE => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: AFC tuning mode: {}", self.common.key, v);
            }
            MSG_AFC_PROGRESS => self.process_afc_progress(data)?,
            MSG_PARK_POSITION => {
                let v = self.extract_xhd_value(data)? as i32;
                let degrees = v / DEGREE_SCALE;
                log::debug!("{}: park position: {} deg", self.common.key, degrees);
                self.common
                    .set_value(&ControlId::ParkPosition, degrees as f64);
            }
            MSG_ANTENNA_SIZE => self.process_antenna_size(data)?,
            MSG_TRANSMIT_POWER => self.process_transmit_power(data)?,
            MSG_INPUT_VOLTAGE => self.process_input_voltage(data)?,
            MSG_HEATER_VOLTAGE => self.process_heater_voltage(data)?,
            MSG_HIGH_VOLTAGE => self.process_high_voltage(data)?,
            MSG_TRANSMIT_CURRENT => self.process_transmit_current(data)?,
            MSG_SYSTEM_TEMPERATURE => self.process_system_temperature(data)?,
            MSG_OPERATION_TIME => self.process_operation_time(data)?,
            MSG_MODULATOR_TIME => self.process_modulator_time(data)?,
            MSG_TRANSMIT_TIME => self.process_transmit_time_total(data)?,
            MSG_RANGE_A_GAIN_MODE => self.process_gain_mode(data)?,
            MSG_RANGE_A_GAIN => self.process_gain_level(data)?,
            MSG_RANGE_A_AUTO_LEVEL => self.process_gain_auto_level(data)?,
            MSG_BEARING_ALIGNMENT => self.process_bearing_alignment(data)?,
            MSG_NOISE_BLANKER => self.process_crosstalk(data)?,
            MSG_RANGE_A_RAIN_MODE => self.process_rain_mode(data)?,
            MSG_RANGE_A_RAIN_GAIN => self.process_rain_level(data)?,
            MSG_RANGE_A_SEA_MODE => self.process_sea_mode(data)?,
            MSG_RANGE_A_SEA_GAIN => self.process_sea_level(data)?,
            MSG_RANGE_A_SEA_STATE => self.process_sea_auto_level(data)?,
            MSG_NO_TX_ZONE_1_MODE => self.process_no_tx_1_mode(data)?,
            MSG_NO_TX_ZONE_1_START => self.process_no_tx_1_start(data)?,
            MSG_NO_TX_ZONE_1_STOP => self.process_no_tx_1_stop(data)?,
            MSG_NO_TX_ZONE_2_MODE => self.process_no_tx_2_mode(data)?,
            MSG_NO_TX_ZONE_2_START => self.process_no_tx_2_start(data)?,
            MSG_NO_TX_ZONE_2_STOP => self.process_no_tx_2_stop(data)?,
            // Range B per-range reports — route to common_b / range_b
            MSG_RANGE_B => self.process_range_b_range(data)?,
            MSG_RANGE_B_GAIN_MODE => self.process_range_b_gain_mode(data)?,
            MSG_RANGE_B_GAIN => self.process_range_b_gain_level(data)?,
            MSG_RANGE_B_RADAR_MODE => self.process_range_b_gain_auto_level(data)?,
            MSG_RANGE_B_RAIN_MODE => self.process_range_b_rain_mode(data)?,
            MSG_RANGE_B_RAIN_GAIN => self.process_range_b_rain_level(data)?,
            MSG_RANGE_B_SEA_MODE => self.process_range_b_sea_mode(data)?,
            MSG_RANGE_B_SEA_GAIN => self.process_range_b_sea_level(data)?,
            MSG_RANGE_B_SEA_STATE => self.process_range_b_sea_auto_level(data)?,
            MSG_RANGE_A_DOPPLER_MODE => self.process_scan_mode(data)?,
            MSG_RANGE_B_DOPPLER_MODE => self.process_range_b_scan_mode(data)?,
            MSG_RANGE_A_DOPPLER_SENSITIVITY | MSG_RANGE_B_DOPPLER_SENSITIVITY => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: doppler sensitivity: {}", self.common.key, v);
            }
            MSG_DEFAULT_DOPPLER_SENSITIVITY => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: default doppler sensitivity: {}", self.common.key, v);
            }
            // Transmit channel (Fantom Pro)
            MSG_TRANSMIT_CHANNEL_MODE => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: transmit channel mode: {}", self.common.key, v);
                // 0=manual, 1=auto. The mode arrives without a channel beside
                // it, and the channel numbers start at 1, so setting the auto
                // flag through a value-bearing call would offer 0 and have the
                // whole update refused for being below the minimum. The channel
                // itself comes from MSG_TRANSMIT_CHANNEL_SELECT below.
                let _ = self
                    .common
                    .info
                    .controls
                    .set_auto_state(&ControlId::TransmitChannel, v == 1);
            }
            MSG_TRANSMIT_CHANNEL_SELECT => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: transmit channel select: {}", self.common.key, v);
                self.common.set_value(&ControlId::TransmitChannel, v as f64);
            }
            MSG_TRANSMIT_CHANNEL_MAX => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: transmit channel max: {}", self.common.key, v);
            }
            // Pulse expansion (xHD2+)
            MSG_RANGE_A_PULSE_EXPANSION => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: pulse expansion A: {}", self.common.key, v);
                self.common.set_value(&ControlId::TargetExpansion, v as f64);
            }
            MSG_RANGE_B_PULSE_EXPANSION => {
                self.with_range_b(data, |common, _rs, d| {
                    let v = Self::extract_value(d)?;
                    log::debug!("{}: pulse expansion B: {}", common.key, v);
                    common.set_value(&ControlId::TargetExpansion, v as f64);
                    Ok(())
                })?;
            }
            // Target size mode (xHD2/Fantom)
            MSG_RANGE_A_TARGET_SIZE => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: target size A: {}", self.common.key, v);
                self.common.set_value(&ControlId::TargetBoost, v as f64);
            }
            MSG_RANGE_B_TARGET_SIZE => {
                self.with_range_b(data, |common, _rs, d| {
                    let v = Self::extract_value(d)?;
                    log::debug!("{}: target size B: {}", common.key, v);
                    common.set_value(&ControlId::TargetBoost, v as f64);
                    Ok(())
                })?;
            }
            // Scan average (xHD3/Fantom Pro)
            MSG_RANGE_A_SCAN_AVERAGE_MODE => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: scan average mode A: {}", self.common.key, v);
                self.common.set_value(&ControlId::ScanAverageMode, v as f64);
            }
            MSG_RANGE_B_SCAN_AVERAGE_MODE => {
                self.with_range_b(data, |common, _rs, d| {
                    let v = Self::extract_value(d)?;
                    log::debug!("{}: scan average mode B: {}", common.key, v);
                    common.set_value(&ControlId::ScanAverageMode, v as f64);
                    Ok(())
                })?;
            }
            MSG_RANGE_A_SCAN_AVERAGE_SENSITIVITY => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: scan average sensitivity A: {}", self.common.key, v);
                self.common
                    .set_value(&ControlId::ScanAverageSensitivity, v as f64);
            }
            MSG_RANGE_B_SCAN_AVERAGE_SENSITIVITY => {
                self.with_range_b(data, |common, _rs, d| {
                    let v = Self::extract_value(d)?;
                    log::debug!("{}: scan average sensitivity B: {}", common.key, v);
                    common.set_value(&ControlId::ScanAverageSensitivity, v as f64);
                    Ok(())
                })?;
            }
            MSG_SENTRY_MODE => self.process_timed_idle_mode(data)?,
            MSG_SENTRY_STANDBY_TIME => self.process_timed_idle_time(data)?,
            MSG_SENTRY_TRANSMIT_TIME => self.process_timed_run_time(data)?,
            MSG_SCANNER_STATE => self.process_scanner_state(data)?,
            MSG_STATE_CHANGE => self.process_state_change(data)?,
            MSG_ERROR_MESSAGE => self.process_message(data)?,
            MSG_CAPABILITY => self.process_capability(data)?,
            MSG_RANGE_TABLE => self.process_range_table(data)?,
            _ => {
                if !self.reported_unknown.contains_key(&header.packet_type) {
                    log::debug!(
                        "{}: Unknown report packet_type={:04X} len={}",
                        self.common.key,
                        header.packet_type,
                        header.payload_len
                    );
                    self.reported_unknown.insert(header.packet_type, true);
                }
            }
        }

        Ok(())
    }

    fn process_data(&mut self, data: &[u8]) -> Result<(), Error> {
        let header: EnhancedSpokeHeader = decode_head(data)?;

        // In dual-range mode, the range indicator selects which CommonRadar /
        // RangeState receives the spoke.
        if header.range_indicator == 1 {
            if let (Some(common_b), Some(rs)) = (&mut self.common_b, &mut self.range_b) {
                Self::process_spoke_for(common_b, rs, &header, data)?;
            }
        } else {
            Self::process_spoke_for(&mut self.common, &mut self.range_a, &header, data)?;
        }

        Ok(())
    }

    fn process_hd_spoke(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < HD_SPOKE_HEADER_SIZE + 4 {
            bail!("HD spoke packet too short: {} bytes", data.len());
        }

        let header: HdSpokeHeader = decode_head(data)?;
        let angle = header.angle;
        let scan_length = header.scan_length as usize;
        let range_meters = header.range_meters + 1;

        log::trace!(
            "{}: HD spoke: angle={} scan_length={} range={}m data_len={}",
            self.common.key,
            angle,
            scan_length,
            range_meters,
            data.len()
        );

        if self.range_a.range_meters != range_meters {
            self.range_a.range_meters = range_meters;
            self.common
                .set_value(&ControlId::Range, range_meters as f64);
        }

        // HD packs 4 spokes per packet
        let spoke_data = &data[HD_SPOKE_HEADER_SIZE..];
        let bytes_per_spoke = scan_length / HD_SPOKES_PER_PACKET;

        if spoke_data.len() < scan_length {
            log::warn!(
                "{}: HD spoke data too short: {} < {}",
                self.common.key,
                spoke_data.len(),
                scan_length
            );
            return Ok(());
        }

        self.common.new_spoke_message();

        for i in 0..HD_SPOKES_PER_PACKET {
            let spokes = self.common.info.spokes_per_revolution;
            let spoke_angle = (angle * 2 + i as u16) % spokes;
            let start = i * bytes_per_spoke;
            let end = start + bytes_per_spoke;

            if end > spoke_data.len() {
                break;
            }

            let packed_data = &spoke_data[start..end];

            // Unpack 1-bit samples to 8-bit
            let samples = unpack_hd_spoke(packed_data, &self.range_a.wire_to_legend);

            self.common
                .add_spoke(range_meters, spoke_angle, None, samples);
        }

        self.common.send_spoke_message();
        Ok(())
    }

    /// Process an enhanced-protocol spoke, routing it to the given
    /// CommonRadar and RangeState. This is a static method so that it
    /// can be called for either Range A or Range B without borrowing
    /// `self` mutably twice.
    fn process_spoke_for(
        common: &mut CommonRadar,
        rs: &mut RangeState,
        header: &EnhancedSpokeHeader,
        data: &[u8],
    ) -> Result<(), Error> {
        let angle = header.angle;
        let range_meters = header.range_meters;
        let scan_length_bytes = header.scan_length_bytes as usize;

        // Validate packet has enough data
        if data.len() < SPOKE_HEADER_SIZE + scan_length_bytes {
            log::warn!(
                "{}: spoke packet incomplete: {} < {} + {}",
                common.key,
                data.len(),
                SPOKE_HEADER_SIZE,
                scan_length_bytes
            );
            return Ok(());
        }

        // Angle is in 1/8 degree units (0-11519 for 0-1439.875 degrees)
        let spokes = common.info.spokes_per_revolution;
        let spoke_angle = (angle / ANGLE_UNITS_PER_SPOKE) % spokes;

        log::trace!(
            "{}: spoke: angle={} spoke_angle={} range={}m data_len={} scan_len={}",
            common.key,
            angle,
            spoke_angle,
            range_meters,
            data.len(),
            scan_length_bytes
        );

        if rs.range_meters != range_meters {
            rs.range_meters = range_meters;
            common.set_value(&ControlId::Range, range_meters as f64);
        }

        let spoke_data = &data[SPOKE_HEADER_SIZE..];
        if spoke_data.is_empty() {
            return Ok(());
        }

        common.new_spoke_message();

        // 8-bit samples, map wire values to legend indices
        let samples: GenericSpoke = spoke_data
            .iter()
            .map(|&v| rs.wire_to_legend[v as usize])
            .collect();

        common.add_spoke(range_meters, spoke_angle, None, samples);
        common.send_spoke_message();
        Ok(())
    }

    fn process_hd_status(&mut self, data: &[u8]) -> Result<(), Error> {
        let report: HdStatusReport = decode_head(data)?;

        let scanner_state = report.scanner_state;
        let warmup = report.warmup;
        let range_meters = report.range_meters + 1;
        let gain_level = report.gain_level;
        let gain_mode = report.gain_mode;
        let sea_clutter_level = report.sea_clutter_level;
        let sea_clutter_mode = report.sea_clutter_mode;
        let rain_clutter_level = report.rain_clutter_level;
        let dome_offset = report.dome_offset;
        let crosstalk_onoff = report.crosstalk_onoff;
        let dome_speed = report.dome_speed;

        log::debug!(
            "{}: HD status: state={} warmup={} range={}m gain={}({}) sea={}({}) rain={} bearing={} ir={} speed={}",
            self.common.key,
            scanner_state,
            warmup,
            range_meters,
            gain_level,
            gain_mode,
            sea_clutter_level,
            sea_clutter_mode,
            rain_clutter_level,
            dome_offset,
            crosstalk_onoff,
            dome_speed
        );

        // Update controls
        let power = match scanner_state {
            HD_STATE_WARMING_UP => Power::Preparing,
            HD_STATE_STANDBY => Power::Standby,
            HD_STATE_TRANSMIT => Power::Transmit,
            HD_STATE_SPINNING_UP => Power::Preparing,
            _ => Power::Off,
        };
        self.note_reported_power(power);

        self.common.set_value_enabled(
            &ControlId::WarmupTime,
            warmup as f64,
            warmup.min(u8::MAX as u16) as u8,
        );

        self.common.set_value_auto(
            &ControlId::Gain,
            gain_level as f64,
            if gain_mode > 0 { 1 } else { 0 },
        );
        self.common.set_value_auto(
            &ControlId::Sea,
            sea_clutter_level as f64,
            if sea_clutter_mode == 2 { 1 } else { 0 },
        );
        self.common
            .set_value(&ControlId::Rain, rain_clutter_level as f64);
        self.common
            .set_value(&ControlId::BearingAlignment, dome_offset as f64);
        self.common
            .set_value(&ControlId::InterferenceRejection, crosstalk_onoff as f64);
        self.common
            .set_value(&ControlId::ScanSpeed, dome_speed as f64);

        Ok(())
    }

    // status handlers
    fn process_scan_speed(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: scan speed: {}", self.common.key, value >> 1);
        self.common
            .set_value(&ControlId::ScanSpeed, (value >> 1) as f64);
        Ok(())
    }

    fn process_transmit_state(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: transmit state: {}", self.common.key, value);
        let power = if value == 1 {
            Power::Transmit
        } else {
            Power::Standby
        };
        self.note_reported_power(power);
        Ok(())
    }

    fn process_range(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: range: {} m", self.common.key, value);
        self.range_a.range_meters = value;
        self.common.set_value(&ControlId::Range, value as f64);
        Ok(())
    }

    fn process_gain_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: gain mode: {}", self.common.key, value);
        // 0 = manual, 2 = auto.
        self.range_a.gain_auto = value == 2;
        self.publish_gain();
        Ok(())
    }

    fn process_gain_level(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        let scaled = value / GAIN_SCALE as u32;
        log::debug!("{}: gain level: {}", self.common.key, scaled);
        self.range_a.gain_level = scaled;
        self.publish_gain();
        Ok(())
    }

    fn process_gain_auto_level(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: gain auto level: {}", self.common.key, value);
        // 0 = auto low, 1 = auto high. Not yet surfaced as a control.
        Ok(())
    }

    /// Push Range A gain state to ControlId::Gain.
    fn publish_gain(&mut self) {
        Self::publish_gain_for(&mut self.common, &self.range_a);
    }

    fn process_bearing_alignment(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)? as i32;
        let degrees = value / DEGREE_SCALE;
        log::debug!("{}: bearing alignment: {} deg", self.common.key, degrees);
        self.common
            .set_value(&ControlId::BearingAlignment, degrees as f64);
        Ok(())
    }

    fn process_crosstalk(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: crosstalk: {}", self.common.key, value);
        self.common
            .set_value(&ControlId::InterferenceRejection, value as f64);
        Ok(())
    }

    fn process_rain_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: rain mode: {}", self.common.key, value);
        let enabled = if value == 1 { 1u8 } else { 0u8 };
        self.common
            .set_value_enabled(&ControlId::Rain, 0.0, enabled);
        Ok(())
    }

    fn process_rain_level(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        let scaled = value / GAIN_SCALE as u32;
        log::debug!("{}: rain level: {}", self.common.key, scaled);
        self.common.set_value(&ControlId::Rain, scaled as f64);
        Ok(())
    }

    fn process_sea_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: sea mode: {}", self.common.key, value);
        let auto = if value == 2 { 1u8 } else { 0u8 };
        self.common.set_value_auto(&ControlId::Sea, 0.0, auto);
        Ok(())
    }

    fn process_sea_level(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        let scaled = value / GAIN_SCALE as u32;
        log::debug!("{}: sea level: {}", self.common.key, scaled);
        self.common.set_value(&ControlId::Sea, scaled as f64);
        Ok(())
    }

    fn process_sea_auto_level(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: sea auto level: {}", self.common.key, value);
        // Just log for now
        Ok(())
    }

    // -----------------------------------------------------------------
    // Range B report handlers — thin wrappers targeting common_b
    // -----------------------------------------------------------------

    fn with_range_b<F>(&mut self, data: &[u8], f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut CommonRadar, &mut RangeState, &[u8]) -> Result<(), Error>,
    {
        if let (Some(common_b), Some(rs)) = (&mut self.common_b, &mut self.range_b) {
            f(common_b, rs, data)
        } else {
            Ok(()) // Silently ignore if no Range B attached
        }
    }

    fn process_range_b_range(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, rs, d| {
            let value = Self::extract_value(d)?;
            log::debug!("{}: range B: {} m", common.key, value);
            rs.range_meters = value;
            common.set_value(&ControlId::Range, value as f64);
            Ok(())
        })
    }

    fn process_range_b_gain_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, rs, d| {
            let value = Self::extract_value(d)?;
            log::debug!("{}: range B gain mode: {}", common.key, value);
            rs.gain_auto = value == 2;
            Self::publish_gain_for(common, rs);
            Ok(())
        })
    }

    fn process_range_b_gain_level(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, rs, d| {
            let value = Self::extract_value(d)?;
            let scaled = value / GAIN_SCALE as u32;
            log::debug!("{}: range B gain level: {}", common.key, scaled);
            rs.gain_level = scaled;
            Self::publish_gain_for(common, rs);
            Ok(())
        })
    }

    fn process_range_b_gain_auto_level(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, _rs, d| {
            let value = Self::extract_value(d)?;
            log::debug!("{}: range B gain auto level: {}", common.key, value);
            Ok(())
        })
    }

    fn process_range_b_rain_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, _rs, d| {
            let value = Self::extract_value(d)?;
            log::debug!("{}: range B rain mode: {}", common.key, value);
            let enabled = if value == 1 { 1u8 } else { 0u8 };
            common.set_value_enabled(&ControlId::Rain, 0.0, enabled);
            Ok(())
        })
    }

    fn process_range_b_rain_level(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, _rs, d| {
            let value = Self::extract_value(d)?;
            let scaled = value / GAIN_SCALE as u32;
            log::debug!("{}: range B rain level: {}", common.key, scaled);
            common.set_value(&ControlId::Rain, scaled as f64);
            Ok(())
        })
    }

    fn process_range_b_sea_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, _rs, d| {
            let value = Self::extract_value(d)?;
            log::debug!("{}: range B sea mode: {}", common.key, value);
            let auto = if value == 2 { 1u8 } else { 0u8 };
            common.set_value_auto(&ControlId::Sea, 0.0, auto);
            Ok(())
        })
    }

    fn process_range_b_sea_level(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, _rs, d| {
            let value = Self::extract_value(d)?;
            let scaled = value / GAIN_SCALE as u32;
            log::debug!("{}: range B sea level: {}", common.key, scaled);
            common.set_value(&ControlId::Sea, scaled as f64);
            Ok(())
        })
    }

    fn process_range_b_sea_auto_level(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, _rs, d| {
            let value = Self::extract_value(d)?;
            log::debug!("{}: range B sea auto level: {}", common.key, value);
            Ok(())
        })
    }

    fn process_range_b_scan_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        self.with_range_b(data, |common, rs, d| {
            let value = Self::extract_value(d)?;
            log::debug!("{}: range B doppler mode: {}", common.key, value);
            let mode = match value {
                1 => DopplerMode::Approaching,
                2 => DopplerMode::Both,
                _ => DopplerMode::None,
            };
            rs.doppler = mode;
            common.set_value(&ControlId::Doppler, mode as i32 as f64);
            Ok(())
        })
    }

    fn process_no_tx_1_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        self.process_no_tx_mode(data, NoTxZone::One)
    }

    fn process_no_tx_1_start(&mut self, data: &[u8]) -> Result<(), Error> {
        self.process_no_tx_start(data, NoTxZone::One)
    }

    fn process_no_tx_1_stop(&mut self, data: &[u8]) -> Result<(), Error> {
        self.process_no_tx_stop(data, NoTxZone::One)
    }

    fn process_no_tx_2_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        self.process_no_tx_mode(data, NoTxZone::Two)
    }

    fn process_no_tx_2_start(&mut self, data: &[u8]) -> Result<(), Error> {
        self.process_no_tx_start(data, NoTxZone::Two)
    }

    fn process_no_tx_2_stop(&mut self, data: &[u8]) -> Result<(), Error> {
        self.process_no_tx_stop(data, NoTxZone::Two)
    }

    fn process_no_tx_mode(&mut self, data: &[u8], zone: NoTxZone) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        let enabled = value == 1;
        log::debug!(
            "{}: no-TX zone {} mode: {} (enabled={})",
            self.common.key,
            zone.number(),
            value,
            enabled
        );
        self.pending_no_tx_mut(zone).enabled = Some(enabled);
        self.try_set_no_tx_sector(zone);
        Ok(())
    }

    fn process_no_tx_start(&mut self, data: &[u8], zone: NoTxZone) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)? as i32;
        let degrees = value / DEGREE_SCALE;
        log::debug!(
            "{}: no-TX zone {} start: {} deg",
            self.common.key,
            zone.number(),
            degrees
        );
        self.pending_no_tx_mut(zone).start = Some(degrees as f64);
        self.try_set_no_tx_sector(zone);
        Ok(())
    }

    fn process_no_tx_stop(&mut self, data: &[u8], zone: NoTxZone) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)? as i32;
        let degrees = value / DEGREE_SCALE;
        log::debug!(
            "{}: no-TX zone {} stop: {} deg",
            self.common.key,
            zone.number(),
            degrees
        );
        self.pending_no_tx_mut(zone).end = Some(degrees as f64);
        self.try_set_no_tx_sector(zone);
        Ok(())
    }

    fn pending_no_tx_mut(&mut self, zone: NoTxZone) -> &mut PendingNoTxSector {
        match zone {
            NoTxZone::One => &mut self.no_tx_1,
            NoTxZone::Two => &mut self.no_tx_2,
        }
    }

    /// Try to set the no-transmit sector for the given zone if all three
    /// fragments (mode, start, stop) have arrived.
    fn try_set_no_tx_sector(&mut self, zone: NoTxZone) {
        let pending = self.pending_no_tx_mut(zone);
        let (Some(enabled), Some(start), Some(end)) = (pending.enabled, pending.start, pending.end)
        else {
            return;
        };
        let control = zone.control_id();
        log::debug!(
            "{}: Setting no-TX zone {}: enabled={} start={} end={}",
            self.common.key,
            zone.number(),
            enabled,
            start,
            end
        );
        self.common.set_sector(&control, start, end, Some(enabled));
    }

    fn process_timed_idle_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: sentry mode: {}", self.common.key, value);
        // 0x0942: 0=off, 1=on. Surface to TimedIdle as a list value.
        self.common.set_value(&ControlId::TimedIdle, value as f64);
        Ok(())
    }

    fn process_timed_idle_time(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: sentry standby time: {} s", self.common.key, value);
        // 0x0943 is the standby period — we expose only the transmit
        // period (TimedRun) for now, since mayara's API has no second
        // sentry-period control.
        Ok(())
    }

    fn process_timed_run_time(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: sentry transmit time: {} s", self.common.key, value);
        self.common.set_value(&ControlId::TimedRun, value as f64);
        Ok(())
    }

    fn process_scanner_state(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: scanner state: {}", self.common.key, value);

        let power = match value {
            STATE_WARMING_UP => Power::Preparing,
            STATE_STANDBY => Power::Standby,
            STATE_SPINNING_UP | STATE_STARTING => Power::Preparing,
            STATE_TRANSMIT => Power::Transmit,
            STATE_STOPPING | STATE_SPINNING_DOWN => Power::Preparing,
            _ => Power::Off,
        };
        self.note_reported_power(power);
        Ok(())
    }

    /// Publish the power the radar reported and let a Standby or Off end our
    /// claim on the transmit, whoever put it there.
    fn note_reported_power(&mut self, power: Power) {
        self.transmit_is_ours = transmit_claim_after_report(self.transmit_is_ours, power);
        self.common
            .set_value(&ControlId::Power, power as i32 as f64);
    }

    /// Nobody is watching: if the radar is transmitting on our behalf, put it
    /// in Standby. The radar itself never stands down on losing a client, so
    /// this is the only way an unwatched Garmin radar stops. A send failure is
    /// logged and retried on the next tick; the claim stays until the radar
    /// reports Standby.
    async fn stand_down_our_transmit(&mut self) {
        let Some(range) = self.transmit_is_ours else {
            return;
        };
        let (target, sender) = match (&self.common_b, range) {
            (Some(cb), DUAL_RANGE_B) => (cb, &mut self.command_sender_b),
            _ => (&self.common, &mut self.command_sender),
        };
        let Some(cs) = sender else {
            return;
        };
        log::info!(
            "{}: nobody watching and the radar transmits for us, sending standby",
            target.key
        );
        let standby = ControlValue::new(ControlId::Power, Value::from(Power::Standby as i32));
        if let Err(e) = cs.set_control(&standby, &target.info.controls).await {
            log::warn!("{}: standby request failed: {}", target.key, e);
        }
    }

    fn process_state_change(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        let seconds = value / 1000;
        log::debug!("{}: state change in {} s", self.common.key, seconds);
        self.common.set_value_enabled(
            &ControlId::WarmupTime,
            seconds as f64,
            seconds.min(u8::MAX as u32) as u8,
        );
        Ok(())
    }

    fn process_message(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < 16 + 64 {
            return Ok(());
        }

        let info: [u8; 64] = data[16..16 + 64].try_into().unwrap();
        if let Some(msg) = c_string(&info) {
            log::debug!("{}: message: \"{}\"", self.common.key, msg);
        }
        Ok(())
    }

    // -----------------------------------------------------------------
    // Telemetry / informational status handlers
    //
    // Most of these surface as read-only controls in /api/v1/radars; a
    // few are still log-only because no matching ControlId exists yet.
    // -----------------------------------------------------------------

    fn process_dither_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: dither mode: {}", self.common.key, value);
        Ok(())
    }

    fn process_range_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        // 0=single, 1=dual. Logged for now; dual range isn't exposed.
        log::debug!("{}: range mode: {}", self.common.key, value);
        Ok(())
    }

    fn process_afc_setting(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: AFC setting: {}", self.common.key, value);
        Ok(())
    }

    fn process_afc_coarse(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: AFC coarse: {}", self.common.key, value);
        Ok(())
    }

    fn process_afc_progress(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: AFC tuning progress: {}%", self.common.key, value);
        Ok(())
    }

    fn process_antenna_size(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: antenna size: {}", self.common.key, value);
        Ok(())
    }

    fn process_transmit_power(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: transmit power: {}", self.common.key, value);
        Ok(())
    }

    fn process_input_voltage(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: input voltage raw: {}", self.common.key, value);
        self.common
            .set_value(&ControlId::SupplyVoltage, value as f64);
        Ok(())
    }

    fn process_heater_voltage(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: heater voltage raw: {}", self.common.key, value);
        Ok(())
    }

    fn process_high_voltage(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: high voltage raw: {}", self.common.key, value);
        Ok(())
    }

    fn process_transmit_current(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: transmit current raw: {}", self.common.key, value);
        // Surface as MagnetronCurrent so it shows up in the API.
        self.common
            .set_value(&ControlId::MagnetronCurrent, value as f64);
        Ok(())
    }

    fn process_system_temperature(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: system temperature raw: {}", self.common.key, value);
        self.common
            .set_value(&ControlId::DeviceTemperature, value as f64);
        Ok(())
    }

    fn process_operation_time(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: operation time: {} s", self.common.key, value);
        self.common
            .set_value(&ControlId::OperatingTime, value as f64);
        Ok(())
    }

    fn process_modulator_time(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: modulator time: {} s", self.common.key, value);
        Ok(())
    }

    fn process_transmit_time_total(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: transmit time: {} s", self.common.key, value);
        self.common
            .set_value(&ControlId::TransmitTime, value as f64);
        Ok(())
    }

    /// Handle the Range A scan mode / MotionScope (0x0960).
    /// Garmin wire values: 0=off, 1=approaching, 2=both.
    /// Internal DopplerMode: None=0, Both=1, Approaching=2 (Navico order).
    fn process_scan_mode(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        log::debug!("{}: scan mode (Doppler): {}", self.common.key, value);

        // Map Garmin wire → DopplerMode (values are swapped vs Navico).
        let mode = match value {
            1 => DopplerMode::Approaching,
            2 => DopplerMode::Both,
            _ => DopplerMode::None,
        };

        if mode != self.range_a.doppler {
            self.range_a.doppler = mode;
            // Rebuild the lookup table so spoke data in the 0xF0–0xFF
            // doppler range maps to the correct legend entries.
            self.range_a.wire_to_legend = wire_to_legend(
                &self.common.info.get_legend(),
                self.radar_type == GarminRadarType::XHD,
                mode != DopplerMode::None,
            );
            log::info!(
                "{}: MotionScope mode changed to {:?}",
                self.common.key,
                mode
            );
        }

        self.common
            .set_value(&ControlId::Doppler, mode as i32 as f64);
        Ok(())
    }

    /// Parse the broadcast range table (`0x09B2`) and replace the
    /// hardcoded fallback in `RadarInfo::ranges` with what the radar
    /// actually supports. We only do this on first receipt; subsequent
    /// `0x09B2` messages are ignored unless the table is observed to
    /// change (which doesn't happen in any observed capture).
    fn process_range_table(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.range_table_seen {
            return Ok(());
        }
        let payload = &data[GMN_HEADER_LEN..];
        match range_table::parse(payload) {
            Some(ranges) => {
                log::info!(
                    "{}: range table received: {} entries",
                    self.common.key,
                    ranges.all.len(),
                );
                self.common.set_ranges(ranges);
                self.range_table_seen = true;
            }
            None => {
                log::warn!(
                    "{}: malformed range table message ({} bytes)",
                    self.common.key,
                    payload.len()
                );
            }
        }
        Ok(())
    }

    /// Parse the capability bitmap (`0x09B1`). The radar broadcasts
    /// this once per session at warmup completion; we use it to know which
    /// features the radar supports for control gating in later phases.
    fn process_capability(&mut self, data: &[u8]) -> Result<(), Error> {
        // Skip the 8-byte GMN header so the parser sees the same payload
        // layout as `feature-detection.md` documents.
        let payload = &data[GMN_HEADER_LEN..];
        match GarminCapabilities::parse(payload) {
            Some(caps) => {
                if !self.capabilities_seen {
                    log::info!(
                        "{}: capabilities received: dual_range={} motionscope={} \
                         echo_trails={} pulse_expansion={} no_tx_zone_2={} sentry={} fantom={}",
                        self.common.key,
                        caps.has_dual_range(),
                        caps.has_motionscope(),
                        caps.has_echo_trails(),
                        caps.has_pulse_expansion(),
                        caps.has_no_tx_zone_2(),
                        caps.has_sentry_mode(),
                        caps.is_fantom(),
                    );
                }
                self.capabilities = caps;
                self.capabilities_seen = true;
            }
            None => {
                log::warn!(
                    "{}: capability message too short: {} bytes",
                    self.common.key,
                    payload.len()
                );
            }
        }
        Ok(())
    }

    /// Extract value from status packet based on length (instance method).
    fn extract_xhd_value(&self, data: &[u8]) -> Result<u32, Error> {
        Self::extract_value(data)
    }

    /// Extract value from status packet based on length (static).
    fn extract_value(data: &[u8]) -> Result<u32, Error> {
        let report: ScalarReport = decode_head(data)?;

        // A width we have no layout for still has to be on the wire. Without
        // this, a header claiming three bytes while carrying none would read
        // as zero, and a caller would publish that as a setting.
        let declared = GMN_HEADER_LEN + report.header.payload_len as usize;
        if data.len() < declared {
            bail!(
                "scalar report declares {} payload bytes, datagram carries {}",
                report.header.payload_len,
                data.len() - GMN_HEADER_LEN
            );
        }

        Ok(report.value.as_u32())
    }

    /// Push combined gain state to a CommonRadar (static version for
    /// use by both Range A and Range B handlers).
    fn publish_gain_for(common: &mut CommonRadar, rs: &RangeState) {
        let auto = if rs.gain_auto { 1u8 } else { 0u8 };
        common.set_value_auto(&ControlId::Gain, rs.gain_level as f64, auto);
    }
}

/// Receive a control update from an optional CommonRadar. Returns
/// `None` (which makes the `tokio::select!` branch dormant) when
/// the CommonRadar is absent.
async fn conditional_recv(
    common: &mut Option<CommonRadar>,
) -> Option<Result<crate::radar::settings::ControlUpdate, tokio::sync::broadcast::error::RecvError>>
{
    match common {
        Some(c) => Some(c.control_update_rx.recv().await),
        None => std::future::pending::<Option<_>>().await,
    }
}

/// Unpack HD 1-bit packed spoke data to 8-bit values
fn unpack_hd_spoke(packed: &[u8], wire_to_legend: &WireToLegendTable) -> GenericSpoke {
    let mut samples = Vec::with_capacity(packed.len() * 8);
    for byte in packed {
        for bit in 0..8 {
            let value = if (byte >> bit) & 1 == 1 { 255u8 } else { 0u8 };
            samples.push(wire_to_legend[value as usize]);
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scalar setting report: the GMN header, then `payload` as the value.
    /// `declared_len` is written into the header's length field, which is what
    /// picks the value's width — it is not always `payload.len()`.
    fn scalar_packet(declared_len: u32, payload: &[u8]) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&MSG_RPM_MODE.to_le_bytes());
        packet.extend_from_slice(&declared_len.to_le_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn scalar_report_reads_the_width_the_header_declares() {
        let one = scalar_packet(1, &[0x2a]);
        let two = scalar_packet(2, &0x1234u16.to_le_bytes());
        let four = scalar_packet(4, &0xdead_beefu32.to_le_bytes());

        assert_eq!(GarminReportReceiver::extract_value(&one).unwrap(), 0x2a);
        assert_eq!(GarminReportReceiver::extract_value(&two).unwrap(), 0x1234);
        assert_eq!(
            GarminReportReceiver::extract_value(&four).unwrap(),
            0xdead_beef
        );
    }

    /// A width we have no layout for reads as zero, as it always has.
    #[test]
    fn scalar_report_of_an_unknown_width_reads_as_zero() {
        let odd = scalar_packet(3, &[0x01, 0x02, 0x03]);

        assert_eq!(GarminReportReceiver::extract_value(&odd).unwrap(), 0);
    }

    /// ... but only when the packet actually carries that width. Reading a
    /// width we cannot decode must not turn a truncated datagram into a
    /// setting worth publishing.
    #[test]
    fn scalar_report_of_an_unknown_width_must_still_carry_it() {
        assert!(GarminReportReceiver::extract_value(&scalar_packet(3, &[])).is_err());
        assert!(GarminReportReceiver::extract_value(&scalar_packet(3, &[0x01, 0x02])).is_err());
    }

    /// The radar sends one setting per packet, but a datagram may carry more
    /// bytes than the value; they are not part of it.
    #[test]
    fn scalar_report_ignores_bytes_after_the_value() {
        let padded = scalar_packet(1, &[0x2a, 0xff, 0xff, 0xff]);

        assert_eq!(GarminReportReceiver::extract_value(&padded).unwrap(), 0x2a);
    }

    /// A packet whose payload is shorter than the width it declares must fail
    /// to decode. Reading the declared width out of the datagram regardless
    /// used to panic and take the report receiver down with it.
    #[test]
    fn scalar_report_shorter_than_its_declared_width_is_an_error() {
        let claims_four_has_one = scalar_packet(4, &[0x2a]);
        let claims_two_has_one = scalar_packet(2, &[0x2a]);
        let header_only = scalar_packet(1, &[]);

        assert!(GarminReportReceiver::extract_value(&claims_four_has_one).is_err());
        assert!(GarminReportReceiver::extract_value(&claims_two_has_one).is_err());
        assert!(GarminReportReceiver::extract_value(&header_only).is_err());
        assert!(GarminReportReceiver::extract_value(&[]).is_err());
    }

    /// An enhanced spoke header with the fields written at the offsets the
    /// old hand-written parser read them from.
    fn enhanced_spoke_header(
        angle: u16,
        range_meters: u32,
        scan_length_bytes: u16,
        range_indicator: u8,
    ) -> [u8; SPOKE_HEADER_SIZE] {
        let mut header = [0u8; SPOKE_HEADER_SIZE];
        header[12..14].copy_from_slice(&angle.to_le_bytes());
        header[16..20].copy_from_slice(&range_meters.to_le_bytes());
        header[SPOKE_RANGE_INDICATOR_OFFSET] = range_indicator;
        header[26..28].copy_from_slice(&scan_length_bytes.to_le_bytes());
        header
    }

    #[test]
    fn enhanced_spoke_header_reads_its_fields() {
        let bytes = enhanced_spoke_header(11519, 1852, 1024, 1);

        let header: EnhancedSpokeHeader = decode_head(&bytes).unwrap();

        assert_eq!(header.angle, 11519);
        assert_eq!(header.range_meters, 1852);
        assert_eq!(header.scan_length_bytes, 1024);
        assert_eq!(header.range_indicator, 1);
    }

    /// The header must fill its 36 bytes; samples follow it and are not part
    /// of it.
    #[test]
    fn enhanced_spoke_header_needs_its_full_length() {
        let bytes = enhanced_spoke_header(8, 1852, 4, 0);

        assert!(decode_head::<EnhancedSpokeHeader>(&bytes[..SPOKE_HEADER_SIZE - 1]).is_err());

        let mut with_samples = bytes.to_vec();
        with_samples.extend_from_slice(&[1, 2, 3, 4]);
        let header: EnhancedSpokeHeader = decode_head(&with_samples).unwrap();
        assert_eq!(header.angle, 8);
    }

    #[test]
    fn hd_spoke_header_reads_its_fields() {
        let mut bytes = [0u8; HD_SPOKE_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&MSG_HD_SPOKE.to_le_bytes());
        bytes[8..10].copy_from_slice(&359u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&1008u16.to_le_bytes());
        // The wire carries one less than the range it means.
        bytes[16..20].copy_from_slice(&5555u32.to_le_bytes());

        let header: HdSpokeHeader = decode_head(&bytes).unwrap();

        assert_eq!(header.angle, 359);
        assert_eq!(header.scan_length, 1008);
        assert_eq!(header.range_meters + 1, 5556);
    }

    #[test]
    fn hd_status_report_reads_its_fields() {
        let mut bytes = [0u8; 48];
        bytes[0..4].copy_from_slice(&MSG_HD_STATE.to_le_bytes());
        bytes[8..10].copy_from_slice(&HD_STATE_TRANSMIT.to_le_bytes());
        bytes[10..12].copy_from_slice(&90u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&1851u32.to_le_bytes());
        bytes[16] = 200; // gain level
        bytes[17] = 1; // gain mode
        bytes[20] = 51; // sea level
        bytes[21] = 2; // sea mode: auto
        bytes[24] = 77; // rain
        bytes[28..30].copy_from_slice(&(-15i16).to_le_bytes());
        bytes[31] = 1; // crosstalk
        bytes[40] = 2; // dome speed

        let report: HdStatusReport = decode_head(&bytes).unwrap();

        assert_eq!(report.scanner_state, HD_STATE_TRANSMIT);
        assert_eq!(report.warmup, 90);
        assert_eq!(report.range_meters + 1, 1852);
        assert_eq!(report.gain_level, 200);
        assert_eq!(report.gain_mode, 1);
        assert_eq!(report.sea_clutter_level, 51);
        assert_eq!(report.sea_clutter_mode, 2);
        assert_eq!(report.rain_clutter_level, 77);
        assert_eq!(report.dome_offset, -15);
        assert_eq!(report.crosstalk_onoff, 1);
        assert_eq!(report.dome_speed, 2);
        assert!(decode_head::<HdStatusReport>(&bytes[..47]).is_err());
    }

    fn identity_lookup() -> WireToLegendTable {
        let mut lookup = [0u8; BYTE_LOOKUP_LENGTH];
        for (j, slot) in lookup.iter_mut().enumerate() {
            *slot = j as u8;
        }
        lookup
    }

    /// Build a minimal `Legend` for tests.
    fn empty_legend() -> Legend {
        Legend {
            pixels: Vec::new(),
            pixel_colors: 0,
            history_start: 0,
            doppler_approaching: None,
            doppler_receding: None,
            doppler_rain: None,
            strong_return: 0,
            medium_return: 0,
            low_return: 0,
            static_background: None,
        }
    }

    #[test]
    fn unpack_hd_spoke_expands_each_bit() {
        // Two bytes = 16 bits = 16 samples. Bit 0 (LSB) of byte 0 first.
        let packed = [0b1010_1010, 0b0000_1111];
        let samples = unpack_hd_spoke(&packed, &identity_lookup());
        assert_eq!(samples.len(), 16);
        // Byte 0: 10101010 → LSB first → 0,1,0,1,0,1,0,1
        assert_eq!(&samples[..8], &[0, 255, 0, 255, 0, 255, 0, 255]);
        // Byte 1: 00001111 → LSB first → 1,1,1,1,0,0,0,0
        assert_eq!(&samples[8..], &[255, 255, 255, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn unpack_hd_spoke_empty_input() {
        let samples = unpack_hd_spoke(&[], &identity_lookup());
        assert!(samples.is_empty());
    }

    #[test]
    fn wire_to_legend_xhd_halves_intensity() {
        let lookup = wire_to_legend(&empty_legend(), true, false);
        assert_eq!(lookup[0], 0);
        assert_eq!(lookup[2], 1);
        assert_eq!(lookup[200], 100);
        assert_eq!(lookup[254], 127);
        assert_eq!(lookup[255], 127);
    }

    #[test]
    fn wire_to_legend_hd_passes_through() {
        let lookup = wire_to_legend(&empty_legend(), false, false);
        assert_eq!(lookup[0], 0);
        assert_eq!(lookup[1], 1);
        assert_eq!(lookup[128], 128);
        assert_eq!(lookup[255], 255);
    }

    #[test]
    fn wire_to_legend_xhd_doppler_maps_bands() {
        // Build a minimal legend with 4 Doppler entries per direction.
        // The approaching band starts at index 120, receding at 124.
        let mut legend = empty_legend();
        legend.doppler_approaching = Some((120, 4));
        legend.doppler_receding = Some((124, 4));
        let lookup = wire_to_legend(&legend, true, true);

        // Normal intensity: 0x00–0xEF halved.
        assert_eq!(lookup[0x00], 0, "zero stays zero");
        assert_eq!(lookup[0x02], 1, "low normal → 1");
        assert_eq!(lookup[0xEF], 0xEF / 2, "top of normal band");

        // Approaching band: 0xF0–0xF7 → 4 legend indices.
        // 8 wire sub-levels mapped to 4 legend entries via sub*4/8:
        // sub 0,1 → idx 0; sub 2,3 → idx 1; sub 4,5 → idx 2; sub 6,7 → idx 3
        assert_eq!(lookup[0xF0], 120);
        assert_eq!(lookup[0xF1], 120);
        assert_eq!(lookup[0xF2], 121);
        assert_eq!(lookup[0xF3], 121);
        assert_eq!(lookup[0xF6], 123);
        assert_eq!(lookup[0xF7], 123);

        // Receding band: 0xF8–0xFF → 4 legend indices.
        assert_eq!(lookup[0xF8], 124);
        assert_eq!(lookup[0xF9], 124);
        assert_eq!(lookup[0xFE], 127);
        assert_eq!(lookup[0xFF], 127);
    }
}
