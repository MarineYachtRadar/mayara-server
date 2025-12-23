use anyhow::{bail, Error};
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::brand::raymarine::RaymarineModel;
use crate::network::create_udp_multicast_listen;
use crate::radar::range::Ranges;
use crate::radar::trail::TrailBuffer;
use crate::radar::{Legend, RadarError, RadarInfo, SharedRadars, Statistics, BYTE_LOOKUP_LENGTH};
use crate::settings::{ControlUpdate, ControlValue};
use crate::tokio_io::TokioIoProvider;
use crate::Session;

// Use unified controller from mayara-core
use mayara_core::controllers::RaymarineController;

use super::BaseModel;

// Debug I/O wrapper for protocol analysis (dev feature only)
#[cfg(feature = "dev")]
use crate::debug::DebugIoProvider;

/// Type alias for the I/O provider used by RaymarineReportReceiver.
/// When dev feature is enabled, wraps TokioIoProvider with DebugIoProvider.
#[cfg(feature = "dev")]
type RaymarineIoProvider = DebugIoProvider<TokioIoProvider>;

#[cfg(not(feature = "dev"))]
type RaymarineIoProvider = TokioIoProvider;

mod quantum;
mod rd;

// Every 5 seconds we ask the radar for reports, so we can update our controls
const REPORT_REQUEST_INTERVAL: Duration = Duration::from_millis(5000);

// The LookupSpokeEnum is an index into an array, really
enum LookupDoppler {
    Normal = 0,
    Doppler = 1,
}
const LOOKUP_DOPPLER_LENGTH: usize = (LookupDoppler::Doppler as usize) + 1;

type PixelToBlobType = [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

pub(super) fn pixel_to_blob(legend: &Legend) -> PixelToBlobType {
    let mut lookup: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH] =
        [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];
    // Cannot use for() in const expr, so use while instead
    for j in 0..BYTE_LOOKUP_LENGTH {
        lookup[LookupDoppler::Normal as usize][j] = j as u8 / 2;
        lookup[LookupDoppler::Doppler as usize][j] = match j {
            0xff => legend.doppler_approaching,
            0xfe => legend.doppler_receding,
            _ => j as u8 / 2,
        };
    }
    log::info!("Created pixel_to_blob from legend {:?}", legend);
    lookup
}

#[derive(PartialEq, PartialOrd, Debug)]
enum ReceiverState {
    Initial,
    InfoRequestReceived,
    FixedRequestReceived,
    StatusRequestReceived,
}

pub(crate) struct RaymarineReportReceiver {
    #[allow(dead_code)]
    session: Session, // Kept for debug_hub access
    replay: bool,
    info: RadarInfo,
    key: String,
    report_socket: Option<UdpSocket>,
    radars: SharedRadars,
    state: ReceiverState,
    model: Option<RaymarineModel>,
    base_model: Option<BaseModel>,
    /// Unified controller from mayara-core
    controller: Option<RaymarineController>,
    /// I/O provider for the controller (wrapped with DebugIoProvider when dev feature enabled)
    io: RaymarineIoProvider,
    control_update_rx: broadcast::Receiver<ControlUpdate>,
    report_request_timeout: Instant,
    reported_unknown: HashMap<u32, bool>,

    // For data (spokes)
    statistics: Statistics,
    pixel_stats: [u32; 256],
    range_meters: u32,
    pixel_to_blob: PixelToBlobType,
    trails: TrailBuffer,
    prev_azimuth: u16,
}

impl RaymarineReportReceiver {
    pub fn new(
        session: Session,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
    ) -> RaymarineReportReceiver {
        let key = info.key();

        let args = session.read().unwrap().args.clone();
        let replay = args.replay;
        log::debug!(
            "{}: Creating RaymarineReportReceiver with args {:?}",
            key,
            args
        );

        // Controller is created when we know the model (from info report)
        let controller = None;

        // Create I/O provider - wrapped with DebugIoProvider when dev feature enabled
        #[cfg(feature = "dev")]
        let io = {
            let inner = TokioIoProvider::new();
            if let Some(hub) = session.debug_hub() {
                log::debug!("{}: Using DebugIoProvider for protocol analysis", key);
                DebugIoProvider::new(inner, hub, key.clone(), "raymarine".to_string())
            } else {
                // Fallback if debug_hub not initialized (shouldn't happen)
                log::warn!(
                    "{}: DebugHub not available, using plain TokioIoProvider",
                    key
                );
                DebugIoProvider::new(
                    inner,
                    std::sync::Arc::new(crate::debug::DebugHub::new()),
                    key.clone(),
                    "raymarine".to_string(),
                )
            }
        };

        #[cfg(not(feature = "dev"))]
        let io = TokioIoProvider::new();

        let control_update_rx = info.controls.control_update_subscribe();

        let pixel_to_blob = pixel_to_blob(&info.legend);
        let trails = TrailBuffer::new(session.clone(), &info);

        RaymarineReportReceiver {
            session,
            replay,
            key,
            info,
            report_socket: None,
            radars,
            state: ReceiverState::Initial,
            model: None, // We don't know this yet, it will be set when we receive the first info report
            base_model: None,
            controller,
            io,
            report_request_timeout: Instant::now(),
            control_update_rx,
            reported_unknown: HashMap::new(),
            statistics: Statistics::new(),
            pixel_stats: [0; 256],
            range_meters: 0,
            pixel_to_blob,
            trails,
            prev_azimuth: 0,
        }
    }

    async fn start_report_socket(&mut self) -> io::Result<()> {
        match create_udp_multicast_listen(&self.info.report_addr, &self.info.nic_addr) {
            Ok(socket) => {
                self.report_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for reports",
                    self.key,
                    &self.info.report_addr,
                    &self.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.key,
                    &self.info.report_addr,
                    &self.info.nic_addr,
                    e
                );
                Ok(())
            }
        }
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.info.command_tx.
    //
    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.key);
        let mut buf = Vec::with_capacity(10000);

        loop {
            let timeout = self.report_request_timeout;
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },
                _ = sleep_until(timeout) => {
                     self.send_report_requests().await?;

                },

                r = self.report_socket.as_ref().unwrap().recv_buf_from(&mut buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            if buf.len() == buf.capacity() {
                                let old = buf.capacity();
                                buf.reserve(1024);
                                log::warn!("{}: UDP report buffer full, increasing size {} -> {}", self.key, old, buf.capacity()   );
                            }
                            else if let Err(e) = self.process_report(&buf).await {
                                log::error!("{}: {}", self.key, e);
                            }
                            buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },
                r = self.control_update_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {let _ = self.process_control_update(cv).await;},
                    }
                }
            }
        }
    }

    async fn process_control_update(
        &mut self,
        control_update: ControlUpdate,
    ) -> Result<(), RadarError> {
        let cv = control_update.control_value;
        let reply_tx = control_update.reply_tx;

        if let Err(e) = self.send_control_to_radar(&cv).await {
            return self
                .info
                .controls
                .send_error_to_client(reply_tx, &cv, &e)
                .await;
        } else {
            self.info.controls.set_refresh(&cv.id);
        }

        Ok(())
    }

    /// Send control command to radar via the unified controller
    async fn send_control_to_radar(&mut self, cv: &ControlValue) -> Result<(), RadarError> {
        let controller = match &mut self.controller {
            Some(c) => c,
            None => {
                return Err(RadarError::CannotSetControlType(
                    "Controller not initialized".to_string(),
                ))
            }
        };

        let value: f32 = cv
            .value
            .parse()
            .map_err(|_| RadarError::MissingValue(cv.id.clone()))?;
        let auto = cv.auto.unwrap_or(false);
        let enabled = cv.enabled.unwrap_or(false);
        let v = Self::scale_100_to_byte(value);

        log::debug!(
            "{}: set_control {} = {} auto={} enabled={}",
            self.key,
            cv.id,
            value,
            auto,
            enabled
        );

        match cv.id.as_str() {
            "power" => {
                // Look up power value using enum
                let transmit = if let Some(control) = self.info.controls.get("power") {
                    let index = control.enum_value_to_index(&cv.value).unwrap_or(1);
                    index == 2 // transmit is index 2
                } else {
                    cv.value.to_lowercase() == "transmit"
                };
                controller.set_power(&mut self.io, transmit);
            }
            "range" => {
                let value = value as i32;
                let ranges = &self.info.ranges;
                let index = if value < ranges.len() as i32 {
                    value as u8
                } else {
                    let mut i = 0u8;
                    for r in ranges.all.iter() {
                        if r.distance() >= value {
                            break;
                        }
                        i += 1;
                    }
                    i
                };
                controller.set_range(&mut self.io, index);
            }
            "gain" => {
                controller.set_gain(&mut self.io, v, auto);
            }
            "sea" => {
                controller.set_sea(&mut self.io, v, auto);
            }
            "rain" => {
                controller.set_rain(&mut self.io, v, enabled);
            }
            "colorGain" => {
                controller.set_color_gain(&mut self.io, v, auto);
            }
            "interferenceRejection" => {
                controller.set_interference_rejection(&mut self.io, v);
            }
            "targetExpansion" => {
                controller.set_target_expansion(&mut self.io, v);
            }
            "bearingAlignment" => {
                controller.set_bearing_alignment(&mut self.io, value);
            }
            "mode" => {
                controller.set_mode(&mut self.io, v);
            }
            "ftc" => {
                // FTC enabled is inverted from auto
                let ftc_enabled = !auto;
                controller.set_ftc(&mut self.io, v, ftc_enabled);
            }
            "mainBangSuppression" => {
                // Main bang suppression enabled is inverted from auto
                let mbs_enabled = !auto;
                controller.set_main_bang_suppression(&mut self.io, mbs_enabled);
            }
            "displayTiming" => {
                controller.set_display_timing(&mut self.io, v);
            }
            "tune" => {
                controller.set_tune(&mut self.io, v, auto);
            }
            _ => {
                return Err(RadarError::CannotSetControlType(cv.id.clone()));
            }
        }

        Ok(())
    }

    fn scale_100_to_byte(a: f32) -> u8 {
        // Map range 0..100 to 0..255
        let mut r = a * 255.0 / 100.0;
        if r > 255.0 {
            r = 255.0;
        } else if r < 0.0 {
            r = 0.0;
        }
        r as u8
    }

    async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        if let Some(controller) = &mut self.controller {
            controller.send_report_requests(&mut self.io);
        }
        self.report_request_timeout += REPORT_REQUEST_INTERVAL;
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
    fn set<T>(&mut self, control_id: &str, value: T, auto: Option<bool>, enabled: Option<bool>)
    where
        f32: From<T>,
    {
        match self
            .info
            .controls
            .set_value_auto_enabled(control_id, value, auto, enabled)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_id).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {} auto {:?} enabled {:?}",
                        self.key,
                        control_id,
                        control.value(),
                        control.auto,
                        control.enabled
                    );
                }
            }
            Ok(None) => {}
        };
    }

    fn set_value<T>(&mut self, control_id: &str, value: T)
    where
        f32: From<T>,
    {
        self.set(control_id, value.into(), None, None)
    }

    fn set_value_auto<T>(&mut self, control_id: &str, value: T, auto: u8)
    where
        f32: From<T>,
    {
        self.set(control_id, value, Some(auto > 0), None)
    }

    fn set_value_enabled<T>(&mut self, control_id: &str, value: T, enabled: u8)
    where
        f32: From<T>,
    {
        self.set(control_id, value, None, Some(enabled > 0))
    }

    fn set_string(&mut self, control_id: &str, value: String) {
        match self.info.controls.set_string(control_id, value) {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(v)) => {
                log::debug!("{}: Control '{}' new value '{}'", self.key, control_id, v);
            }
            Ok(None) => {}
        };
    }

    fn set_wire_range(&mut self, control_id: &str, min: u8, max: u8) {
        match self
            .info
            .controls
            .set_wire_range(control_id, min as f32, max as f32)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_id).unwrap();
                    log::trace!(
                        "{}: Control '{}' new wire min {} max {} value {} auto {:?} enabled {:?} ",
                        self.key,
                        control_id,
                        min,
                        max,
                        control.value(),
                        control.auto,
                        control.enabled,
                    );
                }
            }
            Ok(None) => {}
        };
    }

    async fn process_report(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < 4 {
            bail!("UDP report len {} dropped", data.len());
        }
        log::trace!("{}: UDP report {:02X?}", self.key, data);

        let id = u32::from_le_bytes(data[0..4].try_into().unwrap());
        match id {
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
            0x280001 => {
                quantum::process_info_report(self, data);
            }
            0x280002 => {
                quantum::process_status_report(self, data);
            }
            0x280003 => {
                quantum::process_frame(self, data);
            }
            _ => {
                if self.reported_unknown.get(&id).is_none() {
                    log::warn!("{}: Unknown report ID {:08X?}", self.key, id);
                    self.reported_unknown.insert(id, true);
                }
            }
        }
        Ok(())
    }

    fn set_ranges(&mut self, ranges: Ranges) {
        if self.info.set_ranges(ranges).is_ok() {
            self.radars.update(&self.info);
        }
    }
}
