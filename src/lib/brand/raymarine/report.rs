use anyhow::{Error, bail};
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::Cli;
use crate::brand::raymarine::RaymarineModel;
use crate::network;
use crate::replay::RadarSocket;
use crate::radar::range::Ranges;
use crate::radar::{BYTE_LOOKUP_LENGTH, CommonRadar, Legend, RadarError, RadarInfo, SharedRadars};

// use super::command::Command;
use super::command::Command;

mod quantum;
mod rd;

// The radar drops the connection after ~60 seconds without a heartbeat.
// Send the 1-second keep-alive every second, and the 5-second extended
// keep-alive every 5th cycle.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(1000);

// The LookupSpokeEnum is an index into an array, really
enum LookupDoppler {
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

    if legend.pixel_colors >= 128 {
        for j in 0..BYTE_LOOKUP_LENGTH {
            lookup[LookupDoppler::Normal as usize][j] = j as u8 / 2;
            lookup[LookupDoppler::Doppler as usize][j] = match j {
                0xff => doppler_approaching,
                0xfe => doppler_receding,
                _ => j as u8 / 2,
            };
        }
    } else {
        for j in 0..BYTE_LOOKUP_LENGTH {
            lookup[LookupDoppler::Normal as usize][j] = j as u8;
            lookup[LookupDoppler::Doppler as usize][j] = match j {
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
    pub(crate) fn is_quantum(&self) -> bool { self.has_flag(super::protocol::FEATURE_QUANTUM) }
    pub(crate) fn is_cyclone(&self) -> bool { self.has_flag(super::protocol::FEATURE_CYCLONE) }
    pub(crate) fn has_doppler(&self) -> bool { self.has_flag(super::protocol::FEATURE_DOPPLER) }
    pub(crate) fn has_doppler_auto_acquire(&self) -> bool { self.has_flag(super::protocol::FEATURE_DOPPLER_AUTO_ACQUIRE) }
    pub(crate) fn has_doppler_bird_mode(&self) -> bool { self.has_flag(super::protocol::FEATURE_DOPPLER_BIRD_MODE) }
    pub(crate) fn has_bird_mode(&self) -> bool { self.has_flag(super::protocol::FEATURE_BIRD_MODE) }
    pub(crate) fn has_auto_rain(&self) -> bool { self.has_flag(super::protocol::FEATURE_AUTO_RAIN) }
    pub(crate) fn has_marpa(&self) -> bool { self.has_flag(super::protocol::FEATURE_MARPA) }
    pub(crate) fn has_dual_range_marpa(&self) -> bool { self.has_flag(super::protocol::FEATURE_DUAL_RANGE_MARPA) }
    pub(crate) fn is_analogue(&self) -> bool { self.has_flag(super::protocol::FEATURE_ANALOGUE) }
    pub(crate) fn is_digital(&self) -> bool { self.has_flag(super::protocol::FEATURE_DIGITAL) }
}

pub(crate) struct RaymarineReportReceiver {
    common: CommonRadar,
    report_socket: Option<RadarSocket>,
    state: ReceiverState,
    model: Option<RaymarineModel>,
    command_sender: Option<Command>,
    heartbeat_deadline: Instant,
    heartbeat_counter: u32,
    reported_unknown: HashMap<u32, bool>,
    features: FeatureFlags,
    features_seen: bool,

    // For data (spokes)
    range_meters: u32,
    wire_to_legend: WireToLegendTable,
}

impl RaymarineReportReceiver {
    pub(crate) fn new(
        args: &Cli,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
    ) -> RaymarineReportReceiver {
        let key = info.key();

        let replay = args.is_replay();
        log::debug!(
            "{}: Creating RaymarineReportReceiver with args {:?}",
            key,
            args
        );
        let command_sender = None; // Only known after we receive the model info

        let control_update_rx = info.control_update_subscribe();
        let blob_tx = radars.get_blob_tx();

        let wire_to_legend = wire_to_legend(&info.get_legend());

        let common = CommonRadar::new(
            args,
            key,
            info,
            radars.clone(),
            control_update_rx,
            replay,
            blob_tx,
        );

        let now = Instant::now();
        RaymarineReportReceiver {
            common,
            report_socket: None,
            state: ReceiverState::Initial,
            model: None, // We don't know this yet, it will be set when we receive the first info report
            command_sender,
            heartbeat_deadline: now + HEARTBEAT_INTERVAL,
            heartbeat_counter: 0,
            reported_unknown: HashMap::new(),
            features: FeatureFlags::default(),
            features_seen: false,
            range_meters: 0,
            wire_to_legend,
        }
    }

    async fn start_report_socket(&mut self) -> io::Result<()> {
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
                Ok(())
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Ok(())
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
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
                },
                _ = sleep_until(heartbeat_deadline) => {
                    self.send_heartbeat().await?;
                },

                r = self.report_socket.as_mut().unwrap().recv_buf_from(&mut buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
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
                        Ok(cv) => {let _ = self.common.process_control_update(cv, &mut self.command_sender).await;},
                    }
                }
            }
        }
    }

    async fn send_heartbeat(&mut self) -> Result<(), RadarError> {
        if let Some(ref mut cs) = self.command_sender {
            cs.send_heartbeat().await?;

            // Every 5th heartbeat (every 5 seconds), also send the
            // extended keep-alive with MARPA/AIS option data.
            self.heartbeat_counter += 1;
            if self.heartbeat_counter % 5 == 0 {
                cs.send_heartbeat_5s().await?;
            }
        }
        self.heartbeat_deadline += HEARTBEAT_INTERVAL;
        Ok(())
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_report_socket().await?;
        loop {
            if self.report_socket.is_some() {
                match self.socket_loop(&subsys).await {
                    Err(RadarError::Shutdown) => {
                        return Ok(());
                    }
                    _ => {
                        // Ignore, reopen socket
                    }
                }
                self.report_socket = None;
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.start_report_socket().await?;
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
            // Quantum messages
            0x280001 => {
                quantum::process_info_report(self, data);
            }
            0x280002 => {
                quantum::process_status_report(self, data);
            }
            0x280003 => {
                quantum::process_frame(self, data);
            }
            0x288942 => {
                // Database report — not spoke data. Ignore.
                log::trace!("{}: Quantum database report len={}", self.common.key, data.len());
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
            // Guard zone, alarm, MARPA, self-test, etc. — logged but not acted on
            id if (id & 0xFFFF0000 == 0x28000000 || id & 0xFFFF0000 == 0x01000000) => {
                if self.reported_unknown.get(&id).is_none() {
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
                if self.reported_unknown.get(&id).is_none() {
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
                "{}: Features: quantum={} cyclone={} doppler={} bird_mode={} \
                 marpa={} auto_rain={} (raw=0x{:08x})",
                self.common.key,
                features.is_quantum(),
                features.is_cyclone(),
                features.has_doppler(),
                features.has_bird_mode(),
                features.has_marpa(),
                features.has_auto_rain(),
                flags,
            );

            // Update Doppler capability based on what the radar actually
            // reports, overriding the hardcoded model table.
            if features.has_doppler() != self.common.info.doppler {
                self.common.info.set_doppler(features.has_doppler());
                self.wire_to_legend = wire_to_legend(&self.common.info.get_legend());
                log::info!(
                    "{}: Doppler capability updated to {}",
                    self.common.key,
                    features.has_doppler(),
                );
            }

            self.features = features;
            self.features_seen = true;
        }
    }

    fn set_ranges(&mut self, ranges: Ranges) {
        if let Some(command_sender) = &mut self.command_sender {
            command_sender.set_ranges(ranges.clone());
        }
        self.common.set_ranges(ranges);
    }
}
