//! Furuno Radar TCP Controller
//!
//! Platform-independent controller for Furuno radars using the [`IoProvider`] trait.
//! Handles TCP login, command sending, and response parsing.
//!
//! # Login Sequence
//!
//! Furuno radars use a two-phase connection:
//! 1. Connect to login port (10010 or 10000), send login packet, receive command port
//! 2. Connect to command port for text-based command protocol
//!
//! # Example
//!
//! ```rust,ignore
//! use mayara_core::controllers::FurunoController;
//! use mayara_core::IoProvider;
//!
//! fn run<I: IoProvider>(io: &mut I) {
//!     let mut controller = FurunoController::new("radar-1", "172.31.6.1");
//!
//!     // Poll regularly (10Hz recommended)
//!     loop {
//!         controller.poll(io);
//!
//!         if controller.is_connected() {
//!             controller.set_transmit(io, true);
//!             controller.set_gain(io, 50, false);
//!         }
//!     }
//! }
//! ```

use super::ControllerEvent;
use crate::io::{IoProvider, TcpSocketHandle};
use crate::protocol::furuno::command::{
    format_antenna_height_command, format_auto_acquire_command, format_bird_mode_command,
    format_blind_sector_command, format_gain_command, format_heading_align_command,
    format_interference_rejection_command, format_keepalive, format_main_bang_command,
    format_noise_reduction_command, format_rain_command, format_range_command,
    format_request_modules, format_request_ontime, format_request_txtime, format_rezboost_command,
    format_scan_speed_command, format_sea_command, format_status_command,
    format_target_analyzer_command, format_tx_channel_command, parse_login_response, LOGIN_MESSAGE,
};
use crate::protocol::furuno::{BASE_PORT, BEACON_PORT};
use crate::state::{generate_state_requests, RadarState};

/// Controller state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerState {
    /// Not connected, needs login
    Disconnected,
    /// Sent login message, waiting for response
    LoggingIn,
    /// Got command port, connecting to it
    Connecting,
    /// Connected and ready for commands
    Connected,
    /// Trying fallback direct connection to command port
    TryingFallback,
}

/// Furuno radar TCP controller
///
/// Manages the TCP connection for sending commands to the radar.
/// Handles login, keep-alive, and command sending.
///
/// Uses the [`IoProvider`] trait for all I/O operations, making it
/// platform-independent.
pub struct FurunoController {
    /// Radar ID (for logging)
    radar_id: String,
    /// Radar IP address
    radar_addr: String,
    /// Login socket (port 10000 or 10010)
    login_socket: Option<TcpSocketHandle>,
    /// Command socket (dynamic port)
    command_socket: Option<TcpSocketHandle>,
    /// Current state
    state: ControllerState,
    /// Command port received from login
    command_port: u16,
    /// Last keep-alive time (poll count)
    last_keepalive: u64,
    /// Current poll count
    poll_count: u64,
    /// Pending command to send once connected
    pending_command: Option<String>,
    /// Retry count for connection attempts
    retry_count: u32,
    /// Poll count when last retry started (for backoff)
    last_retry_poll: u64,
    /// Index into login ports to try
    login_port_idx: usize,
    /// Index into fallback command ports to try
    fallback_port_idx: usize,
    /// Firmware version from $N96 response (e.g., "01.05")
    firmware_version: Option<String>,
    /// Radar model from UDP model report (e.g., "DRS4D-NXT")
    /// Note: $N96 contains part numbers, not model names
    model: Option<String>,
    /// Operating hours from $N8E response (total power-on time)
    operating_hours: Option<f64>,
    /// Transmit hours from $N8F response (total transmit time)
    transmit_hours: Option<f64>,
    /// Whether info requests have been sent after connection
    info_requested: bool,
    /// Whether state requests have been sent after connection
    state_requested: bool,
    /// Whether login message has been sent (to avoid sending on every poll)
    login_sent: bool,
    /// Current radar control state
    radar_state: RadarState,
    /// Whether Connected event has been emitted
    connected_event_emitted: bool,
    /// Whether ModelDetected event has been emitted
    model_event_emitted: bool,
    /// Last emitted operating hours (to detect changes)
    last_emitted_hours: Option<f64>,
    /// Last emitted transmit hours (to detect changes)
    last_emitted_tx_hours: Option<f64>,
    /// Previous power state (to detect transitions)
    prev_power_state: crate::state::PowerState,
    /// Poll count when last state refresh was sent (for periodic sync)
    last_state_refresh: u64,
}

impl FurunoController {
    /// Maximum number of connection retries
    const MAX_RETRIES: u32 = 5;
    /// Base delay between retries (in poll counts, ~100ms per poll)
    const RETRY_DELAY_BASE: u64 = 10;
    /// Login ports to try (some radars use 10000, others use 10010)
    const LOGIN_PORTS: [u16; 2] = [BEACON_PORT, BASE_PORT];
    /// Fallback command ports when login port is refused
    const FALLBACK_PORTS: [u16; 3] = [10100, 10001, 10002];
    /// Keep-alive interval in poll counts (~5 seconds at 10 polls/sec)
    const KEEPALIVE_INTERVAL: u64 = 50;
    /// State refresh interval in poll counts (~2 seconds at 10 polls/sec)
    /// This allows us to sync state changes made by other clients (e.g., chart plotter)
    const STATE_REFRESH_INTERVAL: u64 = 20;

    /// Create a new controller for a Furuno radar
    ///
    /// The controller will automatically attempt to connect to get model info.
    pub fn new(radar_id: &str, radar_addr: &str) -> Self {
        let mut controller = Self {
            radar_id: radar_id.to_string(),
            radar_addr: radar_addr.to_string(),
            login_socket: None,
            command_socket: None,
            state: ControllerState::Disconnected,
            command_port: 0,
            last_keepalive: 0,
            poll_count: 0,
            pending_command: None,
            retry_count: 0,
            last_retry_poll: 0,
            login_port_idx: 0,
            fallback_port_idx: 0,
            firmware_version: None,
            model: None,
            operating_hours: None,
            transmit_hours: None,
            info_requested: false,
            state_requested: false,
            login_sent: false,
            radar_state: RadarState::default(),
            connected_event_emitted: false,
            model_event_emitted: false,
            last_emitted_hours: None,
            last_emitted_tx_hours: None,
            prev_power_state: crate::state::PowerState::Off,
            last_state_refresh: 0,
        };
        // Queue keepalive to trigger connection
        controller.request_info();
        controller
    }

    /// Request radar info by initiating a connection
    pub fn request_info(&mut self) {
        // Always set a pending command to trigger connection on first poll
        if self.state == ControllerState::Disconnected && self.pending_command.is_none() {
            let cmd = format_keepalive();
            self.pending_command = Some(cmd.trim().to_string());
        }
    }

    /// Get current connection state
    pub fn state(&self) -> ControllerState {
        self.state
    }

    /// Check if connected and ready for commands
    pub fn is_connected(&self) -> bool {
        self.state == ControllerState::Connected
    }

    /// Get current radar state
    pub fn radar_state(&self) -> &RadarState {
        &self.radar_state
    }

    /// Get radar model if known
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Set the radar model (called when UDP model report is received by locator)
    pub fn set_model(&mut self, model: &str) {
        self.model = Some(model.to_string());
    }

    /// Get firmware version if known
    pub fn firmware_version(&self) -> Option<&str> {
        self.firmware_version.as_deref()
    }

    /// Get operating hours if known (total power-on time)
    pub fn operating_hours(&self) -> Option<f64> {
        self.operating_hours
    }

    /// Get transmit hours if known (total transmit time)
    pub fn transmit_hours(&self) -> Option<f64> {
        self.transmit_hours
    }

    /// Set radar to transmit
    pub fn set_transmit<I: IoProvider>(&mut self, io: &mut I, transmit: bool) {
        let cmd = format_status_command(transmit);
        self.queue_command(io, cmd.trim());
    }

    /// Set radar range in meters
    pub fn set_range<I: IoProvider>(&mut self, io: &mut I, range_meters: u32) {
        let cmd = format_range_command(range_meters as i32);
        self.queue_command(io, cmd.trim());
    }

    /// Set radar gain
    pub fn set_gain<I: IoProvider>(&mut self, io: &mut I, value: i32, auto: bool) {
        let cmd = format_gain_command(value, auto);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.gain.value = value;
        self.radar_state.gain.mode = if auto { "auto".into() } else { "manual".into() };
    }

    /// Set radar sea clutter
    pub fn set_sea<I: IoProvider>(&mut self, io: &mut I, value: i32, auto: bool) {
        let cmd = format_sea_command(value, auto);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.sea.value = value;
        self.radar_state.sea.mode = if auto { "auto".into() } else { "manual".into() };
    }

    /// Set radar rain clutter
    pub fn set_rain<I: IoProvider>(&mut self, io: &mut I, value: i32, auto: bool) {
        let cmd = format_rain_command(value, auto);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.rain.value = value;
        self.radar_state.rain.mode = if auto { "auto".into() } else { "manual".into() };
    }

    /// Set RezBoost (beam sharpening) level
    pub fn set_rezboost<I: IoProvider>(&mut self, io: &mut I, level: i32) {
        let cmd = format_rezboost_command(level, 0);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.beam_sharpening = level;
    }

    /// Set interference rejection
    pub fn set_interference_rejection<I: IoProvider>(&mut self, io: &mut I, enabled: bool) {
        let cmd = format_interference_rejection_command(enabled);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.interference_rejection = enabled;
    }

    /// Set noise reduction
    pub fn set_noise_reduction<I: IoProvider>(&mut self, io: &mut I, enabled: bool) {
        let cmd = format_noise_reduction_command(enabled);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.noise_reduction = enabled;
    }

    /// Set scan speed
    pub fn set_scan_speed<I: IoProvider>(&mut self, io: &mut I, speed: i32) {
        let cmd = format_scan_speed_command(speed);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.scan_speed = speed;
    }

    /// Set bird mode
    pub fn set_bird_mode<I: IoProvider>(&mut self, io: &mut I, level: i32) {
        let cmd = format_bird_mode_command(level, 0);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.bird_mode = level;
    }

    /// Set target analyzer (Doppler mode)
    pub fn set_target_analyzer<I: IoProvider>(&mut self, io: &mut I, enabled: bool, mode: i32) {
        let cmd = format_target_analyzer_command(enabled, mode, 0);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.doppler_mode.enabled = enabled;
        self.radar_state.doppler_mode.mode = if mode == 0 {
            "target".into()
        } else {
            "rain".into()
        };
    }

    /// Set bearing alignment (heading offset)
    pub fn set_bearing_alignment<I: IoProvider>(&mut self, io: &mut I, degrees: f64) {
        let degrees_x10 = (degrees * 10.0) as i32;
        let cmd = format_heading_align_command(degrees_x10);
        self.queue_command(io, cmd.trim());
    }

    /// Set main bang suppression
    pub fn set_main_bang_suppression<I: IoProvider>(&mut self, io: &mut I, percent: i32) {
        let cmd = format_main_bang_command(percent);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.main_bang_suppression = percent;
    }

    /// Set TX channel
    pub fn set_tx_channel<I: IoProvider>(&mut self, io: &mut I, channel: i32) {
        let cmd = format_tx_channel_command(channel);
        self.queue_command(io, cmd.trim());
        // Update local state immediately for responsive UI
        self.radar_state.tx_channel = channel;
    }

    /// Set auto acquire (ARPA by Doppler)
    pub fn set_auto_acquire<I: IoProvider>(&mut self, io: &mut I, enabled: bool) {
        let cmd = format_auto_acquire_command(enabled);
        self.queue_command(io, cmd.trim());
    }

    /// Set antenna height
    pub fn set_antenna_height<I: IoProvider>(&mut self, io: &mut I, meters: i32) {
        let cmd = format_antenna_height_command(meters);
        self.queue_command(io, cmd.trim());
    }

    /// Set blind sector (no-transmit zones)
    /// Protocol: $S77,{s2_enable},{s1_start},{s1_width},{s2_start},{s2_width}
    /// - Sector 1 enabled when width > 0
    /// - Sector 2 enabled when s2_enable=1 AND width > 0
    pub fn set_blind_sector<I: IoProvider>(
        &mut self,
        io: &mut I,
        zone1_enabled: bool,
        zone1_start: i32,
        zone1_end: i32,
        zone2_enabled: bool,
        zone2_start: i32,
        zone2_end: i32,
    ) {
        // Helper to normalize angle to 0-359
        let normalize = |angle: i32| ((angle % 360) + 360) % 360;

        // Zone 1: enabled by width > 0
        let (z1_start, z1_width) = if zone1_enabled {
            let start = normalize(zone1_start);
            let end = normalize(zone1_end);
            let width = ((end - start + 360) % 360).max(1);
            (start, width)
        } else {
            (0, 0) // Disabled: start=0, width=0
        };

        // Zone 2: enabled by s2_enable flag AND width > 0
        let (z2_start, z2_width) = if zone2_enabled {
            let start = normalize(zone2_start);
            let end = normalize(zone2_end);
            let width = ((end - start + 360) % 360).max(1);
            (start, width)
        } else {
            (0, 0) // Disabled: start=0, width=0
        };

        let cmd =
            format_blind_sector_command(zone2_enabled, z1_start, z1_width, z2_start, z2_width);
        self.queue_command(io, cmd.trim());
    }

    /// Queue a command and start connection if needed
    fn queue_command<I: IoProvider>(&mut self, io: &mut I, cmd: &str) {
        io.debug(&format!("[{}] Queueing command: {}", self.radar_id, cmd));
        if self.is_connected() {
            self.send_command(io, cmd);
        } else {
            self.pending_command = Some(cmd.to_string());
            if self.state == ControllerState::Disconnected {
                self.start_login(io);
            }
        }
    }

    /// Poll the controller - call this regularly from the main poll loop
    ///
    /// Returns a list of events for the shell to handle. Events include:
    /// - `Connected` when connection is established
    /// - `Disconnected` when connection is lost
    /// - `ModelDetected` when model and firmware version are identified
    /// - `OperatingHoursUpdated` when operating hours change
    pub fn poll<I: IoProvider>(&mut self, io: &mut I) -> Vec<ControllerEvent> {
        self.poll_count += 1;
        let mut events = Vec::new();

        // Track state before polling for disconnect detection
        let was_connected = self.state == ControllerState::Connected;

        match self.state {
            ControllerState::Disconnected => {
                if self.pending_command.is_some() {
                    // Check backoff
                    if self.retry_count > 0 {
                        let delay = Self::RETRY_DELAY_BASE * (1 << self.retry_count.min(4) as u64);
                        let elapsed = self.poll_count - self.last_retry_poll;
                        if elapsed < delay {
                            return events;
                        }
                        if self.retry_count >= Self::MAX_RETRIES {
                            io.debug(&format!(
                                "[{}] Max retries ({}) reached, giving up",
                                self.radar_id,
                                Self::MAX_RETRIES
                            ));
                            self.pending_command = None;
                            self.retry_count = 0;
                            return events;
                        }
                        io.debug(&format!(
                            "[{}] Retry {} of {}",
                            self.radar_id,
                            self.retry_count + 1,
                            Self::MAX_RETRIES
                        ));
                    }
                    self.start_login(io);
                }
            }
            ControllerState::LoggingIn => {
                self.poll_login(io);
            }
            ControllerState::Connecting => {
                self.poll_connecting(io);
            }
            ControllerState::Connected => {
                self.poll_connected(io);
            }
            ControllerState::TryingFallback => {
                self.poll_fallback(io);
            }
        }

        // Emit Connected event when we first reach Connected state
        if self.state == ControllerState::Connected && !self.connected_event_emitted {
            self.connected_event_emitted = true;
            events.push(ControllerEvent::Connected);
            io.info(&format!("[{}] Controller connected", self.radar_id));
        }

        // Emit Disconnected event when we lose connection
        if was_connected && self.state == ControllerState::Disconnected {
            self.connected_event_emitted = false;
            events.push(ControllerEvent::Disconnected);
            io.info(&format!("[{}] Controller disconnected", self.radar_id));
        }

        // Emit ModelDetected event when model becomes available
        if !self.model_event_emitted {
            if let (Some(model), Some(version)) = (&self.model, &self.firmware_version) {
                self.model_event_emitted = true;
                events.push(ControllerEvent::ModelDetected {
                    model: model.clone(),
                    version: version.clone(),
                });
                io.info(&format!(
                    "[{}] Model detected: {} (firmware {})",
                    self.radar_id, model, version
                ));
            }
        }

        // Emit OperatingHoursUpdated when hours change
        if let Some(hours) = self.operating_hours {
            if self.last_emitted_hours != Some(hours) {
                self.last_emitted_hours = Some(hours);
                events.push(ControllerEvent::OperatingHoursUpdated { hours });
            }
        }

        // Emit TransmitHoursUpdated when hours change
        if let Some(hours) = self.transmit_hours {
            if self.last_emitted_tx_hours != Some(hours) {
                self.last_emitted_tx_hours = Some(hours);
                events.push(ControllerEvent::TransmitHoursUpdated { hours });
            }
        }

        events
    }

    /// Start the login process
    fn start_login<I: IoProvider>(&mut self, io: &mut I) {
        if self.login_port_idx >= Self::LOGIN_PORTS.len() {
            io.debug(&format!(
                "[{}] All login ports exhausted, trying fallback",
                self.radar_id
            ));
            self.login_port_idx = 0;
            self.start_fallback_connection(io);
            return;
        }

        let login_port = Self::LOGIN_PORTS[self.login_port_idx];
        io.debug(&format!(
            "[{}] Starting login to {}:{} (idx {})",
            self.radar_id, self.radar_addr, login_port, self.login_port_idx
        ));

        match io.tcp_create() {
            Ok(socket) => {
                // Raw mode for binary login response
                let _ = io.tcp_set_line_buffering(&socket, false);

                if io
                    .tcp_connect(&socket, &self.radar_addr, login_port)
                    .is_ok()
                {
                    self.login_socket = Some(socket);
                    self.state = ControllerState::LoggingIn;
                    self.login_sent = false; // Reset for new login attempt
                    io.debug(&format!(
                        "[{}] Login connection initiated to port {}",
                        self.radar_id, login_port
                    ));
                } else {
                    io.debug(&format!(
                        "[{}] Failed to initiate login to port {}",
                        self.radar_id, login_port
                    ));
                    io.tcp_close(socket);
                    self.login_port_idx += 1;
                    self.start_login(io);
                }
            }
            Err(e) => {
                io.debug(&format!(
                    "[{}] Failed to create login socket: {}",
                    self.radar_id, e
                ));
                self.login_port_idx += 1;
                self.start_login(io);
            }
        }
    }

    /// Poll during login state
    fn poll_login<I: IoProvider>(&mut self, io: &mut I) -> bool {
        let socket = match self.login_socket {
            Some(s) => s,
            None => {
                io.debug(&format!("[{}] poll_login: no socket", self.radar_id));
                self.state = ControllerState::Disconnected;
                return false;
            }
        };

        if !io.tcp_is_valid(&socket) {
            io.debug(&format!(
                "[{}] Login socket closed on port idx {}",
                self.radar_id, self.login_port_idx
            ));
            io.tcp_close(socket);
            self.login_socket = None;
            self.login_port_idx += 1;
            self.start_login(io);
            return true;
        }

        if !io.tcp_is_connected(&socket) {
            io.debug(&format!(
                "[{}] Login socket still connecting...",
                self.radar_id
            ));
            return true; // Still connecting
        }

        // Send login message ONCE (not on every poll!)
        if !self.login_sent {
            self.login_sent = true;
            io.debug(&format!("[{}] Sending login message", self.radar_id));
            if io.tcp_send(&socket, &LOGIN_MESSAGE).is_err() {
                io.debug(&format!("[{}] Failed to send login message", self.radar_id));
                self.disconnect(io);
                return false;
            }
        }

        // Check for response
        let mut buf = [0u8; 64];
        if let Some(len) = io.tcp_recv_raw(&socket, &mut buf) {
            io.debug(&format!(
                "[{}] Login response: {} bytes",
                self.radar_id, len
            ));

            if let Some(port) = parse_login_response(&buf[..len]) {
                io.debug(&format!("[{}] Got command port: {}", self.radar_id, port));
                self.command_port = port;
                io.tcp_close(socket);
                self.login_socket = None;
                self.start_command_connection(io);
            } else {
                io.debug(&format!("[{}] Invalid login response", self.radar_id));
                self.disconnect(io);
            }
        }

        true
    }

    /// Start connection to command port
    fn start_command_connection<I: IoProvider>(&mut self, io: &mut I) {
        io.debug(&format!(
            "[{}] Connecting to command port {}",
            self.radar_id, self.command_port
        ));

        match io.tcp_create() {
            Ok(socket) => {
                // Line buffering for text protocol
                let _ = io.tcp_set_line_buffering(&socket, true);

                if io
                    .tcp_connect(&socket, &self.radar_addr, self.command_port)
                    .is_ok()
                {
                    self.command_socket = Some(socket);
                    self.state = ControllerState::Connecting;
                } else {
                    io.debug(&format!(
                        "[{}] Failed to connect to command port",
                        self.radar_id
                    ));
                    io.tcp_close(socket);
                    self.state = ControllerState::Disconnected;
                }
            }
            Err(e) => {
                io.debug(&format!(
                    "[{}] Failed to create command socket: {}",
                    self.radar_id, e
                ));
                self.state = ControllerState::Disconnected;
            }
        }
    }

    /// Poll during connecting state
    fn poll_connecting<I: IoProvider>(&mut self, io: &mut I) -> bool {
        let socket = match self.command_socket {
            Some(s) => s,
            None => {
                self.state = ControllerState::Disconnected;
                return false;
            }
        };

        if !io.tcp_is_valid(&socket) {
            io.debug(&format!(
                "[{}] Command socket closed/errored",
                self.radar_id
            ));
            io.tcp_close(socket);
            self.command_socket = None;
            self.state = ControllerState::Disconnected;
            self.retry_count += 1;
            self.last_retry_poll = self.poll_count;
            return false;
        }

        if io.tcp_is_connected(&socket) {
            io.debug(&format!(
                "[{}] Command connection established",
                self.radar_id
            ));
            self.state = ControllerState::Connected;
            self.last_keepalive = self.poll_count;
            self.retry_count = 0;
            self.login_port_idx = 0;

            // Send pending command
            if let Some(cmd) = self.pending_command.take() {
                self.send_command(io, &cmd);
            }
        }

        true
    }

    /// Poll while connected
    fn poll_connected<I: IoProvider>(&mut self, io: &mut I) -> bool {
        let socket = match self.command_socket {
            Some(s) => s,
            None => {
                self.state = ControllerState::Disconnected;
                return false;
            }
        };

        if !io.tcp_is_connected(&socket) {
            io.debug(&format!("[{}] Command connection lost", self.radar_id));
            self.disconnect(io);
            return false;
        }

        // Send info requests once
        if !self.info_requested {
            self.info_requested = true;
            self.send_info_requests(io);
        }

        // Send state requests once
        if !self.state_requested {
            self.state_requested = true;
            self.send_state_requests(io);
        }

        // Process responses
        let mut buf = [0u8; 1024];
        while let Some(len) = io.tcp_recv_line(&socket, &mut buf) {
            let line = String::from_utf8_lossy(&buf[..len]);
            let line = line.trim();
            io.debug(&format!("[{}] Response: {}", self.radar_id, line));
            self.parse_response(io, line);
        }

        // Re-request state when radar transitions to transmit mode
        // Some controls (like mainBangSuppression) may only be available when transmitting
        use crate::state::PowerState;
        if self.radar_state.power == PowerState::Transmit
            && self.prev_power_state != PowerState::Transmit
        {
            io.debug(&format!(
                "[{}] Radar started transmitting, re-requesting state",
                self.radar_id
            ));
            self.send_state_requests(io);
        }
        self.prev_power_state = self.radar_state.power;

        // Send keep-alive
        if self.poll_count - self.last_keepalive > Self::KEEPALIVE_INTERVAL {
            self.send_keepalive(io);
            self.last_keepalive = self.poll_count;
        }

        // Periodic state refresh to sync changes made by other clients (e.g., chart plotter)
        // When refreshing, we force-update the state to accept external values
        if self.poll_count - self.last_state_refresh > Self::STATE_REFRESH_INTERVAL {
            io.debug(&format!(
                "[{}] Periodic state refresh (sync with external clients)",
                self.radar_id
            ));
            // Mark as pending external update so state.rs accepts radar values
            self.radar_state.mark_pending_refresh();
            self.send_state_requests(io);
            self.last_state_refresh = self.poll_count;
        }

        true
    }

    /// Try fallback connection to known command ports
    fn start_fallback_connection<I: IoProvider>(&mut self, io: &mut I) {
        if self.fallback_port_idx >= Self::FALLBACK_PORTS.len() {
            io.debug(&format!("[{}] All fallback ports exhausted", self.radar_id));
            self.fallback_port_idx = 0;
            self.state = ControllerState::Disconnected;
            self.retry_count += 1;
            self.last_retry_poll = self.poll_count;
            return;
        }

        let port = Self::FALLBACK_PORTS[self.fallback_port_idx];
        io.debug(&format!(
            "[{}] Trying fallback port {} (idx {})",
            self.radar_id, port, self.fallback_port_idx
        ));

        match io.tcp_create() {
            Ok(socket) => {
                let _ = io.tcp_set_line_buffering(&socket, true);
                if io.tcp_connect(&socket, &self.radar_addr, port).is_ok() {
                    self.command_socket = Some(socket);
                    self.command_port = port;
                    self.state = ControllerState::TryingFallback;
                } else {
                    io.tcp_close(socket);
                    self.fallback_port_idx += 1;
                    self.start_fallback_connection(io);
                }
            }
            Err(_) => {
                self.fallback_port_idx += 1;
                self.start_fallback_connection(io);
            }
        }
    }

    /// Poll during fallback connection attempt
    fn poll_fallback<I: IoProvider>(&mut self, io: &mut I) -> bool {
        let socket = match self.command_socket {
            Some(s) => s,
            None => {
                self.state = ControllerState::Disconnected;
                return false;
            }
        };

        if !io.tcp_is_valid(&socket) {
            io.tcp_close(socket);
            self.command_socket = None;
            self.fallback_port_idx += 1;
            self.start_fallback_connection(io);
            return true;
        }

        if io.tcp_is_connected(&socket) {
            io.debug(&format!(
                "[{}] Fallback connection established on port {}",
                self.radar_id, self.command_port
            ));
            self.state = ControllerState::Connected;
            self.last_keepalive = self.poll_count;
            self.retry_count = 0;
            self.fallback_port_idx = 0;

            if let Some(cmd) = self.pending_command.take() {
                self.send_command(io, &cmd);
            }
        }

        true
    }

    /// Send a command to the radar
    fn send_command<I: IoProvider>(&self, io: &mut I, cmd: &str) {
        if let Some(socket) = self.command_socket {
            io.debug(&format!("[{}] Sending: {}", self.radar_id, cmd));
            let cmd_with_crlf = format!("{}\r\n", cmd);
            if io.tcp_send(&socket, cmd_with_crlf.as_bytes()).is_err() {
                io.debug(&format!("[{}] Failed to send command", self.radar_id));
            }
        }
    }

    /// Send keep-alive message
    fn send_keepalive<I: IoProvider>(&self, io: &mut I) {
        let cmd = format_keepalive();
        self.send_command(io, cmd.trim());
    }

    /// Send info requests (firmware version, operating hours, transmit hours)
    fn send_info_requests<I: IoProvider>(&self, io: &mut I) {
        let cmd = format_request_modules();
        self.send_command(io, cmd.trim());

        let cmd = format_request_ontime();
        self.send_command(io, cmd.trim());

        let cmd = format_request_txtime();
        self.send_command(io, cmd.trim());

        io.info(&format!(
            "[{}] Sent info requests (including $R96 for firmware)",
            self.radar_id
        ));
    }

    /// Send state requests
    fn send_state_requests<I: IoProvider>(&self, io: &mut I) {
        for cmd in generate_state_requests() {
            self.send_command(io, cmd.trim());
        }
        io.info(&format!(
            "[{}] Sent state requests (including $R83 for mainBangSuppression)",
            self.radar_id
        ));
    }

    /// Parse a response line from the radar
    fn parse_response<I: IoProvider>(&mut self, io: &I, line: &str) {
        // Debug: Log main bang responses specifically (using INFO to ensure visibility)
        if line.starts_with("$N83") {
            io.info(&format!(
                "[{}] Main Bang response received: {} (before update: mbs={})",
                self.radar_id, line, self.radar_state.main_bang_suppression
            ));
        }

        // Update state from control responses
        if self.radar_state.update_from_response(line) {
            io.debug(&format!(
                "[{}] State updated: power={:?}, range={}, mbs={}",
                self.radar_id,
                self.radar_state.power,
                self.radar_state.range,
                self.radar_state.main_bang_suppression
            ));
        }

        // Parse module response for model and firmware version
        // Format: $N96,{part1}-{ver1},{part2}-{ver2},...
        // Example: $N96,0359360-01.05,0359358-01.01,0359359-01.01,0359361-01.05,,,
        // The first part code identifies the radar model (see protocol docs)
        if line.starts_with("$N96") {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                // Parse first module: "0359360-01.05" -> code="0359360", version="01.05"
                let module_parts: Vec<&str> = parts[1].split('-').collect();
                if module_parts.len() >= 2 {
                    let part_code = module_parts[0];
                    let firmware_version = module_parts[1];

                    // Map part code to model name
                    let model = crate::protocol::furuno::report::firmware_to_model(part_code);
                    let model_name = model.as_str();

                    if model_name != "Unknown" {
                        self.model = Some(model_name.to_string());
                        io.info(&format!(
                            "[{}] Model identified from $N96: {} (part {})",
                            self.radar_id, model_name, part_code
                        ));
                    } else {
                        io.info(&format!(
                            "[{}] Unknown part code from $N96: {}",
                            self.radar_id, part_code
                        ));
                    }

                    self.firmware_version = Some(firmware_version.to_string());
                    io.info(&format!(
                        "[{}] Firmware version from $N96: {}",
                        self.radar_id, firmware_version
                    ));
                }
            }
        }

        // Parse operating hours response
        // Protocol: $N8E,{seconds} where seconds is total power-on time
        if line.starts_with("$N8E") {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                if let Ok(seconds) = parts[1].parse::<f64>() {
                    let hours = seconds / 3600.0;
                    self.operating_hours = Some(hours);
                    io.debug(&format!(
                        "[{}] Operating hours: {:.1} ({} seconds)",
                        self.radar_id, hours, seconds as i64
                    ));
                }
            }
        }

        // Parse transmit hours response
        // Protocol: $N8F,{seconds} where seconds is total transmit time
        if line.starts_with("$N8F") {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 2 {
                if let Ok(seconds) = parts[1].parse::<f64>() {
                    let hours = seconds / 3600.0;
                    self.transmit_hours = Some(hours);
                    io.debug(&format!(
                        "[{}] Transmit hours: {:.1} ({} seconds)",
                        self.radar_id, hours, seconds as i64
                    ));
                }
            }
        }
    }

    /// Disconnect and clean up
    fn disconnect<I: IoProvider>(&mut self, io: &mut I) {
        if let Some(socket) = self.login_socket.take() {
            io.tcp_close(socket);
        }
        if let Some(socket) = self.command_socket.take() {
            io.tcp_close(socket);
        }
        self.state = ControllerState::Disconnected;
        self.info_requested = false;
        self.state_requested = false;
        // Note: connected_event_emitted is reset in poll() when Disconnected event is emitted
        // This allows Connected to be emitted again on reconnection
    }

    /// Shutdown the controller
    pub fn shutdown<I: IoProvider>(&mut self, io: &mut I) {
        io.debug(&format!("[{}] Shutting down", self.radar_id));
        self.disconnect(io);
    }
}
