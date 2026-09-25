use anyhow::{Error, bail};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::Cli;
use crate::brand::raymarine::RaymarineModel;
use crate::network;
use crate::radar::range::Ranges;
use crate::radar::settings::{ControlId, ControlValue};
use crate::radar::{
    BYTE_LOOKUP_LENGTH, CommonRadar, Legend, Power, RadarError, RadarInfo, SharedRadars,
};
use crate::replay::RadarSocket;

// use super::command::Command;
use super::command::Command;
use super::{BaseModel, ExternalControllerWitness};

mod quantum;
mod rd;

// The radar drops the connection after ~60 seconds without a heartbeat.
// Send the 1-second keep-alive every second, and the 5-second extended
// keep-alive every 5th cycle.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);

// WiFi wake nudge (`10 00 28 00 00 00 00 00`, == Power::Standby) — see
// research/raymarine/radar-wakeup-analysis.md path A. Sent unicast to the
// radar's command socket while we have a Quantum discovered but no status
// report has arrived yet, on WAKE_INTERVAL cadence. Three gates keep us
// from fighting another controller: base_model == Quantum (only family that
// uses this id), state == Initial (closes the instant the first status
// report arrives, so we never push a transmitting radar to standby), and
// external_seen quiet for EXTERNAL_QUIET_WINDOW (closes the instant an MFD
// / another mayara announces itself on the beacon group). The initial
// OBSERVATION_WINDOW delay gives external controllers a chance to be heard
// before our first nudge.
const WAKE_INTERVAL: Duration = Duration::from_secs(3);
const OBSERVATION_WINDOW: Duration = Duration::from_secs(5);
const EXTERNAL_QUIET_WINDOW: Duration = Duration::from_secs(60);

/// How many Quantum status reports to let past while waiting for the 0x280007
/// features report before showing the radar anyway. They arrive about once a
/// second.
const MAX_STATUS_REPORTS_WITHOUT_FEATURES: u8 = 3;

/// How long to wait for the features report regardless of how many status
/// reports have arrived. Counting reports alone would wait indefinitely on a
/// radar whose reports trickle in.
const MAX_WAIT_FOR_FEATURES: Duration = Duration::from_secs(5);

/// Whether a radar's ranges — and so the radar itself — should be withheld for
/// now, waiting on the features report.
///
/// Only a Quantum waits at all, and only until either enough status reports or
/// enough time has passed: no radar may become undiscoverable over a report we
/// only assume it sends.
fn should_hold_for_features(
    base_model: BaseModel,
    features_seen: bool,
    status_reports_without_features: u8,
    waited: Duration,
) -> bool {
    base_model == BaseModel::Quantum
        && !features_seen
        && status_reports_without_features <= MAX_STATUS_REPORTS_WITHOUT_FEATURES
        && waited < MAX_WAIT_FOR_FEATURES
}

/// The keep-alives one heartbeat tick sends: the 1 s heartbeat, and every
/// fifth tick the extended 5 s one. Nothing while the radar should stand
/// down: the heartbeat is what holds the radar up for us, and the radar
/// drops a controller it has not heard from for ~60 s. `counter` is the
/// number of heartbeats sent so far.
fn heartbeats_for_tick(stand_down: bool, counter: u32) -> (bool, bool) {
    if stand_down {
        return (false, false);
    }
    (true, (counter + 1).is_multiple_of(5))
}

/// Decide how a Power request affects the "re-apply transmit once the radar
/// reaches standby" flag, given the radar's last reported power state.
/// Returns `Some(new_flag)` when the request changes it, `None` to leave it.
///
/// A cold Quantum drops a transmit command while it boots (~25-30 s), so a
/// transmit requested while it is off/unknown must be re-applied once it
/// reports standby. Standby->transmit is honoured immediately (no deferral);
/// a standby/off request cancels any pending transmit.
fn transmit_should_defer(requested: Power, reported: Option<Power>) -> Option<bool> {
    match requested {
        Power::Transmit => {
            let awake = matches!(reported, Some(Power::Standby) | Some(Power::Transmit));
            Some(!awake)
        }
        Power::Standby | Power::Off => Some(false),
        _ => None,
    }
}

/// Whether to re-send the transmit command now: only when a transmit is
/// pending and the radar has just reported it reached Standby (i.e. it has
/// finished booting and will now accept the mode change).
fn should_reapply_transmit(pending: bool, reported: Option<Power>) -> bool {
    pending && reported == Some(Power::Standby)
}

// The LookupSpokeEnum is an index into an array, really. `process_frame`
// picks the row based on whether the radar currently reports Doppler on
// (see `process_doppler_report`).
pub(super) enum LookupDoppler {
    Normal = 0,
    Doppler = 1,
}
const LOOKUP_DOPPLER_LENGTH: usize = (LookupDoppler::Doppler as usize) + 1;

type WireToLegendTable = [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

pub(super) fn wire_to_legend(legend: &Legend) -> WireToLegendTable {
    let mut lookup: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH] =
        [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

    let doppler_approaching = legend.doppler_approaching.map(|(s, _)| s).unwrap_or(0);
    let doppler_receding = legend.doppler_receding.map(|(s, _)| s).unwrap_or(0);

    // `LOOKUP_DOPPLER_LENGTH == 2`, so the array splits cleanly into the
    // two parallel sub-tables (Normal | Doppler) we want to populate.
    let [normal, doppler] = &mut lookup;
    if legend.pixel_colors >= 128 {
        for (j, (n, d)) in normal.iter_mut().zip(doppler.iter_mut()).enumerate() {
            *n = j as u8 / 2;
            *d = match j {
                0xff => doppler_approaching,
                0xfe => doppler_receding,
                _ => j as u8 / 2,
            };
        }
    } else {
        for (j, (n, d)) in normal.iter_mut().zip(doppler.iter_mut()).enumerate() {
            *n = j as u8;
            *d = match j {
                0xff => doppler_approaching,
                0xfe => doppler_receding,
                _ => j as u8,
            };
        }
    }
    log::debug!("Created wire_to_legend from legend {:?}", legend);
    lookup
}

#[derive(PartialEq, PartialOrd, Debug)]
enum ReceiverState {
    Initial,
    InfoRequestReceived,
    FixedRequestReceived,
    StatusRequestReceived,
}

/// Feature flags from the 0x280007 Features message. The radar
/// broadcasts this once after connection; it tells us what the
/// hardware actually supports.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FeatureFlags {
    pub(super) raw: u32,
}

#[allow(dead_code)]
impl FeatureFlags {
    fn has_flag(&self, mask: u32) -> bool {
        (self.raw & mask) != 0
    }
    pub(crate) fn is_quantum(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_QUANTUM)
    }
    pub(crate) fn is_dual_range_scanner(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_DUAL_RANGE_SCANNER)
    }
    pub(crate) fn has_sector_blanking(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_SECTOR_BLANKING)
    }
    pub(crate) fn has_marpa_beyond_12nm(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_MARPA_BEYOND_12NM)
    }
    pub(crate) fn has_96nm_range(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_96NM_RANGE)
    }
    pub(crate) fn has_parameters_message(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_PARAMETERS_MESSAGE)
    }
    pub(crate) fn is_cyclone(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_CYCLONE)
    }
    pub(crate) fn has_doppler(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_DOPPLER)
    }
    pub(crate) fn has_doppler_auto_acquire(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_DOPPLER_AUTO_ACQUIRE)
    }
    pub(crate) fn has_doppler_bird_mode(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_DOPPLER_BIRD_MODE)
    }
    pub(crate) fn has_bird_mode(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_BIRD_MODE)
    }
    pub(crate) fn has_auto_rain(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_AUTO_RAIN)
    }
    pub(crate) fn has_marpa(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_MARPA)
    }
    pub(crate) fn has_dual_range_marpa(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_DUAL_RANGE_MARPA)
    }
    pub(crate) fn is_analogue(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_ANALOGUE)
    }
    pub(crate) fn is_digital(&self) -> bool {
        self.has_flag(super::protocol::FEATURE_DIGITAL)
    }
}

pub(crate) struct RaymarineReportReceiver {
    common: CommonRadar,
    unicast_mode: bool,
    report_socket: Option<RadarSocket>,
    // When the radar streams unicast back to the command source port (MFD
    // acting as WiFi AP, report address unspecified), this is the single
    // connected socket shared with the command sender. The report loop
    // listens on it; `None` for the normal multicast topology.
    unicast_socket: Option<Arc<UdpSocket>>,
    state: ReceiverState,
    model: Option<RaymarineModel>,
    base_model: BaseModel,
    command_sender: Option<Command>,
    heartbeat_deadline: Instant,
    heartbeat_counter: u32,
    /// Whether the last heartbeat tick left the keep-alives out, so the
    /// transition is logged once rather than every second.
    stood_down: bool,
    wake_deadline: Instant,
    external_seen: Arc<ExternalControllerWitness>,
    reported_unknown: HashMap<u32, bool>,
    features: FeatureFlags,
    features_seen: bool,
    status_reports_without_features: u8,
    first_status_without_features: Option<Instant>,
    /// True while the radar's last status report indicated a self-test fault
    /// (Quantum status byte 0x0A). Used to edge-trigger the Signal K alarm
    /// so it raises once on entry and clears once on exit.
    pub(super) self_test_fault: bool,
    /// Most recent per-item Quantum self-test results (24 items, wire
    /// id `0x28080a`). `None` until the first packet arrives — captured
    /// so transitions can be edge-logged instead of spamming once per
    /// broadcast.
    pub(super) self_test_results: Option<[u8; quantum::SELF_TEST_ITEM_COUNT]>,

    /// True when the user asked for Transmit while the radar was not yet
    /// awake/transmitting. A cold Quantum takes ~25-30 s to boot to Standby
    /// before it accepts a mode change, so the transmit command sent alongside
    /// the wake is lost. We re-send it once the radar reports Standby, then
    /// clear this — so off->transmit is a single user action. Cleared if the
    /// user asks for Standby/Off in the meantime.
    pub(super) pending_transmit: bool,

    // For data (spokes)
    range_meters: u32,
    wire_to_legend: WireToLegendTable,
    // Tracks the radar's current Doppler state, fed by `process_doppler_report`.
    // Selects the row in `wire_to_legend`: when off, byte 0xFE/0xFF stay raw;
    // when on, they are remapped to the doppler-receding / -approaching markers.
    doppler: bool,
}

impl RaymarineReportReceiver {
    pub(crate) fn new(
        args: &Cli,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
        base_model: BaseModel,
        external_seen: Arc<ExternalControllerWitness>,
    ) -> RaymarineReportReceiver {
        let key = info.key();

        let replay = args.is_replay();
        log::debug!(
            "{}: Creating RaymarineReportReceiver with args {:?}",
            key,
            args
        );

        // Quantum WiFi:
        // When the radar advertised no report multicast group, the locator
        // set report_addr to an unspecified unicast address (MFD-as-WiFi-AP
        // topology). The radar streams reports/spokes unicast back to the
        // command source port, so a single connected socket must both send
        // commands and receive replies — otherwise a separate connected
        // command socket wins delivery of the replies and starves the listen
        // socket (same host:port). start_report_socket() creates that shared
        // socket (with retry) and hands it to the command sender.
        let unicast_mode = !replay && !info.report_addr.ip().is_multicast();

        // Quantum wired radars (and some RD variants) won't send the
        // 0x280001 info-report until they have received a host keep-alive
        // first — see issue #228. Create the command_sender now with the
        // base model the locator already determined, so the heartbeat loop
        // starts priming the radar immediately. process_info_report() will
        // not overwrite this once the radar replies.
        //
        // In the unicast topology the command sender must never open its own
        // socket (that is the collision this fixes); it stays socketless until
        // start_report_socket() supplies the shared one. The normal multicast
        // topology lets it open its own socket as usual.
        let command_sender = if !replay {
            Some(Command::new(info.clone(), base_model, unicast_mode))
        } else {
            None
        };

        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();

        let wire_to_legend = wire_to_legend(&info.get_legend());

        let mut common =
            CommonRadar::new(args, key, info, radars, control_update_rx, replay, blob_tx);

        // Coalesce roughly 1/32 of a revolution of spokes into each
        // broadcast — Quantum (250 spokes/rev) → batches of 8, RD/HD
        // (2048 spokes/rev) → batches of 64. Both brands emit one or a
        // handful of spokes per UDP frame, so without batching every
        // spoke pays a full compression / WebSocket-framing cycle.
        // `add_spoke` force-flushes on revolution wrap or range change,
        // so trailing partial batches still ship as-is.
        let target = common.info.spokes_per_revolution.div_ceil(32) as usize;
        common.set_spoke_batch_threshold(target);

        let now = Instant::now();
        RaymarineReportReceiver {
            common,
            unicast_mode,
            report_socket: None,
            unicast_socket: None,
            state: ReceiverState::Initial,
            model: None, // We don't know this yet, it will be set when we receive the first info report
            base_model,
            command_sender,
            heartbeat_deadline: now + HEARTBEAT_INTERVAL,
            heartbeat_counter: 0,
            stood_down: false,
            wake_deadline: now + OBSERVATION_WINDOW,
            external_seen,
            reported_unknown: HashMap::new(),
            features: FeatureFlags::default(),
            features_seen: false,
            status_reports_without_features: 0,
            first_status_without_features: None,
            self_test_fault: false,
            self_test_results: None,
            pending_transmit: false,
            range_meters: 0,
            wire_to_legend,
            doppler: false,
        }
    }

    async fn start_report_socket(&mut self) {
        // Unicast topology (report address non-multicast): the report loop and
        // the command sender share one connected socket. Create it on demand
        // here — this is the receiver's retry point — and hand a clone to the
        // command sender so it stops dropping commands.
        if self.unicast_mode {
            if self.unicast_socket.is_none() {
                match network::create_connected_unicast(
                    &self.common.info.nic_addr,
                    self.common.info.report_addr.port(),
                    &self.common.info.send_command_addr,
                ) {
                    Ok(sock) => {
                        // Commands/replies on this socket may cross an Axiom
                        // relay to a WiFi radar, which drops TTL-1 packets
                        // (issue #160). Match the wake burst's TTL.
                        if let Err(e) = sock.set_ttl(super::RAYMARINE_RELAY_TTL) {
                            log::warn!("{}: unicast socket TTL: {}", self.common.key, e);
                        }
                        let sock = Arc::new(sock);
                        if let Some(cs) = &mut self.command_sender {
                            cs.set_shared_socket(sock.clone());
                        }
                        self.unicast_socket = Some(sock);
                    }
                    Err(e) => {
                        // Leave report_socket None so run() retries (it
                        // owns the back-off and observes shutdown while
                        // sleeping); never fall back to a separate command
                        // socket (collision).
                        log::debug!(
                            "{}: {} via {}: unicast report socket failed: {}",
                            self.common.key,
                            self.common.info.report_addr,
                            self.common.info.nic_addr,
                            e
                        );
                        return;
                    }
                }
            }
            let sock = self.unicast_socket.as_ref().unwrap().clone();
            self.report_socket = Some(RadarSocket::SharedUdp(sock));
            log::debug!(
                "{}: {} via {}: listening for unicast reports",
                self.common.key,
                self.common.info.report_addr,
                self.common.info.nic_addr
            );
        } else {
            // Multicast mode
            match network::create_udp_listen(
                &self.common.info.report_addr,
                &self.common.info.nic_addr,
                network::SocketType::Any,
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
                    // Back-off and shutdown observation live in run().
                    log::debug!(
                        "{}: {} via {}: create UDP listen socket failed: {}",
                        self.common.key,
                        self.common.info.report_addr,
                        self.common.info.nic_addr,
                        e
                    );
                }
            }
        }
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.common.info.command_tx.
    //
    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.common.key);
        let mut buf = Vec::with_capacity(10000);

        loop {
            let heartbeat_deadline = self.heartbeat_deadline;
            let wake_deadline = self.wake_deadline;
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
                },
                _ = sleep_until(heartbeat_deadline) => {
                    // A heartbeat send failure must not tear down the report
                    // loop. With the unicast (MFD-as-AP) topology the command
                    // socket is connected to the radar, so a send can fail with
                    // a routing error (e.g. the radar briefly unreachable);
                    // killing the loop here would drop the report socket and
                    // busy-respin once per second, never recovering. Log and
                    // keep listening; the next heartbeat retries.
                    if let Err(e) = self.send_heartbeat().await {
                        log::debug!("{}: heartbeat send failed: {}", self.common.key, e);
                    }
                },
                _ = sleep_until(wake_deadline) => {
                    self.maybe_send_wake_nudge().await?;
                },

                r = self.report_socket.as_mut().unwrap().recv_buf_from(&mut buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            self.common.info.mark_input();
                            if buf.len() == buf.capacity() {
                                let old = buf.capacity();
                                buf.reserve(1024);
                                log::warn!("{}: UDP report buffer full, increasing size {} -> {}", self.common.key, old, buf.capacity()   );
                            }
                            else if let Err(e) = self.process_report(&buf).await {
                                log::error!("{}: {}", self.common.key, e);
                            }
                            buf.clear();
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
                            self.note_power_request(&cv.control_value);
                            let _ = self.common.process_control_update(cv, &mut self.command_sender).await;
                        },
                    }
                }
            }
        }
    }

    /// Observe a Power control request so a Transmit asked for while the radar
    /// is not yet transmitting can be re-applied once it reports Standby (see
    /// `pending_transmit`). A Standby/Off request clears any pending transmit.
    fn note_power_request(&mut self, cv: &ControlValue) {
        if cv.id != ControlId::Power {
            return;
        }
        let Some(power) = cv.value.as_ref().and_then(|v| Power::from_value(v).ok()) else {
            return;
        };
        if let Some(pending) = transmit_should_defer(power, self.common.info.controls.get_status())
        {
            self.pending_transmit = pending;
        }
    }

    /// If the user asked for Transmit while the radar was waking, re-send the
    /// transmit command once it reports Standby (it ignored the first one while
    /// booting). One shot: the flag is cleared whether or not this send
    /// succeeds — if it is lost too, the user can ask again.
    async fn maybe_reapply_transmit(&mut self) {
        if !should_reapply_transmit(
            self.pending_transmit,
            self.common.info.controls.get_status(),
        ) {
            return;
        }
        self.pending_transmit = false;
        if let Some(ref mut cs) = self.command_sender {
            log::info!(
                "{}: radar reached standby, re-applying transmit",
                self.common.key
            );
            let _ = cs.send(&super::protocol::SET_TRANSMIT_QUANTUM).await;
        }
    }

    /// Fire of the wake-nudge timer. Sends the WiFi wake (`10 00 28 00 00…`)
    /// unicast to the radar's command socket only if:
    ///   - this is a Quantum (the RD family does not use this command id),
    ///   - no status report has ever been received (state == Initial),
    ///   - no external Raymarine controller has been observed within
    ///     EXTERNAL_QUIET_WINDOW — see [`ExternalControllerWitness`].
    ///
    /// Both base_model and the receiver state are monotonic, so once either
    /// disqualifies us we silence the arm for the receiver's lifetime; the
    /// witness gate can re-open and stays on the WAKE_INTERVAL cadence.
    async fn maybe_send_wake_nudge(&mut self) -> Result<(), RadarError> {
        if self.base_model != BaseModel::Quantum || self.state != ReceiverState::Initial {
            self.wake_deadline = Instant::now() + Duration::from_secs(3600);
            return Ok(());
        }
        self.wake_deadline += WAKE_INTERVAL;
        if !self.external_seen.quiet_for(EXTERNAL_QUIET_WINDOW) {
            log::debug!(
                "{}: WiFi wake nudge suppressed — external controller seen recently",
                self.common.key
            );
            return Ok(());
        }
        if let Some(ref mut cs) = self.command_sender {
            log::info!("{}: sending WiFi wake nudge", self.common.key);
            cs.send(&super::protocol::WAKE_WIFI).await?;
        }
        Ok(())
    }

    async fn send_heartbeat(&mut self) -> Result<(), RadarError> {
        // Advance the deadline first so a send failure below can't leave it in
        // the past — otherwise the caller's sleep_until fires immediately and
        // busy-loops. The next tick retries the heartbeat a second later.
        self.heartbeat_deadline += HEARTBEAT_INTERVAL;
        if let Some(ref mut cs) = self.command_sender {
            // An MFD using the radar keeps it up with its own heartbeat, so
            // standing down needs no "am I the only controller" check.
            let stand_down = self.common.info.stand_down();
            if stand_down != self.stood_down {
                self.stood_down = stand_down;
                if stand_down {
                    log::info!(
                        "{}: nobody watching, dropping heartbeat so the radar can stand down",
                        self.common.key
                    );
                } else {
                    log::info!("{}: client connected, resuming heartbeat", self.common.key);
                }
            }
            let (heartbeat, extended) = heartbeats_for_tick(stand_down, self.heartbeat_counter);
            if heartbeat {
                cs.send_heartbeat().await?;
                self.heartbeat_counter += 1;
            }
            if extended {
                cs.send_heartbeat_5s().await?;
            }
        }
        Ok(())
    }

    pub(super) async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), RadarError> {
        loop {
            self.start_report_socket().await;
            if self.report_socket.is_none() {
                // Bind keeps failing: back off, but race the sleep against
                // a shutdown request so a graceful shutdown can't hang on
                // a long network outage.
                tokio::select! {
                    _ = sleep(Duration::from_millis(1000)) => continue,
                    _ = subsys.on_shutdown_requested() => return Ok(()),
                }
            }
            match self.socket_loop(subsys).await {
                Err(RadarError::Shutdown) => return Ok(()),
                _ => self.report_socket = None,
            }
        }
    }

    async fn process_report(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < 4 {
            bail!("UDP report len {} dropped", data.len());
        }
        log::trace!("{}: UDP report {:02X?}", self.common.key, data);

        let id = u32::from_le_bytes(data[0..4].try_into().unwrap());
        match id {
            // RD (magnetron) messages
            0x010001 | 0x018801 => {
                rd::process_status_report(self, data);
            }
            0x010002 => {
                rd::process_fixed_report(self, data);
            }
            0x010003 => {
                rd::process_frame(self, data);
            }
            0x010006 => {
                rd::process_info_report(self, data);
            }
            0x018701 => {
                rd::process_hd_info_report(self, data);
            }
            0x018942 => {
                // Database report (HD counterpart of Quantum 0x288942) — a
                // static cyclic table flooded ~25/s in standby. Ignore.
                log::trace!("{}: RD database report len={}", self.common.key, data.len());
            }
            // Quantum messages
            0x280001 => {
                quantum::process_info_report(self, data);
            }
            0x280002 => {
                quantum::process_status_report(self, data);
                self.maybe_reapply_transmit().await;
            }
            0x280003 => {
                quantum::process_frame(self, data);
            }
            0x288942 => {
                // Database report — not spoke data. Ignore.
                log::trace!(
                    "{}: Quantum database report len={}",
                    self.common.key,
                    data.len()
                );
            }
            0x280005 => {
                log::trace!("{}: Quantum radar mode report", self.common.key);
            }
            0x280006 => {
                log::trace!("{}: Quantum signal strength report", self.common.key);
            }
            0x280007 => {
                self.process_features(data);
            }
            0x280008 => {
                log::trace!("{}: Quantum parameters report", self.common.key);
            }
            0x280030 => {
                quantum::process_doppler_report(self, data);
            }
            // SelfTestResults — radar pushes 24 per-item results unsolicited;
            // see research/raymarine/radar-error-reporting.md.
            quantum::SELF_TEST_RESULTS_ID => {
                quantum::process_self_test_results(self, data);
            }
            // Guard zone, alarm, MARPA, etc. — logged but not acted on
            id if (id & 0xFFFF0000 == 0x00280000 || id & 0xFFFF0000 == 0x00010000) => {
                if !self.reported_unknown.contains_key(&id) {
                    log::debug!(
                        "{}: Unhandled report ID 0x{:08X} len={}",
                        self.common.key,
                        id,
                        data.len()
                    );
                    self.reported_unknown.insert(id, true);
                }
            }
            _ => {
                if !self.reported_unknown.contains_key(&id) {
                    log::debug!("{}: Unknown report ID 0x{:08X}", self.common.key, id);
                    self.reported_unknown.insert(id, true);
                }
            }
        }
        Ok(())
    }

    fn process_features(&mut self, data: &[u8]) {
        if data.len() < 8 {
            return;
        }
        let flags = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let features = FeatureFlags { raw: flags };

        if !self.features_seen {
            log::info!(
                "{}: Features: quantum={} cyclone={} dual_range={} doppler={} \
                 bird_mode={} marpa={} auto_rain={} sector_blanking={} \
                 range_96nm={} (raw=0x{:08x})",
                self.common.key,
                features.is_quantum(),
                features.is_cyclone(),
                features.is_dual_range_scanner(),
                features.has_doppler(),
                features.has_bird_mode(),
                features.has_marpa(),
                features.has_auto_rain(),
                features.has_sector_blanking(),
                features.has_96nm_range(),
                flags,
            );

            // The radar's own word on Doppler beats the part-number table,
            // and process_info_report() honours that rather than overwriting
            // it afterwards (#709).
            //
            // Unless the radar has already been published, which only happens
            // when the wait for this very report was given up on. The control
            // set was decided from the table at that point, and changing the
            // capability now would leave the two disagreeing — a Doppler switch
            // on a radar that says it has none, or the reverse. So the table's
            // answer stands for the session and this report is noted, not
            // applied.
            if !self.common.info.ranges.is_empty()
                && features.has_doppler() != self.common.info.doppler
            {
                log::warn!(
                    "{}: features report arrived after the radar was published; \
                     keeping doppler={} from the part number rather than {} from \
                     the radar, so the capability and the controls agree",
                    self.common.key,
                    self.common.info.doppler,
                    features.has_doppler(),
                );
            } else if features.has_doppler() != self.common.info.doppler {
                self.common.info.set_doppler(features.has_doppler());
                self.wire_to_legend = wire_to_legend(&self.common.info.get_legend());
                log::info!(
                    "{}: Doppler capability updated to {}",
                    self.common.key,
                    features.has_doppler(),
                );
            }
            // The control set is not touched here. It is decided once, where
            // the radar stops being held back for this very report, so by then
            // the capability is settled and nothing needs taking back.

            self.features = features;
            self.features_seen = true;
        }
    }

    /// Whether to keep this radar out of sight a little longer.
    ///
    /// A radar becomes visible to clients as soon as it has ranges, so setting
    /// them before the 0x280007 features report has arrived shows a control set
    /// that is about to change — a Q24D sends its first status report 18 ms
    /// before its features. Holding back costs at most one status report, since
    /// those repeat about once a second and carry the ranges every time.
    ///
    /// Two radars never wait. An RD has no features report at all and would
    /// wait for ever. Nor does a Quantum that simply stays quiet: no radar may
    /// become undiscoverable over a report we only assume it sends, which is
    /// the failure both #701 and #713 were about.
    fn hold_for_features(&mut self) -> bool {
        self.status_reports_without_features =
            self.status_reports_without_features.saturating_add(1);

        let waited = self
            .first_status_without_features
            .get_or_insert_with(Instant::now)
            .elapsed();

        let hold = should_hold_for_features(
            self.base_model,
            self.features_seen,
            self.status_reports_without_features,
            waited,
        );
        if !hold && !self.features_seen && self.base_model == BaseModel::Quantum {
            log::warn!(
                "{}: no 0x280007 features report after {} status reports and {:?}; \
                 showing the radar with capabilities from its part number instead",
                self.common.key,
                self.status_reports_without_features,
                waited,
            );
        }
        hold
    }

    fn set_ranges(&mut self, ranges: Ranges) {
        if let Some(command_sender) = &mut self.command_sender {
            command_sender.set_ranges(ranges.clone());
        }
        self.common.set_ranges(ranges);
    }
}

#[cfg(test)]
mod tests {
    use super::{heartbeats_for_tick, should_reapply_transmit, transmit_should_defer};
    use crate::radar::Power;

    // ----- feature bits (0x280007) -----

    /// The two feature words we have seen on the wire must decode to what the
    /// radars actually are. Bit 4 was long read as "is a Quantum", which made
    /// `is_quantum()` false on every Quantum; it is `IsDualRangeScanner`, and
    /// the real flag is bit 12. Bit numbers come from the Axiom's own
    /// accessors — see research/raymarine/quantum-generation-detection.md.
    #[test]
    fn the_captured_feature_words_decode_to_their_radars() {
        use super::FeatureFlags;

        // Quantum Q24C (E70210), firmware v1.62 — MarineYachtRadar#701.
        let q24c = FeatureFlags { raw: 0x0000_1900 };
        assert!(q24c.is_quantum(), "a Q24C is a Quantum scanner");
        assert!(!q24c.has_doppler(), "a Q24C has no Doppler");
        assert!(!q24c.is_dual_range_scanner(), "no Quantum sets this bit");
        assert!(!q24c.is_cyclone());
        assert!(!q24c.has_marpa());
        assert!(!q24c.has_sector_blanking());

        // Quantum 2 Doppler Q24D (E70498) — pelagia captures.
        let q24d = FeatureFlags { raw: 0x004a_7900 };
        assert!(q24d.is_quantum(), "a Q24D is a Quantum scanner too");
        assert!(q24d.has_doppler());
        assert!(q24d.has_marpa());
        assert!(q24d.has_marpa_beyond_12nm());
        assert!(q24d.has_sector_blanking());
        assert!(q24d.has_parameters_message());
        // The radar does not claim the bit. Whether the product supports dual
        // range in some other sense is a separate question — see the constant.
        assert!(!q24d.is_dual_range_scanner(), "no Quantum sets this bit");
        assert!(!q24d.is_cyclone());
        // The Doppler extras are Cyclone-only and must not read as present.
        assert!(!q24d.has_doppler_auto_acquire());
        assert!(!q24d.has_doppler_bird_mode());
    }

    // ----- a features report that arrives after the radar was published -----

    /// Build a receiver without touching the network. `--replay` keeps the
    /// constructor from creating a command sender, and so from opening sockets.
    fn test_receiver(doppler: bool) -> super::RaymarineReportReceiver {
        use crate::Cli;
        use crate::brand::raymarine::{BaseModel, ExternalControllerWitness, settings};
        use crate::radar::SharedRadars;
        use clap::Parser;
        use std::sync::Arc;

        let args = Cli::parse_from(["mayara-server", "--replay"]);
        let mut info =
            crate::radar::ui_strings::radar_info(crate::Brand::Raymarine, &args, |id, tx| {
                settings::new(id, tx, &args, BaseModel::Quantum)
            });
        info.doppler = doppler;
        super::RaymarineReportReceiver::new(
            &args,
            info,
            SharedRadars::new(),
            BaseModel::Quantum,
            Arc::new(ExternalControllerWitness::default()),
        )
    }

    fn features_report(doppler: bool) -> [u8; 8] {
        let flags: u32 = if doppler {
            crate::brand::raymarine::protocol::FEATURE_DOPPLER
        } else {
            0
        };
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&0x0028_0007u32.to_le_bytes());
        data[4..8].copy_from_slice(&flags.to_le_bytes());
        data
    }

    /// Before the radar is published the radar's own word wins, table or no
    /// table — the path every radar we have captured takes.
    #[test]
    fn a_features_report_before_publication_decides_the_capability() {
        let mut receiver = test_receiver(/*doppler=*/ true);
        assert!(receiver.common.info.ranges.is_empty(), "not published yet");

        receiver.process_features(&features_report(false));

        assert!(
            !receiver.common.info.doppler,
            "the radar says it has no Doppler and is believed over the part number"
        );
    }

    /// After the radar is published the part number's answer is frozen, because
    /// the control set was built from it and the two must not disagree. Only
    /// reachable when the wait for the features report was given up on.
    #[test]
    fn a_features_report_after_publication_does_not_move_the_capability() {
        use crate::radar::range::{Range, Ranges};

        use crate::brand::raymarine::{BaseModel, settings};
        use crate::radar::settings::ControlId;

        let mut receiver = test_receiver(/*doppler=*/ true);
        // Publishing a radar means offering its controls and giving it ranges,
        // which is what process_status_report does at the release point.
        settings::offer_doppler_control(
            &mut receiver.common.info.controls,
            BaseModel::Quantum,
            true,
        );
        receiver
            .common
            .set_ranges(Ranges::new(vec![Range::new(1852, 0)]));
        assert!(!receiver.common.info.ranges.is_empty(), "published");
        assert!(
            receiver
                .common
                .info
                .controls
                .get(&ControlId::Doppler)
                .is_some()
        );

        receiver.process_features(&features_report(false));

        // The point of freezing: the two cannot end up disagreeing.
        assert!(
            receiver.common.info.doppler,
            "a late features report must not move the capability"
        );
        assert!(
            receiver
                .common
                .info
                .controls
                .get(&ControlId::Doppler)
                .is_some(),
            "...nor leave the control set it was built from without its control"
        );
    }

    fn info_report(part: &str) -> Vec<u8> {
        let mut data = vec![0u8; 17];
        data[0..4].copy_from_slice(&0x0028_0001u32.to_le_bytes());
        data[4..10].copy_from_slice(part.as_bytes());
        data[10..17].copy_from_slice(b"1140360");
        data
    }

    /// A features report and the part-number table that disagree, driven all the
    /// way through the receiver rather than through `effective_doppler` alone.
    /// Both replay fixtures have the two agreeing, so only this can show which
    /// one survives the whole path — capability and legend together. See #709.
    #[test]
    fn a_conflicting_features_report_still_beats_the_part_number() {
        for (part, table_says, radar_says) in [
            // E70498 is a Q24D: the table claims Doppler, the radar denies it.
            ("E70498", true, false),
            // E70210 is a Q24C: the table denies it, the radar claims it.
            ("E70210", false, true),
        ] {
            let mut receiver = test_receiver(/*doppler=*/ false);
            receiver.process_features(&features_report(radar_says));
            assert_eq!(
                receiver.common.info.doppler, radar_says,
                "{part}: from the radar"
            );

            super::quantum::process_info_report(&mut receiver, &info_report(part));

            assert_eq!(
                receiver.common.info.doppler, radar_says,
                "{part}: the table says {table_says}, the radar says {radar_says}, \
                 and the radar wins"
            );
            let legend = receiver.common.info.get_legend();
            assert_eq!(
                legend.doppler_approaching.is_some(),
                radar_says,
                "{part}: the legend must follow the capability the radar reported"
            );
        }
    }

    /// A real 0x280002 status report from `raymarine-quantum.pcap.gz`, which
    /// carries a usable range table so that processing it publishes the radar.
    const STATUS_REPORT: [u8; 260] = [
        0x02, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x64, 0x01, 0x01, 0x01, 0x07, 0x03, 0x01, 0x41, 0x01, 0x28, 0x00, 0x00, 0x00, 0x00,
        0x01, 0x41, 0x01, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x41, 0x01, 0x28, 0x01, 0x22, 0x01,
        0x49, 0x01, 0x5A, 0x01, 0x32, 0x01, 0x00, 0x00, 0x00, 0x01, 0x01, 0x86, 0x00, 0x00, 0x01,
        0x09, 0x00, 0x01, 0x41, 0x01, 0x32, 0x01, 0x00, 0x00, 0x00, 0x01, 0x4B, 0x01, 0x32, 0x01,
        0x00, 0x00, 0x00, 0x01, 0x4B, 0x01, 0x32, 0x01, 0x00, 0x00, 0x00, 0x01, 0x5A, 0x01, 0x32,
        0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0xA4, 0x00, 0x00, 0x01, 0xFD, 0x01, 0x00, 0x00, 0x24,
        0x06, 0x00, 0x00, 0xB2, 0x07, 0x48, 0x04, 0x00, 0x00, 0x00, 0x00, 0xD0, 0x07, 0x00, 0x00,
        0xC4, 0x09, 0x00, 0x00, 0x96, 0x00, 0xD2, 0x00, 0x00, 0x00, 0x00, 0x00, 0xA4, 0x06, 0x80,
        0x07, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x1F, 0x00,
        0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x7D, 0x00, 0x00, 0x00, 0xFA, 0x00, 0x00, 0x00, 0x77,
        0x01, 0x00, 0x00, 0xF4, 0x01, 0x00, 0x00, 0xEE, 0x02, 0x00, 0x00, 0xE8, 0x03, 0x00, 0x00,
        0xDC, 0x05, 0x00, 0x00, 0xD0, 0x07, 0x00, 0x00, 0xB8, 0x0B, 0x00, 0x00, 0xA0, 0x0F, 0x00,
        0x00, 0x70, 0x17, 0x00, 0x00, 0x40, 0x1F, 0x00, 0x00, 0xE0, 0x2E, 0x00, 0x00, 0x80, 0x3E,
        0x00, 0x00, 0xC0, 0x5D, 0x00, 0x00, 0x00, 0x7D, 0x00, 0x00, 0x80, 0xBB, 0x00, 0x00, 0x00,
        0xFA, 0x00, 0x00, 0xFD, 0x01, 0x00, 0x00, 0x24, 0x06, 0x00, 0x00, 0xB2, 0x07, 0x48, 0x04,
        0x00, 0x00, 0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0xAF, 0x00, 0x00, 0x00, 0x08, 0x07, 0x08,
        0x07, 0x00, 0x00, 0x00, 0x00,
    ];

    fn doppler_status_report(on: bool) -> [u8; 5] {
        let mut data = [0u8; 5];
        data[0..4].copy_from_slice(&0x0028_0030u32.to_le_bytes());
        data[4] = if on { 0x03 } else { 0x00 };
        data
    }

    /// A Doppler status report can arrive while the radar is held back, when
    /// there is no control for it to land on. The state must still reach the
    /// control when one is created, rather than the radar appearing with
    /// Doppler reading Off until the next report.
    ///
    /// The replay fixtures cannot show this: `set_instant_timing()` collapses
    /// the gap, so a later 0x280030 sets the control either way.
    #[test]
    fn a_doppler_report_during_the_hold_reaches_the_control() {
        use crate::radar::settings::ControlId;

        let mut receiver = test_receiver(/*doppler=*/ false);
        // The radar has Doppler and says it is on, both while still held back.
        receiver.process_features(&features_report(true));
        super::quantum::process_info_report(&mut receiver, &info_report("E70498"));
        super::quantum::process_doppler_report(&mut receiver, &doppler_status_report(true));
        assert!(
            receiver
                .common
                .info
                .controls
                .get(&ControlId::Doppler)
                .is_none(),
            "no control yet: the radar has not been published"
        );

        // Publishing it is what creates the control.
        super::quantum::process_status_report(&mut receiver, &STATUS_REPORT);

        assert_eq!(
            receiver
                .common
                .info
                .controls
                .get(&ControlId::Doppler)
                .and_then(|c| c.value())
                .and_then(|v| v.as_f64()),
            Some(1.0),
            "the Doppler state reported during the hold must reach the control"
        );
    }

    // ----- holding a radar back until it says what it can do -----

    /// A radar is visible to clients once it has ranges, so a Quantum waits for
    /// its features report before getting any — but never for ever, and an RD
    /// never waits at all.
    #[test]
    fn only_a_quantum_waits_for_its_features_report_and_not_indefinitely() {
        use super::{MAX_STATUS_REPORTS_WITHOUT_FEATURES as MAX, should_hold_for_features};
        use crate::brand::raymarine::BaseModel;

        use super::MAX_WAIT_FOR_FEATURES;
        use std::time::Duration;
        const SOON: Duration = Duration::ZERO;

        // A Quantum that has not reported yet is held back...
        for n in 1..=MAX {
            assert!(
                should_hold_for_features(BaseModel::Quantum, false, n, SOON),
                "status report {n} of {MAX} should still wait"
            );
        }
        // ...but only so long: an unobserved variant must not become
        // undiscoverable over a report we only assume it sends.
        assert!(
            !should_hold_for_features(BaseModel::Quantum, false, MAX + 1, SOON),
            "past the report limit the radar is shown anyway"
        );
        // Nor may a radar whose reports trickle in wait for ever, so the
        // deadline releases it however few reports have arrived.
        assert!(
            !should_hold_for_features(BaseModel::Quantum, false, 1, MAX_WAIT_FOR_FEATURES),
            "past the deadline the radar is shown however few reports came"
        );

        // Once the report is in there is nothing to wait for.
        assert!(!should_hold_for_features(BaseModel::Quantum, true, 1, SOON));

        // An RD has no features report at all and would wait for ever.
        for n in 1..=MAX + 1 {
            assert!(!should_hold_for_features(BaseModel::RD, false, n, SOON));
        }
    }

    // ----- heartbeat ticks (stand-down, issue #664) -----

    #[test]
    fn every_fifth_heartbeat_carries_the_extended_keep_alive() {
        let sent: Vec<(bool, bool)> = (0..10).map(|n| heartbeats_for_tick(false, n)).collect();
        assert!(sent.iter().all(|(heartbeat, _)| *heartbeat));
        let extended: Vec<u32> = (0..10u32)
            .filter(|n| heartbeats_for_tick(false, *n).1)
            .collect();
        assert_eq!(extended, vec![4, 9]);
    }

    #[test]
    fn standing_down_sends_no_keep_alive_at_all() {
        for n in 0..10 {
            assert_eq!(heartbeats_for_tick(true, n), (false, false));
        }
    }

    #[test]
    fn resuming_picks_the_cadence_up_where_it_left_off() {
        // Four heartbeats sent, then a stand-down; the first tick after
        // resuming is the fifth and carries the extended keep-alive.
        assert_eq!(heartbeats_for_tick(true, 4), (false, false));
        assert_eq!(heartbeats_for_tick(false, 4), (true, true));
    }

    /// Drive a `pending_transmit` flag through the same two decisions the
    /// receiver applies — arm on a Power request (`note_power_request`), and
    /// re-send on a status report (`maybe_reapply_transmit`, which clears the
    /// flag once it fires). Returns (final pending flag, number of transmit
    /// re-sends) for a sequence of (power request, reported status) steps.
    fn run_flow(steps: &[(Option<Power>, Option<Power>)]) -> (bool, u32) {
        let mut pending = false;
        let mut resends = 0;
        for &(request, reported) in steps {
            if let Some(req) = request
                && let Some(p) = transmit_should_defer(req, reported)
            {
                pending = p;
            }
            if should_reapply_transmit(pending, reported) {
                pending = false;
                resends += 1;
            }
        }
        (pending, resends)
    }

    #[test]
    fn off_to_transmit_reapplies_once_at_standby() {
        // Ask for transmit while off, radar stays off/preparing for a while,
        // then reports standby -> exactly one re-send, flag cleared.
        let (pending, resends) = run_flow(&[
            (Some(Power::Transmit), Some(Power::Off)),
            (None, Some(Power::Off)),
            (None, Some(Power::Preparing)),
            (None, Some(Power::Standby)),
            (None, Some(Power::Standby)),
        ]);
        assert!(!pending);
        assert_eq!(resends, 1);
    }

    #[test]
    fn standby_request_cancels_pending_reapply() {
        // Transmit-from-off arms it, but the user then asks for standby before
        // the radar boots -> no re-send when standby finally arrives.
        let (pending, resends) = run_flow(&[
            (Some(Power::Transmit), Some(Power::Off)),
            (Some(Power::Standby), Some(Power::Off)),
            (None, Some(Power::Standby)),
        ]);
        assert!(!pending);
        assert_eq!(resends, 0);
    }

    #[test]
    fn standby_to_transmit_does_not_reapply() {
        // Standby -> transmit is honoured immediately; nothing is deferred.
        let (pending, resends) = run_flow(&[
            (Some(Power::Transmit), Some(Power::Standby)),
            (None, Some(Power::Standby)),
        ]);
        assert!(!pending);
        assert_eq!(resends, 0);
    }

    #[test]
    fn transmit_from_off_or_unknown_defers() {
        // Off or not-yet-reported: the radar is booting, so a transmit
        // request must be re-applied once it reaches standby.
        assert_eq!(transmit_should_defer(Power::Transmit, None), Some(true));
        assert_eq!(
            transmit_should_defer(Power::Transmit, Some(Power::Off)),
            Some(true)
        );
    }

    #[test]
    fn transmit_while_awake_is_not_deferred() {
        // Standby -> transmit is honoured immediately; transmit while already
        // transmitting is a no-op — neither should arm the pending flag.
        assert_eq!(
            transmit_should_defer(Power::Transmit, Some(Power::Standby)),
            Some(false)
        );
        assert_eq!(
            transmit_should_defer(Power::Transmit, Some(Power::Transmit)),
            Some(false)
        );
    }

    #[test]
    fn standby_or_off_cancels_pending_transmit() {
        assert_eq!(transmit_should_defer(Power::Standby, None), Some(false));
        assert_eq!(transmit_should_defer(Power::Off, None), Some(false));
    }

    #[test]
    fn other_states_leave_pending_unchanged() {
        assert_eq!(transmit_should_defer(Power::Preparing, None), None);
        assert_eq!(transmit_should_defer(Power::Fault, None), None);
    }
}
