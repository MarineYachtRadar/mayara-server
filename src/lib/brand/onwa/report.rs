use std::io;
use std::net::SocketAddrV4;
use std::time::Duration;
use tokio::time::{Instant, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::Command;
use super::protocol::*;
use crate::Cli;
use crate::network::{SocketType, create_udp_listen};
use crate::radar::settings::ControlId;
use crate::radar::{CommonRadar, Power, RadarError, RadarInfo, SharedRadars};
use crate::replay::RadarSocket;

/// The chartplotter polls the radar once a second.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(1);

/// The radar never says when it is warm. The K-ASTRAL counts down this long
/// from the moment it starts talking to the radar, however long the radar
/// has been on, so Mayara does the same.
const WARMUP: Duration = Duration::from_secs(100);

pub(crate) struct OnwaReportReceiver {
    common: CommonRadar,
    command_sender: Option<Command>,
    /// From the latest state report or range echo; spokes do not say.
    range_index: Option<u8>,
    prev_angle: Option<u16>,
    warmup: Warmup,
}

/// Mayara's own warm-up countdown, since the radar reports none.
#[derive(Default)]
struct Warmup {
    /// When the warm-up ends; `None` once the radar is known to be warm.
    warm_at: Option<Instant>,
    /// Whether a state report has arrived, so first contact counts once.
    seen: bool,
}

impl Warmup {
    /// Every state report: the first starts the countdown unless the radar
    /// is already transmitting, and a transmitting radar ends it. Returns
    /// the time left.
    fn on_report(&mut self, transmitting: bool, now: Instant) -> Duration {
        if !self.seen {
            self.seen = true;
            if !transmitting {
                self.restart(now);
            }
        }
        if transmitting {
            self.warm_at = None;
        }
        let remaining = self
            .warm_at
            .map_or(Duration::ZERO, |t| t.saturating_duration_since(now));
        if remaining.is_zero() {
            self.warm_at = None;
        }
        remaining
    }

    /// The radar has just powered up.
    fn restart(&mut self, now: Instant) {
        self.warm_at = Some(now + WARMUP);
    }
}

impl OnwaReportReceiver {
    pub(crate) fn new(args: &Cli, radars: SharedRadars, info: RadarInfo) -> Self {
        let key = info.key();
        let command_sender = if args.is_replay() {
            None
        } else {
            Some(Command::new(&info))
        };

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
        // One spoke arrives per datagram; batch ~1/32 of a revolution per
        // broadcast so compression and framing are not paid per spoke.
        let target = common.info.spokes_per_revolution.div_ceil(32) as usize;
        common.set_spoke_batch_threshold(target);

        OnwaReportReceiver {
            common,
            command_sender,
            range_index: None,
            prev_angle: None,
            warmup: Warmup::default(),
        }
    }

    pub(crate) async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), RadarError> {
        loop {
            if let Err(e) = self.data_loop(subsys).await {
                log::error!("{}: data loop error: {}, restarting", self.common.key, e);
            }
            if subsys.is_shutdown_requested() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    fn listen(&self, addr: &SocketAddrV4) -> io::Result<RadarSocket> {
        create_udp_listen(addr, &self.common.info.nic_addr, SocketType::Broadcast)
    }

    async fn data_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        let mut spokes = self.listen(&SPOKE_ADDRESS).map_err(RadarError::Io)?;
        let mut states = self.listen(&STATE_ADDRESS).map_err(RadarError::Io)?;
        let mut replies = self.listen(&REPLY_ADDRESS).map_err(RadarError::Io)?;

        let mut spoke_buf = Vec::with_capacity(2048);
        let mut state_buf = Vec::with_capacity(2048);
        let mut reply_buf = Vec::with_capacity(2048);
        let mut next_keepalive = Instant::now();

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => return Ok(()),

                _ = sleep_until(next_keepalive), if self.command_sender.is_some() => {
                    next_keepalive = Instant::now() + KEEPALIVE_INTERVAL;
                    if let Some(cmd) = self.command_sender.as_mut() {
                        cmd.send_keepalive().await?;
                    }
                }

                r = spokes.recv_buf_from(&mut spoke_buf) => {
                    r.map_err(RadarError::Io)?;
                    self.process_spoke(&spoke_buf);
                    spoke_buf.clear();
                }

                r = states.recv_buf_from(&mut state_buf) => {
                    r.map_err(RadarError::Io)?;
                    self.process_state(&state_buf);
                    state_buf.clear();
                }

                r = replies.recv_buf_from(&mut reply_buf) => {
                    r.map_err(RadarError::Io)?;
                    self.process_reply(&reply_buf);
                    reply_buf.clear();
                }

                r = self.common.control_update_rx.recv() => {
                    if let Ok(cv) = r {
                        self.common.process_control_update(cv, &mut self.command_sender).await?;
                    }
                }
            }
        }
    }

    fn process_spoke(&mut self, data: &[u8]) {
        let Some(spoke) = parse_spoke(data) else {
            return;
        };
        let Some(range) = self.range_index.and_then(range_meters) else {
            return;
        };

        // Every spoke is sent twice, and the angle now and then steps back a
        // few tenths of a degree. Only a spoke ahead of the last one is new;
        // anything behind it would read as a new revolution.
        if let Some(prev) = self.prev_angle {
            let behind = (prev as usize + SPOKES - spoke.angle as usize) % SPOKES;
            if behind < SPOKES / 2 {
                return;
            }
        }
        self.prev_angle = Some(spoke.angle);

        let pixels = spoke.samples.iter().map(|p| p >> 1).collect();

        // The radar knows no heading; the spoke takes the navigation heading.
        self.common.new_spoke_message();
        self.common
            .add_spoke(range as u32, spoke.angle, None, pixels);
        self.common.send_spoke_message();
    }

    fn process_state(&mut self, data: &[u8]) {
        // The radar is alive while it reports, transmitting or not.
        self.common.info.mark_input();
        if let Some(state) = parse_state(data) {
            self.apply_state(&state);
        } else if let Some(gains) = parse_gains(data) {
            let controls = &self.common.info.controls;
            let _ = controls.set(&ControlId::Gain, gains.gain, None);
            let _ = controls.set(&ControlId::Sea, gains.sea, None);
            let _ = controls.set(&ControlId::Rain, gains.rain, None);
        }
    }

    fn apply_state(&mut self, state: &State) {
        self.set_range(state.range_index);
        let remaining = self.warmup.on_report(state.transmit, Instant::now());
        self.common.set_value_enabled(
            &ControlId::WarmupTime,
            remaining.as_secs() as f64,
            (!remaining.is_zero()) as u8,
        );
        let power = if state.transmit {
            Power::Transmit
        } else if !remaining.is_zero() {
            Power::Preparing
        } else {
            Power::Standby
        };
        let controls = &self.common.info.controls;
        let _ = controls.set(&ControlId::Power, power as u32 as f64, None);
        let _ = controls.set(&ControlId::TargetExpansion, state.echo_stretch as f64, None);
        let _ = controls.set(
            &ControlId::InterferenceRejection,
            state.interference_rejection as f64,
            None,
        );
        let _ = controls.set(
            &ControlId::NoiseRejection,
            state.noise_rejection as f64,
            None,
        );
        let _ = controls.set(&ControlId::DisplayTiming, state.sweep_timing as f64, None);
        let (start, end) = state.dead_sector;
        self.common.set_sector(
            &ControlId::NoTransmitSector1,
            start as f64,
            end as f64,
            Some(state.dead_sector_on),
        );
    }

    fn set_range(&mut self, index: u8) {
        let Some(meters) = range_meters(index) else {
            log::debug!("{}: unknown range index {}", self.common.key, index);
            return;
        };
        self.range_index = Some(index);
        let _ = self
            .common
            .info
            .controls
            .set(&ControlId::Range, meters as f64, None);
    }

    fn process_reply(&mut self, data: &[u8]) {
        let Some((name, args)) = parse_reply(data) else {
            return;
        };
        match name {
            "RDACK" => {
                if let Some(index) = acked_range(args) {
                    self.set_range(index);
                }
            }
            "RDREST" => {
                log::info!("{}: radar powered up, warming up", self.common.key);
                self.warmup.restart(Instant::now());
            }
            "ANTFV" => {
                let controls = &self.common.info.controls;
                if let Some(model) = model_from_firmware(args) {
                    let _ = controls.set_string(&ControlId::ModelName, model);
                }
                let _ = controls.set_string(&ControlId::FirmwareVersion, args.to_string());
            }
            _ => log::trace!("{}: reply {} {}", self.common.key, name, args),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_contact_in_standby_counts_down() {
        let t0 = Instant::now();
        let mut w = Warmup::default();
        assert_eq!(w.on_report(false, t0), WARMUP);
        assert_eq!(
            w.on_report(false, t0 + Duration::from_secs(40)),
            Duration::from_secs(60)
        );
        assert_eq!(w.on_report(false, t0 + WARMUP), Duration::ZERO);
    }

    #[test]
    fn a_transmitting_radar_is_warm() {
        let t0 = Instant::now();
        let mut w = Warmup::default();
        assert_eq!(w.on_report(true, t0), Duration::ZERO);
        assert_eq!(
            w.on_report(false, t0),
            Duration::ZERO,
            "no countdown later either"
        );

        let mut w = Warmup::default();
        w.on_report(false, t0);
        assert_eq!(
            w.on_report(true, t0 + Duration::from_secs(10)),
            Duration::ZERO,
            "transmitting ends the countdown"
        );
        assert_eq!(
            w.on_report(false, t0 + Duration::from_secs(11)),
            Duration::ZERO
        );
    }

    #[test]
    fn a_reboot_restarts_the_countdown() {
        let t0 = Instant::now();
        let mut w = Warmup::default();
        w.on_report(false, t0);
        let later = t0 + Duration::from_secs(300);
        assert_eq!(w.on_report(false, later), Duration::ZERO);
        w.restart(later);
        assert_eq!(w.on_report(false, later), WARMUP);
    }
}
