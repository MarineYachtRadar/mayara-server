//! Connection state machine for radar communication.
//!
//! This module provides platform-independent connection state management
//! that can be used by both native (server) and WASM (SignalK) implementations.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  ConnectionState (this module)                              │
//! │  - Platform-independent state machine                       │
//! │  - Pure state transitions, no I/O                          │
//! │  - Used by both server and WASM                            │
//! └─────────────────────────────────────────────────────────────┘
//!                    │
//!      ┌─────────────┴─────────────┐
//!      │                           │
//!      ▼                           ▼
//! ┌──────────────┐          ┌──────────────┐
//! │ Server       │          │ WASM         │
//! │ (TokioIO)    │          │ (WasmIO)     │
//! └──────────────┘          └──────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use mayara_core::connection::{ConnectionState, ConnectionManager};
//!
//! let mut conn = ConnectionManager::new();
//!
//! // State transitions driven by I/O layer
//! conn.start_connecting();
//! // ... I/O layer performs connection ...
//! conn.connected();
//! // ... I/O layer receives data ...
//! conn.data_received();
//! ```

use serde::{Deserialize, Serialize};

// =============================================================================
// Connection State
// =============================================================================

/// Connection state for radar communication channels.
///
/// This represents the lifecycle of a TCP or UDP connection to a radar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    /// Not connected, no connection attempt in progress
    Disconnected,
    /// Connection attempt in progress (TCP connect or UDP bind)
    Connecting,
    /// Login/authentication in progress (Furuno port negotiation)
    Authenticating,
    /// Connected but not yet receiving data
    Connected,
    /// Connected and actively receiving data
    Active,
    /// Connection error occurred, will retry
    Error,
    /// Shutting down, no more connection attempts
    ShuttingDown,
}

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState::Disconnected
    }
}

impl ConnectionState {
    /// Check if the connection is usable for sending commands
    pub fn can_send(&self) -> bool {
        matches!(self, ConnectionState::Connected | ConnectionState::Active)
    }

    /// Check if connection attempt is in progress
    pub fn is_connecting(&self) -> bool {
        matches!(
            self,
            ConnectionState::Connecting | ConnectionState::Authenticating
        )
    }

    /// Check if connection is fully established
    pub fn is_established(&self) -> bool {
        matches!(self, ConnectionState::Connected | ConnectionState::Active)
    }

    /// Check if we should attempt reconnection
    pub fn should_reconnect(&self) -> bool {
        matches!(self, ConnectionState::Disconnected | ConnectionState::Error)
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Disconnected => write!(f, "Disconnected"),
            ConnectionState::Connecting => write!(f, "Connecting"),
            ConnectionState::Authenticating => write!(f, "Authenticating"),
            ConnectionState::Connected => write!(f, "Connected"),
            ConnectionState::Active => write!(f, "Active"),
            ConnectionState::Error => write!(f, "Error"),
            ConnectionState::ShuttingDown => write!(f, "Shutting Down"),
        }
    }
}

// =============================================================================
// Connection Manager
// =============================================================================

/// Manages connection state and retry logic.
///
/// This is a pure state machine with no I/O - the actual connection
/// operations are performed by the platform-specific I/O layer.
#[derive(Debug, Clone)]
pub struct ConnectionManager {
    /// Current connection state
    state: ConnectionState,
    /// Number of consecutive connection failures
    failure_count: u32,
    /// Timestamp of last state change (milliseconds since start)
    last_state_change_ms: u64,
    /// Timestamp of last successful data receive
    last_data_ms: u64,
    /// Whether we've received any data on this connection
    has_received_data: bool,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionManager {
    /// Create a new connection manager in disconnected state.
    pub fn new() -> Self {
        ConnectionManager {
            state: ConnectionState::Disconnected,
            failure_count: 0,
            last_state_change_ms: 0,
            last_data_ms: 0,
            has_received_data: false,
        }
    }

    /// Get current connection state.
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Get number of consecutive failures.
    pub fn failure_count(&self) -> u32 {
        self.failure_count
    }

    /// Check if we've received data on this connection.
    pub fn has_received_data(&self) -> bool {
        self.has_received_data
    }

    /// Check if the connection is usable for sending commands.
    pub fn can_send(&self) -> bool {
        self.state.can_send()
    }

    /// Check if connection attempt is in progress.
    pub fn is_connecting(&self) -> bool {
        self.state.is_connecting()
    }

    /// Check if connection is fully established.
    pub fn is_established(&self) -> bool {
        self.state.is_established()
    }

    /// Check if we should attempt reconnection.
    pub fn should_reconnect(&self) -> bool {
        self.state.should_reconnect()
    }

    /// Get recommended backoff delay in milliseconds.
    ///
    /// Uses exponential backoff: 1s, 2s, 4s, 8s, max 30s
    pub fn backoff_ms(&self) -> u64 {
        let base_ms = 1000u64;
        let max_ms = 30000u64;
        let delay = base_ms * (1u64 << self.failure_count.min(5));
        delay.min(max_ms)
    }

    /// Calculate time since last state change.
    pub fn time_in_state_ms(&self, current_time_ms: u64) -> u64 {
        current_time_ms.saturating_sub(self.last_state_change_ms)
    }

    /// Calculate time since last data received.
    pub fn time_since_data_ms(&self, current_time_ms: u64) -> u64 {
        if self.last_data_ms == 0 {
            u64::MAX // Never received data
        } else {
            current_time_ms.saturating_sub(self.last_data_ms)
        }
    }

    // -------------------------------------------------------------------------
    // State Transitions
    // -------------------------------------------------------------------------

    /// Transition to connecting state.
    ///
    /// Call this when starting a connection attempt.
    pub fn start_connecting(&mut self, current_time_ms: u64) {
        if self.state != ConnectionState::ShuttingDown {
            self.set_state(ConnectionState::Connecting, current_time_ms);
        }
    }

    /// Transition to authenticating state.
    ///
    /// Call this after TCP connect succeeds, before login handshake.
    pub fn start_authenticating(&mut self, current_time_ms: u64) {
        if self.state == ConnectionState::Connecting {
            self.set_state(ConnectionState::Authenticating, current_time_ms);
        }
    }

    /// Transition to connected state.
    ///
    /// Call this when connection/authentication completes successfully.
    pub fn connected(&mut self, current_time_ms: u64) {
        if self.state.is_connecting() {
            self.set_state(ConnectionState::Connected, current_time_ms);
            self.failure_count = 0;
            self.has_received_data = false;
        }
    }

    /// Record that data was received.
    ///
    /// Transitions to Active state if not already there.
    pub fn data_received(&mut self, current_time_ms: u64) {
        if self.state == ConnectionState::Connected {
            self.set_state(ConnectionState::Active, current_time_ms);
        }
        if self.state == ConnectionState::Active {
            self.last_data_ms = current_time_ms;
            self.has_received_data = true;
        }
    }

    /// Transition to error state.
    ///
    /// Call this when a connection error occurs.
    pub fn error(&mut self, current_time_ms: u64) {
        if self.state != ConnectionState::ShuttingDown {
            self.set_state(ConnectionState::Error, current_time_ms);
            self.failure_count = self.failure_count.saturating_add(1);
        }
    }

    /// Transition to disconnected state (ready to retry).
    ///
    /// Call this after backoff delay has elapsed.
    pub fn disconnected(&mut self, current_time_ms: u64) {
        if self.state == ConnectionState::Error {
            self.set_state(ConnectionState::Disconnected, current_time_ms);
        }
    }

    /// Transition to shutting down state.
    ///
    /// Call this when shutdown is requested. No further connection attempts.
    pub fn shutdown(&mut self, current_time_ms: u64) {
        self.set_state(ConnectionState::ShuttingDown, current_time_ms);
    }

    /// Reset to disconnected state (clears failure count).
    ///
    /// Use this when the connection should be reset completely.
    pub fn reset(&mut self, current_time_ms: u64) {
        self.set_state(ConnectionState::Disconnected, current_time_ms);
        self.failure_count = 0;
        self.has_received_data = false;
    }

    fn set_state(&mut self, new_state: ConnectionState, current_time_ms: u64) {
        if self.state != new_state {
            self.state = new_state;
            self.last_state_change_ms = current_time_ms;
        }
    }
}

// =============================================================================
// Furuno Login Protocol
// =============================================================================

/// Furuno login protocol constants and messages.
pub mod furuno {
    /// Timeout for login TCP connection (milliseconds)
    pub const LOGIN_TIMEOUT_MS: u64 = 500;

    /// Base port for Furuno radar communication
    pub const BASE_PORT: u16 = 10000;

    /// Login message for Furuno port negotiation.
    ///
    /// From fnet.dll function "login_via_copyright".
    /// The message contains the copyright string:
    /// "COPYRIGHT (C) 2001 FURUNO ELECTRIC CO.,LTD. "
    pub const LOGIN_MESSAGE: [u8; 56] = [
        0x08, 0x01, 0x00, 0x38, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x43, 0x4f, 0x50,
        0x59, // "COPY"
        0x52, 0x49, 0x47, 0x48, 0x54, 0x20, 0x28, 0x43, // "RIGHT (C"
        0x29, 0x20, 0x32, 0x30, 0x30, 0x31, 0x20, 0x46, // ") 2001 F"
        0x55, 0x52, 0x55, 0x4e, 0x4f, 0x20, 0x45, 0x4c, // "URUNO EL"
        0x45, 0x43, 0x54, 0x52, 0x49, 0x43, 0x20, 0x43, // "ECTRIC C"
        0x4f, 0x2e, 0x2c, 0x4c, 0x54, 0x44, 0x2e, 0x20, // "O.,LTD. "
    ];

    /// Expected response header from radar login.
    pub const LOGIN_RESPONSE_HEADER: [u8; 8] = [0x09, 0x01, 0x00, 0x0c, 0x01, 0x00, 0x00, 0x00];

    /// Parse the login response to extract the assigned port.
    ///
    /// # Arguments
    /// * `header` - First 8 bytes of response (should match LOGIN_RESPONSE_HEADER)
    /// * `port_bytes` - Next 4 bytes containing port assignment
    ///
    /// # Returns
    /// * `Some(port)` - The assigned port number
    /// * `None` - If header doesn't match expected format
    pub fn parse_login_response(header: &[u8; 8], port_bytes: &[u8; 4]) -> Option<u16> {
        if header != &LOGIN_RESPONSE_HEADER {
            return None;
        }
        let port = BASE_PORT + ((port_bytes[0] as u16) << 8) + port_bytes[1] as u16;
        Some(port)
    }

    /// Keepalive interval for Furuno TCP connections (milliseconds).
    pub const KEEPALIVE_INTERVAL_MS: u64 = 5000;

    /// Reconnect backoff delay (milliseconds).
    pub const RECONNECT_DELAY_MS: u64 = 1000;
}

// =============================================================================
// Socket Type Detection
// =============================================================================

/// Type of receive socket being used for spoke data.
///
/// Furuno radars can send data via multicast or broadcast.
/// The system tries multicast first and falls back to broadcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveSocketType {
    /// Using both socket types (initial state)
    Both,
    /// Using multicast only (239.255.0.2:10024)
    Multicast,
    /// Using broadcast only (172.31.255.255:10024)
    Broadcast,
}

impl Default for ReceiveSocketType {
    fn default() -> Self {
        ReceiveSocketType::Both
    }
}

impl ReceiveSocketType {
    /// Check if multicast should be tried
    pub fn try_multicast(&self) -> bool {
        matches!(self, ReceiveSocketType::Both | ReceiveSocketType::Multicast)
    }

    /// Check if broadcast should be tried
    pub fn try_broadcast(&self) -> bool {
        matches!(self, ReceiveSocketType::Both | ReceiveSocketType::Broadcast)
    }

    /// Record that multicast is working
    pub fn multicast_working(&mut self) {
        *self = ReceiveSocketType::Multicast;
    }

    /// Record that broadcast is working
    pub fn broadcast_working(&mut self) {
        *self = ReceiveSocketType::Broadcast;
    }

    /// Record that multicast failed (fall back to broadcast)
    pub fn multicast_failed(&mut self) {
        if *self == ReceiveSocketType::Both {
            *self = ReceiveSocketType::Broadcast;
        }
    }

    /// Record that broadcast failed (fall back to multicast)
    pub fn broadcast_failed(&mut self) {
        if *self == ReceiveSocketType::Both {
            *self = ReceiveSocketType::Multicast;
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_transitions() {
        let mut conn = ConnectionManager::new();
        assert_eq!(conn.state(), ConnectionState::Disconnected);
        assert!(conn.should_reconnect());

        conn.start_connecting(100);
        assert_eq!(conn.state(), ConnectionState::Connecting);
        assert!(conn.is_connecting());
        assert!(!conn.can_send());

        conn.start_authenticating(200);
        assert_eq!(conn.state(), ConnectionState::Authenticating);
        assert!(conn.is_connecting());

        conn.connected(300);
        assert_eq!(conn.state(), ConnectionState::Connected);
        assert!(conn.is_established());
        assert!(conn.can_send());
        assert_eq!(conn.failure_count(), 0);

        conn.data_received(400);
        assert_eq!(conn.state(), ConnectionState::Active);
        assert!(conn.has_received_data());

        conn.error(500);
        assert_eq!(conn.state(), ConnectionState::Error);
        assert_eq!(conn.failure_count(), 1);
        assert!(!conn.can_send());

        conn.disconnected(600);
        assert_eq!(conn.state(), ConnectionState::Disconnected);
        assert!(conn.should_reconnect());
    }

    #[test]
    fn test_backoff_calculation() {
        let mut conn = ConnectionManager::new();

        // Initial: 1s
        assert_eq!(conn.backoff_ms(), 1000);

        // After 1 failure: 2s
        conn.error(0);
        assert_eq!(conn.backoff_ms(), 2000);

        // After 2 failures: 4s
        conn.disconnected(0);
        conn.error(0);
        assert_eq!(conn.backoff_ms(), 4000);

        // After 5+ failures: max 30s
        for _ in 0..10 {
            conn.disconnected(0);
            conn.error(0);
        }
        assert_eq!(conn.backoff_ms(), 30000);
    }

    #[test]
    fn test_shutdown_prevents_reconnect() {
        let mut conn = ConnectionManager::new();
        conn.shutdown(100);
        assert_eq!(conn.state(), ConnectionState::ShuttingDown);

        // Should not transition out of shutdown
        conn.start_connecting(200);
        assert_eq!(conn.state(), ConnectionState::ShuttingDown);

        conn.error(300);
        assert_eq!(conn.state(), ConnectionState::ShuttingDown);
    }

    #[test]
    fn test_furuno_login_parse() {
        let header = furuno::LOGIN_RESPONSE_HEADER;
        let port_bytes: [u8; 4] = [0x00, 0x01, 0x00, 0x00]; // Port offset 1

        let port = furuno::parse_login_response(&header, &port_bytes);
        assert_eq!(port, Some(10001)); // BASE_PORT + 1

        // Invalid header should return None
        let bad_header: [u8; 8] = [0xFF; 8];
        assert_eq!(furuno::parse_login_response(&bad_header, &port_bytes), None);
    }

    #[test]
    fn test_receive_socket_type() {
        let mut sock_type = ReceiveSocketType::default();
        assert!(sock_type.try_multicast());
        assert!(sock_type.try_broadcast());

        sock_type.multicast_working();
        assert!(sock_type.try_multicast());
        assert!(!sock_type.try_broadcast());

        sock_type = ReceiveSocketType::Both;
        sock_type.multicast_failed();
        assert!(!sock_type.try_multicast());
        assert!(sock_type.try_broadcast());
    }

    #[test]
    fn test_time_calculations() {
        let mut conn = ConnectionManager::new();

        conn.start_connecting(1000);
        assert_eq!(conn.time_in_state_ms(1500), 500);

        conn.connected(2000);
        conn.data_received(3000);
        assert_eq!(conn.time_since_data_ms(3500), 500);
    }
}
