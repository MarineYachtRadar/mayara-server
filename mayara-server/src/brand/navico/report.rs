use anyhow::{bail, Error};
use std::cmp::min;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::*;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use std::net::SocketAddrV4;

use crate::brand::navico::info::Information;
use crate::network::create_udp_multicast_listen;
use crate::radar::range::{RangeDetection, RangeDetectionResult};
use crate::radar::target::MS_TO_KN;
use crate::radar::{DopplerMode, RadarError, RadarInfo, SharedRadars};
use crate::settings::{ControlUpdate, ControlValue, DataUpdate};
use crate::tokio_io::TokioIoProvider;
use crate::Session;

// Use unified controller from mayara-core
use mayara_core::controllers::{NavicoController, NavicoModel};

use super::Model;

use crate::radar::Status;

// Use mayara-core for report parsing and packet types (pure, WASM-compatible)
use mayara_core::protocol::navico::{
    parse_report_01, parse_report_02, parse_report_03, parse_report_04,
    parse_report_06_68, parse_report_06_74, parse_report_08,
    HaloHeadingPacket, HaloNavigationPacket, HaloSpeedPacket,
    INFO_ADDR, INFO_PORT, SPEED_ADDR_A, SPEED_PORT_A,
};

pub struct NavicoReportReceiver {
    replay: bool,
    transmit_after_range_detection: bool,
    info: RadarInfo,
    key: String,
    report_buf: Vec<u8>,
    report_socket: Option<UdpSocket>,
    info_buf: Vec<u8>,
    info_socket: Option<UdpSocket>,
    speed_buf: Vec<u8>,
    speed_socket: Option<UdpSocket>,
    radars: SharedRadars,
    model: Model,
    /// Unified controller from mayara-core
    controller: Option<NavicoController>,
    /// I/O provider for the controller
    io: TokioIoProvider,
    info_sender: Option<Information>,
    data_tx: broadcast::Sender<DataUpdate>,
    control_update_rx: broadcast::Receiver<ControlUpdate>,
    range_timeout: Instant,
    info_request_timeout: Instant,
    report_request_timeout: Instant,
    reported_unknown: [bool; 256],
}

// Every 5 seconds we ask the radar for reports, so we can update our controls
const REPORT_REQUEST_INTERVAL: Duration = Duration::from_millis(5000);

// When others send INFO reports, we do not want to send our own INFO reports
const INFO_BY_OTHERS_TIMEOUT: Duration = Duration::from_secs(15);

// When we send INFO reports, the interval is short
const INFO_BY_US_INTERVAL: Duration = Duration::from_millis(250);

// When we are detecting ranges, we wait for 2 seconds before we send the next range
const RANGE_DETECTION_INTERVAL: Duration = Duration::from_secs(2);

// Used when we don't want to wait for something, we use now plus this
const FAR_FUTURE: Duration = Duration::from_secs(86400 * 365 * 30);

// Report type constants
const REPORT_01_C4_18: u8 = 0x01;
const REPORT_02_C4_99: u8 = 0x02;

const REPORT_03_C4_129: u8 = 0x03;
const REPORT_04_C4_66: u8 = 0x04;
const REPORT_06_C4_68: u8 = 0x06;
const REPORT_08_C4_18_OR_21_OR_22: u8 = 0x08;

impl NavicoReportReceiver {
    pub fn new(
        session: Session,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
        model: Model,
    ) -> NavicoReportReceiver {
        let key = info.key();

        let args = session.read().unwrap().args.clone();
        let replay = args.replay;
        log::debug!(
            "{}: Creating NavicoReportReceiver with args {:?}",
            key,
            args
        );

        // Convert server Model to core NavicoModel
        let core_model = match model {
            Model::BR24 => NavicoModel::BR24,
            Model::Gen3 => NavicoModel::Gen3,
            Model::Gen4 => NavicoModel::Gen4,
            Model::HALO => NavicoModel::Halo,
            Model::Unknown => NavicoModel::Unknown,
        };

        // If we are in replay mode, we don't need a controller
        let controller = if !replay {
            log::debug!("{}: Starting controller (unified)", key);
            Some(NavicoController::new(
                &key,
                &info.send_command_addr.ip().to_string(),
                info.send_command_addr.port(),
                &info.report_addr.ip().to_string(),
                info.report_addr.port(),
                &info.nic_addr.to_string(),
                core_model,
            ))
        } else {
            log::debug!("{}: No controller, replay mode", key);
            None
        };
        let io = TokioIoProvider::new();

        let info_sender = if !replay {
            log::debug!("{}: Starting info sender", key);
            Some(Information::new(key.clone(), &info))
        } else {
            log::debug!("{}: No info sender, replay mode", key);
            None
        };

        let control_update_rx = info.controls.control_update_subscribe();
        let data_update_tx = info.controls.get_data_update_tx();

        let now = Instant::now();
        NavicoReportReceiver {
            replay,
            transmit_after_range_detection: false,
            key,
            info,
            report_buf: Vec::with_capacity(1000),
            report_socket: None,
            info_buf: Vec::with_capacity(::core::mem::size_of::<HaloHeadingPacket>()),
            info_socket: None,
            speed_buf: Vec::with_capacity(::core::mem::size_of::<HaloSpeedPacket>()),
            speed_socket: None,
            radars,
            model,
            controller,
            io,
            info_sender,
            range_timeout: now + FAR_FUTURE,
            info_request_timeout: now,
            report_request_timeout: now,
            data_tx: data_update_tx,
            control_update_rx,
            reported_unknown: [false; 256],
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

    async fn start_info_socket(&mut self) -> io::Result<()> {
        if self.info_socket.is_some() {
            return Ok(()); // Already started
        }
        let info_addr = SocketAddrV4::new(INFO_ADDR.parse().unwrap(), INFO_PORT);
        match create_udp_multicast_listen(&info_addr, &self.info.nic_addr) {
            Ok(socket) => {
                self.info_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for info reports",
                    self.key,
                    &self.info.report_addr,
                    &self.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
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

    async fn start_speed_socket(&mut self) -> io::Result<()> {
        if self.speed_socket.is_some() {
            return Ok(()); // Already started
        }
        let speed_addr = SocketAddrV4::new(SPEED_ADDR_A.parse().unwrap(), SPEED_PORT_A);
        match create_udp_multicast_listen(&speed_addr, &self.info.nic_addr) {
            Ok(socket) => {
                self.speed_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for speed reports",
                    self.key,
                    &self.info.report_addr,
                    &self.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
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

        loop {
            if !self.replay {
                self.start_info_socket().await?;
                self.start_speed_socket().await?;
            }

            let timeout = min(
                min(self.report_request_timeout, self.range_timeout),
                self.info_request_timeout,
            );

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(timeout) => {
                    let now = Instant::now();
                    if self.range_timeout <= now {
                        self.process_range(0).await?;
                    }
                    if self.report_request_timeout <= now {
                        self.send_report_requests().await?;
                    }
                    if self.info_request_timeout <= now {
                        self.send_info_requests().await?;
                    }
                },

                r = self.report_socket.as_ref().unwrap().recv_buf_from(&mut self.report_buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            if let Err(e) = self.process_report().await {
                                log::error!("{}: {}", self.key, e);
                            }
                            self.report_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = self.info_socket.as_ref().unwrap().recv_buf_from(&mut self.info_buf),
                    if self.info_socket.is_some() => {
                    match r {
                        Ok((_len, addr)) => {
                            self.process_info(&addr);
                            self.info_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive info error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },


                r = self.speed_socket.as_ref().unwrap().recv_buf_from(&mut self.speed_buf),
                    if self.speed_socket.is_some() => {
                    match r {
                        Ok((_len, addr)) => {
                            self.process_speed(&addr);
                            self.speed_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive speed error: {}", self.key, e);
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

        log::debug!("{}: process_control_update id={} value={}", self.key, cv.id, cv.value);

        match self.send_control_to_radar(&cv) {
            Ok(()) => {
                self.info.controls.set_refresh(&cv.id);
            }
            Err(e) => {
                return self
                    .info
                    .controls
                    .send_error_to_client(reply_tx, &cv, &e)
                    .await;
            }
        }

        Ok(())
    }

    /// Send a control command to the radar via the unified controller
    fn send_control_to_radar(&mut self, cv: &ControlValue) -> Result<(), RadarError> {
        let controller = match &mut self.controller {
            Some(c) => c,
            None => return Ok(()), // Replay mode, no controller
        };

        // Handle power control first (string value, not numeric)
        if cv.id.as_str() == "power" {
            let transmit = if let Some(control) = self.info.controls.get("power") {
                let index = control.enum_value_to_index(&cv.value).unwrap_or(1);
                index == 2 // transmit is index 2
            } else {
                cv.value.to_lowercase() == "transmit"
            };
            log::info!("{}: set_power transmit={} (value='{}')", self.key, transmit, cv.value);
            controller.set_power(&mut self.io, transmit);
            return Ok(());
        }

        let value = cv
            .value
            .parse::<f32>()
            .map_err(|_| RadarError::MissingValue(cv.id.clone()))?;
        let auto = cv.auto.unwrap_or(false);
        let enabled = cv.enabled.unwrap_or(false);

        // Scale 0-100 to 0-255 for controls that use byte values
        fn scale_100_to_byte(a: f32) -> u8 {
            let r = a * 255.0 / 100.0;
            r.clamp(0.0, 255.0) as u8
        }

        fn mod_deci_degrees(a: i32) -> i16 {
            ((a + 7200) % 3600) as i16
        }

        fn get_angle_value(id: &str, controls: &crate::settings::SharedControls) -> i16 {
            if let Some(control) = controls.get(id) {
                if let Some(value) = control.value {
                    return mod_deci_degrees((value * 10.0) as i32);
                }
            }
            0
        }

        let deci_value = (value * 10.0) as i32;

        match cv.id.as_str() {
            "range" => {
                controller.set_range(&mut self.io, deci_value);
            }
            "bearingAlignment" => {
                controller.set_bearing_alignment(&mut self.io, mod_deci_degrees(deci_value));
            }
            "gain" => {
                controller.set_gain(&mut self.io, scale_100_to_byte(value), auto);
            }
            "sea" => {
                controller.set_sea(&mut self.io, scale_100_to_byte(value), auto);
            }
            "rain" => {
                controller.set_rain(&mut self.io, scale_100_to_byte(value));
            }
            "sidelobeSuppression" => {
                controller.set_sidelobe_suppression(&mut self.io, scale_100_to_byte(value), auto);
            }
            "interferenceRejection" => {
                controller.set_interference_rejection(&mut self.io, value as u8);
            }
            "targetExpansion" => {
                controller.set_target_expansion(&mut self.io, value as u8);
            }
            "targetBoost" => {
                controller.set_target_boost(&mut self.io, value as u8);
            }
            "seaState" => {
                controller.set_sea_state(&mut self.io, value as u8);
            }
            "localInterferenceRejection" => {
                controller.set_local_interference_rejection(&mut self.io, value as u8);
            }
            "scanSpeed" => {
                controller.set_scan_speed(&mut self.io, value as u8 > 0);
            }
            "mode" => {
                controller.set_mode(&mut self.io, value as u8);
            }
            "noiseRejection" => {
                controller.set_noise_rejection(&mut self.io, value as u8);
            }
            "targetSeparation" => {
                controller.set_target_separation(&mut self.io, value as u8);
            }
            "dopplerMode" => {
                controller.set_doppler_mode(&mut self.io, value as u8);
            }
            "dopplerSpeed" => {
                controller.set_doppler_speed(&mut self.io, (value as u16) * 16);
            }
            "antennaHeight" => {
                controller.set_antenna_height(&mut self.io, deci_value as u16);
            }
            "accentLight" => {
                controller.set_accent_light(&mut self.io, value as u8);
            }
            "noTransmitStart1" => {
                let start = mod_deci_degrees(deci_value);
                let end = get_angle_value("noTransmitEnd1", &self.info.controls);
                controller.set_no_transmit_zone(&mut self.io, 0, start, end, enabled);
            }
            "noTransmitEnd1" => {
                let start = get_angle_value("noTransmitStart1", &self.info.controls);
                let end = mod_deci_degrees(deci_value);
                controller.set_no_transmit_zone(&mut self.io, 0, start, end, enabled);
            }
            "noTransmitStart2" => {
                let start = mod_deci_degrees(deci_value);
                let end = get_angle_value("noTransmitEnd2", &self.info.controls);
                controller.set_no_transmit_zone(&mut self.io, 1, start, end, enabled);
            }
            "noTransmitEnd2" => {
                let start = get_angle_value("noTransmitStart2", &self.info.controls);
                let end = mod_deci_degrees(deci_value);
                controller.set_no_transmit_zone(&mut self.io, 1, start, end, enabled);
            }
            "noTransmitStart3" => {
                let start = mod_deci_degrees(deci_value);
                let end = get_angle_value("noTransmitEnd3", &self.info.controls);
                controller.set_no_transmit_zone(&mut self.io, 2, start, end, enabled);
            }
            "noTransmitEnd3" => {
                let start = get_angle_value("noTransmitStart3", &self.info.controls);
                let end = mod_deci_degrees(deci_value);
                controller.set_no_transmit_zone(&mut self.io, 2, start, end, enabled);
            }
            "noTransmitStart4" => {
                let start = mod_deci_degrees(deci_value);
                let end = get_angle_value("noTransmitEnd4", &self.info.controls);
                controller.set_no_transmit_zone(&mut self.io, 3, start, end, enabled);
            }
            "noTransmitEnd4" => {
                let start = get_angle_value("noTransmitStart4", &self.info.controls);
                let end = mod_deci_degrees(deci_value);
                controller.set_no_transmit_zone(&mut self.io, 3, start, end, enabled);
            }
            _ => return Err(RadarError::CannotSetControlType(cv.id.clone())),
        }

        Ok(())
    }

    async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        if let Some(controller) = &mut self.controller {
            controller.send_report_requests(&mut self.io);
        }
        self.report_request_timeout += REPORT_REQUEST_INTERVAL;
        Ok(())
    }

    async fn send_info_requests(&mut self) -> Result<(), RadarError> {
        if let Some(info_sender) = &mut self.info_sender {
            info_sender.send_info_requests().await?;
        }
        self.info_request_timeout += INFO_BY_US_INTERVAL;
        Ok(())
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        // Initialize controller sockets (command socket for sending commands to radar)
        if let Some(controller) = &mut self.controller {
            controller.poll(&mut self.io);
            log::debug!("{}: Controller initialized", self.key);
        }

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

    fn set(&mut self, control_type: &str, value: f32, auto: Option<bool>) {
        match self.info.controls.set(control_type, value, auto) {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {} auto {:?} enabled {:?}",
                        self.key,
                        control_type,
                        control.value(),
                        control.auto,
                        control.enabled
                    );
                }
            }
            Ok(None) => {}
        };
    }

    fn set_value(&mut self, control_type: &str, value: f32) {
        self.set(control_type, value, None)
    }

    fn set_value_auto(&mut self, control_type: &str, value: f32, auto: u8) {
        self.set(control_type, value, Some(auto > 0))
    }

    fn set_value_with_many_auto(
        &mut self,
        control_type: &str,
        value: f32,
        auto_value: f32,
    ) {
        match self
            .info
            .controls
            .set_value_with_many_auto(control_type, value, auto_value)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::debug!(
                        "{}: Control '{}' new value {} auto_value {:?} auto {:?}",
                        self.key,
                        control_type,
                        control.value(),
                        control.auto_value,
                        control.auto
                    );
                }
            }
            Ok(None) => {}
        };
    }

    fn set_string(&mut self, control: &str, value: String) {
        match self.info.controls.set_string(control, value) {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(v)) => {
                log::debug!("{}: Control '{}' new value '{}'", self.key, control, v);
            }
            Ok(None) => {}
        };
    }

    // If range detection is in progress, go to the next range
    async fn process_range(&mut self, range: i32) -> Result<(), RadarError> {
        let range = range / 10;
        if self.info.ranges.len() == 0 && self.info.range_detection.is_none() && !self.replay {
            if let Some(status) = self.info.controls.get_status() {
                if status == Status::Transmit {
                    log::warn!(
                        "{}: No ranges available, but radar is transmitting, standby during range detection",
                        self.key
                    );
                    self.send_status(Status::Standby).await?;
                    self.transmit_after_range_detection = true;
                }
            } else {
                log::warn!(
                    "{}: No ranges available and no radar status found, cannot start range detection",
                    self.key
                );
                return Ok(());
            }
            if let Some(control) = self.info.controls.get("range") {
                self.info.range_detection = Some(RangeDetection::new_for_brand(
                    self.key.clone(),
                    mayara_core::Brand::Navico,
                    50,
                    control.item().max_value.unwrap() as i32,
                ));
            }
        }

        if let Some(range_detection) = &mut self.info.range_detection {
            match range_detection.found_range(range) {
                RangeDetectionResult::NoRange => {
                    return Ok(());
                }
                RangeDetectionResult::Complete(ranges, saved_range) => {
                    self.info.ranges = ranges.clone();
                    self.info
                        .controls
                        .set_valid_ranges("range", &ranges)?;
                    self.info.range_detection = None;
                    self.range_timeout = Instant::now() + FAR_FUTURE;

                    self.radars.update(&self.info);

                    self.send_range(saved_range).await?;
                    if self.transmit_after_range_detection {
                        self.transmit_after_range_detection = false;
                        self.send_status(Status::Transmit).await?;
                    }
                }
                RangeDetectionResult::NextRange(r) => {
                    self.range_timeout = Instant::now() + RANGE_DETECTION_INTERVAL;

                    self.send_range(r).await?;
                }
            }
        }

        Ok(())
    }

    async fn send_status(&mut self, status: Status) -> Result<(), RadarError> {
        if let Some(controller) = &mut self.controller {
            let transmit = status == Status::Transmit;
            controller.set_power(&mut self.io, transmit);
        }
        Ok(())
    }

    async fn send_range(&mut self, range: i32) -> Result<(), RadarError> {
        if let Some(controller) = &mut self.controller {
            // Range is in decimeters (range * 10)
            controller.set_range(&mut self.io, range * 10);
        }
        Ok(())
    }

    fn process_info(&mut self, addr: &SocketAddr) {
        if let SocketAddr::V4(addr) = addr {
            if addr.ip() == &self.info.nic_addr {
                log::trace!("{}: Ignoring info from ourselves ({})", self.key, addr);
            } else {
                log::trace!("{}: {} is sending information updates", self.key, addr);
                self.info_request_timeout = Instant::now() + INFO_BY_OTHERS_TIMEOUT;

                if self.info_buf.len() >= ::core::mem::size_of::<HaloNavigationPacket>() {
                    if self.info_buf[36] == 0x02 {
                        if let Ok(report) = HaloNavigationPacket::transmute(&self.info_buf) {
                            let sog = u16::from_le_bytes(report.sog) as f64 * 0.01 * MS_TO_KN;
                            let cog = u16::from_le_bytes(report.cog) as f64 * 360.0 / 63488.0;
                            log::debug!(
                                "{}: Halo sog={sog} cog={cog} from navigation report {:?}",
                                self.key,
                                report
                            );
                        }
                    } else {
                        if let Ok(report) = HaloHeadingPacket::transmute(&self.info_buf) {
                            log::debug!("{}: Halo heading report {:?}", self.key, report);
                        }
                    }
                }
            }
        }
    }

    fn process_speed(&mut self, addr: &SocketAddr) {
        if let SocketAddr::V4(addr) = addr {
            if addr.ip() != &self.info.nic_addr {
                if let Ok(report) = HaloSpeedPacket::transmute(&self.speed_buf) {
                    log::debug!("{}: Halo speed report {:?}", self.key, report);
                }
            }
        }
    }

    async fn process_report(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;

        if data.len() < 2 {
            bail!("UDP report len {} dropped", data.len());
        }

        if data[1] != 0xc4 {
            if data[1] == 0xc6 {
                match data[0] {
                    0x11 => {
                        if data.len() != 3 || data[2] != 0 {
                            bail!("Strange content of report 0x0a 0xc6: {:02X?}", data);
                        }
                        // this is just a response to the MFD sending 0x0a 0xc2,
                        // not sure what purpose it serves.
                    }
                    _ => {
                        log::trace!("Unknown report 0x{:02x} 0xc6: {:02X?}", data[0], data);
                    }
                }
            } else {
                log::trace!("Unknown report {:02X?} dropped", data)
            }
            return Ok(());
        }
        let report_identification = data[0];
        match report_identification {
            REPORT_01_C4_18 => {
                return self.process_report_01().await;
            }
            REPORT_02_C4_99 => {
                if self.model != Model::Unknown {
                    return self.process_report_02().await;
                }
            }
            REPORT_03_C4_129 => {
                return self.process_report_03().await;
            }
            REPORT_04_C4_66 => {
                return self.process_report_04().await;
            }
            REPORT_06_C4_68 => {
                if self.model != Model::Unknown {
                    if data.len() == 68 {
                        return self.process_report_06_68().await;
                    }
                    return self.process_report_06_74().await;
                }
            }
            REPORT_08_C4_18_OR_21_OR_22 => {
                if self.model != Model::Unknown {
                    return self.process_report_08().await;
                }
            }
            _ => {
                if !self.reported_unknown[report_identification as usize] {
                    self.reported_unknown[report_identification as usize] = true;
                    log::trace!(
                        "Unknown report identification {} len {} data {:02X?} dropped",
                        report_identification,
                        data.len(),
                        data
                    );
                }
            }
        }
        Ok(())
    }

    async fn process_report_01(&mut self) -> Result<(), Error> {
        // Use mayara-core parsing
        let status = parse_report_01(&self.report_buf)
            .map_err(|e| anyhow::anyhow!("{}: Report 01 parse error: {}", self.key, e))?;

        log::debug!("{}: report 01 - status {:?}", self.key, status);

        // Convert mayara_core::protocol::navico::Status to crate::radar::Status
        let status = match status {
            mayara_core::protocol::navico::Status::Off => Status::Off,
            mayara_core::protocol::navico::Status::Standby => Status::Standby,
            mayara_core::protocol::navico::Status::Transmit => Status::Transmit,
            mayara_core::protocol::navico::Status::Preparing => Status::Preparing,
        };
        self.set_value("power", status as i32 as f32);
        Ok(())
    }

    async fn process_report_02(&mut self) -> Result<(), Error> {
        // Use mayara-core parsing
        let report = parse_report_02(&self.report_buf)
            .map_err(|e| anyhow::anyhow!("{}: Report 02 parse error: {}", self.key, e))?;

        log::trace!("{}: report 02 - {:?}", self.key, report);

        let range = report.range;
        let mode = report.mode as i32;
        let gain_auto = if report.gain_auto { 1u8 } else { 0u8 };
        let gain = report.gain as i32;
        let sea_auto = report.sea_auto;
        let sea = report.sea;
        let rain = report.rain as i32;
        let interference_rejection = report.interference_rejection as i32;
        let target_expansion = report.target_expansion as i32;
        let target_boost = report.target_boost as i32;

        self.set_value("range", range as f32);
        if self.model == Model::HALO {
            self.set_value("mode", mode as f32);
        }
        self.set_value_auto("gain", gain as f32, gain_auto);
        if self.model != Model::HALO {
            self.set_value_auto("sea", sea as f32, sea_auto);
        } else {
            self.info
                .controls
                .set_auto_state("sea", sea_auto > 0)
                .unwrap(); // Only crashes if control not supported which would be an internal bug
        }
        self.set_value("rain", rain as f32);
        self.set_value(
            "interferenceRejection",
            interference_rejection as f32,
        );
        self.set_value("targetExpansion", target_expansion as f32);
        self.set_value("targetBoost", target_boost as f32);

        self.process_range(range).await?;

        Ok(())
    }

    async fn process_report_03(&mut self) -> Result<(), Error> {
        // Use mayara-core parsing
        let report = parse_report_03(&self.report_buf)
            .map_err(|e| anyhow::anyhow!("{}: Report 03 parse error: {}", self.key, e))?;

        log::trace!("{}: report 03 - {:?}", self.key, report);

        let model_raw = report.model_byte;
        let hours = report.operating_hours as i32;

        // Model is already the core Model type (now used directly)
        let model = report.model;

        match model {
            Model::Unknown => {
                if !self.reported_unknown[model_raw as usize] {
                    self.reported_unknown[model_raw as usize] = true;
                    log::error!("{}: Unknown radar model 0x{:02x}", self.key, model_raw);
                }
            }
            _ => {
                if self.model != model {
                    log::info!("{}: Radar is model {}", self.key, model);
                    let info2 = self.info.clone();
                    self.model = model;

                    // Update the controller's model
                    if let Some(controller) = &mut self.controller {
                        let core_model = match model {
                            Model::BR24 => NavicoModel::BR24,
                            Model::Gen3 => NavicoModel::Gen3,
                            Model::Gen4 => NavicoModel::Gen4,
                            Model::HALO => NavicoModel::Halo,
                            Model::Unknown => NavicoModel::Unknown,
                        };
                        controller.set_model(core_model);
                    }

                    super::settings::update_when_model_known(
                        &mut self.info.controls,
                        model,
                        &info2,
                    );
                    self.info.set_doppler(model.has_doppler());

                    self.radars.update(&self.info);

                    self.data_tx
                        .send(DataUpdate::Legend(self.info.legend.clone()))?;
                }
            }
        }

        let firmware = format!("{} {}", report.firmware_date, report.firmware_time);
        self.set_value("operatingHours", hours as f32);
        self.set_string("firmwareVersion", firmware);

        Ok(())
    }

    async fn process_report_04(&mut self) -> Result<(), Error> {
        // Use mayara-core parsing
        let report = parse_report_04(&self.report_buf)
            .map_err(|e| anyhow::anyhow!("{}: Report 04 parse error: {}", self.key, e))?;

        log::trace!("{}: report 04 - {:?}", self.key, report);

        self.set_value("bearingAlignment", report.bearing_alignment as f32);
        self.set_value("antennaHeight", report.antenna_height as f32);
        if self.model == Model::HALO {
            self.set_value("accentLight", report.accent_light as f32);
        }

        Ok(())
    }

    ///
    /// Blanking (No Transmit) report as seen on HALO 2006
    ///
    async fn process_report_06_68(&mut self) -> Result<(), Error> {
        // Use mayara-core parsing
        let report = parse_report_06_68(&self.report_buf)
            .map_err(|e| anyhow::anyhow!("{}: Report 06 (68) parse error: {}", self.key, e))?;

        log::trace!("{}: report 06 (68) - {:?}", self.key, report);

        if let Some(name) = &report.name {
            self.set_string("modelName", name.clone());
        }

        for (i, start, end) in super::BLANKING_SETS {
            if i < report.sectors.len() {
                let sector = &report.sectors[i];
                let enabled = Some(sector.enabled);
                self.info
                    .controls
                    .set_value_auto_enabled(&start, sector.start_angle as f32, None, enabled)?;
                self.info
                    .controls
                    .set_value_auto_enabled(&end, sector.end_angle as f32, None, enabled)?;
            }
        }

        Ok(())
    }

    ///
    /// Blanking (No Transmit) report as seen on HALO 24 (Firmware 2023)
    ///
    async fn process_report_06_74(&mut self) -> Result<(), Error> {
        // Use mayara-core parsing
        let report = parse_report_06_74(&self.report_buf)
            .map_err(|e| anyhow::anyhow!("{}: Report 06 (74) parse error: {}", self.key, e))?;

        log::trace!("{}: report 06 (74) - {:?}", self.key, report);

        // self.set_string("modelName", report.name.clone().unwrap_or("".to_string()));
        log::debug!(
            "Radar name '{}' model '{}'",
            report.name.as_deref().unwrap_or("null"),
            self.model
        );

        for (i, start, end) in super::BLANKING_SETS {
            if i < report.sectors.len() {
                let sector = &report.sectors[i];
                let enabled = Some(sector.enabled);
                self.info
                    .controls
                    .set_value_auto_enabled(&start, sector.start_angle as f32, None, enabled)?;
                self.info
                    .controls
                    .set_value_auto_enabled(&end, sector.end_angle as f32, None, enabled)?;
            }
        }

        Ok(())
    }

    async fn process_report_08(&mut self) -> Result<(), Error> {
        // Use mayara-core parsing
        let report = parse_report_08(&self.report_buf)
            .map_err(|e| anyhow::anyhow!("{}: Report 08 parse error: {}", self.key, e))?;

        log::trace!("{}: report 08 - {:?}", self.key, report);

        let sea_state = report.sea_state as i32;
        let local_interference_rejection = report.local_interference_rejection as i32;
        let scan_speed = report.scan_speed as i32;
        let sidelobe_suppression_auto = if report.sidelobe_suppression_auto { 1u8 } else { 0u8 };
        let sidelobe_suppression = report.sidelobe_suppression as i32;
        let noise_reduction = report.noise_rejection as i32;
        let target_sep = report.target_separation as i32;
        let sea_clutter = report.sea_clutter as i32;
        let auto_sea_clutter = report.auto_sea_clutter;

        // Handle Doppler settings if present (extended report)
        if let (Some(doppler_state), Some(doppler_speed)) = (report.doppler_state, report.doppler_speed) {
            let doppler_mode: Result<DopplerMode, _> = doppler_state.try_into();
            match doppler_mode {
                Err(_) => {
                    bail!(
                        "{}: Unknown doppler state {}",
                        self.key,
                        doppler_state
                    );
                }
                Ok(doppler_mode) => {
                    log::debug!(
                        "{}: doppler mode={} speed={}",
                        self.key,
                        doppler_mode,
                        doppler_speed
                    );
                    self.data_tx.send(DataUpdate::Doppler(doppler_mode))?;
                }
            }
            self.set_value("dopplerMode", doppler_state as f32);
            self.set_value("dopplerSpeed", doppler_speed as f32);
        }

        if self.model == Model::HALO {
            self.set_value("seaState", sea_state as f32);
            self.set_value_with_many_auto(
                "sea",
                sea_clutter as f32,
                auto_sea_clutter as f32,
            );
        }
        self.set_value(
            "localInterferenceRejection",
            local_interference_rejection as f32,
        );
        self.set_value("scanSpeed", scan_speed as f32);
        self.set_value_auto(
            "sidelobeSuppression",
            sidelobe_suppression as f32,
            sidelobe_suppression_auto,
        );
        self.set_value("noiseRejection", noise_reduction as f32);
        if self.model.has_dual_range() {
            self.set_value("targetSeparation", target_sep as f32);
        } else if target_sep > 0 {
            log::trace!(
                "{}: Target separation value {} not supported on model {}",
                self.key,
                target_sep,
                self.model
            );
        }

        Ok(())
    }
}
