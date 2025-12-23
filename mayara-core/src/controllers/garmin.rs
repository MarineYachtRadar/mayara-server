//! Garmin Radar UDP Controller
//!
//! Platform-independent controller for Garmin xHD series radars using the
//! [`IoProvider`] trait. All communication is via UDP.
//!
//! # Protocol
//!
//! Garmin radars use UDP for all communication:
//! - Reports: Received on multicast 239.254.2.0:50100
//! - Data: Received on multicast 239.254.2.0:50102
//! - Commands: Sent to radar IP on port 50101
//!
//! # Command Format
//!
//! Commands use an 8-byte header plus 4-byte value:
//! ```text
//! [4 bytes] packet type (LE u32)
//! [4 bytes] data length (LE u32, always 4)
//! [4 bytes] value (LE u32)
//! ```

use crate::io::{IoProvider, UdpSocketHandle};
use crate::protocol::garmin;

/// Controller state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GarminControllerState {
    /// Not initialized
    Disconnected,
    /// Sockets created, waiting for reports
    Listening,
    /// Receiving reports, ready for commands
    Connected,
}

/// Garmin radar UDP controller
///
/// Manages UDP communication for Garmin xHD radars.
pub struct GarminController {
    /// Radar ID (for logging)
    radar_id: String,
    /// Radar IP address (for commands)
    radar_addr: String,
    /// Command socket
    command_socket: Option<UdpSocketHandle>,
    /// Report socket
    report_socket: Option<UdpSocketHandle>,
    /// Current state
    state: GarminControllerState,
    /// Poll count
    poll_count: u64,
}

impl GarminController {
    /// Create a new Garmin controller
    pub fn new(radar_id: &str, radar_addr: &str) -> Self {
        Self {
            radar_id: radar_id.to_string(),
            radar_addr: radar_addr.to_string(),
            command_socket: None,
            report_socket: None,
            state: GarminControllerState::Disconnected,
            poll_count: 0,
        }
    }

    /// Get current state
    pub fn state(&self) -> GarminControllerState {
        self.state
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.state == GarminControllerState::Connected
    }

    /// Poll the controller
    pub fn poll<I: IoProvider>(&mut self, io: &mut I) -> bool {
        self.poll_count += 1;

        match self.state {
            GarminControllerState::Disconnected => {
                self.start_sockets(io);
                true
            }
            GarminControllerState::Listening | GarminControllerState::Connected => {
                self.poll_connected(io)
            }
        }
    }

    fn start_sockets<I: IoProvider>(&mut self, io: &mut I) {
        // Create command socket
        match io.udp_create() {
            Ok(socket) => {
                if io.udp_bind(&socket, 0).is_ok() {
                    self.command_socket = Some(socket);
                    io.debug(&format!(
                        "[{}] Command socket created for {}:{}",
                        self.radar_id,
                        self.radar_addr,
                        garmin::SEND_PORT
                    ));
                } else {
                    io.udp_close(socket);
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
                if io.udp_bind(&socket, garmin::REPORT_PORT).is_ok() {
                    if io
                        .udp_join_multicast(&socket, garmin::REPORT_ADDR, "")
                        .is_ok()
                    {
                        self.report_socket = Some(socket);
                        io.debug(&format!(
                            "[{}] Joined report multicast {}:{}",
                            self.radar_id,
                            garmin::REPORT_ADDR,
                            garmin::REPORT_PORT
                        ));
                        self.state = GarminControllerState::Listening;
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
                if self.state == GarminControllerState::Listening {
                    self.state = GarminControllerState::Connected;
                }
            }
        }

        activity
    }

    fn process_report<I: IoProvider>(&mut self, io: &I, data: &[u8]) {
        if let Ok(report) = garmin::parse_report(data) {
            io.debug(&format!("[{}] Report: {:?}", self.radar_id, report));
        }
    }

    fn send_command<I: IoProvider>(&self, io: &mut I, data: &[u8]) {
        if let Some(socket) = self.command_socket {
            if let Err(e) = io.udp_send_to(&socket, data, &self.radar_addr, garmin::SEND_PORT) {
                io.debug(&format!(
                    "[{}] Failed to send command: {}",
                    self.radar_id, e
                ));
            }
        }
    }

    // Control methods

    /// Set power state (transmit/standby)
    pub fn set_power<I: IoProvider>(&mut self, io: &mut I, transmit: bool) {
        let cmd = garmin::create_transmit_command(transmit);
        self.send_command(io, &cmd);
        io.debug(&format!("[{}] Set power: {}", self.radar_id, transmit));
    }

    /// Set range in meters
    pub fn set_range<I: IoProvider>(&mut self, io: &mut I, range_meters: u32) {
        let cmd = garmin::create_range_command(range_meters);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set range: {} m",
            self.radar_id, range_meters
        ));
    }

    /// Set gain (0-100)
    pub fn set_gain<I: IoProvider>(&mut self, io: &mut I, value: u32, auto: bool) {
        let cmd = garmin::create_gain_command(auto, value);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set gain: {} auto={}",
            self.radar_id, value, auto
        ));
    }

    /// Set sea clutter (0-100)
    pub fn set_sea<I: IoProvider>(&mut self, io: &mut I, value: u32, auto: bool) {
        let cmd = garmin::create_sea_clutter_command(auto, value);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set sea: {} auto={}",
            self.radar_id, value, auto
        ));
    }

    /// Set rain clutter (0-100)
    pub fn set_rain<I: IoProvider>(&mut self, io: &mut I, value: u32, auto: bool) {
        let cmd = garmin::create_rain_clutter_command(auto, value);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set rain: {} auto={}",
            self.radar_id, value, auto
        ));
    }

    /// Set bearing alignment in degrees
    pub fn set_bearing_alignment<I: IoProvider>(&mut self, io: &mut I, degrees: f32) {
        let cmd = garmin::create_bearing_alignment_command(degrees);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set bearing alignment: {}",
            self.radar_id, degrees
        ));
    }

    /// Set no-transmit zone
    pub fn set_ntz<I: IoProvider>(
        &mut self,
        io: &mut I,
        enabled: bool,
        start_deg: f32,
        end_deg: f32,
    ) {
        let cmd = garmin::create_ntz_command(enabled, start_deg, end_deg);
        self.send_command(io, &cmd);
        io.debug(&format!(
            "[{}] Set NTZ: enabled={} {}-{}°",
            self.radar_id, enabled, start_deg, end_deg
        ));
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
        self.state = GarminControllerState::Disconnected;
    }
}
