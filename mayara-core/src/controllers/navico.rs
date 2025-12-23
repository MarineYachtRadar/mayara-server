//! Navico Radar UDP Controller
//!
//! Platform-independent controller for Navico radars (BR24, 3G, 4G, HALO) using the
//! [`IoProvider`] trait. All communication is via UDP multicast.
//!
//! # Protocol
//!
//! Navico radars use UDP multicast for all communication:
//! - Commands: Sent to radar's command multicast address
//! - Reports: Received on report multicast address
//! - Data: Received on spoke data multicast address
//!
//! # Models
//!
//! | Model | Max Range | Doppler | Features |
//! |-------|-----------|---------|----------|
//! | BR24 | 24 NM | No | Legacy |
//! | 3G | 36 NM | No | Gen3 |
//! | 4G | 48 NM | No | Gen4 |
//! | HALO | 96 NM | Yes | Advanced |

use crate::io::{IoProvider, UdpSocketHandle};
use crate::protocol::navico;

/// Navico radar model
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NavicoModel {
    /// Unknown model - will be detected from Report 03
    /// Behaves like Gen4 for command compatibility
    #[default]
    Unknown,
    BR24,
    Gen3,
    Gen4,
    Halo,
}

impl NavicoModel {
    /// Check if this is a HALO model (has Doppler, accent light, etc.)
    pub fn is_halo(&self) -> bool {
        matches!(self, NavicoModel::Halo)
    }

    /// Check if model is known (not Unknown)
    pub fn is_known(&self) -> bool {
        !matches!(self, NavicoModel::Unknown)
    }
}

/// Controller state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavicoControllerState {
    /// Not initialized
    Disconnected,
    /// Sockets created, waiting for reports
    Listening,
    /// Receiving reports, ready for commands
    Connected,
}

/// Navico radar UDP controller
///
/// Manages UDP multicast communication for Navico radars.
pub struct NavicoController {
    /// Radar ID (for logging)
    radar_id: String,
    /// Radar addresses from beacon
    command_addr: String,
    command_port: u16,
    report_addr: String,
    report_port: u16,
    /// NIC address to bind to (ensures packets go out correct interface)
    nic_addr: String,
    /// Command socket
    command_socket: Option<UdpSocketHandle>,
    /// Report socket
    report_socket: Option<UdpSocketHandle>,
    /// Current state
    state: NavicoControllerState,
    /// Radar model
    model: NavicoModel,
    /// Poll count
    poll_count: u64,
    /// Last report request time
    last_report_request: u64,
    /// Last stay-on command time
    last_stay_on: u64,
}

impl NavicoController {
    /// Report request interval (poll counts, ~5 seconds at 10Hz)
    const REPORT_REQUEST_INTERVAL: u64 = 50;
    /// Stay-on command interval (poll counts, ~1 second)
    const STAY_ON_INTERVAL: u64 = 10;

    /// Create a new Navico controller
    pub fn new(
        radar_id: &str,
        command_addr: &str,
        command_port: u16,
        report_addr: &str,
        report_port: u16,
        nic_addr: &str,
        model: NavicoModel,
    ) -> Self {
        Self {
            radar_id: radar_id.to_string(),
            command_addr: command_addr.to_string(),
            command_port,
            report_addr: report_addr.to_string(),
            report_port,
            nic_addr: nic_addr.to_string(),
            command_socket: None,
            report_socket: None,
            state: NavicoControllerState::Disconnected,
            model,
            poll_count: 0,
            last_report_request: 0,
            last_stay_on: 0,
        }
    }

    /// Get current state
    pub fn state(&self) -> NavicoControllerState {
        self.state
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.state == NavicoControllerState::Connected
    }

    /// Get radar model
    pub fn model(&self) -> NavicoModel {
        self.model
    }

    /// Set radar model (called when model is detected from reports)
    pub fn set_model(&mut self, model: NavicoModel) {
        self.model = model;
    }

    /// Poll the controller
    pub fn poll<I: IoProvider>(&mut self, io: &mut I) -> bool {
        self.poll_count += 1;

        match self.state {
            NavicoControllerState::Disconnected => {
                self.start_sockets(io);
                true
            }
            NavicoControllerState::Listening | NavicoControllerState::Connected => {
                self.poll_connected(io)
            }
        }
    }

    fn start_sockets<I: IoProvider>(&mut self, io: &mut I) {
        // Create command socket bound to the correct NIC
        match io.udp_create() {
            Ok(socket) => {
                // Bind to NIC address to ensure packets go out the correct interface
                if io.udp_bind_interface(&socket, &self.nic_addr).is_ok() {
                    self.command_socket = Some(socket);
                    io.debug(&format!(
                        "[{}] Command socket created for {}:{} via {}",
                        self.radar_id, self.command_addr, self.command_port, self.nic_addr
                    ));
                } else {
                    io.debug(&format!(
                        "[{}] Failed to bind command socket to {}, falling back to any interface",
                        self.radar_id, self.nic_addr
                    ));
                    // Fallback to any interface if binding to NIC fails
                    if io.udp_bind(&socket, 0).is_ok() {
                        self.command_socket = Some(socket);
                        io.debug(&format!(
                            "[{}] Command socket created for {}:{} (fallback)",
                            self.radar_id, self.command_addr, self.command_port
                        ));
                    } else {
                        io.udp_close(socket);
                    }
                }
            }
            Err(e) => {
                io.debug(&format!(
                    "[{}] Failed to create command socket: {}",
                    self.radar_id, e
                ));
            }
        }

        // Create report socket
        match io.udp_create() {
            Ok(socket) => {
                if io.udp_bind(&socket, self.report_port).is_ok() {
                    if io
                        .udp_join_multicast(&socket, &self.report_addr, "")
                        .is_ok()
                    {
                        self.report_socket = Some(socket);
                        io.debug(&format!(
                            "[{}] Joined report multicast {}:{}",
                            self.radar_id, self.report_addr, self.report_port
                        ));
                        self.state = NavicoControllerState::Listening;
                    } else {
                        io.debug(&format!(
                            "[{}] Failed to join report multicast",
                            self.radar_id
                        ));
                        io.udp_close(socket);
                    }
                } else {
                    io.debug(&format!("[{}] Failed to bind report socket", self.radar_id));
                    io.udp_close(socket);
                }
            }
            Err(e) => {
                io.debug(&format!(
                    "[{}] Failed to create report socket: {}",
                    self.radar_id, e
                ));
            }
        }
    }

    fn poll_connected<I: IoProvider>(&mut self, io: &mut I) -> bool {
        let mut activity = false;

        // Process incoming reports
        if let Some(socket) = self.report_socket {
            let mut buf = [0u8; 2048];
            while let Some((len, _addr, _port)) = io.udp_recv_from(&socket, &mut buf) {
                self.process_report(io, &buf[..len]);
                activity = true;
                if self.state == NavicoControllerState::Listening {
                    self.state = NavicoControllerState::Connected;
                }
            }
        }

        // Send periodic report requests
        if self.poll_count - self.last_report_request > Self::REPORT_REQUEST_INTERVAL {
            self.request_reports(io);
            self.last_report_request = self.poll_count;
        }

        // Send stay-on command
        if self.poll_count - self.last_stay_on > Self::STAY_ON_INTERVAL {
            self.stay_on(io);
            self.last_stay_on = self.poll_count;
        }

        activity
    }

    fn process_report<I: IoProvider>(&mut self, io: &I, data: &[u8]) {
        if data.len() < 2 {
            return;
        }

        // Report type is in first two bytes
        let report_type = (data[1] as u16) << 8 | data[0] as u16;
        io.debug(&format!(
            "[{}] Report type: 0x{:04X}, len: {}",
            self.radar_id,
            report_type,
            data.len()
        ));

        // Parse based on report type
        // 0x01C4 = Report 01 (Status)
        // 0x02C4 = Report 02 (Settings)
        // 0x03C4 = Report 03 (Model)
        // etc.
    }

    fn request_reports<I: IoProvider>(&self, io: &mut I) {
        // Request report 03 (model/firmware)
        self.send_command(io, &navico::REQUEST_03_REPORT);
        // Request multiple reports
        self.send_command(io, &navico::REQUEST_MANY2_REPORT);
    }

    fn stay_on<I: IoProvider>(&self, io: &mut I) {
        self.send_command(io, &navico::COMMAND_STAY_ON_A);
    }

    fn send_command<I: IoProvider>(&self, io: &mut I, data: &[u8]) {
        if let Some(socket) = self.command_socket {
            if let Err(e) = io.udp_send_to(&socket, data, &self.command_addr, self.command_port) {
                io.debug(&format!(
                    "[{}] Failed to send command: {}",
                    self.radar_id, e
                ));
            }
        } else {
            io.debug(&format!(
                "[{}] WARNING: No command socket - command dropped!",
                self.radar_id
            ));
        }
    }

    // Control methods

    /// Set power state (transmit/standby)
    ///
    /// Navico requires a two-part command sequence:
    /// 1. `00 C1 01` - Prepare for status change
    /// 2. `01 C1 XX` - Execute (XX = 00 for standby, 01 for transmit)
    pub fn set_power<I: IoProvider>(&mut self, io: &mut I, transmit: bool) {
        // Part 1: Prepare for status change
        let prepare_cmd = [0x00, 0xC1, 0x01];
        self.send_command(io, &prepare_cmd);

        // Part 2: Execute the state change
        let execute_cmd = [0x01, 0xC1, if transmit { 0x01 } else { 0x00 }];
        self.send_command(io, &execute_cmd);

        io.debug(&format!(
            "[{}] Set power: {} (sent prepare + execute)",
            self.radar_id,
            if transmit { "transmit" } else { "standby" }
        ));
    }

    /// Set range in decimeters
    pub fn set_range<I: IoProvider>(&mut self, io: &mut I, range_dm: i32) {
        let mut cmd = vec![0x03, 0xC1];
        cmd.extend_from_slice(&range_dm.to_le_bytes());
        io.info(&format!(
            "[{}] Set range: {} dm (cmd: {:02X?})",
            self.radar_id, range_dm, cmd
        ));
        self.send_command(io, &cmd);
    }

    /// Set gain (0-255 scale)
    pub fn set_gain<I: IoProvider>(&mut self, io: &mut I, value: u8, auto: bool) {
        let auto_val: u32 = if auto { 1 } else { 0 };
        let mut cmd = vec![0x06, 0xC1, 0x00, 0x00, 0x00, 0x00];
        cmd.extend_from_slice(&auto_val.to_le_bytes());
        cmd.push(value);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set gain: {} auto={}",
            self.radar_id, value, auto
        ));
    }

    /// Set sea clutter (0-255 scale)
    pub fn set_sea<I: IoProvider>(&mut self, io: &mut I, value: u8, auto: bool) {
        // Different command for HALO vs older models
        if self.model.is_halo() {
            let auto_val: u32 = if auto { 1 } else { 0 };
            let mut cmd = vec![0x11, 0xC1];
            cmd.extend_from_slice(&auto_val.to_le_bytes());
            cmd.push(value);
            self.send_command(io, &cmd);
        } else {
            let auto_val: u32 = if auto { 1 } else { 0 };
            let mut cmd = vec![0x06, 0xC1, 0x02, 0x00, 0x00, 0x00];
            cmd.extend_from_slice(&auto_val.to_le_bytes());
            cmd.push(value);
            self.send_command(io, &cmd);
        }
        io.debug(&format!(
            "[{}] Set sea: {} auto={}",
            self.radar_id, value, auto
        ));
    }

    /// Set rain clutter (0-255 scale)
    pub fn set_rain<I: IoProvider>(&mut self, io: &mut I, value: u8) {
        let cmd = vec![
            0x06, 0xC1, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, value,
        ];
        self.send_command(io, &cmd);
        io.debug(&format!("[{}] Set rain: {}", self.radar_id, value));
    }

    /// Set interference rejection (0-3)
    pub fn set_interference_rejection<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        let cmd = [0x08, 0xC1, level];
        self.send_command(io, &cmd);
        io.debug(&format!("[{}] Set IR: {}", self.radar_id, level));
    }

    /// Set target expansion (0-2)
    pub fn set_target_expansion<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        let cmd_id = if self.model.is_halo() { 0x12 } else { 0x09 };
        let cmd = [cmd_id, 0xC1, level];
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set target expansion: {}",
            self.radar_id, level
        ));
    }

    /// Set target boost (0-2)
    pub fn set_target_boost<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        let cmd = [0x0A, 0xC1, level];
        self.send_command(io, &cmd);
        io.debug(&format!("[{}] Set target boost: {}", self.radar_id, level));
    }

    /// Set scan speed (0=Off/Normal, 1=Medium, 2=Medium-High)
    pub fn set_scan_speed<I: IoProvider>(&mut self, io: &mut I, speed: u8) {
        let cmd = [0x0F, 0xC1, speed.min(2)];
        self.send_command(io, &cmd);
        io.debug(&format!("[{}] Set scan speed: {}", self.radar_id, speed));
    }

    /// Set bearing alignment in deci-degrees
    pub fn set_bearing_alignment<I: IoProvider>(&mut self, io: &mut I, deci_degrees: i16) {
        let mut cmd = vec![0x05, 0xC1];
        cmd.extend_from_slice(&deci_degrees.to_le_bytes());
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set bearing alignment: {}",
            self.radar_id, deci_degrees
        ));
    }

    /// Set antenna height in decimeters (1/10 meter)
    ///
    /// The wire protocol (0x30 0xC1) expects height in decimeters.
    /// Note: Report 04 returns height in millimeters, but the command uses decimeters.
    pub fn set_antenna_height<I: IoProvider>(&mut self, io: &mut I, height_dm: u16) {
        let mut cmd = vec![0x30, 0xC1, 0x01, 0x00, 0x00, 0x00];
        cmd.extend_from_slice(&height_dm.to_le_bytes());
        cmd.extend_from_slice(&[0x00, 0x00]);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set antenna height: {} dm ({} m)",
            self.radar_id,
            height_dm,
            height_dm as f32 / 10.0
        ));
    }

    /// Set doppler mode (HALO only, 0=off, 1=normal, 2=approaching)
    pub fn set_doppler_mode<I: IoProvider>(&mut self, io: &mut I, mode: u8) {
        if self.model.is_halo() {
            let cmd = [0x23, 0xC1, mode];
            self.send_command(io, &cmd);
            io.debug(&format!("[{}] Set doppler mode: {}", self.radar_id, mode));
        }
    }

    /// Set doppler speed threshold (HALO only)
    pub fn set_doppler_speed<I: IoProvider>(&mut self, io: &mut I, speed: u16) {
        if self.model.is_halo() {
            let mut cmd = vec![0x24, 0xC1];
            cmd.extend_from_slice(&speed.to_le_bytes());
            self.send_command(io, &cmd);
            io.debug(&format!("[{}] Set doppler speed: {}", self.radar_id, speed));
        }
    }

    /// Set mode (HALO only, 0-3)
    pub fn set_mode<I: IoProvider>(&mut self, io: &mut I, mode: u8) {
        if self.model.is_halo() {
            let cmd = [0x10, 0xC1, mode];
            self.send_command(io, &cmd);
            io.debug(&format!("[{}] Set mode: {}", self.radar_id, mode));
        }
    }

    /// Set sidelobe suppression (0-255 scale)
    pub fn set_sidelobe_suppression<I: IoProvider>(&mut self, io: &mut I, value: u8, auto: bool) {
        let auto_val: u8 = if auto { 1 } else { 0 };
        let cmd = [
            0x06, 0xC1, 0x05, 0x00, 0x00, 0x00, auto_val, 0x00, 0x00, 0x00, value,
        ];
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set sidelobe suppression: {} auto={}",
            self.radar_id, value, auto
        ));
    }

    /// Set sea state (HALO only, 0=calm, 1=moderate, 2=rough)
    pub fn set_sea_state<I: IoProvider>(&mut self, io: &mut I, state: u8) {
        let cmd = [0x0B, 0xC1, state];
        self.send_command(io, &cmd);
        io.debug(&format!("[{}] Set sea state: {}", self.radar_id, state));
    }

    /// Set local interference rejection (0-3)
    pub fn set_local_interference_rejection<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        let cmd = [0x0E, 0xC1, level];
        self.send_command(io, &cmd);
        io.debug(&format!("[{}] Set local IR: {}", self.radar_id, level));
    }

    /// Set noise rejection (0-3)
    pub fn set_noise_rejection<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        let cmd = [0x21, 0xC1, level];
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set noise rejection: {}",
            self.radar_id, level
        ));
    }

    /// Set target separation (0-3)
    pub fn set_target_separation<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        let cmd = [0x22, 0xC1, level];
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set target separation: {}",
            self.radar_id, level
        ));
    }

    /// Set accent light (HALO only, 0-3)
    pub fn set_accent_light<I: IoProvider>(&mut self, io: &mut I, level: u8) {
        if self.model.is_halo() {
            let cmd = [0x31, 0xC1, level];
            self.send_command(io, &cmd);
            io.debug(&format!("[{}] Set accent light: {}", self.radar_id, level));
        }
    }

    /// Set no-transmit zone (sector 0-3)
    /// Start and end angles are in deci-degrees (0-3599)
    pub fn set_no_transmit_zone<I: IoProvider>(
        &mut self,
        io: &mut I,
        sector: u8,
        start_angle: i16,
        end_angle: i16,
        enabled: bool,
    ) {
        let enabled_val: u8 = if enabled { 1 } else { 0 };

        // Send enable/disable command first
        let cmd1 = [0x0D, 0xC1, sector, 0x00, 0x00, 0x00, enabled_val];
        self.send_command(io, &cmd1);

        // Send zone angles
        let mut cmd2 = vec![0xC0, 0xC1, sector, 0x00, 0x00, 0x00, enabled_val];
        cmd2.extend_from_slice(&start_angle.to_le_bytes());
        cmd2.extend_from_slice(&end_angle.to_le_bytes());
        self.send_command(io, &cmd2);

        io.debug(&format!(
            "[{}] Set no-transmit zone {}: {}° to {}° enabled={}",
            self.radar_id,
            sector,
            start_angle as f32 / 10.0,
            end_angle as f32 / 10.0,
            enabled
        ));
    }

    /// Send report requests to the radar
    pub fn send_report_requests<I: IoProvider>(&mut self, io: &mut I) {
        self.send_command(io, &navico::REQUEST_03_REPORT);
        self.send_command(io, &navico::REQUEST_MANY2_REPORT);
        self.send_command(io, &navico::COMMAND_STAY_ON_A);
    }

    /// Shutdown the controller
    pub fn shutdown<I: IoProvider>(&mut self, io: &mut I) {
        io.debug(&format!("[{}] Shutting down", self.radar_id));
        if let Some(socket) = self.command_socket.take() {
            io.udp_close(socket);
        }
        if let Some(socket) = self.report_socket.take() {
            io.udp_close(socket);
        }
        self.state = NavicoControllerState::Disconnected;
    }
}
