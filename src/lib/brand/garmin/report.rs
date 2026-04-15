use anyhow::{Error, bail};
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tokio::time::sleep;
use tokio_graceful_shutdown::SubsystemHandle;

use super::GarminRadarType;
use super::capabilities::GarminCapabilities;
use super::command::Command;
use super::protocol::*;
use super::range_table;
use crate::Cli;
use crate::network;
use crate::replay::RadarSocket;
use crate::radar::settings::ControlId;
use crate::radar::spoke::GenericSpoke;
use crate::radar::{
    BYTE_LOOKUP_LENGTH, CommonRadar, DopplerMode, Legend, Power, RadarError, RadarInfo, SharedRadars,
};
use crate::util::c_string;

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
            let (appr_start, appr_count) = legend
                .doppler_approaching
                .unwrap_or((0, 0));
            let (recv_start, recv_count) = legend
                .doppler_receding
                .unwrap_or((0, 0));
            for j in 0..BYTE_LOOKUP_LENGTH {
                let jb = j as u8;
                lookup[j] = if jb >= DOPPLER_RECEDING_START {
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
            for j in 0..BYTE_LOOKUP_LENGTH {
                lookup[j] = (j / 2) as u8;
            }
        }
    } else {
        // HD: binary data, no transformation needed
        for j in 0..BYTE_LOOKUP_LENGTH {
            lookup[j] = j as u8;
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

        let wire_to_legend =
            wire_to_legend(&info.get_legend(), radar_type == GarminRadarType::XHD, false);

        let common = CommonRadar::new(
            args,
            key,
            info,
            radars.clone(),
            control_update_rx,
            replay,
            blob_tx,
        );

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
        let wire_to_legend =
            wire_to_legend(&info.get_legend(), self.radar_type == GarminRadarType::XHD, false);
        let command_sender_b = Some(Command::new_range_b(self.radar_type, info.send_command_addr));

        self.common_b = Some(CommonRadar::new(
            args,
            key,
            info,
            radars,
            control_update_rx,
            replay,
            blob_tx,
        ));
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
        match network::create_udp_listen(&self.common.info.report_addr, &self.common.info.nic_addr, network::SocketType::Multicast)
        {
            Ok(socket) => {
                self.report_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for reports",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr
                );
            }
            Err(e) => {
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
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
                        &self.common.info.spoke_data_addr,
                        &self.common.info.nic_addr
                    );
                }
                Err(e) => {
                    log::debug!(
                        "{}: {} via {}: create data multicast failed: {}",
                        self.common.key,
                        &self.common.info.spoke_data_addr,
                        &self.common.info.nic_addr,
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

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
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
                            let _ = self.common.process_control_update(cv, &mut self.command_sender).await;
                        },
                    }
                },
                Some(r) = conditional_recv(&mut self.common_b) => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            if let Some(ref mut cb) = self.common_b {
                                let _ = cb.process_control_update(cv, &mut self.command_sender_b).await;
                            }
                        },
                    }
                }
            }
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        loop {
            if let Err(e) = self.start_sockets().await {
                log::warn!("{}: Failed to start sockets: {}", self.common.key, e);
                sleep(Duration::from_millis(1000)).await;
                continue;
            }

            match self.socket_loop(&subsys).await {
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

        let packet_type = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(data[4..8].try_into().unwrap());

        log::trace!(
            "{}: Report packet_type={:04X} len={}",
            self.common.key,
            packet_type,
            len
        );

        match packet_type {
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
            MSG_TRANSMIT_MODE | MSG_TRANSMIT_MODE_CURRENT => {
                self.process_transmit_state(data)?
            }
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
                // 0=manual, 1=auto
                self.common
                    .set_value_auto(&ControlId::TransmitChannel, 0.0, if v == 1 { 1 } else { 0 });
            }
            MSG_TRANSMIT_CHANNEL_SELECT => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: transmit channel select: {}", self.common.key, v);
                self.common
                    .set_value(&ControlId::TransmitChannel, v as f64);
            }
            MSG_TRANSMIT_CHANNEL_MAX => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: transmit channel max: {}", self.common.key, v);
            }
            // Pulse expansion (xHD2+)
            MSG_RANGE_A_PULSE_EXPANSION => {
                let v = self.extract_xhd_value(data)?;
                log::debug!("{}: pulse expansion A: {}", self.common.key, v);
                self.common
                    .set_value(&ControlId::TargetExpansion, v as f64);
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
                self.common
                    .set_value(&ControlId::TargetBoost, v as f64);
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
                self.common
                    .set_value(&ControlId::ScanAverageMode, v as f64);
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
                if self.reported_unknown.get(&packet_type).is_none() {
                    log::debug!(
                        "{}: Unknown report packet_type={:04X} len={}",
                        self.common.key,
                        packet_type,
                        len
                    );
                    self.reported_unknown.insert(packet_type, true);
                }
            }
        }

        Ok(())
    }

    fn process_data(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < SPOKE_HEADER_SIZE {
            bail!("Data too short: {} bytes", data.len());
        }

        // In dual-range mode, the range indicator at data[24] selects
        // which CommonRadar / RangeState receives the spoke.
        let range_indicator = if data.len() > SPOKE_RANGE_INDICATOR_OFFSET {
            data[SPOKE_RANGE_INDICATOR_OFFSET]
        } else {
            0
        };

        if range_indicator == 1 {
            if let (Some(common_b), Some(rs)) =
                (&mut self.common_b, &mut self.range_b)
            {
                Self::process_spoke_for(common_b, rs, data)?;
            }
        } else {
            Self::process_spoke_for(&mut self.common, &mut self.range_a, data)?;
        }

        Ok(())
    }

    fn process_hd_spoke(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < HD_SPOKE_HEADER_SIZE + 4 {
            bail!("HD spoke packet too short: {} bytes", data.len());
        }

        // Parse header
        let angle = u16::from_le_bytes(data[8..10].try_into().unwrap());
        let scan_length = u16::from_le_bytes(data[10..12].try_into().unwrap()) as usize;
        let range_meters = u32::from_le_bytes(data[16..20].try_into().unwrap()) + 1;

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
        data: &[u8],
    ) -> Result<(), Error> {
        if data.len() < SPOKE_HEADER_SIZE {
            bail!("spoke packet too short: {} bytes", data.len());
        }

        // Parse header (matching C++ radar_line struct)
        // Offsets: packet_type(0-3), len1(4-7), fill_1(8-9), scan_length(10-11),
        //          angle(12-13), fill_2(14-15), range_meters(16-19), display_meters(20-23),
        //          range_indicator(24), fill(25), scan_length_bytes_s(26-27),
        //          fills_4(28-29), scan_length_bytes_i(30-33), fills_5(34-35),
        //          line_data(36+)
        let angle = u16::from_le_bytes(data[12..14].try_into().unwrap());
        let range_meters = u32::from_le_bytes(data[16..20].try_into().unwrap());
        let scan_length_bytes = u16::from_le_bytes(data[26..28].try_into().unwrap()) as usize;

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
        if data.len() < 48 {
            bail!("HD status packet too short");
        }

        let scanner_state = u16::from_le_bytes(data[8..10].try_into().unwrap());
        let warmup = u16::from_le_bytes(data[10..12].try_into().unwrap());
        let range_meters = u32::from_le_bytes(data[12..16].try_into().unwrap()) + 1;
        let gain_level = data[16];
        let gain_mode = data[17];
        let sea_clutter_level = data[20];
        let sea_clutter_mode = data[21];
        let rain_clutter_level = data[24];
        let dome_offset = i16::from_le_bytes(data[28..30].try_into().unwrap());
        let crosstalk_onoff = data[31];
        let dome_speed = data[40];

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
        self.common
            .set_value(&ControlId::Power, power as i32 as f64);

        if warmup > 0 {
            self.common.set_value(&ControlId::WarmupTime, warmup as f64);
        }

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
        self.common
            .set_value(&ControlId::Power, power as i32 as f64);
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
        log::debug!(
            "{}: bearing alignment: {} deg",
            self.common.key,
            degrees
        );
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
        if let (Some(common_b), Some(rs)) =
            (&mut self.common_b, &mut self.range_b)
        {
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
        self.common
            .set_sector(&control, start, end, Some(enabled));
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
        self.common
            .set_value(&ControlId::Power, power as i32 as f64);
        Ok(())
    }

    fn process_state_change(&mut self, data: &[u8]) -> Result<(), Error> {
        let value = self.extract_xhd_value(data)?;
        let seconds = value / 1000;
        log::debug!("{}: state change in {} s", self.common.key, seconds);
        if seconds > 0 {
            self.common
                .set_value(&ControlId::WarmupTime, seconds as f64);
        }
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
        log::debug!(
            "{}: system temperature raw: {}",
            self.common.key,
            value
        );
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
        if data.len() < 9 {
            bail!("packet too short");
        }

        let len = u32::from_le_bytes(data[4..8].try_into().unwrap());

        match len {
            1 => Ok(data[8] as u32),
            2 => Ok(u16::from_le_bytes(data[8..10].try_into().unwrap()) as u32),
            4 => Ok(u32::from_le_bytes(data[8..12].try_into().unwrap())),
            _ => Ok(0),
        }
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

    fn identity_lookup() -> WireToLegendTable {
        let mut lookup = [0u8; BYTE_LOOKUP_LENGTH];
        for j in 0..BYTE_LOOKUP_LENGTH {
            lookup[j] = j as u8;
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
