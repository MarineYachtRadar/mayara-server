use anyhow::{Context, Error, bail};
use num_traits::FromPrimitive;
use std::f64::consts::TAU;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::io::ReadHalf;
use tokio::net::UdpSocket;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::Command;
use super::protocol::{
    CommandId, DATA_BROADCAST_ADDRESS, ECHO_GAIN_DEFAULT, ECHO_GAIN_LOW_POWER,
    ENCODING_1_REPEAT_DEFAULT, ENCODING_3_REPEAT_DEFAULT, FRAME_DUAL_RANGE_BIT,
    FRAME_ENCODING_MASK, FRAME_ENCODING_SHIFT, FRAME_HEADING_VALID_BIT, FRAME_MAGIC,
    FRAME_SCALE_HIGH_MASK, FRAME_SPOKE_DATA_LEN_HIGH_BIT, FRAME_SWEEP_LEN_HIGH_MASK,
    FRAME_WIRE_INDEX_MASK, RadarModel, SPOKE_ALIGNMENT_MASK, SPOKE_ANGLE_HIGH_MASK, SPOKE_LEN,
    SPOKES, WIRE_UNIT_KM, WIRE_UNIT_NM, wire_index_to_meters_for_unit,
};
use super::settings;
use crate::Cli;
use crate::network::{create_udp_listen, create_udp_multicast_listen};
use crate::radar::CommonRadar;
use crate::radar::SharedRadars;
use crate::radar::SpokeBearing;
use crate::radar::settings::ControlId;
use crate::radar::{Power, RadarError, RadarInfo};
use crate::util::PrintableSpoke;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReceiveAddressType {
    Both,
    Multicast,
    Broadcast,
}

#[derive(Debug)]
struct FurunoSpokeMetadata {
    sweep_count: u32,
    sweep_len: u32,
    encoding: u8,
    have_heading: u8,
    range: u32,
    radar_no: u8, // 0 = Range A, 1 = Range B (dual range)
    /// Number of samples that cover the configured display range.
    /// Extracted from header bytes 14-15 as `((byte[15] & 0x07) << 8) | byte[14]`.
    /// The radar always transmits `sweep_len` total samples per spoke, but only
    /// the first `scale` of them map to 0..range_meters. Samples beyond `scale`
    /// are oversampled data outside the display range.
    scale: u32,
}

pub struct FurunoReportReceiver {
    common: CommonRadar,
    /// Second CommonRadar for Range B in dual range mode.
    common_b: Option<CommonRadar>,
    stream: Option<TcpStream>,
    command_sender: Option<Command>,
    report_request_interval: Duration,
    model_known: bool,
    model: RadarModel,

    receive_type: ReceiveAddressType,
    multicast_socket: Option<UdpSocket>,
    broadcast_socket: Option<UdpSocket>,

    // Delta-decoding state kept per range (index 0 = A, 1 = B) because
    // dual-range interleaves two independent spoke streams on the same UDP
    // socket. Sharing a single prev_spoke across A/B would corrupt the first
    // delta-decoded spoke after every switch.
    prev_spoke: [Vec<u8>; 2],
    prev_angle: [u16; 2],
    guard_zone_alarm: [bool; 2],
    alarm_active: bool,
}

impl FurunoReportReceiver {
    pub fn new(args: &Cli, radars: SharedRadars, info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();
        let command_sender = if args.replay {
            None
        } else {
            Some(Command::new(&info, false))
        };

        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();

        let common = CommonRadar::new(
            args,
            key,
            info.clone(),
            radars.clone(),
            control_update_rx,
            args.replay,
            blob_tx,
        );

        FurunoReportReceiver {
            common,
            common_b: None,
            stream: None,
            command_sender,
            report_request_interval: Duration::from_millis(5000),
            model_known: false,
            model: RadarModel::Unknown,
            receive_type: ReceiveAddressType::Both,
            multicast_socket: None,
            broadcast_socket: None,
            prev_spoke: [Vec::new(), Vec::new()],
            prev_angle: [0, 0],
            guard_zone_alarm: [false, false],
            alarm_active: false,
        }
    }

    /// Set the Range B RadarInfo for dual range mode.
    pub fn set_range_b(&mut self, args: &Cli, radars: &SharedRadars, info_b: RadarInfo) {
        let key_b = info_b.key();
        let control_update_rx_b = info_b.control_update_subscribe();
        let blob_tx_b = radars.get_blob_tx();

        self.common_b = Some(CommonRadar::new(
            args,
            key_b,
            info_b,
            radars.clone(),
            control_update_rx_b,
            args.replay,
            blob_tx_b,
        ));

        if let Some(ref mut cs) = self.command_sender {
            cs.has_dual_range = true;
        }
    }

    async fn start_command_stream(&mut self) -> Result<(), RadarError> {
        if self.command_sender.is_none() {
            // Cannot connect to TCP in replay mode, can't send commands
            return Ok(());
        }
        if self.common.info.send_command_addr.port() == 0 {
            // Port not set yet, we need to login to the radar first.
            return Err(RadarError::InvalidPort);
        }
        let sock = TcpSocket::new_v4().map_err(|e| RadarError::Io(e))?;
        self.stream = Some(
            sock.connect(std::net::SocketAddr::V4(self.common.info.send_command_addr))
                .await
                .map_err(|e| RadarError::Io(e))?,
        );
        Ok(())
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.common.info.command_tx.
    //
    async fn data_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.common.key);

        let stream = self.stream.take();
        let mut reader = {
            if let Some(stream) = stream {
                let (reader, writer) = tokio::io::split(stream);
                if let Some(ref mut cs) = self.command_sender {
                    cs.set_writer(writer);
                }
                Some(BufReader::new(reader))
            } else {
                None
            }
        };
        // self.common.command_sender.init(&mut writer).await?;

        let mut line = String::new();
        let mut deadline = Instant::now() + self.report_request_interval;
        let mut first_report_received = false;

        let mut buf = Vec::with_capacity(9000);
        let mut buf2 = Vec::with_capacity(9000);

        let mut multicast_socket = self.multicast_socket.take();
        let mut broadcast_socket = self.broadcast_socket.take();

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(deadline) => {
                    if let Some(cs) = &mut self.command_sender {
                        cs.send_report_requests().await?;
                    }
                    deadline = Instant::now() + self.report_request_interval;
                },

                Some(r) = conditional_read(&mut reader, &mut line) => {
                    match r {
                        Ok(len) => {
                            if len > 2 {
                                if let Err(e) = self.process_report(&line) {
                                    log::error!("{}: {}", self.common.key, e);
                                } else if !first_report_received {
                                    if let Some(ref mut cs) = self.command_sender {
                                        cs.init().await?;
                                    }
                                    first_report_received = true;
                                }
                            }
                            line.clear();
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
                            // Range A control update: set dual_range_id=0
                            if let Some(ref mut cs) = self.command_sender {
                                cs.dual_range_id = 0;
                            }
                            if let Err(e) = self.common.process_control_update( cv, &mut self.command_sender).await {
                                return Err(e);
                            }
                        },
                    }
                },

                Some(r) = async {
                    if let Some(ref mut cb) = self.common_b {
                        Some(cb.control_update_rx.recv().await)
                    } else {
                        std::future::pending::<Option<_>>().await
                    }
                } => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            // Range B control update: set dual_range_id=1
                            if let Some(ref mut cs) = self.command_sender {
                                cs.dual_range_id = 1;
                            }
                            if let Some(ref mut cb) = self.common_b {
                                if let Err(e) = cb.process_control_update(cv, &mut self.command_sender).await {
                                    return Err(e);
                                }
                            }
                        },
                    }
                },

                Some(r) = conditional_receive(&multicast_socket, &mut buf)  => {
                    log::trace!("Furuno data multicast recv {:?}", r);
                    match r {
                        Ok((len, addr)) => {
                            if self.verify_source_address(&addr) {
                                self.process_frame(&buf[..len]);
                                self.receive_type = ReceiveAddressType::Multicast;
                                broadcast_socket = None;
                            }
                        },
                        Err(e) => {
                            log::error!("Furuno data socket: {}", e);
                            return Err(RadarError::Io(e));
                        }
                    };
                    buf.clear();
                },

                Some(r) = conditional_receive(&broadcast_socket, &mut buf2)  => {
                    log::trace!("Furuno data broadcast recv {:?}", r);
                    match r {
                        Ok((len, addr)) => {
                            if self.verify_source_address(&addr) {
                                self.process_frame(&buf2[..len]);
                                self.receive_type = ReceiveAddressType::Broadcast;
                                multicast_socket = None;
                            }
                        },
                        Err(e) => {
                            log::error!("Furuno data socket: {}", e);
                            return Err(RadarError::Io(e));
                        }
                    };
                    buf2.clear();
                },

            }
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        loop {
            // Each time we start the loop, there is no stream
            // and none of the data sockets are open.
            self.stream = None;
            self.multicast_socket = None;
            self.broadcast_socket = None;

            if let Err(e) = self.start_data_socket().await {
                log::warn!("{}: Failed to start data sockets: {}", self.common.key, e);
            } else if let Err(e) = self.login_to_radar() {
                log::warn!("{}: Failed to login to radar: {}", self.common.key, e);
            } else if let Err(e) = self.start_command_stream().await {
                log::warn!("{}: Failed to start command stream: {}", self.common.key, e);
            } else {
                match self.data_loop(&subsys).await {
                    Err(RadarError::Shutdown) => return Ok(()),
                    _ => {}
                }
            }

            tokio::select! {
                _ = subsys.on_shutdown_requested() => return Ok(()),
                _ = sleep(Duration::from_millis(1000)) => {}
            }
        }
    }

    fn login_to_radar(&mut self) -> Result<(), RadarError> {
        if self.command_sender.is_none() {
            return Ok(());
        }

        // Furuno radars use a single TCP/IP connection to send commands and
        // receive status reports, so report_addr and send_command_addr are identical.
        // Only one of these would be enough for Furuno.
        let port: u16 = match super::login_to_radar(self.common.info.addr) {
            Err(e) => {
                log::error!(
                    "{}: Unable to connect for login: {}",
                    self.common.info.key(),
                    e
                );
                return Err(RadarError::LoginFailed);
            }
            Ok(p) => p,
        };
        if port != self.common.info.send_command_addr.port() {
            self.common.info.send_command_addr.set_port(port);
            self.common.info.report_addr.set_port(port);
        }
        Ok(())
    }

    /// Return a mutable reference to the CommonRadar for the given dual range ID.
    /// `drid` 0 = Range A (self.common), `drid` 1 = Range B (self.common_b).
    /// Falls back to Range A if Range B is not configured.
    fn common_for_range(&mut self, drid: u8) -> &mut CommonRadar {
        if drid == 1 {
            if let Some(ref mut cb) = self.common_b {
                return cb;
            }
        }
        &mut self.common
    }

    /// Extract the dual range ID from a per-range response.
    /// Verified against live Wireshark captures from DRS4D-NXT with TimeZero.
    /// The drid position varies per command:
    ///   Status: $N69,{status},{drid},{wman},{w_send},{w_stop},0 — index 1
    ///   Gain:   $N63,{auto},{val},{drid},{auto_val},0            — index 2
    ///   Sea:    $N64,{auto},{val},{auto_val},{drid},0,0          — index 3
    ///   Rain:   $N65,{auto},{val},0,{drid},0,0                  — index 3
    ///   Range:  $N62,{wire_idx},{unit},{drid}                    — index 2
    ///   Tune:   $N75,{auto},{value},{drid}                       — index 2
    fn extract_drid(&self, command_id: &CommandId, numbers: &[f64]) -> u8 {
        if self.common_b.is_none() {
            return 0;
        }
        let drid = match command_id {
            CommandId::Status => {
                // $N69,{status},{drid},{wman},{w_send},{w_stop},0
                numbers.get(1).copied().unwrap_or(0.0)
            }
            CommandId::Gain | CommandId::Range | CommandId::Tune => {
                // $N63,{auto},{val},{drid},{auto_val},0
                // $N62,{wire_idx},{unit},{drid}
                // $N75,{auto},{value},{drid}
                numbers.get(2).copied().unwrap_or(0.0)
            }
            CommandId::Sea | CommandId::Rain => {
                // $N64,{auto},{val},{auto_val},{drid},0,0
                // $N65,{auto},{val},0,{drid},0,0
                numbers.get(3).copied().unwrap_or(0.0)
            }
            _ => {
                // Unknown commands: assume last field
                numbers.last().copied().unwrap_or(0.0)
            }
        };
        drid as u8
    }

    fn process_report(&mut self, line: &str) -> Result<(), Error> {
        let line = match line.find('$') {
            Some(pos) => {
                if pos > 0 {
                    log::warn!(
                        "{}: Ignoring first {} bytes of TCP report",
                        self.common.key,
                        pos
                    );
                    &line[pos..]
                } else {
                    line
                }
            }
            None => {
                log::warn!("{}: TCP report dropped, no $", self.common.key);
                return Ok(());
            }
        };

        if line.len() < 2 {
            bail!("TCP report {:?} dropped", line);
        }
        let (prefix, mut line) = line.split_at(2);
        if prefix != "$N" {
            bail!("TCP report {:?} dropped", line);
        }
        line = line.trim_end_matches("\r\n");

        log::trace!("{}: processing $N{}", self.common.key, line);

        let mut values_iter = line.split(',');

        let cmd_str = values_iter
            .next()
            .ok_or(io::Error::new(io::ErrorKind::Other, "No command ID"))?;
        let cmd = u8::from_str_radix(cmd_str, 16)?;

        let command_id = match CommandId::from_u8(cmd) {
            Some(c) => c,
            None => {
                log::debug!(
                    "{}: ignoring unimplemented command {}",
                    self.common.key,
                    cmd_str
                );
                return Ok(());
            }
        };

        // Match commands that do not have just numbers as arguments first

        let strings: Vec<&str> = values_iter.collect();
        log::debug!(
            "{}: command {:02X} strings {:?}",
            self.common.key,
            cmd,
            strings
        );
        let numbers: Vec<f64> = strings
            .iter()
            .map(|s| s.trim().parse::<f64>().unwrap_or(0.0))
            .collect();

        if numbers.len() != strings.len() {
            log::trace!("Parsed strings: $N{:02X},{:?}", cmd, strings);
        } else {
            log::trace!("Parsed numbers: $N{:02X},{:?}", cmd, numbers);
        }

        match command_id {
            CommandId::Modules => {
                self.parse_modules(&strings);
                return Ok(());
            }

            CommandId::Status => {
                // Response format: $N69,{status},{drid},{wman},{w_send},{w_stop},0
                if numbers.len() < 1 {
                    bail!("No arguments for Status command");
                }
                let generic_state = match numbers[0] {
                    0. => Power::Preparing,
                    1. => Power::Standby,
                    2. => Power::Transmit,
                    3. => Power::Off,
                    _ => Power::Off,
                };

                let power_value = generic_state as i32 as f64;
                let drid = self.extract_drid(&command_id, &numbers);
                let target = self.common_for_range(drid);
                target.set_value(&ControlId::Power, power_value);

                if numbers.len() >= 5 {
                    let wman = numbers[2] as i32;
                    let w_send = numbers[3];
                    target.set_value(&ControlId::TimedIdle, wman as f64);
                    target.set_value(&ControlId::TimedRun, w_send);
                }

                // Coupled transmit: on DRS models both ranges share TX state.
                // Propagate power to the other range.
                if self.common_b.is_some() {
                    let other = if drid == 1 {
                        &mut self.common
                    } else {
                        self.common_b.as_mut().unwrap()
                    };
                    other.set_value(&ControlId::Power, power_value);
                }
            }
            CommandId::Gain => {
                // Response format: $N63,{auto},{val},{screen},{auto_val},{drid}
                if numbers.len() < 2 {
                    bail!(
                        "Insufficient ({}) arguments for Gain command",
                        numbers.len()
                    );
                }
                let auto = numbers[0] as u8;
                let gain = numbers[1];
                let drid = self.extract_drid(&command_id, &numbers);
                self.common_for_range(drid)
                    .set_value_auto(&ControlId::Gain, gain, auto);
            }
            CommandId::Sea => {
                // Response format: $N64,{auto},{val},{auto_val},{screen},0,{drid}
                if numbers.len() < 2 {
                    bail!("Insufficient ({}) arguments for Sea command", numbers.len());
                }
                let auto = numbers[0] as u8;
                let sea = numbers[1];
                let drid = self.extract_drid(&command_id, &numbers);
                self.common_for_range(drid)
                    .set_value_auto(&ControlId::Sea, sea, auto);
            }
            CommandId::Rain => {
                // Response format: $N65,{auto},{val},0,{screen},{drid},0
                if numbers.len() < 2 {
                    bail!(
                        "Insufficient ({}) arguments for Rain command",
                        numbers.len()
                    );
                }
                let auto = numbers[0] as u8;
                let rain = numbers[1];
                let drid = self.extract_drid(&command_id, &numbers);
                self.common_for_range(drid)
                    .set_value_auto(&ControlId::Rain, rain, auto);
            }
            CommandId::ScanSpeed => {
                // Response format: $N89,{mode},0
                // mode: 0=24RPM, 2=Auto
                if numbers.len() < 1 {
                    bail!(
                        "Insufficient ({}) arguments for ScanSpeed command",
                        numbers.len()
                    );
                }
                let mode = numbers[0];
                self.common.set_value(&ControlId::ScanSpeed, mode);
            }
            CommandId::BlindSector => {
                // Response format: $N77,{s2_enable},{s1_start},{s1_width},{s2_start},{s2_width}
                if numbers.len() < 5 {
                    bail!(
                        "Insufficient ({}) arguments for BlindSector command",
                        numbers.len()
                    );
                }
                let s2_enable = numbers[0] != 0.0;
                let s1_start = numbers[1];
                let s1_width = numbers[2];
                let s2_start = numbers[3];
                let s2_width = numbers[4];

                // Convert from start/width to start/end
                let s1_end = (s1_start + s1_width) % 360.0;
                let s2_end = (s2_start + s2_width) % 360.0;

                // Zone 1 is enabled if width is non-zero
                let s1_enable = s1_width != 0.0;

                self.common.set_sector(
                    &ControlId::NoTransmitSector1,
                    s1_start,
                    s1_end,
                    Some(s1_enable),
                );
                self.common.set_sector(
                    &ControlId::NoTransmitSector2,
                    s2_start,
                    s2_end,
                    Some(s2_enable),
                );
            }
            CommandId::Range => {
                // Response format: $N62,{wire_idx},{unit},{drid}
                // Confirmed from capture: $N62,10,0,1 = wire_idx=10, unit=0(NM), drid=1(B)
                if numbers.len() < 3 {
                    bail!(
                        "Insufficient ({}) arguments for Range command",
                        numbers.len()
                    );
                }
                let wire_index = numbers[0] as i32;
                let wire_unit = numbers[1] as i32;
                let range_meters =
                    wire_index_to_meters_for_unit(wire_index, wire_unit)
                        .with_context(|| {
                            format!(
                                "Unknown wire index {} (unit {}) from radar range response",
                                wire_index, wire_unit
                            )
                        })?;

                let drid = self.extract_drid(&command_id, &numbers);
                let range_units_value = match wire_unit {
                    WIRE_UNIT_KM => 1.0, // Metric
                    _ => 0.0,                            // Nautical
                };
                let target = self.common_for_range(drid);
                target.set_value(&ControlId::Range, range_meters as f64);
                target.set_value(&ControlId::RangeUnits, range_units_value);
            }
            CommandId::OnTime => {
                let seconds = numbers[0];
                self.common.set_value(&ControlId::OperatingTime, seconds);
            }
            CommandId::TxTime => {
                let seconds = numbers[0];
                self.common.set_value(&ControlId::TransmitTime, seconds);
            }
            CommandId::MainBangSize => {
                // Response format: $N83,{value},0
                // value: 0-255 (raw value, needs conversion to 0-100%)
                if numbers.len() < 1 {
                    bail!(
                        "Insufficient ({}) arguments for MainBangSize command",
                        numbers.len()
                    );
                }
                // Convert 0-255 to 0-100%
                let percent = (numbers[0] as i32 * 100) / 255;
                self.common
                    .set_value(&ControlId::MainBangSuppression, percent as f64);
            }

            // NXT-specific features
            CommandId::SignalProcessing => {
                // Response format (varies):
                // - From SET echo: $N67,0,{feature},{value},{screen} (4 args)
                // - From REQUEST: $N67,{feature},{value},{screen} (3 args)
                // feature 0: Interference Rejection (0=OFF, 2=ON)
                // feature 3: Noise Reduction (0=OFF, 1=ON)
                let (feature, value) = if numbers.len() >= 4 && numbers[0] == 0.0 {
                    // SET echo format
                    (numbers[1] as i32, numbers[2] as i32)
                } else if numbers.len() >= 2 {
                    // REQUEST response format
                    (numbers[0] as i32, numbers[1] as i32)
                } else {
                    bail!(
                        "Insufficient ({}) arguments for SignalProcessing command",
                        numbers.len()
                    );
                };

                match feature {
                    0 => {
                        // Interference Rejection: value 2=ON, 0=OFF
                        let enabled = if value == 2 { 1.0 } else { 0.0 };
                        self.common
                            .set_value(&ControlId::InterferenceRejection, enabled);
                    }
                    3 => {
                        // Noise Reduction: value 1=ON, 0=OFF
                        let enabled = if value == 1 { 1.0 } else { 0.0 };
                        self.common.set_value(&ControlId::NoiseRejection, enabled);
                    }
                    _ => {
                        log::debug!(
                            "Unknown SignalProcessing feature {}: value {}",
                            feature,
                            value
                        );
                    }
                }
            }
            CommandId::RezBoost => {
                // Response format: $NEE,{level},{screen}
                // level: 0=OFF, 1=Low, 2=Medium, 3=High
                if numbers.len() < 1 {
                    bail!(
                        "Insufficient ({}) arguments for RezBoost command",
                        numbers.len()
                    );
                }
                self.common
                    .set_value(&ControlId::TargetSeparation, numbers[0]);
            }
            CommandId::BirdMode => {
                // Response format: $NED,{level},{screen}
                // level: 0=OFF, 1=Low, 2=Medium, 3=High
                if numbers.len() < 1 {
                    bail!(
                        "Insufficient ({}) arguments for BirdMode command",
                        numbers.len()
                    );
                }
                self.common.set_value(&ControlId::BirdMode, numbers[0]);
            }
            CommandId::JammingAble => {
                if numbers.is_empty() {
                    bail!(
                        "Insufficient ({}) arguments for JammingAble command",
                        numbers.len()
                    );
                }
                self.common.set_value(&ControlId::AntiJamming, numbers[0]);
            }
            CommandId::TargetAnalyzer => {
                // Response format: $NEF,{enabled},{mode},{screen}
                // Wire format: enabled=0/1, mode=0(Target)/1(Rain)
                // Control value: 0=Off, 1=Target, 2=Rain
                if numbers.len() < 2 {
                    bail!(
                        "Insufficient ({}) arguments for TargetAnalyzer command",
                        numbers.len()
                    );
                }
                let enabled = numbers[0] as i32;
                let mode = numbers[1] as i32;

                let value = if enabled == 0 {
                    0.0 // Off
                } else if mode == 0 {
                    1.0 // Target
                } else {
                    2.0 // Rain
                };

                self.common.set_value(&ControlId::Doppler, value);
            }

            CommandId::Tune => {
                // Response format: $N75,{auto},{value},{screen}
                // screen is drid for dual range
                if numbers.len() >= 2 {
                    let auto = numbers[0] as u8;
                    let tune = numbers[1];
                    let drid = self.extract_drid(&command_id, &numbers);
                    self.common_for_range(drid)
                        .set_value_auto(&ControlId::Tune, tune, auto);
                }
            }

            CommandId::PulseWidth => {
                // $N68,<pulse>,<range>,<unit>,<imgNo>,<screen>
                if let Some(&pulse) = numbers.first() {
                    let name = match pulse as i32 {
                        0 => "S1",
                        1 => "S2",
                        2 => "M1",
                        3 => "M2",
                        4 => "M3",
                        5 => "L",
                        _ => "Unknown",
                    };
                    let drid = self.extract_drid(&command_id, &numbers);
                    let _ = self
                        .common_for_range(drid)
                        .info
                        .controls
                        .set_string(&ControlId::PulseWidth, name.to_string());
                }
            }

            CommandId::Alarm => {
                // $N7D,<type>,<d1>,<d2>,<d3> — generic radar alarm; idle = all zero.
                // Bit meanings not yet decoded; log raw values on state change.
                if numbers.len() >= 4 {
                    let alarm_type = numbers[0] as u32;
                    if alarm_type != 0 && !self.alarm_active {
                        log::warn!(
                            "{}: radar alarm type {} details {} {} {}",
                            self.common.key,
                            alarm_type,
                            numbers[1] as u32,
                            numbers[2] as u32,
                            numbers[3] as u32,
                        );
                        self.alarm_active = true;
                    } else if self.alarm_active {
                        log::info!("{}: radar alarm cleared", self.common.key);
                        self.alarm_active = false;
                    }
                }
            }

            CommandId::ArpaAlarm => {
                // $NAF,<bitmask> — ARPA subsystem status bits. Bit meanings not yet decoded.
                if let Some(&bits) = numbers.first() {
                    log::debug!(
                        "{}: ARPA alarm status 0x{:04x}",
                        self.common.key,
                        bits as u32
                    );
                }
            }

            CommandId::GuardStatus => {
                // $N70,<count>,<status0>,<status1> — log on state change only
                if numbers.len() >= 3 {
                    let alarms = [numbers[1] as i32 != 0, numbers[2] as i32 != 0];
                    for (i, &active) in alarms.iter().enumerate() {
                        if active != self.guard_zone_alarm[i] {
                            self.guard_zone_alarm[i] = active;
                            if active {
                                log::warn!(
                                    "{}: Guard zone {} alarm ACTIVE",
                                    self.common.key,
                                    i + 1
                                );
                            } else {
                                log::info!(
                                    "{}: Guard zone {} alarm cleared",
                                    self.common.key,
                                    i + 1
                                );
                            }
                        }
                    }
                }
            }

            CommandId::NearSTC | CommandId::MiddleSTC | CommandId::FarSTC | CommandId::STCRange => {
                if let Some(&value) = numbers.first() {
                    // Single-field responses ($N85,2) have no drid — default to range A.
                    // Multi-field responses include drid as the last field.
                    let drid = if numbers.len() > 1 {
                        self.extract_drid(&command_id, &numbers)
                    } else {
                        0
                    };
                    let control_id = match command_id {
                        CommandId::NearSTC => ControlId::NearStcCurve,
                        CommandId::MiddleSTC => ControlId::MiddleStcCurve,
                        CommandId::FarSTC => ControlId::FarStcCurve,
                        _ => ControlId::StcRange,
                    };
                    self.common_for_range(drid).set_value(&control_id, value);
                }
            }

            // Silently handled (no state to update)
            CommandId::AliveCheck
            | CommandId::NN3Command
            | CommandId::CustomPictureAll
            | CommandId::AntennaType
            | CommandId::DispMode
            | CommandId::RingSuppression
            | CommandId::TrailMode
            | CommandId::TrailProcess
            | CommandId::CustomATFSettings
            | CommandId::ATFSettings
            | CommandId::AutoAcquire
            | CommandId::TuneIndicator
            | CommandId::GuardMode
            | CommandId::GuardFan => {}

            _ => {
                log::debug!(
                    "{}: unhandled command {:?} values {:?}",
                    self.common.key,
                    command_id,
                    numbers
                );
            }
        }
        Ok(())
    }

    /// Parse the connect reply from the radar.
    /// The DRS 4D-NXT radar sends a connect reply with the following format:
    /// $N96,0359360-01.05,0359358-01.01,0359359-01.01,0359361-01.05,,,
    /// The 4th, 5th and 6th values are for the FPGA and other parts, we don't store
    /// that (yet).
    fn parse_modules(&mut self, values: &Vec<&str>) {
        if self.model_known {
            return;
        }
        self.model_known = true; // We set this even if we can't parse the model, there is no point in logging errors many times.

        if let Some((model, version)) = values[0].split_once('-') {
            let model = RadarModel::from_part_number(model);
            log::info!(
                "{}: Radar model {} version {}",
                self.common.key,
                model,
                version
            );
            self.model = model;
            settings::update_when_model_known(&mut self.common.info, model, version);
            if let Some(cs) = &mut self.command_sender {
                cs.set_ranges(self.common.info.ranges.clone());
            }
            self.common.update();

            // Also update Range B if present
            if let Some(ref mut cb) = self.common_b {
                settings::update_when_model_known(&mut cb.info, model, version);
                cb.update();
            }
            return;
        }
        log::error!(
            "{}: Model {} is unknown radar type: modules {:?}",
            self.common.key,
            self.common
                .info
                .controls
                .model_name()
                .unwrap_or_else(|| { "unknown".to_string() }),
            values
        );
    }

    async fn start_multicast_socket(&mut self) -> io::Result<()> {
        match create_udp_multicast_listen(
            &self.common.info.spoke_data_addr,
            &self.common.info.nic_addr,
        ) {
            Ok(sock) => {
                self.multicast_socket = Some(sock);
                log::debug!(
                    "{} via {}: listening for spoke data",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "{} via {}: listen multicast failed: {}",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    async fn start_broadcast_socket(&mut self) -> io::Result<()> {
        match create_udp_listen(
            &DATA_BROADCAST_ADDRESS,
            &self.common.info.nic_addr,
            true,
        ) {
            Ok(sock) => {
                self.broadcast_socket = Some(sock);
                log::debug!(
                    "{} via {}: listening for spoke data",
                    &DATA_BROADCAST_ADDRESS,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "{} via {}: listen broadcast failed: {}",
                    &DATA_BROADCAST_ADDRESS,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    async fn start_data_socket(&mut self) -> io::Result<()> {
        let want_multicast = matches!(
            self.receive_type,
            ReceiveAddressType::Both | ReceiveAddressType::Multicast
        );
        let want_broadcast = matches!(
            self.receive_type,
            ReceiveAddressType::Both | ReceiveAddressType::Broadcast
        );

        let mut r = Ok(());

        if want_multicast {
            if let Err(e) = self.start_multicast_socket().await {
                r = Err(e);
            }
        }
        if want_broadcast {
            if let Err(e) = self.start_broadcast_socket().await {
                r = Err(e);
            }
        }

        if self.multicast_socket.is_some() || self.broadcast_socket.is_some() {
            r = Ok(());
        }

        r
    }

    #[cfg(target_os = "macos")]
    fn verify_source_address(&self, addr: &SocketAddr) -> bool {
        addr.ip() == std::net::SocketAddr::V4(self.common.info.addr).ip() || self.common.replay
    }
    #[cfg(not(target_os = "macos"))]
    fn verify_source_address(&self, addr: &SocketAddr) -> bool {
        addr.ip() == std::net::SocketAddr::V4(self.common.info.addr).ip()
    }

    fn process_frame(&mut self, data: &[u8]) {
        if data.len() < 16 || data[0] != FRAME_MAGIC {
            log::debug!("Dropping invalid frame");
            return;
        }

        let metadata: FurunoSpokeMetadata = self.parse_metadata_header(&data);

        let sweep_count = metadata.sweep_count;
        let sweep_len = metadata.sweep_len as usize;
        let is_range_b = metadata.radar_no == 1 && self.common_b.is_some();

        log::debug!(
            "Received UDP frame with {} spokes (range {})",
            sweep_count,
            if is_range_b { "B" } else { "A" }
        );

        if is_range_b {
            self.common_b.as_mut().unwrap().new_spoke_message();
        } else {
            self.common.new_spoke_message();
        }

        let mut sweep: &[u8] = &data[16..];
        for sweep_idx in 0..sweep_count {
            if sweep.len() < 5 {
                log::error!("Unsufficient data for sweep {}", sweep_idx);
                break;
            }
            let angle = (((sweep[1] & SPOKE_ANGLE_HIGH_MASK) as u16) << 8) | sweep[0] as u16;
            let heading = (((sweep[3] & SPOKE_ANGLE_HIGH_MASK) as u16) << 8) | sweep[2] as u16;
            sweep = &sweep[4..];

            let range_idx = if is_range_b { 1 } else { 0 };
            let (mut generic_spoke, used) = match metadata.encoding {
                0 => Self::decode_sweep_encoding_0(sweep),
                1 => Self::decode_sweep_encoding_1(sweep, sweep_len),
                2 => {
                    if sweep_idx == 0 {
                        Self::decode_sweep_encoding_1(sweep, sweep_len)
                    } else {
                        Self::decode_sweep_encoding_2(
                            sweep,
                            self.prev_spoke[range_idx].as_slice(),
                            sweep_len,
                        )
                    }
                }
                3 => Self::decode_sweep_encoding_3(
                    sweep,
                    self.prev_spoke[range_idx].as_slice(),
                    sweep_len,
                ),
                _ => {
                    panic!("Impossible encoding value")
                }
            };

            // Pad short spokes to sweep_len with zeros. This happens on radars
            // with compact compressed data (e.g., DRS4W) where the decompressor
            // runs out of input before producing sweep_len samples. The missing
            // samples represent zero-return (empty) pixels.
            if generic_spoke.len() < sweep_len {
                generic_spoke.resize(sweep_len, 0);
            }

            sweep = &sweep[used..];

            // The GUI buffers each angle in a slot of SPOKE_LEN samples
            // and treats that whole slot as covering the spoke's reported
            // physical range, so sample i is drawn at
            // `i / SPOKE_LEN * metadata.range`.
            //
            // The radar always transmits `sweep_len` total samples per spoke,
            // but only the first `metadata.scale` of them cover the configured
            // display range (0..range_meters). Samples beyond `scale` are
            // oversampled data outside the display range. Verified against
            // radar.dll disassembly and ARM MFD firmware (imoecho.c).
            //
            // Stretch the first `scale` samples to fill SPOKE_LEN so
            // that sample i in the output represents physical distance
            // `i / SPOKE_LEN * range_meters`.
            let send_spoke: Vec<u8> = Self::stretch_spoke(
                &generic_spoke,
                metadata.scale as usize,
                SPOKE_LEN,
            );

            // Low-power radars (DRS4W: 2.2 kW WiFi) produce raw echo values
            // well below the encoding maximum (~124 vs 252), so the 64-color
            // palette is only half-utilised and targets appear uniformly blue.
            // A software gain of 2× doubles the palette spread without
            // affecting full-power models (NXT, FAR) where values already
            // reach the encoding ceiling.
            let echo_gain: u8 = match self.model {
                RadarModel::DRS4W | RadarModel::DRS => ECHO_GAIN_LOW_POWER,
                _ => ECHO_GAIN_DEFAULT,
            };

            if is_range_b {
                Self::add_spoke_to_common(
                    self.common_b.as_mut().unwrap(),
                    &metadata,
                    angle,
                    heading,
                    &send_spoke,
                    echo_gain,
                );
            } else {
                Self::add_spoke_to_common(
                    &mut self.common,
                    &metadata,
                    angle,
                    heading,
                    &send_spoke,
                    echo_gain,
                );
            }

            self.prev_angle[range_idx] = angle;
            self.prev_spoke[range_idx] = generic_spoke;
        }

        if is_range_b {
            self.common_b.as_mut().unwrap().send_spoke_message();
        } else {
            self.common.send_spoke_message();
        }
    }

    fn decode_sweep_encoding_0(sweep: &[u8]) -> (Vec<u8>, usize) {
        let spoke = sweep.to_vec();

        let used = sweep.len();
        (spoke, used)
    }

    fn decode_sweep_encoding_1(sweep: &[u8], sweep_len: usize) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(SPOKE_LEN);
        let mut used = 0;
        let mut strength: u8 = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x01 == 0 {
                strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = (sweep[used] >> 1) as usize;
                if repeat == 0 {
                    repeat = ENCODING_1_REPEAT_DEFAULT;
                }

                for _ in 0..repeat {
                    spoke.push(strength);
                }
            }
            used += 1;
        }

        used = (used + 3) & SPOKE_ALIGNMENT_MASK;
        (spoke, used)
    }

    fn decode_sweep_encoding_2(
        sweep: &[u8],
        prev_spoke: &[u8],
        sweep_len: usize,
    ) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(SPOKE_LEN);
        let mut used = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x01 == 0 {
                let strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = (sweep[used] >> 1) as usize;
                if repeat == 0 {
                    repeat = ENCODING_1_REPEAT_DEFAULT;
                }

                for _ in 0..repeat {
                    let i = spoke.len();
                    let strength = if prev_spoke.len() > i {
                        prev_spoke[i]
                    } else {
                        0
                    };
                    spoke.push(strength);
                }
            }
            used += 1;
        }

        used = (used + 3) & SPOKE_ALIGNMENT_MASK;
        (spoke, used)
    }

    fn decode_sweep_encoding_3(
        sweep: &[u8],
        prev_spoke: &[u8],
        sweep_len: usize,
    ) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(SPOKE_LEN);
        let mut used = 0;
        let mut strength: u8 = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x03 == 0 {
                strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = (sweep[used] >> 2) as usize;
                if repeat == 0 {
                    repeat = ENCODING_3_REPEAT_DEFAULT;
                }

                if sweep[used] & 0x01 == 0 {
                    for _ in 0..repeat {
                        let i = spoke.len();
                        strength = if prev_spoke.len() > i {
                            prev_spoke[i]
                        } else {
                            0
                        };
                        spoke.push(strength);
                    }
                } else {
                    for _ in 0..repeat {
                        spoke.push(strength);
                    }
                }
            }
            used += 1;
        }

        used = (used + 3) & SPOKE_ALIGNMENT_MASK;
        (spoke, used)
    }

    /// Stretch a decoded spoke of `src_effective` meaningful samples (taken
    /// from the front of `src`) to `dst_len` samples using nearest-neighbour
    /// interpolation.
    ///
    /// On most Furuno models the native spoke length matches SPOKE_LEN
    /// and `src_effective == src.len()` — the stretch becomes a no-op copy.
    ///
    /// The DRS4W is special: every spoke carries 430 samples on the wire, but
    /// only the first N of those cover the configured display range.
    /// N varies per wire_index because the radar changes pulse
    /// width with range. Callers pass `src_effective = effective_samples(wi)`
    /// for DRS4W and `src_effective = src.len()` otherwise, so sample `i` of
    /// the output always represents physical distance
    /// `i / dst_len * metadata.range`.
    fn stretch_spoke(src: &[u8], src_effective: usize, dst_len: usize) -> Vec<u8> {
        if src.is_empty() || dst_len == 0 {
            return vec![0; dst_len];
        }
        let effective = src_effective.min(src.len()).max(1);
        if effective >= dst_len {
            return src[..dst_len].to_vec();
        }
        let mut out = vec![0u8; dst_len];
        for i in 0..dst_len {
            let j = (i * effective) / dst_len;
            out[i] = src[j];
        }
        out
    }

    fn add_spoke_to_common(
        common: &mut CommonRadar,
        metadata: &FurunoSpokeMetadata,
        angle: SpokeBearing,
        heading: SpokeBearing,
        sweep: &[u8],
        echo_gain: u8,
    ) {
        if common.replay {
            let _ = common
                .info
                .controls
                .set(&ControlId::Range, metadata.range as f64, None);
            let _ =
                common
                    .info
                    .controls
                    .set(&ControlId::Power, Power::Transmit as u32 as f64, None);
        }

        let heading: Option<u16> = if metadata.have_heading > 0 {
            Some(heading as u16)
        } else {
            let heading = crate::navdata::get_heading_true();
            heading.map(|h| (h * SPOKES as f64 / TAU) as u16)
        };

        let pixel_max = common.info.pixel_values.saturating_sub(1) as u16;
        let mut data = vec![0; sweep.len()];

        for (i, b) in sweep.iter().enumerate() {
            // Map raw echo byte to palette index. The raw value range depends
            // on the encoding (max 252 for encoding 3, 254 for encoding 1/2).
            // Low-power radars (DRS4W: 2.2 kW) produce values well below the
            // hardware maximum, so echo_gain > 1 applies software amplification
            // before the palette mapping. Clamped to pixel_max (63 for the
            // default 64-color palette).
            let amplified = (*b as u16 * echo_gain as u16) >> 2;
            data[i] = amplified.min(pixel_max) as u8;
        }
        if common.replay {
            data[sweep.len() - 1] = 64;
        }

        log::trace!(
            "Received {:04}/{:04} spoke {}",
            angle,
            heading.unwrap_or(9999),
            PrintableSpoke::new(&data)
        );

        common.add_spoke(metadata.range, angle, heading, data);
    }

    // From RadarDLLAccess RmGetEchoData() we know that the following should be in the header:
    // status, sweep_len, scale, range, angle, heading, hdg_flag.
    //
    // derived from ghidra fec/radar.dll function 'decode_sweep_2' @ 10002740
    // called from DecodeImoEchoFormat
    // Here's a typical header:
    //  [2,    #  0: 0x02 - Always 2, checked in radar.dll
    //   149,  #  1: 0x95
    //   0,
    //   1,
    //   0, 0, 0, 0,
    //   48,   #  8: 0x30 - low byte of range? (= range * 4 + 4)
    //   17,   #  9: 0x11 - bit 0 = high bit of range
    //   116,  # 10: 0x74 - low byte of sweep_len
    //   219,  # 11: 0xDB - bits 2..0 (011) = bits 10..8 of sweep_len
    //                    - bits 4..3 (11) = encoding 3
    //                    - bits 7..5 (110) = ?
    //   6,    # 12: 0x06
    //   0,    # 13: 0x00
    //   240,  # 14: 0xF0
    //   9]    # 15: 0x09
    //
    //  multi byte data: sweep_len = 0b011 << 8 | 0x74 => 0x374 = 884

    //  -> sweep_count=8 sweep_len=884 encoding=3 have_heading=0 range=496

    // Some more headers from FAR-2127:
    // [2, 250, 0, 1, 0, 0, 0, 0, 36, 49, 116, 59, 0, 0, 240, 9]

    fn parse_metadata_header(&self, data: &[u8]) -> FurunoSpokeMetadata {
        // Frame header layout (16 bytes), derived from radar.dll disassembly:
        //
        // Bytes 0-7: Packet header
        //   [0]    packet_type (always 0x02)
        //   [1]    sequence_number
        //   [2-3]  total_length (big-endian)
        //   [4-7]  timestamp (little-endian u32)
        //
        // Bytes 8-11: Sweep metadata
        //   [8]    spoke_data_len low byte
        //   [9]    bit 0: spoke_data_len high bit; bits 1-7: spoke_count
        //   [10]   sample_count low byte
        //   [11]   bits 0-2: sample_count high; bits 3-4: encoding;
        //          bit 5: heading_valid; bits 6-7: unknown (observed as 0b11)
        //
        // Bytes 12-15: Range and status
        //   [12]   bits 0-5: range wire index; bits 6-7: range_status
        //   [13]   range resolution metadata
        //   [14]   range_value low byte
        //   [15]   bits 0-2: range_value high; bit 3: flag;
        //          bits 4-5: echo_type; bit 6: dual_range_id (0=A, 1=B);
        //          bit 7: unknown

        let _spoke_data_len =
            (data[8] as u32 + (data[9] as u32 & FRAME_SPOKE_DATA_LEN_HIGH_BIT as u32) * 256) * 4
                + 4;
        let sweep_count = (data[9] >> 1) as u32;
        let sweep_len =
            ((data[11] & FRAME_SWEEP_LEN_HIGH_MASK) as u32) << 8 | data[10] as u32;
        let encoding = (data[11] & FRAME_ENCODING_MASK) >> FRAME_ENCODING_SHIFT;
        let have_heading = (data[11] & FRAME_HEADING_VALID_BIT) >> 5;
        let radar_no = (data[15] & FRAME_DUAL_RANGE_BIT) >> 6;
        let wire_index = (data[12] & FRAME_WIRE_INDEX_MASK) as i32;

        // The radar's active range unit (NM / km) determines which wire-index
        // table to use: the same wire index means different physical distances
        // in nautical vs metric mode. Read RangeUnits from the controls that
        // belong to the specific range this spoke is for — Range A and Range B
        // can be configured with different units.
        let range_controls = if radar_no == 1 {
            self.common_b
                .as_ref()
                .map(|cb| &cb.info.controls)
                .unwrap_or(&self.common.info.controls)
        } else {
            &self.common.info.controls
        };
        let wire_unit = range_controls
            .get(&crate::radar::settings::ControlId::RangeUnits)
            .and_then(|c| c.value)
            .map(|v| {
                if v as i32 == 1 {
                    WIRE_UNIT_KM
                } else {
                    WIRE_UNIT_NM
                }
            })
            .unwrap_or(WIRE_UNIT_NM);
        let range = wire_index_to_meters_for_unit(wire_index, wire_unit)
            .unwrap_or_else(|| {
                log::warn!(
                    "Unknown wire index {} (unit {}) in spoke header: {:?}",
                    wire_index,
                    wire_unit,
                    &data[0..16]
                );
                0
            });
        let range = range as u32;

        // scale = effective sample count for the configured display range.
        // Extracted from bytes 14-15: ((byte[15] & 0x07) << 8) | byte[14].
        // The radar always transmits sweep_len total samples, but only the
        // first `scale` map to 0..range_meters. Verified against radar.dll
        // disassembly (DecodeImoEchoFormat) and the ARM MFD firmware
        // (libNAVNETDLL.so, not-stripped symbols from imoecho.c).
        let scale = (((data[15] & FRAME_SCALE_HIGH_MASK) as u32) << 8) | data[14] as u32;
        // Fall back to sweep_len if scale is zero (malformed packet)
        let scale = if scale == 0 { sweep_len } else { scale };

        let metadata = FurunoSpokeMetadata {
            sweep_count,
            sweep_len,
            encoding,
            have_heading,
            range,
            radar_no,
            scale,
        };
        log::trace!(
            "header {:?} -> sweep_count={} sweep_len={} encoding={} have_heading={} range={} radar_no={} scale={}",
            &data[0..16],
            sweep_count,
            sweep_len,
            encoding,
            have_heading,
            range,
            radar_no,
            scale,
        );

        metadata
    }
}

async fn conditional_receive(
    socket: &Option<UdpSocket>,
    buf: &mut Vec<u8>,
) -> Option<io::Result<(usize, SocketAddr)>> {
    match socket {
        Some(s) => Some(s.recv_buf_from(buf).await),
        None => None,
    }
}

async fn conditional_read(
    reader: &mut Option<BufReader<ReadHalf<TcpStream>>>,
    line: &mut String,
) -> Option<io::Result<usize>> {
    match reader {
        Some(s) => Some(s.read_line(line).await),
        None => None,
    }
}
