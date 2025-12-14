use anyhow::{Context, Error};
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

use std::time::Duration;
use tokio::io::WriteHalf;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

// Use mayara-core for parsing (pure, WASM-compatible)
use mayara_core::protocol::furuno::report::{
    parse_report, model_from_modules, version_from_modules,
    FurunoReport, RadarState as CoreRadarState,
};
use mayara_core::protocol::furuno::dispatch::{
    parse_control_response, ControlUpdate as CoreControlUpdate,
};

use super::settings;
use super::RadarModel;
use crate::radar::{RadarError, RadarInfo, Status};
use crate::settings::ControlUpdate;
use crate::Session;

use super::command::Command;

pub struct FurunoReportReceiver {
    session: Session,
    info: RadarInfo,
    key: String,
    command_sender: Command,
    stream: Option<TcpStream>,
    report_request_interval: Duration,
    model_known: bool,
}

impl FurunoReportReceiver {
    pub fn new(session: Session, info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();

        let command_sender = Command::new(&info);

        FurunoReportReceiver {
            session,
            info,
            key,
            command_sender,
            stream: None,
            report_request_interval: Duration::from_millis(5000),
            model_known: false,
        }
    }

    async fn start_stream(&mut self) -> Result<(), RadarError> {
        if self.info.send_command_addr.port() == 0 {
            // Port not set yet, we need to login to the radar first.
            return Err(RadarError::InvalidPort);
        }
        let sock = TcpSocket::new_v4().map_err(|e| RadarError::Io(e))?;
        self.stream = Some(
            sock.connect(std::net::SocketAddr::V4(self.info.send_command_addr))
                .await
                .map_err(|e| RadarError::Io(e))?,
        );
        Ok(())
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.info.command_tx.
    //
    async fn data_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.key);
        let mut command_rx = self.info.control_update_subscribe();

        let stream = self.stream.take().unwrap();
        let (reader, mut writer) = tokio::io::split(stream);
        // self.command_sender.init(&mut writer).await?;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let mut deadline = Instant::now() + self.report_request_interval;
        let mut first_report_received = false;

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(deadline) => {
                    self.command_sender.send_report_requests(&mut writer).await?;
                    deadline = Instant::now() + self.report_request_interval;
                },

                r = reader.read_line(&mut line) => {
                    match r {
                        Ok(len) => {
                            if len > 2 {
                                if let Err(e) = self.process_report(&line) {
                                    log::error!("{}: {}", self.key, e);
                                } else if !first_report_received {
                                    self.command_sender.init(&mut writer).await?;
                                    first_report_received = true;
                                }
                            }
                            line.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = command_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            if let Err(e) = self.process_control_update(&mut writer, cv).await {
                                return Err(e);
                            }
                        },
                    }
                }
            }
        }
    }

    async fn process_control_update(
        &mut self,
        write: &mut WriteHalf<TcpStream>,
        control_update: ControlUpdate,
    ) -> Result<(), RadarError> {
        let cv = control_update.control_value;
        let reply_tx = control_update.reply_tx;

        if let Err(e) = self.command_sender.set_control(write, &cv).await {
            self.info
                .controls
                .send_error_to_client(reply_tx, &cv, &e)
                .await?;
            match &e {
                RadarError::Io(_) => {
                    return Err(e);
                }
                _ => {}
            }
        } else {
            self.info.controls.set_refresh(&cv.id);
        }

        Ok(())
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_stream().await?;
        loop {
            if self.stream.is_some() {
                match self.data_loop(&subsys).await {
                    Err(RadarError::Shutdown) => {
                        return Ok(());
                    }
                    _ => {
                        // Ignore, reopen socket
                    }
                }
                self.stream = None;
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.login_to_radar()?;
                self.start_stream().await?;
            }
        }
    }

    fn login_to_radar(&mut self) -> Result<(), RadarError> {
        // Furuno radars use a single TCP/IP connection to send commands and
        // receive status reports, so report_addr and send_command_addr are identical.
        // Only one of these would be enough for Furuno.
        let port: u16 = match super::login_to_radar(self.session.clone(), self.info.addr) {
            Err(e) => {
                log::error!("{}: Unable to connect for login: {}", self.info.key(), e);
                return Err(RadarError::LoginFailed);
            }
            Ok(p) => p,
        };
        if port != self.info.send_command_addr.port() {
            self.info.send_command_addr.set_port(port);
            self.info.report_addr.set_port(port);
        }
        Ok(())
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
                        "{}: Control '{}' new value {} enabled {:?}",
                        self.key,
                        control_type,
                        control.value(),
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
        match self
            .info
            .controls
            .set_value_auto(control_type, auto > 0, value)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::debug!(
                        "{}: Control '{}' new value {} auto {}",
                        self.key,
                        control_type,
                        control.value(),
                        auto
                    );
                }
            }
            Ok(None) => {}
        };
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

    /// Process a TCP report line using mayara-core parsing
    #[inline(never)]
    fn process_report(&mut self, line: &str) -> Result<(), Error> {
        // First try dispatch parsing for extended controls (beamSharpening, birdMode, etc.)
        // These are $NEE, $NED, $NEF, $N67 format responses
        if let Some(update) = parse_control_response(line) {
            log::trace!("{}: dispatch parsed control update", self.key);
            self.apply_control_update(update);
            return Ok(());
        }

        // Fall back to FurunoReport parsing for base controls and other reports
        let report = match parse_report(line) {
            Ok(r) => r,
            Err(e) => {
                log::debug!("{}: Failed to parse report: {}", self.key, e);
                return Ok(()); // Ignore unparseable reports
            }
        };

        log::trace!("{}: parsed report {:?}", self.key, report);

        // Apply the parsed report to server state
        self.apply_report(report)
    }

    /// Apply a parsed FurunoReport to server state
    #[inline(never)]
    fn apply_report(&mut self, report: FurunoReport) -> Result<(), Error> {
        match report {
            FurunoReport::Status(s) => {
                let generic_state = match s.state {
                    CoreRadarState::Preparing => Status::Preparing,
                    CoreRadarState::Standby => Status::Standby,
                    CoreRadarState::Transmit => Status::Transmit,
                    CoreRadarState::Off => Status::Off,
                };
                self.set_value("power", generic_state as i32 as f32);
            }

            FurunoReport::Gain(g) => {
                log::trace!(
                    "Gain: {} auto {} auto_value={}",
                    g.value,
                    g.auto,
                    g.auto_value
                );
                self.set_value_auto("gain", g.value, if g.auto { 1 } else { 0 });
            }

            FurunoReport::Sea(s) => {
                self.set_value_auto("sea", s.value, if s.auto { 1 } else { 0 });
            }

            FurunoReport::Rain(r) => {
                self.set_value_auto("rain", r.value, if r.auto { 1 } else { 0 });
            }

            FurunoReport::Range(r) => {
                let range = self.info.ranges.all.get(r.range_index).with_context(|| {
                    format!(
                        "Range index {} out of bounds for ranges {}",
                        r.range_index, self.info.ranges,
                    )
                })?;
                self.set_value("range", range.distance() as f32);
            }

            FurunoReport::OnTime(o) => {
                self.set_value("operatingHours", o.hours);
            }

            FurunoReport::Modules(m) => {
                self.handle_modules_report(&m);
            }

            FurunoReport::AliveCheck => {
                // No action needed for keepalive response
            }

            FurunoReport::CustomPictureAll(_) => {
                // TODO: Handle custom picture settings
                log::trace!("{}: CustomPictureAll received (not yet implemented)", self.key);
            }

            FurunoReport::AntennaType(_) => {
                // TODO: Handle antenna type
                log::trace!("{}: AntennaType received (not yet implemented)", self.key);
            }

            FurunoReport::BlindSector(b) => {
                // TODO: Apply blind sector settings
                log::trace!("{}: BlindSector received: {:?} (not yet implemented)", self.key, b);
            }

            FurunoReport::MainBangSize(m) => {
                // Convert 0-255 to 0-100%
                let percent = (m.value * 100) / 255;
                self.set_value("mainBangSuppression", percent as f32);
            }

            FurunoReport::AntennaHeight(h) => {
                self.set_value("antennaHeight", h.meters as f32);
            }

            FurunoReport::NearSTC(v) => {
                log::trace!("{}: NearSTC = {} (not yet implemented)", self.key, v);
            }

            FurunoReport::MiddleSTC(v) => {
                log::trace!("{}: MiddleSTC = {} (not yet implemented)", self.key, v);
            }

            FurunoReport::FarSTC(v) => {
                log::trace!("{}: FarSTC = {} (not yet implemented)", self.key, v);
            }

            FurunoReport::WakeUpCount(v) => {
                log::trace!("{}: WakeUpCount = {} (not yet implemented)", self.key, v);
            }

            FurunoReport::Unknown { command_id, values } => {
                log::debug!(
                    "{}: Unknown command {:02X} with values {:?}",
                    self.key,
                    command_id,
                    values
                );
            }
        }
        Ok(())
    }

    /// Apply a control update from dispatch parsing (for extended controls)
    #[inline(never)]
    fn apply_control_update(&mut self, update: CoreControlUpdate) {
        match update {
            CoreControlUpdate::Power(transmitting) => {
                let value = if transmitting { Status::Transmit } else { Status::Standby };
                self.set_value("power", value as i32 as f32);
            }
            CoreControlUpdate::Range(range_index) => {
                if let Some(range) = self.info.ranges.all.get(range_index as usize) {
                    self.set_value("range", range.distance() as f32);
                }
            }
            CoreControlUpdate::Gain { auto, value } => {
                self.set_value_auto("gain", value as f32, if auto { 1 } else { 0 });
            }
            CoreControlUpdate::Sea { auto, value } => {
                self.set_value_auto("sea", value as f32, if auto { 1 } else { 0 });
            }
            CoreControlUpdate::Rain { auto, value } => {
                self.set_value_auto("rain", value as f32, if auto { 1 } else { 0 });
            }
            CoreControlUpdate::NoiseReduction(enabled) => {
                self.set_value("noiseReduction", if enabled { 1.0 } else { 0.0 });
            }
            CoreControlUpdate::InterferenceRejection(enabled) => {
                self.set_value("interferenceRejection", if enabled { 1.0 } else { 0.0 });
            }
            CoreControlUpdate::BeamSharpening(level) => {
                self.set_value("beamSharpening", level as f32);
            }
            CoreControlUpdate::BirdMode(level) => {
                self.set_value("birdMode", level as f32);
            }
            CoreControlUpdate::DopplerMode { enabled, mode } => {
                // Store as compound: enabled flag and mode value
                self.set("dopplerMode", mode as f32, Some(enabled));
            }
            CoreControlUpdate::ScanSpeed(mode) => {
                self.set_value("scanSpeed", mode as f32);
            }
            CoreControlUpdate::MainBangSuppression(percent) => {
                self.set_value("mainBangSuppression", percent as f32);
            }
            CoreControlUpdate::TxChannel(channel) => {
                self.set_value("txChannel", channel as f32);
            }
            CoreControlUpdate::BlindSector(state) => {
                // Apply all four sector values (end is calculated from start + width)
                self.set_value("noTransmitStart1", state.sector1_start as f32);
                self.set_value("noTransmitEnd1", state.sector1_end() as f32);
                self.set_value("noTransmitStart2", state.sector2_start as f32);
                self.set_value("noTransmitEnd2", state.sector2_end() as f32);
            }
            CoreControlUpdate::OperatingTime(seconds) => {
                self.set_value("operatingHours", seconds as f32 / 3600.0);
            }
        }
    }

    /// Handle the Modules report using mayara-core model lookup
    fn handle_modules_report(&mut self, modules: &mayara_core::protocol::furuno::report::ModulesReport) {
        if self.model_known {
            return;
        }
        self.model_known = true;

        // Use mayara-core's firmware_to_model mapping
        let core_model = model_from_modules(modules);
        let version = version_from_modules(modules).unwrap_or_default();

        // Convert core Model to server RadarModel
        let model = Self::core_model_to_radar_model(core_model);

        log::info!(
            "{}: Radar model {} version {}",
            self.key,
            model.to_str(),
            version
        );
        settings::update_when_model_known(&mut self.info, model, &version);
        self.command_sender.set_ranges(self.info.ranges.clone());
    }

    /// Convert mayara-core Model to server RadarModel
    fn core_model_to_radar_model(core: mayara_core::protocol::furuno::Model) -> RadarModel {
        use mayara_core::protocol::furuno::Model as CoreModel;
        match core {
            CoreModel::Unknown => RadarModel::Unknown,
            CoreModel::FAR21x7 => RadarModel::FAR21x7,
            CoreModel::DRS => RadarModel::DRS,
            CoreModel::FAR14x7 => RadarModel::FAR14x7,
            CoreModel::DRS4DL => RadarModel::DRS4DL,
            CoreModel::FAR3000 => RadarModel::FAR3000,
            CoreModel::DRS4DNXT => RadarModel::DRS4DNXT,
            CoreModel::DRS6ANXT => RadarModel::DRS6ANXT,
            CoreModel::DRS6AXCLASS => RadarModel::DRS6AXCLASS,
            CoreModel::FAR15x3 => RadarModel::FAR15x3,
            CoreModel::FAR14x6 => RadarModel::FAR14x6,
            CoreModel::DRS12ANXT => RadarModel::DRS12ANXT,
            CoreModel::DRS25ANXT => RadarModel::DRS25ANXT,
        }
    }

}
