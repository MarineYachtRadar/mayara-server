use anyhow::{Context, Error, bail};
use num_traits::FromPrimitive;
use std::f64::consts::TAU;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::io::ReadHalf;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::Command;
use super::protocol::{
    CommandId, DATA_BROADCAST_ADDRESS, ECHO_FLOOR, ENCODING_1_REPEAT_DEFAULT,
    ENCODING_3_REPEAT_DEFAULT, FRAME_DUAL_RANGE_BIT, FRAME_ENCODING_MASK, FRAME_ENCODING_SHIFT,
    FRAME_HEADING_VALID_BIT, FRAME_MAGIC, FRAME_SCALE_HIGH_MASK, FRAME_SPOKE_DATA_LEN_HIGH_BIT,
    FRAME_SWEEP_LEN_HIGH_MASK, FRAME_WIRE_INDEX_MASK, PIXEL_VALUES, RadarModel,
    SPOKE_ALIGNMENT_MASK, SPOKE_ANGLE_HIGH_MASK, SPOKE_LEN, SPOKES, TILE_MAGIC,
    TILE_REPEAT_DEFAULT, TILE_SCALE, WIRE_UNIT_KM, WIRE_UNIT_NM, wire_index_to_meters_for_unit,
};
use super::settings;
use crate::Cli;
use crate::network;
use crate::radar::CommonRadar;
use crate::radar::SharedRadars;
use crate::radar::SpokeBearing;
use crate::radar::settings::ControlId;
use crate::radar::{Power, RadarError, RadarInfo};
use crate::replay::RadarSocket;
use crate::util::PrintableSpoke;

/// TCP keepalive timing for the Furuno control socket. Tunes how quickly the
/// kernel detects that the radar has silently dropped the connection after
/// idle (DRS4D-NXT does this after ~2 min in standby). With these settings the
/// kernel tears the socket down within ~60 s of the peer going silent, so the
/// next user PUT sees either a fresh writer or a clean NotConnected — never a
/// stale half-closed socket that just hangs.
const CONTROL_KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const CONTROL_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const CONTROL_KEEPALIVE_RETRIES: u32 = 3;

/// Furuno wire-format decoding mode. When Target Analyzer is active on NXT
/// radars, each echo byte encodes `[dopplerClass:2 | intensity:4 | 00:2]`
/// rather than a plain intensity in 0..PIXEL_VALUES. The report receiver
/// rebuilds the `wire_to_legend` table when this mode changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DopplerWireMode {
    Off,
    On,
}

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

pub(crate) struct FurunoReportReceiver {
    common: CommonRadar,
    /// Second CommonRadar for Range B in dual range mode.
    common_b: Option<CommonRadar>,
    stream: Option<TcpStream>,
    command_sender: Option<Command>,
    report_request_interval: Duration,
    model_known: bool,
    model: RadarModel,

    receive_type: ReceiveAddressType,
    multicast_socket: Option<RadarSocket>,
    broadcast_socket: Option<RadarSocket>,

    // Delta-decoding state kept per range (index 0 = A, 1 = B) because
    // dual-range interleaves two independent spoke streams on the same UDP
    // socket. Sharing a single prev_spoke across A/B would corrupt the first
    // delta-decoded spoke after every switch.
    prev_spoke: [Vec<u8>; 2],
    prev_angle: [u16; 2],
    guard_zone_alarm: [bool; 2],
    alarm_active: bool,
    /// Precomputed raw-byte → legend-index lookup tables, one per range
    /// (index 0 = Range A, 1 = Range B). Range A and B can have independent
    /// Target Analyzer state in dual-range mode, so each needs its own LUT.
    /// DRS4W/DRS use an 18th-root gamma curve to spread the bottom-heavy
    /// echo distribution; other models use a linear mapping.
    wire_to_legend: [[u8; 256]; 2],
    /// Whether the Target Analyzer (Doppler) state for each range has been
    /// confirmed by either a `$NEF` report from the radar or by learning
    /// that the model has no TA capability. Until this is true, the LUT
    /// in `wire_to_legend` may not match the radar's actual wire format,
    /// so we drop spokes rather than render them with wrong colors.
    /// Index 0 = Range A, 1 = Range B.
    target_analyzer_known: [bool; 2],
}

impl FurunoReportReceiver {
    pub(crate) fn new(args: &Cli, radars: SharedRadars, info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();
        let command_sender = if args.is_replay() {
            None
        } else {
            Some(Command::new(&info, false))
        };

        // In replay mode the model is already set on RadarInfo by mod.rs.
        // In live mode, model_name may not be set yet (identified later via $N96).
        let model_name = info.controls.model_name();
        let model = model_name
            .as_deref()
            .map(RadarModel::from_model_name)
            .unwrap_or(RadarModel::Unknown);
        let low_power = model.is_low_power();
        let initial_lut = Self::wire_to_legend(&info.get_legend(), DopplerWireMode::Off, low_power);
        let wire_to_legend = [initial_lut, initial_lut];

        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();

        let common = CommonRadar::new(
            args,
            key,
            info.clone(),
            radars.clone(),
            control_update_rx,
            args.is_replay(),
            blob_tx,
        );

        // In replay mode the saved control state already drives the LUT, so
        // treat TA state as known from the start. In live mode we wait for
        // either a `$NEF` reply or model identification to confirm whether
        // the radar has TA at all.
        let initial_ta_known = args.is_replay();

        FurunoReportReceiver {
            common,
            common_b: None,
            stream: None,
            command_sender,
            report_request_interval: Duration::from_millis(5000),
            model_known: args.is_replay() && model != RadarModel::Unknown,
            model,
            receive_type: ReceiveAddressType::Both,
            multicast_socket: None,
            broadcast_socket: None,
            prev_spoke: [Vec::new(), Vec::new()],
            prev_angle: [0, 0],
            guard_zone_alarm: [false, false],
            alarm_active: false,
            wire_to_legend,
            target_analyzer_known: [initial_ta_known, initial_ta_known],
        }
    }

    /// Set the Range B RadarInfo for dual range mode.
    pub(crate) fn set_range_b(&mut self, args: &Cli, radars: &SharedRadars, info_b: RadarInfo) {
        let key_b = info_b.key();
        let control_update_rx_b = info_b.control_update_subscribe();
        let blob_tx_b = radars.get_blob_tx();

        self.common_b = Some(CommonRadar::new(
            args,
            key_b,
            info_b,
            radars.clone(),
            control_update_rx_b,
            args.is_replay(),
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
        let stream = sock
            .connect(std::net::SocketAddr::V4(self.common.info.send_command_addr))
            .await
            .map_err(|e| RadarError::Io(e))?;

        // Furuno radars silently drop the control TCP socket after an idle
        // period (observed on DRS4D-NXT after ~2 min). Without keepalive the
        // first PUT after idle hits the stale half-closed socket, write_all
        // fails with BrokenPipe, and the user sees a 400 "I/O operation
        // failed". The data_loop only notices on its next 5s report-tick
        // and reconnects, but by then the GUI has already shown the error
        // and the radar is still in standby. Timing constants are at the
        // top of the file.
        let keepalive = socket2::TcpKeepalive::new()
            .with_time(CONTROL_KEEPALIVE_IDLE)
            .with_interval(CONTROL_KEEPALIVE_INTERVAL)
            .with_retries(CONTROL_KEEPALIVE_RETRIES);
        socket2::SockRef::from(&stream)
            .set_tcp_keepalive(&keepalive)
            .map_err(RadarError::Io)?;

        self.stream = Some(stream);
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

                    // Recompute idle: standby + zero spoke subscribers. Exit
                    // points (control PUT, power-change report, new WS
                    // subscriber) clear is_idle directly so this only needs
                    // to handle the entry direction and the rare case where
                    // the subscriber count drops to zero between ticks.
                    // Each range has independent power/subscribers in dual-
                    // range mode so refresh both flags.
                    let was_idle_a = self.common.info.is_idle();
                    self.common.info.refresh_idle_flag();
                    // Encoding 3 reconstructs spokes from `prev_spoke[range_idx]`
                    // across frame boundaries. If we just entered idle, the
                    // next wake-up frame would decode against a stale buffer
                    // and the first revolution would render as garbage.
                    // Reset on the inactive→idle edge so the next exit starts
                    // delta-decoding against an empty (transparent) baseline.
                    if !was_idle_a && self.common.info.is_idle() {
                        self.prev_spoke[0].clear();
                    }
                    if let Some(ref common_b) = self.common_b {
                        let was_idle_b = common_b.info.is_idle();
                        common_b.info.refresh_idle_flag();
                        if !was_idle_b && common_b.info.is_idle() {
                            self.prev_spoke[1].clear();
                        }
                    }
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

                                // NXT models support Tile echo format via ImoEchoSwitch (0xB8).
                                // Not auto-switched: the DRS4D-NXT (fw v01.05) acknowledges
                                // but does not change format. The user can experiment via the
                                // EchoFormat control; process_frame() detects and decodes Tile.
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
                            if let Some(ref mut cb) = self.common_b
                                && let Err(e) = cb.process_control_update(cv, &mut self.command_sender).await {
                                    return Err(e);
                                }
                        },
                    }
                },

                Some(r) = conditional_receive(&mut multicast_socket, &mut buf)  => {
                    log::trace!("Furuno data multicast recv {:?}", r);
                    match r {
                        Ok((len, addr)) => {
                            if self.verify_source_address(&addr) {
                                // Idle mode: drain the recv but skip decoding.
                                // Furuno radars emit spokes even in Standby;
                                // when nobody is watching, decoding them costs
                                // ~1.5 cores for nothing. See issue #274. In
                                // dual-range mode each range has its own idle
                                // flag; only skip if BOTH are idle, because
                                // process_frame routes the frame to the right
                                // common based on metadata.radar_no.
                                if !self.both_ranges_idle() {
                                    self.process_frame(&buf[..len]);
                                }
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

                Some(r) = conditional_receive(&mut broadcast_socket, &mut buf2)  => {
                    log::trace!("Furuno data broadcast recv {:?}", r);
                    match r {
                        Ok((len, addr)) => {
                            if self.verify_source_address(&addr) {
                                if !self.both_ranges_idle() {
                                    self.process_frame(&buf2[..len]);
                                }
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

    pub(super) async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), RadarError> {
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
        if drid == 1
            && let Some(ref mut cb) = self.common_b
        {
            return cb;
        }
        &mut self.common
    }

    /// True only when every active range is idle. We must keep decoding
    /// frames as long as ANY range still has subscribers or is transmitting,
    /// because spokes for Range A and Range B share the same UDP socket and
    /// `process_frame` routes per-frame via `metadata.radar_no`.
    fn both_ranges_idle(&self) -> bool {
        if !self.common.info.is_idle() {
            return false;
        }
        match self.common_b {
            Some(ref cb) => cb.info.is_idle(),
            None => true,
        }
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
            CommandId::Gain | CommandId::Range | CommandId::Tune | CommandId::TargetAnalyzer => {
                // $N63,{auto},{val},{drid},{auto_val},0
                // $N62,{wire_idx},{unit},{drid}
                // $N75,{auto},{value},{drid}
                // $NEF,{enabled},{mode},{screen}
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

                let is_standby = matches!(generic_state, Power::Standby);
                let power_value = generic_state as i32 as f64;
                let drid = self.extract_drid(&command_id, &numbers);
                let target = self.common_for_range(drid);
                target.set_value(&ControlId::Power, power_value);

                // Exit idle on any transition away from Standby so the next
                // received spoke frame is decoded immediately. The 5s
                // periodic re-check would catch it too, but waiting a tick
                // means up to one revolution of "frames discarded after the
                // radar started transmitting", which the user would see as
                // a blank PPI for a beat.
                if !is_standby {
                    target.info.wake_up();
                }

                if numbers.len() >= 5 {
                    let wman = numbers[2] as i32;
                    let w_send = numbers[3];
                    // Wire format always includes wman/w_send fields, but
                    // models without the watchman capability (DRS, DRS4W)
                    // never registered the controls — skip the set_value to
                    // avoid spamming "not supported" errors on every Status
                    // report.
                    if target.info.controls.contains_key(&ControlId::TimedIdle) {
                        target.set_value(&ControlId::TimedIdle, wman as f64);
                    }
                    if target.info.controls.contains_key(&ControlId::TimedRun) {
                        target.set_value(&ControlId::TimedRun, w_send);
                    }
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
                    if !is_standby {
                        other.info.wake_up();
                    }
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

                // Wire format always includes blind-sector fields, but models
                // without sector_blanking (DRS, DRS4W) never registered the
                // controls — skip the set_sector to avoid spamming "not
                // supported" errors on every report.
                if self
                    .common
                    .info
                    .controls
                    .contains_key(&ControlId::NoTransmitSector1)
                {
                    self.common.set_sector(
                        &ControlId::NoTransmitSector1,
                        s1_start,
                        s1_end,
                        Some(s1_enable),
                    );
                }
                if self
                    .common
                    .info
                    .controls
                    .contains_key(&ControlId::NoTransmitSector2)
                {
                    self.common.set_sector(
                        &ControlId::NoTransmitSector2,
                        s2_start,
                        s2_end,
                        Some(s2_enable),
                    );
                }
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
                let range_meters = wire_index_to_meters_for_unit(wire_index, wire_unit)
                    .with_context(|| {
                        format!(
                            "Unknown wire index {} (unit {}) from radar range response",
                            wire_index, wire_unit
                        )
                    })?;

                let drid = self.extract_drid(&command_id, &numbers);
                let range_units_value = match wire_unit {
                    WIRE_UNIT_KM => 1.0, // Metric
                    _ => 0.0,            // Nautical
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

                let drid = self.extract_drid(&command_id, &numbers);
                let range_idx = if drid == 1 && self.common_b.is_some() {
                    1
                } else {
                    0
                };
                let old_mode = self.doppler_wire_mode_for(range_idx);
                self.common_for_range(drid)
                    .set_value(&ControlId::Doppler, value);
                let new_mode = self.doppler_wire_mode_for(range_idx);
                if old_mode != new_mode {
                    let low_power = self.model.is_low_power();
                    let legend = if range_idx == 1 {
                        self.common_b.as_ref().unwrap().info.get_legend()
                    } else {
                        self.common.info.get_legend()
                    };
                    self.wire_to_legend[range_idx] =
                        Self::wire_to_legend(&legend, new_mode, low_power);
                    let key = if range_idx == 1 {
                        &self.common_b.as_ref().unwrap().key
                    } else {
                        &self.common.key
                    };
                    log::debug!("{}: Doppler wire mode changed to {:?}", key, new_mode);
                }
                // First `$NEF` for this range confirms the TA state, so the
                // LUT is now trustworthy and spoke emission can resume.
                if !self.target_analyzer_known[range_idx] {
                    self.target_analyzer_known[range_idx] = true;
                    let key = if range_idx == 1 {
                        &self.common_b.as_ref().unwrap().key
                    } else {
                        &self.common.key
                    };
                    log::info!(
                        "{}: Target Analyzer state confirmed (mode {:?}), spoke output enabled",
                        key,
                        self.doppler_wire_mode_for(range_idx)
                    );
                }
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
            let low_power = model.is_low_power();
            // `update_when_model_known` reshapes the legend for the detected
            // model — adding rain / Doppler reservation slots on NXT shrinks
            // the intensity-ramp portion (`pixel_colors`). The LUT must be
            // built against the post-update legend; otherwise wire bytes
            // map past the ramp end into Doppler / history reservation
            // slots, painting strong stationary echoes blue.
            settings::update_when_model_known(&mut self.common.info, model, version);
            // Models without a Doppler control will never send `$NEF`, so the
            // initial TA-Off LUT is correct for them and we can lift the gate.
            if !self.common.info.controls.contains_key(&ControlId::Doppler) {
                self.target_analyzer_known[0] = true;
            }
            self.wire_to_legend[0] = Self::wire_to_legend(
                &self.common.info.get_legend(),
                self.doppler_wire_mode_for(0),
                low_power,
            );
            if low_power {
                log::info!(
                    "{}: using gamma echo curve for low-power radar",
                    self.common.key
                );
            }
            if let Some(cs) = &mut self.command_sender {
                cs.set_ranges(self.common.info.ranges.clone());
            }
            self.common.update();

            // Also update Range B if present
            if self.common_b.is_some() {
                let legend_b = {
                    let cb = self.common_b.as_mut().unwrap();
                    settings::update_when_model_known(&mut cb.info, model, version);
                    if !cb.info.controls.contains_key(&ControlId::Doppler) {
                        self.target_analyzer_known[1] = true;
                    }
                    let legend = cb.info.get_legend();
                    cb.update();
                    legend
                };
                let mode_b = self.doppler_wire_mode_for(1);
                self.wire_to_legend[1] = Self::wire_to_legend(&legend_b, mode_b, low_power);
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
        match network::create_udp_listen(
            &self.common.info.spoke_data_addr,
            &self.common.info.nic_addr,
            network::SocketType::Multicast,
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
        match network::create_udp_listen(
            &DATA_BROADCAST_ADDRESS,
            &self.common.info.nic_addr,
            network::SocketType::Broadcast,
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

        if want_multicast && let Err(e) = self.start_multicast_socket().await {
            r = Err(e);
        }
        if want_broadcast && let Err(e) = self.start_broadcast_socket().await {
            r = Err(e);
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
        if data.len() < 16 {
            log::debug!("Dropping short frame ({} bytes)", data.len());
            return;
        }

        // Tile echo format: first uint32 bits 29-31 == TILE_MAGIC (2)
        if data.len() >= 12 {
            let header_word = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
            if (header_word >> 29) == TILE_MAGIC {
                self.process_tile_frame(data);
                return;
            }
        }

        if data[0] != FRAME_MAGIC {
            log::debug!("Dropping invalid frame (magic={:#04x})", data[0]);
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

            // Decoders return `used` rounded up to the next 4-byte boundary so
            // the following sweep starts aligned. On the last spoke of a frame
            // the decoder may consume the buffer exactly, leaving no padding to
            // skip — clamp to avoid a slice OOB on the next iteration's bounds
            // check.
            sweep = &sweep[used.min(sweep.len())..];

            // The GUI buffers each angle in a slot of SPOKE_LEN samples
            // and treats that whole slot as covering the spoke's reported
            // physical range, so sample i is drawn at
            // `i / SPOKE_LEN * spoke_range`.
            //
            // The radar always transmits `sweep_len` total samples per spoke,
            // but only the first `metadata.scale` of them cover the configured
            // display range (0..range_meters). Samples beyond `scale` are
            // oversampled returns from past the display range — keep them and
            // widen the reported spoke range so the renderer can draw them in
            // the corners outside the outer range ring.
            let (send_spoke, spoke_range) = Self::map_with_overshoot(
                &generic_spoke,
                metadata.scale as usize,
                sweep_len,
                metadata.range,
            );

            // Defer emission until the radar's Target Analyzer state is
            // confirmed; otherwise the LUT may misclassify wire bytes and
            // strong stationary echoes get rendered with Doppler colors.
            // Delta-decoding state is still updated so the spoke stream
            // resumes correctly once the gate lifts.
            if self.target_analyzer_known[range_idx] {
                let wire_to_legend = &self.wire_to_legend[range_idx];
                if is_range_b {
                    Self::add_spoke_to_common(
                        self.common_b.as_mut().unwrap(),
                        &metadata,
                        spoke_range,
                        angle,
                        heading,
                        &send_spoke,
                        wire_to_legend,
                    );
                } else {
                    Self::add_spoke_to_common(
                        &mut self.common,
                        &metadata,
                        spoke_range,
                        angle,
                        heading,
                        &send_spoke,
                        wire_to_legend,
                    );
                }
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

    /// Decode a Tile echo frame (NXT-only format).
    ///
    /// The Tile format uses bitstream-packed headers and a different RLE scheme
    /// from IMO. Each frame contains one or more spoke strips (64 or 256 cells).
    /// The literal pixel encoding uses a bit-twist:
    /// `decoded = (byte & 0x7F) >> 1 | (byte & 1) << 7`.
    ///
    /// Reference: `DecodeTileEchoFormat` @ 0x5eda0 in libNAVNETDLL.so.
    fn process_tile_frame(&mut self, data: &[u8]) {
        if data.len() < 24 {
            log::debug!("Tile frame too short ({} bytes)", data.len());
            return;
        }

        // First header word at byte offset 8
        let header_word = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let content_length = (header_word & 0x7FF) as usize; // bits 0-10

        // Dual range ID: check byte 15 bit 6, same position as IMO format.
        // If this turns out wrong for Tile, we fall back to Range A only.
        let radar_no = (data[15] >> 6) & 0x01;
        let is_range_b = radar_no == 1 && self.common_b.is_some();
        let range_idx = if is_range_b { 1 } else { 0 };

        // Tile frames don't participate in IMO delta decoding. Clear the
        // previous-spoke history so a switch back to IMO starts fresh.
        self.prev_spoke[range_idx].clear();

        // Read range from the correct control state (A or B) since Tile
        // format doesn't carry wire_index in its header like IMO does.
        let range_controls = if is_range_b {
            &self.common_b.as_ref().unwrap().info.controls
        } else {
            &self.common.info.controls
        };
        let range = range_controls
            .get(&ControlId::Range)
            .and_then(|c| c.value)
            .unwrap_or(0.0) as u32;

        if is_range_b {
            self.common_b.as_mut().unwrap().new_spoke_message();
        } else {
            self.common.new_spoke_message();
        }

        // Per-spoke records start after the header words (byte offset 24).
        // The firmware loop condition is `offset <= content_length + 7` where
        // offset starts at 8, so content_length + 7 is the frame boundary.
        let mut pos: usize = 24;
        let frame_end = (content_length + 7).min(data.len());

        while pos + 4 <= frame_end {
            // Per-spoke sub-header: angle, heading/flags, first pixel + strip size
            let angle = ((data[pos] as u16) | ((data[pos + 1] as u16 & 0x1F) << 8)) as u16;
            let heading = ((data[pos + 2] as u16) | ((data[pos + 3] as u16 & 0x1F) << 8)) as u16;
            pos += 4;

            if pos >= frame_end {
                break;
            }

            let flag = data[pos];
            pos += 1;
            let pixel_count = if flag & 0x80 != 0 { 256 } else { 64 };

            // First pixel is a bit-twisted literal from `flag` bits 0-6
            let first_pixel = Self::tile_literal(flag);
            let mut spoke = Vec::with_capacity(pixel_count);
            spoke.push(first_pixel);

            // Decode remaining pixels via Tile RLE
            while spoke.len() < pixel_count && pos < frame_end {
                let byte = data[pos];
                pos += 1;

                if byte & 0x80 == 0 {
                    spoke.push(Self::tile_literal(byte));
                } else {
                    // RLE: repeat previous value
                    let mut count = (byte & 0x7F) as usize;
                    if count == 0 {
                        count = TILE_REPEAT_DEFAULT;
                    }
                    let prev = *spoke.last().unwrap_or(&0);
                    for _ in 0..count {
                        if spoke.len() >= pixel_count {
                            break;
                        }
                        spoke.push(prev);
                    }
                }
            }

            // Pad to TILE_SCALE samples: the first pixel_count are echo data,
            // the rest are zero (outside display range), matching IMO's scale
            // semantics where only the first `scale` samples cover 0..range.
            spoke.resize(TILE_SCALE as usize, 0);

            let send_spoke = Self::stretch_spoke(&spoke, TILE_SCALE as usize, SPOKE_LEN);

            let metadata = FurunoSpokeMetadata {
                sweep_count: 1,
                sweep_len: pixel_count as u32,
                encoding: 0,
                have_heading: 1,
                range,
                radar_no,
                scale: TILE_SCALE,
            };

            // See IMO path: gate emission on confirmed TA state. Tile frames
            // pad to TILE_SCALE so there's no overshoot to surface; the spoke
            // range equals the metadata range.
            if self.target_analyzer_known[range_idx] {
                let wire_to_legend = &self.wire_to_legend[range_idx];
                if is_range_b {
                    Self::add_spoke_to_common(
                        self.common_b.as_mut().unwrap(),
                        &metadata,
                        metadata.range,
                        angle,
                        heading,
                        &send_spoke,
                        wire_to_legend,
                    );
                } else {
                    Self::add_spoke_to_common(
                        &mut self.common,
                        &metadata,
                        metadata.range,
                        angle,
                        heading,
                        &send_spoke,
                        wire_to_legend,
                    );
                }
            }

            self.prev_angle[range_idx] = angle;
        }

        if is_range_b {
            self.common_b.as_mut().unwrap().send_spoke_message();
        } else {
            self.common.send_spoke_message();
        }
    }

    /// Decode a Tile-format bit-twisted literal: rotates bit 0 to bit 7.
    /// Only 7 input bits are used (bit 7 is the RLE marker), so the output
    /// has bit 6 always zero — max value is 191, giving 128 distinct levels.
    /// This is inherent to the Tile wire format, not a bug.
    #[inline]
    fn tile_literal(byte: u8) -> u8 {
        (byte & 0x7F) >> 1 | (byte & 1) << 7
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

    /// Map a Furuno spoke into the fixed-size GUI buffer while preserving the
    /// oversampled tail beyond the configured display range.
    ///
    /// The radar transmits `sweep_len` samples per spoke where only the first
    /// `scale` cover `0..range_meters`; the remaining `sweep_len - scale`
    /// samples are real returns from past the display range, used by Furuno's
    /// own MFD to fill the canvas corners outside the outer range ring.
    ///
    /// Returns the stretched buffer plus the widened spoke range
    /// `range_meters * sweep_len / scale`. When `scale` is missing or already
    /// covers the whole spoke this degrades to the previous behaviour:
    /// `sweep_len` is mapped 1:1 into `SPOKE_LEN` and the reported range
    /// equals `range_meters`.
    fn map_with_overshoot(
        src: &[u8],
        scale: usize,
        sweep_len: usize,
        range_meters: u32,
    ) -> (Vec<u8>, u32) {
        let usable = sweep_len.min(src.len());
        if scale == 0 || scale >= usable || range_meters == 0 {
            return (
                Self::stretch_spoke(src, usable.max(1), SPOKE_LEN),
                range_meters,
            );
        }

        // Map [0..usable] of the source onto [0..SPOKE_LEN] so sample i covers
        // physical distance `i / SPOKE_LEN * spoke_range`.
        let stretched = Self::stretch_spoke(src, usable, SPOKE_LEN);

        // `spoke_range = range_meters * usable / scale`, using u64 to avoid
        // overflow when usable or range_meters approach u32 limits.
        let widened = ((range_meters as u64) * (usable as u64) / (scale as u64)) as u32;
        (stretched, widened.max(range_meters))
    }

    /// Stretch a decoded spoke of `src_effective` meaningful samples (taken
    /// from the front of `src`) to `dst_len` samples using nearest-neighbour
    /// interpolation.
    ///
    /// On most Furuno models the native spoke length matches SPOKE_LEN
    /// and `src_effective == src.len()` — the stretch becomes a no-op copy.
    ///
    /// The DRS4W is special: every spoke carries 430 samples on the wire, but
    /// only the first N of those cover the configured display range. N varies
    /// per wire_index because the radar changes pulse width with range.
    /// Callers pass `src_effective` as the number of source samples that are
    /// physically meaningful for the active model and range.
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

    /// Build the raw-byte → legend-index lookup table.
    ///
    /// With `doppler_mode == Off` each wire byte is a plain intensity in
    /// `0..=PIXEL_VALUES`. Low-power radars (DRS4W, DRS) have a bottom-heavy
    /// echo distribution where 95% of returns are below raw value 64; an
    /// 18th-root (gamma 0.056) curve aggressively compresses the range so
    /// that even weak returns reach red, matching the Furuno iOS app's vivid
    /// visual output. Full-power models (NXT, FAR) use a linear mapping.
    ///
    /// With `doppler_mode == On` (NXT Target Analyzer active) each byte is
    /// `[dopplerClass:2 | intensity:4 | 00:2]`. The top two bits select the
    /// class (rain / stationary / approaching); the middle four bits are a
    /// 16-level intensity within the band. Separator ranges `0x3D..=0x3F`,
    /// `0x7D..=0x7F`, `0xBD..=0xBF` are never emitted by the radar and map
    /// to transparent. See research/furuno/mfd-radar-palette.md.
    fn wire_to_legend(
        legend: &crate::radar::Legend,
        doppler_mode: DopplerWireMode,
        low_power: bool,
    ) -> [u8; 256] {
        match doppler_mode {
            DopplerWireMode::Off => Self::wire_to_legend_off(legend, low_power),
            DopplerWireMode::On => Self::wire_to_legend_on(legend, low_power),
        }
    }

    fn wire_to_legend_off(legend: &crate::radar::Legend, low_power: bool) -> [u8; 256] {
        let pixel_colors = legend.pixel_colors;
        let pixel_max = pixel_colors.saturating_sub(1) as u16;
        let usable = pixel_max.saturating_sub(ECHO_FLOOR);
        let mut lut = [0u8; 256];
        for raw in 1u16..256 {
            let mapped = if low_power {
                let normalized = raw as f64 / PIXEL_VALUES as f64;
                ECHO_FLOOR + (normalized.powf(1.0 / 18.0) * usable as f64) as u16
            } else {
                ECHO_FLOOR + raw * usable / PIXEL_VALUES as u16
            };
            lut[raw as usize] = mapped.min(pixel_max) as u8;
        }
        lut
    }

    fn wire_to_legend_on(legend: &crate::radar::Legend, low_power: bool) -> [u8; 256] {
        // Per-band constants derived from the 3-band wire encoding:
        // high 2 bits = class, middle 4 bits = intensity, low 2 bits = 00.
        // Valid intra-band bytes are `base..=(base + BAND_TOP_OFFSET)`, giving
        // 16 distinct intensity levels per band.
        const BAND_SIZE: u16 = 0x40;
        const BAND_TOP_OFFSET: u16 = 0x3C; // 60 = 15 * 4
        const RAIN_BASE: u16 = 0x00;
        const STATIONARY_BASE: u16 = 0x40;
        const APPROACHING_BASE: u16 = 0x80;

        let stationary_lut = Self::wire_to_legend_off(legend, low_power);
        let mut lut = [0u8; 256];

        let (rain_start, rain_count) = legend.doppler_rain.unwrap_or((0, 0));
        let (appr_start, appr_count) = legend.doppler_approaching.unwrap_or((0, 0));

        for b in 0u16..BAND_SIZE {
            if b > BAND_TOP_OFFSET {
                continue; // separator bytes 0x3D..=0x3F stay 0 (transparent)
            }
            let sub = (b >> 2) as u8; // 0..=15 intensity within band
            // Rain band
            if rain_count > 0 {
                lut[(RAIN_BASE + b) as usize] = rain_start + sub * rain_count / 16;
            } else {
                // No rain slot available: fall through to the low end of the
                // stationary intensity ramp so rain is still visible.
                lut[(RAIN_BASE + b) as usize] = stationary_lut[(STATIONARY_BASE + b) as usize];
            }
            // Stationary band: reuse the TA-off intensity mapping for the
            // equivalent raw byte (b + 0x40).
            lut[(STATIONARY_BASE + b) as usize] = stationary_lut[(STATIONARY_BASE + b) as usize];
            // Approaching band
            if appr_count > 0 {
                lut[(APPROACHING_BASE + b) as usize] = appr_start + sub * appr_count / 16;
            } else {
                // No approaching slot available: fall through to the stationary
                // intensity ramp so approaching targets are still visible.
                lut[(APPROACHING_BASE + b) as usize] =
                    stationary_lut[(STATIONARY_BASE + b) as usize];
            }
        }
        // Byte 0 is always transparent; explicitly restate for clarity.
        lut[0] = 0;
        // The 0xC0..=0xFF band is unused in TA mode on NXT; leave as 0.
        lut
    }

    /// Derive the wire-decoding mode from the current Doppler control value
    /// for the given range (0 = A, 1 = B). `0` (Off) → `Off`; any non-zero
    /// value (Target, Rain) → `On`.
    fn doppler_wire_mode_for(&self, range_idx: usize) -> DopplerWireMode {
        let common = if range_idx == 1 {
            self.common_b.as_ref().unwrap_or(&self.common)
        } else {
            &self.common
        };
        let v = common
            .info
            .controls
            .get(&ControlId::Doppler)
            .and_then(|c| c.value)
            .unwrap_or(0.0);
        if v == 0.0 {
            DopplerWireMode::Off
        } else {
            DopplerWireMode::On
        }
    }

    /// `spoke_range` is the physical distance covered by the spoke buffer end
    /// to end. For full-coverage spokes it matches `metadata.range`; when the
    /// radar's `sweep_len` exceeds `scale` we widen the spoke range to
    /// `range * sweep_len / scale` so the renderer can draw the overshoot
    /// samples in the corners outside the outer range ring.
    /// The configured display range still drives the `ControlId::Range`
    /// update (replay only); only the spoke metadata that downstream
    /// distance math keys off is widened.
    fn add_spoke_to_common(
        common: &mut CommonRadar,
        metadata: &FurunoSpokeMetadata,
        spoke_range: u32,
        angle: SpokeBearing,
        heading: SpokeBearing,
        sweep: &[u8],
        wire_to_legend: &[u8; 256],
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

        let mut data = vec![0; sweep.len()];

        for (i, b) in sweep.iter().enumerate() {
            data[i] = wire_to_legend[*b as usize];
        }

        log::trace!(
            "Received {:04}/{:04} spoke {}",
            angle,
            heading.unwrap_or(9999),
            PrintableSpoke::new(&data)
        );

        common.add_spoke(spoke_range, angle, heading, data);
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
        let sweep_len = ((data[11] & FRAME_SWEEP_LEN_HIGH_MASK) as u32) << 8 | data[10] as u32;
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
        let range = wire_index_to_meters_for_unit(wire_index, wire_unit).unwrap_or_else(|| {
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
    socket: &mut Option<RadarSocket>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radar::Legend;

    /// Minimal `Legend` shaped like a post-TA NXT: 120 intensity slots,
    /// 1 approaching slot, 1 receding slot, 1 rain slot, mirroring what
    /// `default_legend(.., doppler_levels=1, has_rain_class=true, 120)`
    /// would produce. Only the fields used by `wire_to_legend_{off,on}`
    /// are populated.
    fn nxt_ta_legend() -> Legend {
        Legend {
            pixels: Vec::new(),
            pixel_colors: 120,
            history_start: 0,
            doppler_approaching: Some((120, 1)),
            doppler_receding: Some((121, 1)),
            doppler_rain: Some((122, 1)),
            strong_return: 0,
            medium_return: 0,
            low_return: 0,
            static_background: None,
        }
    }

    #[test]
    fn wire_to_legend_off_nxt_is_linear() {
        let legend = nxt_ta_legend();
        let lut = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::Off, false);
        assert_eq!(lut[0], 0, "raw 0 must be transparent");
        // raw 252 (PIXEL_VALUES) should reach the top of the intensity ramp.
        assert_eq!(lut[PIXEL_VALUES as usize], legend.pixel_colors - 1);
    }

    #[test]
    fn wire_to_legend_off_drs4w_is_nonlinear() {
        let legend = nxt_ta_legend();
        let lut_linear = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::Off, false);
        let lut_gamma = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::Off, true);
        // 18th-root curve must lift low raw values above the linear mapping.
        assert!(lut_gamma[4] > lut_linear[4]);
    }

    #[test]
    fn wire_to_legend_on_maps_three_bands() {
        let legend = nxt_ta_legend();
        let lut = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::On, false);

        let rain_start = legend.doppler_rain.unwrap().0;
        let appr_start = legend.doppler_approaching.unwrap().0;

        // Transparent / separators
        assert_eq!(lut[0], 0, "byte 0 is transparent");
        assert_eq!(lut[0x3D], 0, "rain/stationary separator is transparent");
        assert_eq!(lut[0x3F], 0);
        assert_eq!(lut[0x7D], 0, "stationary/approaching separator");
        assert_eq!(lut[0x7F], 0);
        assert_eq!(lut[0xBD], 0, "approaching/unused separator");
        assert_eq!(lut[0xBF], 0);
        assert_eq!(lut[0xFC], 0, "unused band stays transparent");

        // Rain band: 0x00..=0x3C with single-slot rain legend
        assert_eq!(lut[0x04], rain_start);
        assert_eq!(lut[0x3C], rain_start);

        // Stationary band: reuses the TA-off intensity ramp for the same
        // raw byte value.
        let off = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::Off, false);
        assert_eq!(lut[0x40], off[0x40]);
        assert_eq!(lut[0x7C], off[0x7C]);

        // Approaching band
        assert_eq!(lut[0x80], appr_start);
        assert_eq!(lut[0xBC], appr_start);
    }

    #[test]
    fn wire_to_legend_on_uses_16_level_gradient_per_band() {
        // Legend with 16 sub-levels per Doppler band, mirroring what
        // set_doppler_levels(16) + set_has_rain_class(true) produces.
        let legend = Legend {
            pixels: Vec::new(),
            pixel_colors: 120,
            history_start: 0,
            doppler_approaching: Some((120, 16)),
            doppler_receding: Some((136, 16)),
            doppler_rain: Some((152, 16)),
            strong_return: 0,
            medium_return: 0,
            low_return: 0,
            static_background: None,
        };
        let lut = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::On, false);
        // Each step of 4 in the raw byte moves one slot in the legend.
        assert_eq!(lut[0x80], 120, "approaching lowest intensity");
        assert_eq!(lut[0x84], 121);
        assert_eq!(lut[0xBC], 135, "approaching highest intensity");
        assert_eq!(lut[0x00], 0, "raw byte 0 is transparent");
        assert_eq!(lut[0x04], 153, "rain second-lowest intensity");
        assert_eq!(lut[0x3C], 167, "rain highest intensity");
    }

    #[test]
    fn wire_to_legend_on_without_rain_slot_falls_back_to_intensity() {
        let mut legend = nxt_ta_legend();
        legend.doppler_rain = None;
        let lut = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::On, false);
        let off = FurunoReportReceiver::wire_to_legend(&legend, DopplerWireMode::Off, false);
        // Without a rain slot, rain-range bytes reuse the stationary
        // intensity mapping at (b + 0x40).
        assert_eq!(lut[0x04], off[0x44]);
    }

    /// Captured tail of a Furuno DRS2D encoding-3 sweep that consumed its
    /// input buffer exactly (issue #195). The decoder returns `used`
    /// rounded up to the next 4-byte boundary, so process_frame must clamp
    /// before slicing to avoid `range start index 184 out of range for
    /// slice of length 182`.
    #[test]
    fn decode_sweep_encoding_3_alignment_can_overshoot() {
        let sweep: [u8; 182] = [
            0x0e, 0x64, 0x06, 0x6c, 0x06, 0x10, 0x12, 0x3c, 0x9c, 0x0a, 0x05, 0xf0, 0x68, 0x12,
            0x0c, 0x24, 0x70, 0x74, 0x60, 0x0e, 0x08, 0x10, 0x06, 0x14, 0x24, 0x32, 0x04, 0x00,
            0x0e, 0xfc, 0x06, 0x00, 0x24, 0x0a, 0x0c, 0x06, 0x05, 0x0a, 0x98, 0x0a, 0xcc, 0xdc,
            0x06, 0x1c, 0x14, 0x1a, 0x88, 0x90, 0x32, 0x05, 0x0e, 0x04, 0x00, 0x05, 0x1e, 0xec,
            0x0e, 0x1c, 0x06, 0x90, 0x8c, 0xa0, 0xac, 0x88, 0x06, 0x08, 0x06, 0x24, 0x06, 0x4c,
            0x10, 0x1c, 0x00, 0x16, 0x05, 0x20, 0x74, 0x4c, 0x0a, 0x08, 0x1a, 0x09, 0x16, 0x08,
            0x16, 0x08, 0x62, 0x05, 0x12, 0x2c, 0x5c, 0x16, 0x05, 0x1a, 0x05, 0x16, 0x09, 0x08,
            0x6a, 0x05, 0x76, 0x0c, 0x16, 0x18, 0x1e, 0x05, 0x1e, 0x0c, 0x22, 0x05, 0x6a, 0x05,
            0x46, 0x04, 0xb2, 0x05, 0x22, 0x08, 0x0a, 0x04, 0x66, 0x0c, 0x7e, 0x05, 0x12, 0x30,
            0x0c, 0x12, 0x18, 0x04, 0x0e, 0x09, 0x3a, 0x1c, 0x2a, 0x05, 0x0e, 0x05, 0x12, 0x05,
            0x26, 0x05, 0x6a, 0x30, 0x28, 0x16, 0x04, 0x1a, 0x08, 0x0e, 0x04, 0x66, 0x05, 0x5e,
            0x0c, 0x16, 0x64, 0x7e, 0x05, 0xfe, 0x00, 0x0e, 0x05, 0x0a, 0x04, 0x4e, 0x08, 0x12,
            0x04, 0xfe, 0x00, 0x12, 0x05, 0x22, 0x14, 0x2a, 0x05, 0x5a, 0x04, 0x0e, 0x05, 0x4e,
        ];
        let prev: Vec<u8> = vec![0; SPOKE_LEN];
        let sweep_len = 884;
        let (spoke, used) = FurunoReportReceiver::decode_sweep_encoding_3(&sweep, &prev, sweep_len);
        assert_eq!(spoke.len(), sweep_len);
        assert!(
            used > sweep.len(),
            "alignment must overshoot for regression: used={} sweep.len()={}",
            used,
            sweep.len()
        );
        // The fix in process_frame is `sweep = &sweep[used.min(sweep.len())..]`;
        // verify the clamp keeps us in bounds.
        let _ = &sweep[used.min(sweep.len())..];
    }

    #[test]
    fn map_with_overshoot_widens_range_when_scale_is_shorter() {
        // sweep_len = 8 samples, scale = 4 → spoke physically extends 2x the
        // configured range. Renderer-side machinery uses the widened value to
        // scale the polar texture past the outer ring.
        let src = vec![7u8; 8];
        let (spoke, spoke_range) = FurunoReportReceiver::map_with_overshoot(&src, 4, 8, 1000);
        assert_eq!(spoke.len(), SPOKE_LEN);
        assert_eq!(spoke_range, 2000);
    }

    #[test]
    fn map_with_overshoot_falls_back_when_scale_covers_whole_sweep() {
        // scale == usable: no overshoot exists, range stays as configured.
        let src = vec![7u8; 8];
        let (_, same_range) = FurunoReportReceiver::map_with_overshoot(&src, 8, 8, 1000);
        assert_eq!(same_range, 1000);
    }

    #[test]
    fn map_with_overshoot_falls_back_when_range_is_zero() {
        // range_meters == 0 happens when the radar hasn't reported a range
        // yet; widening would multiply zero by a fraction, so preserve zero.
        let src = vec![7u8; 8];
        let (_, zero_range) = FurunoReportReceiver::map_with_overshoot(&src, 4, 8, 0);
        assert_eq!(zero_range, 0);
    }

    #[test]
    fn map_with_overshoot_falls_back_when_scale_is_zero() {
        // scale == 0 means metadata didn't carry a usable scale field; the
        // safe default is to map the whole spoke 1:1 with the reported range.
        let src = vec![7u8; 8];
        let (_, same_range) = FurunoReportReceiver::map_with_overshoot(&src, 0, 8, 1000);
        assert_eq!(same_range, 1000);
    }
}
