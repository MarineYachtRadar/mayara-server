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
    ENCODING_3_REPEAT_DEFAULT, FurunoImoFrameHeader, FurunoTileFrameHeader,
    HEADING_ADJUST_FULL_TURN, HEADING_STATE_VALID, KEEP_ALIVE, PIXEL_VALUES, RadarModel,
    SPOKE_ALIGNMENT_MASK, SPOKE_ANGLE_HIGH_MASK, SPOKE_LEN, SPOKES, TILE_MAGIC,
    TILE_REPEAT_DEFAULT, TILE_SCALE, TX_CHANNEL_AUTO, WIRE_UNIT_KM, wire_index_to_meters_for_unit,
};
use super::settings;
use crate::Cli;
use crate::brand::CommandSender;
use crate::network;
use crate::radar::CommonRadar;
use crate::radar::SharedRadars;
use crate::radar::SpokeBearing;
use crate::radar::settings::{ControlId, ControlValue};
use crate::radar::{
    DUAL_RANGE_A, DUAL_RANGE_B, Power, RadarError, RadarInfo, transmit_claim_after_request,
};
use crate::replay::RadarSocket;
use crate::util::{PrintableSpoke, decode_head};
use serde_json::Value;

/// TCP keepalive timing for the Furuno control socket. Tunes how quickly the
/// kernel detects that the radar has silently dropped the connection after
/// idle (DRS4D-NXT does this after ~2 min in standby). With these settings the
/// kernel tears the socket down within ~60 s of the peer going silent, so the
/// next user PUT sees either a fresh writer or a clean NotConnected — never a
/// stale half-closed socket that just hangs.
const CONTROL_KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const CONTROL_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const CONTROL_KEEPALIVE_RETRIES: u32 = 3;

/// Reconnect backoff after the radar drops the control session. Every login
/// consumes a session slot in the radar's firmware; hammering relogins at a
/// fixed short interval can exhaust the slot table and lock ALL clients out
/// until the radar garbage-collects (observed on DRS4D-NXT: churn every ~6 s
/// kept the table full indefinitely). Back off exponentially and reset only
/// after a connection has proven stable.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A session that survived this long counts as stable: reset the backoff.
const RECONNECT_STABLE_AFTER: Duration = Duration::from_secs(60);

/// The signals a transmitting radar flags as present in the first field of
/// `$N7D`, each with the alarm raised while its bit is clear.
const SIGNAL_ALARMS: [(u32, &str); 4] = [
    (0x1, "Heading line signal lost"),
    (0x2, "Azimuth signal lost"),
    (0x4, "Trigger signal lost"),
    (0x8, "Video signal lost"),
];
const SIGNALS_PRESENT_MASK: u32 = 0xF;

/// `$NF5` subs that carry a supply rail in hundredths of a volt.
const NN3_SUPPLY_12V: u32 = 3;
const NN3_SUPPLY_5V: u32 = 4;

/// The signals that count as missing after `status`, given those that did
/// before. The antenna stops in standby, so only a transmitting radar can lose
/// a signal; one already lost stays lost until `status` sees it again, because
/// a DRS-NXT that loses its heading line or bearing pulses for about 20 s
/// drops itself to standby and the alarm is the reason why.
fn missing_signals(missing: u32, status: u32, transmitting: bool) -> u32 {
    let absent = !status & SIGNALS_PRESENT_MASK;
    if transmitting {
        absent
    } else {
        missing & absent
    }
}

/// Split out from the connection loop so the policy can be exercised without a
/// radar: the loop supplies real elapsed times and sleeps.
fn next_reconnect_backoff(
    current: Duration,
    session_lasted: Duration,
    short_lived_sessions: u32,
) -> (Duration, u32) {
    if session_lasted >= RECONNECT_STABLE_AFTER {
        // Proven stable — start from scratch so a later blip reconnects fast.
        (RECONNECT_BACKOFF_MIN, 0)
    } else {
        (
            (current * 2).min(RECONNECT_BACKOFF_MAX),
            short_lived_sessions + 1,
        )
    }
}

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
    have_heading: bool,
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
    /// The dual-range id a client asked to Transmit through mayara, while
    /// that transmit is still ours to stand down. A Furuno radar keeps
    /// transmitting until some client tells it to stop, so standing down
    /// means sending that request ourselves, and only for a transmit that was
    /// ours to begin with: an MFD's transmit is never touched. Both ranges
    /// share one transmitter, so this is one fact for the antenna. Cleared as
    /// soon as the radar reports anything but Transmit, or a client asks for
    /// Standby through mayara; only recorded once the request reached the
    /// radar.
    transmit_is_ours: Option<i32>,
    /// Range A already transmitted for someone else when we switched Range
    /// B on. Standing B down takes A down too, so A goes straight back on.
    range_a_is_theirs: bool,
    /// We switched our Range A back on after Range B stood down, and the
    /// radar has not yet reported A transmitting again: the standby it
    /// reports for A meanwhile does not end our claim.
    range_a_restoring: bool,
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
    /// `SIGNAL_ALARMS` bits currently raised as notifications.
    missing_signals: u32,
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
    /// `announced` is the model the radar's beacon names. It is passed in
    /// rather than read back from `info`, where saved settings may have
    /// replaced it with the model a previous run identified.
    pub(crate) fn new(
        args: &Cli,
        radars: SharedRadars,
        info: RadarInfo,
        announced: RadarModel,
    ) -> FurunoReportReceiver {
        let key = info.key();
        let command_sender = if args.is_replay() {
            None
        } else {
            Some(Command::new(&info, false))
        };

        // A beacon name mayara does not recognise falls back to the model a
        // previous run saved; in live mode `$N96` identifies it later.
        let model = match announced {
            RadarModel::Unknown => info
                .controls
                .model_name()
                .as_deref()
                .map(RadarModel::from_model_name)
                .unwrap_or(RadarModel::Unknown),
            announced => announced,
        };
        let low_power = model.is_low_power();
        let initial_lut = Self::wire_to_legend(&info.get_legend(), DopplerWireMode::Off, low_power);
        let wire_to_legend = [initial_lut, initial_lut];

        let control_update_rx = info.control_update_subscribe();
        let arpa_tx = radars.get_arpa_tx();

        let mut common = CommonRadar::new(
            args,
            key,
            info,
            radars,
            control_update_rx,
            args.is_replay(),
            arpa_tx,
        );
        // Coalesce ~1/32 of a revolution of spokes per broadcast. Furuno's
        // per-UDP sweep_count is wire-driven (variable), so batching pulls
        // the per-broadcast spoke count up to a consistent ~1/32 rev and
        // amortises one compression/WebSocket-framing cycle across it.
        let target = common.info.spokes_per_revolution.div_ceil(32) as usize;
        common.set_spoke_batch_threshold(target);

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
            transmit_is_ours: None,
            range_a_is_theirs: false,
            range_a_restoring: false,
            prev_angle: [0, 0],
            guard_zone_alarm: [false, false],
            missing_signals: 0,
            wire_to_legend,
            target_analyzer_known: [initial_ta_known, initial_ta_known],
        }
    }

    /// Raise a notification for each signal that went missing and clear it
    /// for each that came back.
    fn set_missing_signals(&mut self, missing: u32) {
        let changed = missing ^ self.missing_signals;
        self.missing_signals = missing;
        for (bit, name) in SIGNAL_ALARMS {
            if changed & bit == 0 {
                continue;
            }
            let active = missing & bit != 0;
            if active {
                log::warn!("{}: {}", self.common.key, name);
            } else {
                log::info!("{}: {} cleared", self.common.key, name);
            }
            self.common
                .info
                .controls
                .send_radar_error_notification(bit, Some(name), active);
        }
    }

    /// Set the Range B RadarInfo for dual range mode.
    pub(crate) fn set_range_b(&mut self, args: &Cli, radars: &SharedRadars, info_b: RadarInfo) {
        let key_b = info_b.key();
        let control_update_rx_b = info_b.control_update_subscribe();
        let arpa_tx_b = radars.get_arpa_tx();

        let mut common_b = CommonRadar::new(
            args,
            key_b,
            info_b,
            radars.clone(),
            control_update_rx_b,
            args.is_replay(),
            arpa_tx_b,
        );
        let target = common_b.info.spokes_per_revolution.div_ceil(32) as usize;
        common_b.set_spoke_batch_threshold(target);
        self.common_b = Some(common_b);

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
        let sock = TcpSocket::new_v4().map_err(RadarError::Io)?;
        let stream = sock
            .connect(std::net::SocketAddr::V4(self.common.info.send_command_addr))
            .await
            .map_err(RadarError::Io)?;

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

                    if self.common.info.stand_down() {
                        self.stand_down_our_transmit().await?;
                    }

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
                                // The report stream is shared by both ranges of a
                                // dual-range radar, so a report keeps both alive.
                                self.common.info.mark_input();
                                if let Some(ref cb) = self.common_b {
                                    cb.info.mark_input();
                                }
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
                            if let Some(ref mut cs) = self.command_sender {
                                cs.dual_range_id = DUAL_RANGE_A;
                            }
                            let claim = transmit_claim_after_request(self.transmit_is_ours, DUAL_RANGE_A, &cv.control_value);
                            self.common.process_control_update( cv, &mut self.command_sender).await?;
                            self.transmit_is_ours = claim;
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
                            if let Some(ref mut cs) = self.command_sender {
                                cs.dual_range_id = DUAL_RANGE_B;
                            }
                            let claim = transmit_claim_after_request(self.transmit_is_ours, DUAL_RANGE_B, &cv.control_value);
                            let a_transmits = transmits(&self.common);
                            let restore_a = restores_range_a(
                                &cv.control_value,
                                a_transmits,
                                self.common_b.as_ref().is_some_and(transmits),
                            );
                            let a_is_ours = self.transmit_is_ours == Some(DUAL_RANGE_A)
                                || (self.transmit_is_ours == Some(DUAL_RANGE_B) && !self.range_a_is_theirs);
                            self.range_a_is_theirs = range_a_is_theirs(self.transmit_is_ours, claim, self.range_a_is_theirs, a_transmits);
                            if let Some(ref mut cb) = self.common_b
                                && let Err(e) = cb.process_control_update(cv, &mut self.command_sender).await {
                                    return Err(e);
                                }
                            self.transmit_is_ours = claim;
                            if restore_a {
                                self.switch_range_a_back_on().await?;
                                if a_is_ours {
                                    self.transmit_is_ours = Some(DUAL_RANGE_A);
                                    self.range_a_restoring = true;
                                }
                            }
                        },
                    }
                },

                Some(r) = conditional_receive(&mut multicast_socket, &mut buf)  => {
                    log::trace!("Furuno data multicast recv {:?}", r);
                    match r {
                        Ok((len, addr)) => {
                            if self.verify_source_address(&addr) {
                                // Spoke traffic proves the radar is alive even
                                // while the TCP report session is down — keep
                                // the silence watchdog from decaying a
                                // transmitting radar to Off.
                                self.common.info.mark_input();
                                if let Some(ref cb) = self.common_b {
                                    cb.info.mark_input();
                                }
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
                                self.common.info.mark_input();
                                if let Some(ref cb) = self.common_b {
                                    cb.info.mark_input();
                                }
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
        let mut backoff = RECONNECT_BACKOFF_MIN;
        let mut short_lived_sessions = 0u32;
        loop {
            // Each time we start the loop, there is no stream
            // and none of the data sockets are open.
            self.stream = None;
            self.multicast_socket = None;
            self.broadcast_socket = None;

            let mut reached_data_loop = false;
            if let Err(e) = self.start_data_socket().await {
                log::warn!("{}: Failed to start data sockets: {}", self.common.key, e);
            } else if let Err(e) = self.login_to_radar() {
                log::warn!("{}: Failed to login to radar: {}", self.common.key, e);
            } else if let Err(e) = self.start_command_stream().await {
                log::warn!("{}: Failed to start command stream: {}", self.common.key, e);
            } else {
                reached_data_loop = true;
                let started = Instant::now();
                match self.data_loop(subsys).await {
                    Err(RadarError::Shutdown) => return Ok(()),
                    Err(e) => {
                        log::warn!(
                            "{}: Radar control session ended after {:?}: {}; reconnecting in {:?}",
                            self.common.key,
                            started.elapsed(),
                            e,
                            backoff
                        );
                    }
                    Ok(()) => {}
                }
                let was_stable = started.elapsed() >= RECONNECT_STABLE_AFTER;
                (backoff, short_lived_sessions) =
                    next_reconnect_backoff(backoff, started.elapsed(), short_lived_sessions);
                if !was_stable {
                    // Furuno radars keep one control session per client IP.
                    // A pattern of sessions that die right after the radar has
                    // answered the initial requests means another client on
                    // THIS host (e.g. a second mayara instance) keeps taking
                    // the slot — name the cause instead of churning silently.
                    if short_lived_sessions == 3 {
                        log::error!(
                            "{}: three control sessions in a row died within seconds — \
                             another client on this machine (a second mayara instance?) \
                             is likely competing for the radar's single per-host control \
                             session. Spoke data still flows; controls will not work \
                             until only one instance talks to this radar.",
                            self.common.key
                        );
                    }
                }
            }

            // A setup failure (socket, login or command stream) never reached
            // data_loop, so nothing escalated the backoff above. Treat it as a
            // zero-length session — otherwise a radar refusing logins would be
            // retried every second forever.
            if !reached_data_loop {
                (backoff, short_lived_sessions) =
                    next_reconnect_backoff(backoff, Duration::ZERO, short_lived_sessions);
            }

            tokio::select! {
                _ = subsys.on_shutdown_requested() => return Ok(()),
                _ = sleep(backoff) => {}
            }
        }
    }

    /// Nobody is watching: if the radar is transmitting on our behalf, put it
    /// back in Standby. The radar itself never stands down on losing a
    /// client, so this is the only way an unwatched Furuno radar stops.
    async fn stand_down_our_transmit(&mut self) -> Result<(), RadarError> {
        let (Some(range), Some(cs)) = (self.transmit_is_ours, &mut self.command_sender) else {
            return Ok(());
        };
        let target = match (&self.common_b, range) {
            (Some(cb), DUAL_RANGE_B) => cb,
            _ => &self.common,
        };
        log::info!(
            "{}: nobody watching and the radar transmits for us, sending standby",
            target.key
        );
        cs.dual_range_id = range;
        let standby = ControlValue::new(ControlId::Power, Value::from(Power::Standby as i32));
        cs.set_control(&standby, &target.info.controls).await?;
        self.transmit_is_ours = None;
        if range == DUAL_RANGE_B && self.range_a_is_theirs {
            self.switch_range_a_back_on().await?;
        }
        Ok(())
    }

    /// Range B standing down takes Range A down too; this puts A straight
    /// back on, for whoever was using it.
    async fn switch_range_a_back_on(&mut self) -> Result<(), RadarError> {
        let Some(cs) = &mut self.command_sender else {
            return Ok(());
        };
        log::info!(
            "{}: Range B stood down, switching Range A back on",
            self.common.key
        );
        cs.dual_range_id = DUAL_RANGE_A;
        let transmit = ControlValue::new(ControlId::Power, Value::from(Power::Transmit as i32));
        cs.set_control(&transmit, &self.common.info.controls).await
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

    /// Both ranges of a dual-range radar, or the only one: for a setting of
    /// the antenna, which the two share.
    fn both_ranges(&mut self) -> impl Iterator<Item = &mut CommonRadar> {
        std::iter::once(&mut self.common).chain(self.common_b.as_mut())
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
    ///   RezBoost / BirdMode: $NEE,{level},{drid}                  — index 1
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
            CommandId::RezBoost | CommandId::BirdMode => {
                // $NEE,{level},{drid}
                // $NED,{level},{drid}
                numbers.get(1).copied().unwrap_or(0.0)
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
        let line = line.strip_prefix(KEEP_ALIVE).unwrap_or(line);
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
            .ok_or(io::Error::other("No command ID"))?;
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
                if numbers.is_empty() {
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
                if drid as i32 == DUAL_RANGE_A && self.range_a_restoring {
                    self.range_a_restoring = !matches!(generic_state, Power::Transmit | Power::Off);
                    if generic_state == Power::Off {
                        self.transmit_is_ours = None;
                    }
                } else if self.transmit_is_ours == Some(drid as i32)
                    && generic_state != Power::Transmit
                {
                    self.transmit_is_ours = None;
                }
                // Someone else switched Range B on next to our Range A, and
                // standing A down would take B down with it.
                if drid as i32 == DUAL_RANGE_B
                    && generic_state == Power::Transmit
                    && self.transmit_is_ours == Some(DUAL_RANGE_A)
                {
                    self.transmit_is_ours = None;
                }
                // Range B transmits only alongside Range A, and the radar
                // reports every range a power change moves, so each range
                // takes only its own report.
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
                // Response format: $N64,{auto},{val},{auto_val},{drid},{auto sea mode},0
                if numbers.len() < 2 {
                    bail!("Insufficient ({}) arguments for Sea command", numbers.len());
                }
                let auto = numbers[0] as u8;
                let sea = numbers[1];
                let drid = self.extract_drid(&command_id, &numbers);
                if let (Some(&level), Some(cs)) = (numbers.get(2), &mut self.command_sender) {
                    cs.set_sea_auto_level(drid, level as i32);
                }
                let common = self.common_for_range(drid);
                common.set_value_auto(&ControlId::Sea, sea, auto);
                if let Some(&mode) = numbers.get(4)
                    && common.info.controls.contains_key(&ControlId::SeaState)
                {
                    common.set_value(&ControlId::SeaState, mode);
                }
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
                if numbers.is_empty() {
                    bail!(
                        "Insufficient ({}) arguments for ScanSpeed command",
                        numbers.len()
                    );
                }
                let mode = numbers[0];
                for common in self.both_ranges() {
                    common.set_value(&ControlId::ScanSpeed, mode);
                }
            }
            CommandId::BlindSector => {
                // Response format: $N77,{x},{s1_start},{s1_width},{s2_start},{s2_width}
                // A sector blanks whenever its width is not zero, whatever x
                // says. The antenna blanks both ranges alike.
                if numbers.len() < 5 {
                    bail!(
                        "Insufficient ({}) arguments for BlindSector command",
                        numbers.len()
                    );
                }
                let s1_start = numbers[1];
                let s1_width = numbers[2];
                let s2_start = numbers[3];
                let s2_width = numbers[4];

                // Convert from start/width to start/end
                let s1_end = (s1_start + s1_width) % 360.0;
                let s2_end = (s2_start + s2_width) % 360.0;

                let s1_enable = s1_width != 0.0;
                let s2_enable = s2_width != 0.0;

                // Wire format always includes blind-sector fields, but models
                // without sector_blanking (DRS, DRS4W) never registered the
                // controls — skip the set_sector to avoid spamming "not
                // supported" errors on every report.
                let sectors = [
                    (ControlId::NoTransmitSector1, s1_start, s1_end, s1_enable),
                    (ControlId::NoTransmitSector2, s2_start, s2_end, s2_enable),
                ];
                for common in self.both_ranges() {
                    for (control_id, start, end, enabled) in &sectors {
                        if common.info.controls.contains_key(control_id) {
                            common.set_sector(control_id, *start, *end, Some(*enabled));
                        }
                    }
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
                let seconds = first_number(&numbers, "OnTime")?;
                for common in self.both_ranges() {
                    common.set_value(&ControlId::OperatingTime, seconds);
                }
            }
            CommandId::TxTime => {
                let seconds = first_number(&numbers, "TxTime")?;
                for common in self.both_ranges() {
                    common.set_value(&ControlId::TransmitTime, seconds);
                }
            }
            CommandId::MainBangSize => {
                // Response format: $N83,{value},0
                // value: 0-255 (raw value, needs conversion to 0-100%)
                if numbers.is_empty() {
                    bail!(
                        "Insufficient ({}) arguments for MainBangSize command",
                        numbers.len()
                    );
                }
                // Convert 0-255 to 0-100%
                let percent = (numbers[0] as i32 * 100) / 255;
                for common in self.both_ranges() {
                    common.set_value(&ControlId::MainBangSuppression, percent as f64);
                }
            }
            CommandId::AntennaHeight => {
                // Response format: $N84,0,{meters},0
                let meters = field(&strings, 1, "AntennaHeight")?;
                for common in self.both_ranges() {
                    common.set_value(&ControlId::AntennaHeight, meters);
                }
            }
            CommandId::HeadingAdjust => {
                // Response format: $N81,{tenths of a degree, 0..3599},0
                let tenths = first_field(&strings, "HeadingAdjust")?;
                if !(0.0..HEADING_ADJUST_FULL_TURN as f64).contains(&tenths) {
                    bail!("HeadingAdjust report {} is not within one turn", tenths);
                }
                for common in self.both_ranges() {
                    common.set_value(&ControlId::BearingAlignment, tenths);
                }
            }
            CommandId::TxChannel => {
                // Response format: $NEC,{channel}: 0 = Auto, 1-3 = that channel
                // Auto names no channel, and the channels start at 1, so a
                // value-bearing call would offer 0 and be refused. The antenna
                // has one transmitter, so both ranges show its channel.
                let channel = first_field(&strings, "TxChannel")?;
                for common in self.both_ranges() {
                    if channel == TX_CHANNEL_AUTO as f64 {
                        let _ = common
                            .info
                            .controls
                            .set_auto_state(&ControlId::TransmitChannel, true);
                    } else {
                        common.set_value_auto(&ControlId::TransmitChannel, channel, 0);
                    }
                }
            }

            // NXT-specific features
            CommandId::SignalProcessing => {
                // Response format (varies):
                // - From SET echo: $N67,0,{feature},{value},{drid} (4 args)
                // - From REQUEST: $N67,{feature},{value},{drid} (3 args)
                // feature 0: Interference Rejection; the NXT keeps any ON as
                //   1 and sets it on both ranges, the classic DRS reports 2
                // feature 2: Echo Averaging
                // feature 3: Noise Reduction
                // A level above the control's highest is that highest level:
                // on, for a radar with only off and on.
                let (feature, value, drid) = if numbers.len() >= 4 && numbers[0] == 0.0 {
                    // SET echo format
                    (numbers[1] as i32, numbers[2] as i32, numbers[3])
                } else if numbers.len() >= 2 {
                    // REQUEST response format
                    let drid = numbers.get(2).copied().unwrap_or(0.0);
                    (numbers[0] as i32, numbers[1] as i32, drid)
                } else {
                    bail!(
                        "Insufficient ({}) arguments for SignalProcessing command",
                        numbers.len()
                    );
                };

                match feature {
                    0 | 2 | 3 => {
                        let control_id = match feature {
                            0 => ControlId::InterferenceRejection,
                            2 => ControlId::ScanAverageMode,
                            _ => ControlId::NoiseRejection,
                        };
                        let common = self.common_for_range(drid as u8);
                        if let Some(control) = common.info.controls.get(&control_id) {
                            let highest = control.item().max_value.unwrap_or(1.);
                            common.set_value(&control_id, (value as f64).min(highest));
                        }
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
                // Response format: $NEE,{level},{drid}
                // level: 0=OFF, 1=Low, 2=Medium, 3=High
                if numbers.is_empty() {
                    bail!(
                        "Insufficient ({}) arguments for RezBoost command",
                        numbers.len()
                    );
                }
                let drid = self.extract_drid(&command_id, &numbers);
                self.common_for_range(drid)
                    .set_value(&ControlId::TargetSeparation, numbers[0]);
            }
            CommandId::BirdMode => {
                // Response format: $NED,{level},{drid}
                // level: 0=OFF, 1=Low, 2=Medium, 3=High
                if numbers.is_empty() {
                    bail!(
                        "Insufficient ({}) arguments for BirdMode command",
                        numbers.len()
                    );
                }
                let drid = self.extract_drid(&command_id, &numbers);
                self.common_for_range(drid)
                    .set_value(&ControlId::BirdMode, numbers[0]);
            }
            CommandId::JammingAble => {
                if numbers.is_empty() {
                    bail!(
                        "Insufficient ({}) arguments for JammingAble command",
                        numbers.len()
                    );
                }
                for common in self.both_ranges() {
                    common.set_value(&ControlId::AntiJamming, numbers[0]);
                }
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

                // Target Analyzer is shared between the ranges of a dual-range
                // antenna: the firmware mirrors the state, and the radar only
                // reports it for the range the controlling client addressed.
                // Apply the report to both ranges so Range B's emission gate
                // lifts even when the confirmation arrives as Range A.
                let range_indices: &[u8] = if self.common_b.is_some() {
                    &[0, 1]
                } else {
                    &[0]
                };
                for &range_idx in range_indices {
                    let old_mode = self.doppler_wire_mode_for(range_idx as usize);
                    self.common_for_range(range_idx)
                        .set_value(&ControlId::Doppler, value);
                    let new_mode = self.doppler_wire_mode_for(range_idx as usize);
                    let key = if range_idx == 1 {
                        &self.common_b.as_ref().unwrap().key
                    } else {
                        &self.common.key
                    };
                    if old_mode != new_mode {
                        let low_power = self.model.is_low_power();
                        let legend = if range_idx == 1 {
                            self.common_b.as_ref().unwrap().info.get_legend()
                        } else {
                            self.common.info.get_legend()
                        };
                        self.wire_to_legend[range_idx as usize] =
                            Self::wire_to_legend(&legend, new_mode, low_power);
                        log::debug!("{}: Doppler wire mode changed to {:?}", key, new_mode);
                    }
                    // First `$NEF` confirms the TA state, so the LUT is now
                    // trustworthy and spoke emission can resume.
                    if !self.target_analyzer_known[range_idx as usize] {
                        self.target_analyzer_known[range_idx as usize] = true;
                        log::info!(
                            "{}: Target Analyzer state confirmed (mode {:?}), spoke output enabled",
                            key,
                            self.doppler_wire_mode_for(range_idx as usize)
                        );
                    }
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
                // $N7D,<status>,<d1>,<d2>,<d3> — status bits flag the signals
                // the radar sees; see SIGNAL_ALARMS.
                if self.model.reports_signal_alarms() {
                    let status = first_field(&strings, "Alarm")?;
                    let transmitting =
                        self.common.info.controls.get_status() == Some(Power::Transmit);
                    self.set_missing_signals(missing_signals(
                        self.missing_signals,
                        status as u32,
                        transmitting,
                    ));
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

            CommandId::NN3Command => {
                // $NF5,<sub>,<p1>,<p2>,<p3>,<p4>. The rails belong to the
                // antenna, so both ranges show them.
                let control_id = match first_field(&strings, "NN3Command")? as u32 {
                    NN3_SUPPLY_12V => Some(ControlId::Supply12Voltage),
                    NN3_SUPPLY_5V => Some(ControlId::Supply5Voltage),
                    _ => None,
                };
                if let Some(control_id) = control_id
                    && self.common.info.controls.contains_key(&control_id)
                {
                    let centivolts = field(&strings, 1, "NN3Command")?;
                    for common in self.both_ranges() {
                        common.set_value(&control_id, centivolts);
                    }
                }
            }

            CommandId::NearSTC | CommandId::MiddleSTC | CommandId::FarSTC | CommandId::STCRange => {
                // $N85,{curve}: one curve for the antenna, so both ranges.
                if let Some(&value) = numbers.first() {
                    let control_id = match command_id {
                        CommandId::NearSTC => ControlId::NearStcCurve,
                        CommandId::MiddleSTC => ControlId::MiddleStcCurve,
                        CommandId::FarSTC => ControlId::FarStcCurve,
                        _ => ControlId::StcRange,
                    };
                    for common in self.both_ranges() {
                        common.set_value(&control_id, value);
                    }
                }
            }

            // Silently handled (no state to update)
            CommandId::AliveCheck
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
    fn parse_modules(&mut self, values: &[&str]) {
        if self.model_known {
            return;
        }
        self.model_known = true; // We set this even if we can't parse the model, there is no point in logging errors many times.

        if let Some((model, version)) = model_and_version(values) {
            let model = model.as_announced(self.model);
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
            let legend_b = self.common_b.as_mut().map(|cb| {
                settings::update_when_model_known(&mut cb.info, model, version);
                let has_doppler = cb.info.controls.contains_key(&ControlId::Doppler);
                let legend = cb.info.get_legend();
                cb.update();
                (legend, has_doppler)
            });
            if let Some((legend_b, has_doppler)) = legend_b {
                if !has_doppler {
                    self.target_analyzer_known[1] = true;
                }
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
                    self.common.info.spoke_data_addr,
                    self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "{} via {}: listen multicast failed: {}",
                    self.common.info.spoke_data_addr,
                    self.common.info.nic_addr,
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
                    DATA_BROADCAST_ADDRESS,
                    self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "{} via {}: listen broadcast failed: {}",
                    DATA_BROADCAST_ADDRESS,
                    self.common.info.nic_addr,
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

        // The word at byte 8 is what tells the two echo formats apart.
        if let Ok(header) = decode_head::<FurunoTileFrameHeader>(&data[8..])
            && header.magic as u32 == TILE_MAGIC
        {
            self.process_tile_frame(data);
            return;
        }

        // The declaration carries the frame magic, so a frame that is not an
        // IMO echo frame fails to decode rather than being checked for first.
        let Some(metadata) = self.parse_metadata_header(data) else {
            log::debug!("Dropping invalid frame (magic={:#04x})", data[0]);
            return;
        };

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

        let Ok(header) = decode_head::<FurunoTileFrameHeader>(&data[8..]) else {
            return;
        };
        let content_length = header.content_length as usize;

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
            let angle = (data[pos] as u16) | ((data[pos + 1] as u16 & 0x1F) << 8);
            let heading = (data[pos + 2] as u16) | ((data[pos + 3] as u16 & 0x1F) << 8);
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
                have_heading: true,
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
        for (i, slot) in out.iter_mut().enumerate() {
            let j = (i * effective) / dst_len;
            *slot = src[j];
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

        let heading: Option<u16> = if metadata.have_heading {
            Some(heading)
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

    fn parse_metadata_header(&self, data: &[u8]) -> Option<FurunoSpokeMetadata> {
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
        //          bits 5-7: unknown (0b110 from a DRS-NXT, 0b001 from a DRS2D)
        //
        // Bytes 12-15: Range and status
        //   [12]   bits 0-5: range wire index; bits 6-7: range unit
        //   [13]   range resolution metadata
        //   [14]   range_value low byte
        //   [15]   bits 0-2: range_value high; bit 3: flag;
        //          bits 4-5: heading_state (1 = the spokes carry the
        //          radar's heading); bit 6: dual_range_id (0=A, 1=B);
        //          bit 7: unknown

        let header = decode_head::<FurunoImoFrameHeader>(data).ok()?;
        let wire_index = header.wire_index as i32;

        // The same wire index means a different distance in each unit, and
        // the frame says which unit it counts in.
        let wire_unit = header.range_unit as i32;
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
        // The radar always transmits sweep_len total samples, but only the
        // first `scale` map to 0..range_meters. Verified against radar.dll
        // disassembly (DecodeImoEchoFormat) and the ARM MFD firmware
        // (libNAVNETDLL.so, not-stripped symbols from imoecho.c).
        //
        // Fall back to sweep_len if scale is zero (malformed packet).
        let scale = if header.scale == 0 {
            header.sweep_len
        } else {
            header.scale
        };

        let metadata = FurunoSpokeMetadata {
            sweep_count: header.sweep_count,
            sweep_len: header.sweep_len,
            encoding: header.encoding,
            have_heading: header.heading_state == HEADING_STATE_VALID,
            range,
            radar_no: header.radar_no,
            scale,
        };
        log::trace!("header {:?} -> {:?}", &data[0..16], metadata);

        Some(metadata)
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

/// The first argument of a report that needs one. A radar is free to send
/// `$N8E` with nothing after it -- and mayara asks for exactly that with
/// `$R8E,0` -- so the absence has to be an error rather than an index.
fn transmits(common: &CommonRadar) -> bool {
    common
        .info
        .controls
        .get(&ControlId::Power)
        .and_then(|c| c.value)
        == Some(Power::Transmit as i32 as f64)
}

/// Whether a request to Range B must be followed by switching Range A back
/// on: standing B down while both transmit takes A down too, and only B
/// was asked for.
fn restores_range_a(cv: &ControlValue, a_transmits: bool, b_transmits: bool) -> bool {
    cv.id == ControlId::Power
        && a_transmits
        && b_transmits
        && cv
            .as_value()
            .ok()
            .and_then(|v| Power::from_value(&v).ok())
            .is_some_and(|power| power != Power::Transmit)
}

/// Whether Range A transmitted for someone else before we claimed Range B,
/// worked out when a request moves our claim. Range B transmits only
/// alongside Range A, so switching B on finds A already transmitting
/// unless we started both.
fn range_a_is_theirs(
    current: Option<i32>,
    claim: Option<i32>,
    a_is_theirs: bool,
    a_transmits: bool,
) -> bool {
    match (current, claim) {
        (None, Some(DUAL_RANGE_B)) => a_transmits,
        (Some(DUAL_RANGE_B), Some(DUAL_RANGE_B)) => a_is_theirs,
        _ => false,
    }
}

fn first_number(numbers: &[f64], command: &str) -> Result<f64, Error> {
    match numbers.first() {
        Some(value) => Ok(*value),
        None => bail!("Insufficient (0) arguments for {} command", command),
    }
}

/// The first argument of a report, refusing one that is not a number.
fn first_field(strings: &[&str], command: &str) -> Result<f64, Error> {
    field(strings, 0, command)
}

/// Argument `index` of a report, refusing one that is missing or not a
/// number. The parsed `numbers` read such a field as 0, which for a setting
/// like the alignment is a value of its own rather than an absence.
fn field(strings: &[&str], index: usize, command: &str) -> Result<f64, Error> {
    let Some(field) = strings.get(index) else {
        bail!(
            "Insufficient ({}) arguments for {} command",
            strings.len(),
            command
        );
    };
    field
        .trim()
        .parse()
        .with_context(|| format!("{} report has {:?} for a number", command, field))
}

/// The model and firmware version out of a `$N96` module list, whose first
/// entry reads `<part number>-<version>`. A reply carrying no entries, or a
/// first entry in another shape, names no model.
fn model_and_version<'a>(values: &[&'a str]) -> Option<(RadarModel, &'a str)> {
    let (part, version) = values.first()?.split_once('-')?;

    Some((RadarModel::from_part_number(part), version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radar::Legend;
    use clap::Parser;
    use std::net::{Ipv4Addr, SocketAddrV4};

    /// An NXT's radar info with its model known; `dual` names its range.
    fn nxt_info(radars: &SharedRadars, args: &Cli, dual: Option<&str>) -> RadarInfo {
        model_info(radars, args, dual, RadarModel::DRS4DNXT)
    }

    /// The radar info of a `model` with its model known.
    fn model_info(
        radars: &SharedRadars,
        args: &Cli,
        dual: Option<&str>,
        model: RadarModel,
    ) -> RadarInfo {
        let addr = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 10000);
        let mut info = RadarInfo::new(
            radars,
            args,
            crate::Brand::Furuno,
            Some("TEST0001"),
            None,
            dual,
            PIXEL_VALUES,
            SPOKES,
            SPOKE_LEN,
            addr,
            Ipv4Addr::new(10, 0, 0, 1),
            addr,
            addr,
            addr,
            |id, tx| settings::new(id, tx, args),
            true,
            true,
        );
        info.controls.set_user_name(info.key());
        settings::update_when_model_known(&mut info, model, "1.00");
        info
    }

    /// An NXT receiver with its model known, as the report loop holds it.
    fn nxt_receiver() -> FurunoReportReceiver {
        let radars = SharedRadars::new();
        let args = Cli::parse_from(["mayara-server"]);
        let info = nxt_info(&radars, &args, None);
        FurunoReportReceiver::new(&args, radars, info, RadarModel::DRS4DNXT)
    }

    /// The same with Range B alongside, as a dual-range NXT runs.
    fn dual_range_receiver() -> FurunoReportReceiver {
        let radars = SharedRadars::new();
        let args = Cli::parse_from(["mayara-server"]);
        let info = nxt_info(&radars, &args, Some("A"));
        let info_b = nxt_info(&radars, &args, Some("B"));
        let mut receiver =
            FurunoReportReceiver::new(&args, radars.clone(), info, RadarModel::DRS4DNXT);
        receiver.set_range_b(&args, &radars, info_b);
        receiver
    }

    /// A DRS2D-NXT runs the DRS4D-NXT's firmware and reports its part number
    /// in `$N96`; only the model its beacon announces tells the two apart,
    /// even when saved settings name the other one.
    #[tokio::test]
    async fn a_drs2d_nxt_keeps_the_model_it_announces() {
        for (announced, expected) in [
            (RadarModel::DRS2DNXT, "DRS2DNXT"),
            (RadarModel::DRS4DNXT, "DRS4DNXT"),
        ] {
            let radars = SharedRadars::new();
            let args = Cli::parse_from(["mayara-server"]);
            let info = nxt_info(&radars, &args, None);
            let mut receiver = FurunoReportReceiver::new(&args, radars, info, announced);

            receiver
                .process_report("$N96,0359360-01.11,0359358-01.01,0359359-01.01,0359361-01.08,,,")
                .unwrap();

            assert_eq!(receiver.model, announced);
            assert_eq!(
                receiver.common.info.controls.model_name().as_deref(),
                Some(expected)
            );
        }
    }

    /// A spoke frame's range is read in the unit the frame names, even
    /// while the RangeUnits control has not been set, as in a replay. A
    /// DRS2D-NXT on wire index 6 is 2 km or 2 NM.
    #[test]
    fn a_spoke_frame_is_ranged_in_the_unit_it_names() {
        let receiver = nxt_receiver();
        let mut frame = [
            2, 186, 0, 1, 0, 0, 0, 0, 33, 8, 100, 219, 0x46, 0, 240, 0x19,
        ];
        let range = |frame: &[u8]| receiver.parse_metadata_header(frame).unwrap().range;

        assert_eq!(range(&frame), 2000);
        frame[12] = 0x06;
        assert_eq!(range(&frame), 2 * 1852);
    }

    /// A beacon naming a model mayara does not recognise leaves the model
    /// saved settings name in place until `$N96` arrives.
    #[test]
    fn an_unknown_announced_model_keeps_the_saved_one() {
        let radars = SharedRadars::new();
        let args = Cli::parse_from(["mayara-server"]);
        let info = nxt_info(&radars, &args, None);
        let receiver = FurunoReportReceiver::new(&args, radars, info, RadarModel::Unknown);

        assert_eq!(receiver.model, RadarModel::DRS4DNXT);
    }

    /// The alignment comes back in tenths of a degree, 0 to 3599, and one past
    /// half a turn is the bearing left of the bow. A DRS4D-NXT reported
    /// `$N81,70,0` and `$N81,3550,0` for 7.0 and -5.0 degrees.
    #[tokio::test]
    async fn a_reported_alignment_reads_in_degrees_either_side_of_the_bow() {
        for (report, expected) in [
            ("$N81,70,0", 7.0),
            ("$N81,3550,0", -5.0),
            ("$N81,1800,0", 180.0),
        ] {
            let mut receiver = nxt_receiver();

            receiver.process_report(report).unwrap();

            assert_alignment(&receiver.common, expected, report);
        }
    }

    /// The radar echoes a negative number it was sent, as its 16-bit two's
    /// complement, before it reports the alignment it kept: a DRS4D-NXT sent
    /// `$S81,-50,0` replied `$N81,65486,0` and then `$N81,70,0`. The echo is
    /// beyond a turn, so it is not taken for an alignment.
    #[tokio::test]
    async fn the_echo_of_a_refused_alignment_is_not_taken_for_one() {
        let mut receiver = nxt_receiver();

        receiver.process_report("$N81,70,0").unwrap();
        assert!(receiver.process_report("$N81,65486,0").is_err());

        assert_alignment(&receiver.common, 7.0, "after the echo");
    }

    /// The alignment is the antenna's, so a dual-range radar shows it on both
    /// ranges.
    #[tokio::test]
    async fn a_reported_alignment_shows_on_both_ranges() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$N81,3550,0").unwrap();

        assert_alignment(&receiver.common, -5.0, "Range A");
        assert_alignment(receiver.common_b.as_ref().unwrap(), -5.0, "Range B");
    }

    /// A report whose alignment is missing, not a number or outside the
    /// 0..3599 the radar reports in is refused rather than taken for an
    /// alignment: read as 0, it would turn the picture back to the bow.
    #[tokio::test]
    async fn a_malformed_alignment_is_refused() {
        let mut receiver = dual_range_receiver();
        receiver.process_report("$N81,70,0").unwrap();

        for report in ["$N81", "$N81,,0", "$N81,x,0", "$N81,-100,0", "$N81,3600,0"] {
            assert!(receiver.process_report(report).is_err(), "{report}");
        }

        assert_alignment(&receiver.common, 7.0, "Range A");
        assert_alignment(receiver.common_b.as_ref().unwrap(), 7.0, "Range B");
    }

    /// The antenna height is the second field, in meters, and the antenna's,
    /// so a dual-range radar shows it on both ranges. A DRS4D-NXT reported
    /// `$N84,0,10,0` for 10 m.
    #[tokio::test]
    async fn a_reported_antenna_height_shows_on_both_ranges() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$N84,0,10,0").unwrap();

        for common in [&receiver.common, receiver.common_b.as_ref().unwrap()] {
            assert_eq!(antenna_height(common), Some(10.));
        }
    }

    /// A report whose height is missing or not a number is refused rather
    /// than read as 0.
    #[tokio::test]
    async fn a_malformed_antenna_height_is_refused() {
        let mut receiver = nxt_receiver();
        receiver.process_report("$N84,0,10,0").unwrap();

        for report in ["$N84", "$N84,0", "$N84,0,,0", "$N84,0,x,0"] {
            assert!(receiver.process_report(report).is_err(), "{report}");
        }

        assert_eq!(antenna_height(&receiver.common), Some(10.));
    }

    fn antenna_height(common: &CommonRadar) -> Option<f64> {
        common
            .info
            .controls
            .get(&ControlId::AntennaHeight)
            .and_then(|c| c.value)
    }

    /// The alignment one range shows, in degrees.
    fn alignment(common: &CommonRadar) -> Option<f64> {
        common
            .info
            .controls
            .get(&ControlId::BearingAlignment)
            .and_then(|c| c.value)
            .map(f64::to_degrees)
    }

    fn assert_alignment(common: &CommonRadar, expected: f64, when: &str) {
        let degrees = alignment(common).expect("an alignment");
        assert!((degrees - expected).abs() < 1e-6, "{when}: {degrees}");
    }

    /// Channel 0 is the radar choosing for itself, which is auto rather than
    /// a channel; the channels it can be given are 1 to 3. A DRS4D-NXT
    /// reported `$NEC,0` on Auto and `$NEC,2` on channel 2.
    #[tokio::test]
    async fn a_reported_transmit_channel_of_zero_is_auto() {
        let mut receiver = nxt_receiver();
        let channel = |receiver: &FurunoReportReceiver| {
            let control = receiver
                .common
                .info
                .controls
                .get(&ControlId::TransmitChannel)
                .unwrap();
            (control.value, control.auto)
        };

        receiver.process_report("$NEC,2").unwrap();
        assert_eq!(channel(&receiver), (Some(2.), Some(false)));

        receiver.process_report("$NEC,0").unwrap();
        assert_eq!(channel(&receiver), (Some(2.), Some(true)));
    }

    /// A report whose channel is missing or not a number is refused rather
    /// than read as 0, which would turn Auto on.
    #[tokio::test]
    async fn a_malformed_transmit_channel_is_refused() {
        let mut receiver = nxt_receiver();
        receiver.process_report("$NEC,2").unwrap();

        for report in ["$NEC", "$NEC,", "$NEC,x"] {
            assert!(receiver.process_report(report).is_err(), "{report}");
        }

        let control = receiver
            .common
            .info
            .controls
            .get(&ControlId::TransmitChannel)
            .unwrap();
        assert_eq!((control.value, control.auto), (Some(2.), Some(false)));
    }

    /// The antenna has one transmitter, so a dual-range radar shows its
    /// channel on both ranges.
    #[tokio::test]
    async fn a_reported_transmit_channel_shows_on_both_ranges() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$NEC,3").unwrap();
        receiver.process_report("$NEC,0").unwrap();

        for common in [&receiver.common, receiver.common_b.as_ref().unwrap()] {
            let control = common
                .info
                .controls
                .get(&ControlId::TransmitChannel)
                .unwrap();
            assert_eq!((control.value, control.auto), (Some(3.), Some(true)));
        }
    }

    /// A transmitting radar flags each signal it sees; a clear bit is a lost
    /// signal. A FAR-2127 sent `$N7D,15` with all four present, a DRS4D-NXT
    /// `$N7D,64543` whose bits above the low four are boot leftovers.
    #[test]
    fn a_clear_signal_bit_is_lost_only_while_transmitting() {
        assert_eq!(missing_signals(0, 15, true), 0);
        assert_eq!(missing_signals(0, 0xFC1F, true), 0);
        assert_eq!(missing_signals(0, 0b1011, true), 0x4);
        assert_eq!(missing_signals(0, 0, true), SIGNALS_PRESENT_MASK);
        assert_eq!(missing_signals(0, 0, false), 0);
    }

    /// A signal lost while transmitting stays lost through standby, where the
    /// radar may have put itself because of it, until the radar sees it again.
    #[test]
    fn a_lost_signal_stays_lost_until_it_returns() {
        assert_eq!(missing_signals(0x1, 0, false), 0x1);
        assert_eq!(missing_signals(0x1, 0xF, false), 0);
        assert_eq!(missing_signals(0x3, 0b1101, false), 0x2);
    }

    /// A lost signal is raised while the radar transmits, survives standby,
    /// and clears when the signal returns.
    #[tokio::test]
    async fn a_lost_signal_is_raised_while_transmitting() {
        let mut receiver = nxt_receiver();

        receiver.process_report("$N7D,0,0,0,0").unwrap();
        assert_eq!(receiver.missing_signals, 0, "standby");

        receiver.process_report("$N69,2,0,0,60,300,0").unwrap();
        receiver.process_report("$N7D,11,0,0,0").unwrap();
        assert_eq!(receiver.missing_signals, 0x4, "trigger lost");

        receiver.process_report("$N7D,15,0,0,0").unwrap();
        assert_eq!(receiver.missing_signals, 0, "trigger back");

        receiver.process_report("$N7D,14,0,0,0").unwrap();
        receiver.process_report("$N69,1,0,0,60,300,0").unwrap();
        receiver.process_report("$N7D,0,0,0,0").unwrap();
        assert_eq!(
            receiver.missing_signals, 0x1,
            "heading still lost in standby"
        );

        receiver.process_report("$N7D,1,0,0,0").unwrap();
        assert_eq!(receiver.missing_signals, 0, "heading back");
    }

    /// A status that is not a number is refused rather than read as 0, which
    /// on a transmitting radar would raise all four lost signals.
    #[tokio::test]
    async fn a_malformed_signal_status_is_refused() {
        let mut receiver = nxt_receiver();
        receiver.process_report("$N69,2,0,0,60,300,0").unwrap();

        for report in ["$N7D", "$N7D,", "$N7D,x,0,0,0"] {
            assert!(receiver.process_report(report).is_err(), "{report}");
        }
        assert_eq!(receiver.missing_signals, 0);
    }

    /// The power reports a DRS2D-NXT sent as each range was switched: A
    /// transmits alone, B transmitting takes A along, and either range
    /// standing by from both transmitting takes the other along.
    #[tokio::test]
    async fn each_range_takes_only_its_own_power_report() {
        let mut receiver = dual_range_receiver();
        let power = |receiver: &FurunoReportReceiver| {
            [false, true].map(|b| value_on(receiver, b, &ControlId::Power))
        };
        let (transmit, standby) = (
            Some(Power::Transmit as i32 as f64),
            Some(Power::Standby as i32 as f64),
        );

        receiver.process_report("$N69,2,0,0,60,540,0").unwrap();
        receiver.process_report("$N69,1,1,0,60,540,0").unwrap();
        assert_eq!(power(&receiver), [transmit, standby], "A alone");

        receiver.process_report("$N69,2,1,0,60,540,0").unwrap();
        receiver.process_report("$N69,2,0,0,60,540,0").unwrap();
        assert_eq!(power(&receiver), [transmit, transmit], "B takes A along");

        receiver.process_report("$N69,1,1,0,60,540,0").unwrap();
        receiver.process_report("$N69,1,0,0,60,540,0").unwrap();
        assert_eq!(power(&receiver), [standby, standby], "B takes A down");
    }

    /// Standing Range B down takes Range A down too, so mayara remembers
    /// whether A already transmitted for someone else when it claimed B.
    #[test]
    fn range_a_is_theirs_when_it_transmitted_before_our_range_b() {
        let (a, b) = (Some(DUAL_RANGE_A), Some(DUAL_RANGE_B));
        assert!(range_a_is_theirs(None, b, false, true), "plotter's A");
        assert!(!range_a_is_theirs(None, b, false, false), "we start both");
        assert!(!range_a_is_theirs(a, b, false, true), "A was ours");
        assert!(range_a_is_theirs(b, b, true, true), "B asked again");
        assert!(!range_a_is_theirs(b, None, true, true), "B standby");
    }

    /// Range B asked to stand down while both ranges transmit takes Range A
    /// down too, so A goes back on; any other request leaves A alone.
    #[test]
    fn standing_range_b_down_puts_range_a_back_on() {
        let power = |p: Power| ControlValue::new(ControlId::Power, Value::from(p as i32));
        let standby = power(Power::Standby);
        assert!(restores_range_a(&standby, true, true), "both transmit");
        assert!(
            !restores_range_a(&standby, true, false),
            "B already standby"
        );
        assert!(!restores_range_a(&standby, false, false), "both standby");
        assert!(
            !restores_range_a(&power(Power::Transmit), true, true),
            "transmit"
        );
        let gain = ControlValue::new(ControlId::Gain, Value::from(1));
        assert!(!restores_range_a(&gain, true, true), "not power");
    }

    /// Range B standing down takes our Range A through standby before it
    /// transmits again; that standby must not end our claim on A, or mayara
    /// would never stand A down.
    #[tokio::test]
    async fn our_range_a_stays_ours_while_it_comes_back_on() {
        let mut receiver = dual_range_receiver();
        receiver.transmit_is_ours = Some(DUAL_RANGE_A);
        receiver.range_a_restoring = true;

        for report in [
            "$N69,1,1,0,60,540,0",
            "$N69,1,0,0,60,540,0",
            "$N69,2,0,0,60,540,0",
        ] {
            receiver.process_report(report).unwrap();
            assert_eq!(receiver.transmit_is_ours, Some(DUAL_RANGE_A), "{report}");
        }
        assert!(!receiver.range_a_restoring);

        receiver.process_report("$N69,1,0,0,60,540,0").unwrap();
        assert_eq!(receiver.transmit_is_ours, None, "a later standby ends it");
    }

    /// A radar that goes off while A comes back on ends our claim.
    #[tokio::test]
    async fn range_a_going_off_while_it_comes_back_on_ends_our_claim() {
        let mut receiver = dual_range_receiver();
        receiver.transmit_is_ours = Some(DUAL_RANGE_A);
        receiver.range_a_restoring = true;

        receiver.process_report("$N69,3,0,0,60,540,0").unwrap();

        assert_eq!(receiver.transmit_is_ours, None);
        assert!(!receiver.range_a_restoring);
    }

    /// Range B switched on by someone else next to our Range A ends our claim:
    /// standing A down would take their B down too.
    #[tokio::test]
    async fn someone_else_s_range_b_ends_our_claim_on_range_a() {
        let mut receiver = dual_range_receiver();
        receiver.transmit_is_ours = Some(DUAL_RANGE_A);

        receiver.process_report("$N69,2,0,0,60,540,0").unwrap();
        assert_eq!(receiver.transmit_is_ours, Some(DUAL_RANGE_A));

        receiver.process_report("$N69,2,1,0,60,540,0").unwrap();
        assert_eq!(receiver.transmit_is_ours, None);
    }

    /// The auto sea mode a chartplotter chose comes back in the sea report,
    /// and echo averaging lands on the range it names.
    #[tokio::test]
    async fn auto_sea_mode_and_echo_averaging_are_read_back() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$N64,1,30,10,0,2,0").unwrap();
        receiver.process_report("$N67,0,2,1,1").unwrap();

        assert_eq!(value_on(&receiver, false, &ControlId::SeaState), Some(2.));
        assert_eq!(
            value_on(&receiver, true, &ControlId::ScanAverageMode),
            Some(1.)
        );
        assert_eq!(
            value_on(&receiver, false, &ControlId::ScanAverageMode),
            Some(0.),
            "Range A stays off"
        );
    }

    /// The rails belong to the antenna, so a dual-range NXT shows them on
    /// both ranges, and a reading that is not a number is refused.
    #[tokio::test]
    async fn the_supply_rails_show_on_both_ranges() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$NF5,3,1206,0,0,0").unwrap();
        assert!(receiver.process_report("$NF5,3,x,0,0,0").is_err());

        for common in [&receiver.common, receiver.common_b.as_ref().unwrap()] {
            let control = common
                .info
                .controls
                .get(&ControlId::Supply12Voltage)
                .unwrap();
            assert_eq!(control.value, Some(12.06));
        }
    }

    /// The value a range shows for a control.
    fn value_on(receiver: &FurunoReportReceiver, range_b: bool, id: &ControlId) -> Option<f64> {
        let common = if range_b {
            receiver.common_b.as_ref().unwrap()
        } else {
            &receiver.common
        };
        common.info.controls.get(id).unwrap().value
    }

    /// The antenna has one STC curve per band, reported without a range, so
    /// both ranges show it, and far curve 3 is the DRS4D-NXT's own default:
    /// refusing it filled the log every five seconds.
    #[tokio::test]
    async fn an_stc_curve_shows_on_both_ranges() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$N87,3").unwrap();
        receiver.process_report("$N85,5").unwrap();

        for range_b in [false, true] {
            assert_eq!(
                value_on(&receiver, range_b, &ControlId::FarStcCurve),
                Some(3.)
            );
            assert_eq!(
                value_on(&receiver, range_b, &ControlId::NearStcCurve),
                Some(5.)
            );
        }
    }

    /// A sector blanks whenever it has a width: a chartplotter sends the
    /// first field as 0 or 1 for the same sector. Both ranges blank alike.
    #[tokio::test]
    async fn a_sector_is_on_when_it_has_a_width() {
        let mut receiver = dual_range_receiver();

        for report in ["$N77,0,124,144,0,0", "$N77,1,124,144,0,0"] {
            receiver.process_report(report).unwrap();

            for common in [&receiver.common, receiver.common_b.as_ref().unwrap()] {
                let controls = &common.info.controls;
                let first = controls.get(&ControlId::NoTransmitSector1).unwrap();
                let second = controls.get(&ControlId::NoTransmitSector2).unwrap();
                assert_eq!(first.enabled, Some(true), "{report}");
                assert_eq!(second.enabled, Some(false), "{report}");
            }
        }
    }

    /// Target separation and bird mode are kept per range and say which one.
    #[tokio::test]
    async fn target_separation_and_bird_mode_land_on_the_range_they_name() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$NEE,2,1").unwrap();
        receiver.process_report("$NED,3,0").unwrap();

        assert_eq!(
            value_on(&receiver, true, &ControlId::TargetSeparation),
            Some(2.)
        );
        assert_eq!(
            value_on(&receiver, false, &ControlId::TargetSeparation),
            Some(0.)
        );
        assert_eq!(value_on(&receiver, false, &ControlId::BirdMode), Some(3.));
        assert_eq!(value_on(&receiver, true, &ControlId::BirdMode), Some(0.));
    }

    /// The radar keeps interference rejection on as 1, whatever it was sent,
    /// and reports it once per range.
    #[tokio::test]
    async fn interference_rejection_reported_as_1_is_on() {
        let mut receiver = dual_range_receiver();

        receiver.process_report("$N67,0,0,1,1").unwrap();
        receiver.process_report("$N67,0,0,0,0").unwrap();

        let ir = ControlId::InterferenceRejection;
        assert_eq!(value_on(&receiver, true, &ir), Some(1.));
        assert_eq!(value_on(&receiver, false, &ir), Some(0.));
    }

    /// A rejection level above the control's highest is read as that
    /// highest level: the classic DRS reports interference rejection on as
    /// 2, which on its off/on control is on. The DRS4DL has three levels of
    /// noise rejection, so its 2 stays 2.
    #[tokio::test]
    async fn a_rejection_level_beyond_the_control_is_its_highest() {
        for (model, report, id, expected) in [
            (
                RadarModel::DRS,
                "$N67,0,0,2,0",
                ControlId::InterferenceRejection,
                1.,
            ),
            (
                RadarModel::DRS4DL,
                "$N67,0,3,2,0",
                ControlId::NoiseRejection,
                2.,
            ),
        ] {
            let radars = SharedRadars::new();
            let args = Cli::parse_from(["mayara-server"]);
            let info = model_info(&radars, &args, None, model);
            let mut receiver = FurunoReportReceiver::new(&args, radars, info, model);

            receiver.process_report(report).unwrap();

            assert_eq!(value_on(&receiver, false, &id), Some(expected), "{model}");
        }
    }

    /// The keep-alive frame the radar sends every 30 s has no line ending of
    /// its own, so it arrives in front of the next report.
    #[tokio::test]
    async fn a_report_behind_the_keep_alive_is_read() {
        let mut receiver = nxt_receiver();

        receiver
            .process_report(&format!("{KEEP_ALIVE}$N86,1"))
            .unwrap();

        assert_eq!(
            value_on(&receiver, false, &ControlId::MiddleStcCurve),
            Some(1.)
        );
    }

    /// `$NF5` subs 3 and 4 are the 12 V and 5 V rails in hundredths of a
    /// volt; TimeZero showed 12.06V and 5.04V for a DRS2D-NXT.
    #[tokio::test]
    async fn the_supply_rails_are_read_in_hundredths_of_a_volt() {
        let mut receiver = nxt_receiver();
        let volts = |receiver: &FurunoReportReceiver, id: &ControlId| {
            receiver.common.info.controls.get(id).unwrap().value
        };

        receiver.process_report("$NF5,3,1206,0,0,0").unwrap();
        receiver.process_report("$NF5,4,504,0,0,0").unwrap();

        assert_eq!(volts(&receiver, &ControlId::Supply12Voltage), Some(12.06));
        assert_eq!(volts(&receiver, &ControlId::Supply5Voltage), Some(5.04));
    }

    /// A report that should carry a number but does not must be refused, not
    /// indexed. `$N8E` and `$N8F` arrive bare, and mayara solicits them.
    #[test]
    fn a_report_without_its_number_is_an_error() {
        assert_eq!(first_number(&[42.0], "OnTime").unwrap(), 42.0);
        assert!(first_number(&[], "OnTime").is_err());
        assert!(first_number(&[], "TxTime").is_err());
    }

    /// `$N96` names the model in its first entry. Anything else names none.
    #[test]
    fn a_module_list_names_a_model_only_when_it_can() {
        let (model, version) =
            model_and_version(&["0359360-01.05", "0359358-01.01"]).expect("a DRS4D-NXT part");
        assert_eq!(model, RadarModel::DRS4DNXT);
        assert_eq!(version, "01.05");

        // An unknown part number is still a model/version split.
        let (model, version) = model_and_version(&["9999999-02.00"]).expect("a split");
        assert_eq!(model, RadarModel::Unknown);
        assert_eq!(version, "02.00");

        // A bare `$N96`, or an entry that is not a part number at all.
        assert!(model_and_version(&[]).is_none());
        assert!(model_and_version(&["0359360"]).is_none());
    }

    /// A radar that keeps dropping the control session must be backed off
    /// exponentially: hammering relogins exhausts the firmware's session slot
    /// table and locks out every client, which is the failure this policy
    /// exists to avoid.
    #[test]
    fn reconnect_backoff_escalates_while_sessions_are_short_lived() {
        let short = Duration::from_secs(1);
        let (b1, n1) = next_reconnect_backoff(RECONNECT_BACKOFF_MIN, short, 0);
        assert_eq!(b1, Duration::from_secs(2));
        assert_eq!(n1, 1);

        let (b2, n2) = next_reconnect_backoff(b1, short, n1);
        assert_eq!(b2, Duration::from_secs(4));
        assert_eq!(n2, 2);
    }

    #[test]
    fn reconnect_backoff_is_capped() {
        let short = Duration::from_secs(1);
        let (capped, _) = next_reconnect_backoff(RECONNECT_BACKOFF_MAX, short, 9);
        assert_eq!(capped, RECONNECT_BACKOFF_MAX);
    }

    /// A radar that refuses the login, or whose sockets will not open, never
    /// reaches the data loop. That path must still back off — retrying a
    /// refusing radar every second is exactly the churn that exhausts the
    /// firmware's session slots.
    #[test]
    fn reconnect_backoff_escalates_when_setup_never_reaches_the_data_loop() {
        let mut backoff = RECONNECT_BACKOFF_MIN;
        let mut count = 0;
        for expected in [2u64, 4, 8, 16] {
            // Duration::ZERO is how the loop reports "no session happened".
            (backoff, count) = next_reconnect_backoff(backoff, Duration::ZERO, count);
            assert_eq!(backoff, Duration::from_secs(expected));
        }
        // And it is still bounded.
        for _ in 0..10 {
            (backoff, count) = next_reconnect_backoff(backoff, Duration::ZERO, count);
        }
        assert_eq!(backoff, RECONNECT_BACKOFF_MAX);
    }

    /// Once a session has proven stable the next blip should reconnect
    /// immediately rather than inherit a minute-long delay.
    #[test]
    fn reconnect_backoff_resets_after_a_stable_session() {
        let (reset, count) =
            next_reconnect_backoff(Duration::from_secs(32), RECONNECT_STABLE_AFTER, 5);
        assert_eq!(reset, RECONNECT_BACKOFF_MIN);
        assert_eq!(count, 0, "a stable session clears the churn counter");
    }

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
