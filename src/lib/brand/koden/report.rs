use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::Command;
use super::protocol::*;
use crate::Cli;
use crate::radar::CommonRadar;
use crate::radar::SharedRadars;
use crate::radar::SpokeBearing;
use crate::radar::settings::ControlId;
use crate::radar::{Power, RadarError, RadarInfo};
use crate::util::PrintableSlice;

pub(crate) struct KodenReportReceiver {
    common: CommonRadar,
    command_sender: Option<Command>,
}

impl KodenReportReceiver {
    pub(crate) fn new(args: &Cli, radars: SharedRadars, info: RadarInfo) -> Self {
        let key = info.key();
        let command_sender = if args.is_replay() {
            None
        } else {
            Some(Command::new(&info))
        };

        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();

        let common = CommonRadar::new(
            args,
            key,
            info,
            radars.clone(),
            control_update_rx,
            args.is_replay(),
            blob_tx,
        );

        KodenReportReceiver {
            common,
            command_sender,
        }
    }

    pub(crate) async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        loop {
            if let Err(e) = self.data_loop(&subsys).await {
                log::error!("{}: Data loop error: {}, restarting", self.common.key, e);
            }
            if subsys.is_shutdown_requested() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn data_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        // Bind a UDP socket for sending commands
        if let Some(ref mut cmd) = self.command_sender {
            let send_socket = UdpSocket::bind("0.0.0.0:0").await.map_err(RadarError::Io)?;
            send_socket.set_broadcast(true).map_err(RadarError::Io)?;
            cmd.set_socket(send_socket);

            // Send startup request
            cmd.send_startup().await?;
        }

        // Bind receive socket on the Koden port
        let recv_socket = UdpSocket::bind(("0.0.0.0", RADAR_PORT))
            .await
            .map_err(RadarError::Io)?;
        recv_socket.set_broadcast(true).map_err(RadarError::Io)?;

        let mut buf = [0u8; 4096];
        let mut next_keepalive = Instant::now() + Duration::from_secs(KEEPALIVE_INTERVAL_SECS);

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    return Ok(());
                }

                _ = sleep_until(next_keepalive) => {
                    if let Some(ref cmd) = self.command_sender {
                        cmd.send_keepalive().await?;
                    }
                    next_keepalive = Instant::now() + Duration::from_secs(KEEPALIVE_INTERVAL_SECS);
                }

                result = recv_socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, _from)) => {
                            if len >= 3 {
                                self.process_packet(&buf[..len]);
                            }
                        }
                        Err(e) => {
                            log::error!("{}: recv error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                }

                r = self.common.control_update_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            if let Err(e) = self.common.process_control_update(cv, &mut self.command_sender).await {
                                return Err(e);
                            }
                        },
                    }
                }
            }
        }
    }

    fn process_packet(&mut self, data: &[u8]) {
        if data.len() < 3 {
            return;
        }

        match data[0] {
            CONTROL_PREFIX => self.process_control_response(data),
            STATUS_PREFIX => self.process_status_response(data),
            b'{' => {
                if data.len() >= IMG_MIN_SIZE && data[0..4] == IMAGE_MARKER {
                    self.process_image_data(data);
                }
            }
            _ => {
                log::trace!(
                    "{}: Unknown packet start 0x{:02X}",
                    self.common.key,
                    data[0]
                );
            }
        }
    }

    fn process_control_response(&mut self, data: &[u8]) {
        if data.len() < 3 || data[data.len() - 1] != PACKET_END {
            return;
        }

        match data[1] {
            RESP_WARMUP => {
                let seconds = data[2];
                if seconds == WARMUP_COMPLETE {
                    log::info!("{}: Warmup complete, standby", self.common.key);
                    let _ = self.common.info.controls.set(
                        &ControlId::Power,
                        Power::Standby as u32 as f64,
                        None,
                    );
                } else {
                    log::debug!(
                        "{}: Warming up: {}:{:02}",
                        self.common.key,
                        seconds / 60,
                        seconds % 60
                    );
                    let _ = self.common.info.controls.set(
                        &ControlId::Power,
                        Power::Preparing as u32 as f64,
                        None,
                    );
                }
            }
            RESP_POWER => {
                log::info!("{}: Power on", self.common.key);
                let _ = self.common.info.controls.set(
                    &ControlId::Power,
                    Power::Preparing as u32 as f64,
                    None,
                );
            }
            RESP_ERROR => {
                if data.len() >= 4 {
                    log::warn!("{}: Radar error code: 0x{:02X}", self.common.key, data[2]);
                }
            }
            _ => {
                log::trace!(
                    "{}: Unknown control response: {}",
                    self.common.key,
                    PrintableSlice::new(data)
                );
            }
        }
    }

    fn process_status_response(&mut self, data: &[u8]) {
        if data.len() < 3 || data[data.len() - 1] != PACKET_END {
            return;
        }

        let cmd = data[1];
        match cmd {
            CMD_GAIN => {
                if data.len() == 4 {
                    let val = (data[2] as f64) / 2.55;
                    let _ = self.common.info.controls.set(&ControlId::Gain, val, None);
                }
            }
            CMD_STC => {
                if data.len() == 4 {
                    let val = (data[2] as f64) / 2.55;
                    let _ = self.common.info.controls.set(&ControlId::Sea, val, None);
                }
            }
            CMD_FTC => {
                if data.len() == 4 {
                    let val = (data[2] as f64) / 2.55;
                    let _ = self.common.info.controls.set(&ControlId::Rain, val, None);
                }
            }
            CMD_AUTO_GAIN_MODE => {
                if data.len() == 4 {
                    let auto = data[2] == WIRE_TRUE;
                    let _ = self.common.info.controls.set(
                        &ControlId::Gain,
                        self.common
                            .info
                            .controls
                            .get(&ControlId::Gain)
                            .and_then(|c| c.value)
                            .unwrap_or(0.),
                        Some(auto),
                    );
                }
            }
            CMD_TARGET_EXPANSION => {
                if data.len() == 4 {
                    // Wire: 0x00=on, 0x11=off (inverted)
                    let val = if data[2] == WIRE_FALSE { 0. } else { 1. };
                    let _ = self
                        .common
                        .info
                        .controls
                        .set(&ControlId::TargetExpansion, val, None);
                }
            }
            CMD_AUTO_STC_MODE => {
                if data.len() == 4 {
                    let val = match data[2] {
                        0x00 => 0., // Manual
                        0x11 => 1., // Auto
                        0x22 => 2., // Harbor
                        _ => 0.,
                    };
                    let _ = self
                        .common
                        .info
                        .controls
                        .set(&ControlId::SeaState, val, None);
                }
            }
            CMD_PULSE_LENGTH => {
                if data.len() == 4 {
                    let _ =
                        self.common
                            .info
                            .controls
                            .set(&ControlId::PulseWidth, data[2] as f64, None);
                }
            }
            CMD_INTERFERENCE_REJECTION => {
                if data.len() == 4 {
                    let val = match data[2] {
                        0x00 => 0.,
                        0x11 => 1.,
                        0x22 => 2.,
                        0x33 => 3.,
                        _ => 0.,
                    };
                    let _ =
                        self.common
                            .info
                            .controls
                            .set(&ControlId::InterferenceRejection, val, None);
                }
            }
            CMD_TRANSMISSION_MODE => {
                if data.len() == 4 {
                    if data[2] == WIRE_TRUE {
                        log::info!("{}: Transmitting", self.common.key);
                        let _ = self.common.info.controls.set(
                            &ControlId::Power,
                            Power::Transmit as u32 as f64,
                            None,
                        );
                    } else {
                        log::info!("{}: Standby", self.common.key);
                        let _ = self.common.info.controls.set(
                            &ControlId::Power,
                            Power::Standby as u32 as f64,
                            None,
                        );
                    }
                }
            }
            CMD_MODEL_INFO => {
                if data.len() >= 4 {
                    let rom_version = data[2] as char;
                    let sw_version = if data.len() >= 5 { data[3] } else { 0 };
                    let serial = if data.len() >= 8 {
                        u32::from_be_bytes([data[4], data[5], data[6], data[7]])
                    } else {
                        0
                    };
                    log::info!(
                        "{}: Model info: ROM={}, SW={}, serial={}",
                        self.common.key,
                        rom_version,
                        sw_version,
                        serial
                    );
                    let _ = self
                        .common
                        .info
                        .controls
                        .set_string(&ControlId::SerialNumber, format!("{}", serial));
                }
            }
            CMD_MAC_ADDRESS => {
                if data.len() >= 9 {
                    log::info!(
                        "{}: MAC address: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
                        self.common.key,
                        data[2],
                        data[3],
                        data[4],
                        data[5],
                        data[6],
                        data[7]
                    );
                }
            }
            CMD_MODEL_CODE => {
                if data.len() == 4 {
                    let name = model_name(data[2]);
                    log::info!("{}: Model code {}: {}", self.common.key, data[2], name);
                    let _ = self
                        .common
                        .info
                        .controls
                        .set_string(&ControlId::ModelName, name.to_string());
                }
            }
            CMD_KEEPALIVE_ACK => {
                log::trace!("{}: Keep-alive ACK", self.common.key);
            }
            CMD_TUNING_MODE => {
                if data.len() == 4 {
                    let auto = data[2] == WIRE_AUTO;
                    let _ = self
                        .common
                        .info
                        .controls
                        .set_auto_state(&ControlId::Tune, auto);
                }
            }
            CMD_COARSE_TUNING => {
                if data.len() == 4 {
                    let _ = self
                        .common
                        .info
                        .controls
                        .set(&ControlId::Tune, data[2] as f64, None);
                }
            }
            CMD_FINE_TUNING => {
                if data.len() == 4 {
                    let _ =
                        self.common
                            .info
                            .controls
                            .set(&ControlId::TuneFine, data[2] as f64, None);
                }
            }
            CMD_TRIGGER_DELAY => {
                if data.len() == 4 {
                    let _ = self.common.info.controls.set(
                        &ControlId::DisplayTiming,
                        data[2] as f64,
                        None,
                    );
                }
            }
            CMD_ANTENNA_SPEED => {
                if data.len() == 4 {
                    let _ =
                        self.common
                            .info
                            .controls
                            .set(&ControlId::ScanSpeed, data[2] as f64, None);
                }
            }
            CMD_BLANKING_SECTOR => {
                if data.len() == 7 {
                    let start = ((data[2] as u16) << 8 | data[3] as u16) as f64;
                    let end = ((data[4] as u16) << 8 | data[5] as u16) as f64;
                    let enabled = start != end;
                    self.common.set_sector(
                        &ControlId::NoTransmitSector1,
                        start,
                        end,
                        Some(enabled),
                    );
                }
            }
            CMD_PARK_ANGLE => {
                if data.len() == 5 {
                    let wire = ((data[2] as u16) << 8 | data[3] as u16) as f64;
                    let _ = self
                        .common
                        .info
                        .controls
                        .set(&ControlId::ParkPosition, wire, None);
                }
            }
            _ => {
                log::trace!(
                    "{}: Status cmd=0x{:02X} len={}",
                    self.common.key,
                    cmd,
                    data.len()
                );
            }
        }
    }

    fn process_image_data(&mut self, data: &[u8]) {
        let transfer_type = data[IMG_TRANSFER_TYPE];

        // Only handle normal (2) and rotated ('R') transfers
        if transfer_type != TRANSFER_NORMAL && transfer_type != TRANSFER_ROTATED {
            return;
        }

        // Mark as transmitting
        let _ =
            self.common
                .info
                .controls
                .set(&ControlId::Power, Power::Transmit as u32 as f64, None);

        let range_index = data[IMG_RANGE_INDEX];
        let range_meters = range_index_to_meters(range_index) as u32;
        let num_spokes = data[IMG_NUM_SPOKES] as u16;
        let samples_per_spoke =
            (data[IMG_SAMPLES_PER_SPOKE_HI] as u16) << 8 | data[IMG_SAMPLES_PER_SPOKE_LO] as u16;
        let total_spokes =
            (data[IMG_TOTAL_SPOKES_HI] as u16) << 8 | data[IMG_TOTAL_SPOKES_LO] as u16;
        let start_angle = (data[IMG_START_ANGLE_HI] as u16) << 8 | data[IMG_START_ANGLE_LO] as u16;

        let actual_pixels = actual_spoke_pixels(samples_per_spoke);
        if actual_pixels == 0 || num_spokes == 0 || total_spokes == 0 {
            return;
        }

        let spoke_data_offset = if transfer_type == TRANSFER_ROTATED {
            IMG_SPOKE_DATA_ROTATED
        } else {
            IMG_SPOKE_DATA
        };

        // Update range
        let _ = self
            .common
            .info
            .controls
            .set(&ControlId::Range, range_meters as f64, None);

        // Get heading from navdata if available
        let heading: Option<u16> = {
            crate::navdata::get_heading_true().map(|h| {
                let normalized = h.rem_euclid(std::f64::consts::TAU);
                ((normalized * SPOKES as f64 / std::f64::consts::TAU).floor() as u16)
                    .min(SPOKES as u16 - 1)
            })
        };

        // Process each spoke in this frame
        let spoke_byte_len = actual_pixels as usize;

        self.common.new_spoke_message();

        for spoke_idx in 0..num_spokes {
            let angle_raw = start_angle.wrapping_add(spoke_idx) % total_spokes;

            // Map to our internal spoke count
            let angle: SpokeBearing =
                (angle_raw as u32 * SPOKES as u32 / total_spokes as u32) as u16;

            let spoke_offset = spoke_data_offset + (spoke_idx as usize) * spoke_byte_len;
            let spoke_end = spoke_offset + spoke_byte_len;
            if spoke_end > data.len() {
                break;
            }

            let spoke_data = &data[spoke_offset..spoke_end];

            let mut scaled = Vec::with_capacity(spoke_byte_len);
            for pixel in spoke_data.iter() {
                // Scale raw 0-255 to 0-127, make room for history colors
                scaled.push(*pixel >> 1);
            }

            self.common.add_spoke(range_meters, angle, heading, scaled);
        }

        self.common.send_spoke_message();
    }
}
