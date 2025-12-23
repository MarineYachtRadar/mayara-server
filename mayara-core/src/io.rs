//! I/O abstraction for platform-independent radar code.
//!
//! This module defines traits that abstract socket operations, allowing the same
//! locator and controller logic to run on both native (tokio) and WASM (FFI) platforms.
//!
//! # Design
//!
//! The traits use a **poll-based** interface (not async) because:
//! - WASM has no async runtime
//! - Native code can easily adapt async to poll with `try_recv`/`try_send`
//!
//! # Example
//!
//! ```rust,ignore
//! use mayara_core::io::{IoProvider, UdpSocketOps, IoError};
//!
//! fn discover_radars<I: IoProvider>(io: &mut I) -> Vec<Discovery> {
//!     let socket = io.udp_create().unwrap();
//!     io.udp_bind(&socket, 10010).unwrap();
//!     io.udp_set_broadcast(&socket, true).unwrap();
//!
//!     // Poll for incoming data
//!     let mut buf = [0u8; 1024];
//!     if let Some((len, addr, port)) = io.udp_recv_from(&socket, &mut buf) {
//!         // Process beacon...
//!     }
//!     vec![]
//! }
//! ```

use core::fmt;

// =============================================================================
// Error Types
// =============================================================================

/// I/O error type for cross-platform socket operations.
///
/// This is kept minimal since WASM FFI only returns error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoError {
    /// Error code (negative values indicate errors, specific meaning varies by platform)
    pub code: i32,
    /// Human-readable error message
    pub message: String,
}

impl IoError {
    /// Create a new I/O error with a code and message.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Create an error from just a code (message will be generic).
    pub fn from_code(code: i32) -> Self {
        Self {
            code,
            message: format!("I/O error: {}", code),
        }
    }

    /// Create a "would block" error (no data available, non-blocking).
    pub fn would_block() -> Self {
        Self::new(-11, "Operation would block")
    }

    /// Create a "not connected" error.
    pub fn not_connected() -> Self {
        Self::new(-1, "Not connected")
    }

    /// Create an "address in use" error.
    pub fn address_in_use() -> Self {
        Self::new(-98, "Address already in use")
    }

    /// Check if this is a "would block" error.
    pub fn is_would_block(&self) -> bool {
        self.code == -11
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

// =============================================================================
// Socket Handle Types
// =============================================================================

/// Opaque handle to a UDP socket.
///
/// The actual socket implementation is platform-specific.
/// This is just an identifier used by the IoProvider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UdpSocketHandle(pub i32);

/// Opaque handle to a TCP socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TcpSocketHandle(pub i32);

// =============================================================================
// IoProvider Trait
// =============================================================================

/// Platform-independent I/O provider.
///
/// This trait abstracts all socket operations needed for radar discovery and control.
/// Implementations exist for:
/// - **WASM**: Uses SignalK FFI calls
/// - **Native**: Uses tokio sockets (wrapped in non-blocking interface)
///
/// # Poll-based Design
///
/// All operations are non-blocking and poll-based. Receive operations return
/// `None` when no data is available instead of blocking or returning an error.
/// This matches the WASM polling model where `plugin_poll()` is called periodically.
pub trait IoProvider {
    // -------------------------------------------------------------------------
    // UDP Operations
    // -------------------------------------------------------------------------

    /// Create a new UDP socket.
    fn udp_create(&mut self) -> Result<UdpSocketHandle, IoError>;

    /// Bind a UDP socket to a port.
    ///
    /// Use port 0 to let the OS choose an available port.
    fn udp_bind(&mut self, socket: &UdpSocketHandle, port: u16) -> Result<(), IoError>;

    /// Enable or disable broadcast mode on a UDP socket.
    ///
    /// Must be called before sending to broadcast addresses.
    fn udp_set_broadcast(&mut self, socket: &UdpSocketHandle, enabled: bool)
        -> Result<(), IoError>;

    /// Join a multicast group.
    ///
    /// The `interface` parameter is the local interface address to use (empty for default).
    fn udp_join_multicast(
        &mut self,
        socket: &UdpSocketHandle,
        group: &str,
        interface: &str,
    ) -> Result<(), IoError>;

    /// Send data to a specific address.
    ///
    /// Returns the number of bytes sent.
    fn udp_send_to(
        &mut self,
        socket: &UdpSocketHandle,
        data: &[u8],
        addr: &str,
        port: u16,
    ) -> Result<usize, IoError>;

    /// Receive data from a UDP socket (non-blocking).
    ///
    /// Returns `None` if no data is available.
    /// Returns `Some((len, addr, port))` on success where:
    /// - `len` is the number of bytes received (written to `buf`)
    /// - `addr` is the sender's IP address as a string
    /// - `port` is the sender's port number
    fn udp_recv_from(
        &mut self,
        socket: &UdpSocketHandle,
        buf: &mut [u8],
    ) -> Option<(usize, String, u16)>;

    /// Check if data is available to receive on a UDP socket.
    fn udp_pending(&self, socket: &UdpSocketHandle) -> i32;

    /// Close a UDP socket.
    fn udp_close(&mut self, socket: UdpSocketHandle);

    /// Bind a UDP socket to a specific interface IP for outgoing packets.
    ///
    /// This is used for broadcast sockets to ensure packets go out on the
    /// correct interface in multi-NIC setups. Call this before `udp_send_to`.
    ///
    /// Default implementation does nothing (uses OS routing).
    fn udp_bind_interface(
        &mut self,
        _socket: &UdpSocketHandle,
        _interface: &str,
    ) -> Result<(), IoError> {
        Ok(())
    }

    // -------------------------------------------------------------------------
    // TCP Operations
    // -------------------------------------------------------------------------

    /// Create a new TCP socket.
    fn tcp_create(&mut self) -> Result<TcpSocketHandle, IoError>;

    /// Initiate a TCP connection (non-blocking).
    ///
    /// This starts the connection process. Use `tcp_is_connected()` to check
    /// when the connection is established.
    fn tcp_connect(
        &mut self,
        socket: &TcpSocketHandle,
        addr: &str,
        port: u16,
    ) -> Result<(), IoError>;

    /// Check if a TCP socket is connected.
    fn tcp_is_connected(&self, socket: &TcpSocketHandle) -> bool;

    /// Check if a TCP socket is still valid (not closed due to error).
    fn tcp_is_valid(&self, socket: &TcpSocketHandle) -> bool;

    /// Set line buffering mode on a TCP socket.
    ///
    /// - `true`: Line-buffered mode - `tcp_recv_line()` returns complete lines
    /// - `false`: Raw mode - `tcp_recv_raw()` returns data as available
    fn tcp_set_line_buffering(
        &mut self,
        socket: &TcpSocketHandle,
        enabled: bool,
    ) -> Result<(), IoError>;

    /// Send data over a TCP connection.
    ///
    /// Returns the number of bytes sent.
    fn tcp_send(&mut self, socket: &TcpSocketHandle, data: &[u8]) -> Result<usize, IoError>;

    /// Receive a complete line from a TCP socket (non-blocking).
    ///
    /// Only works in line-buffered mode.
    /// Returns `None` if no complete line is available.
    fn tcp_recv_line(&mut self, socket: &TcpSocketHandle, buf: &mut [u8]) -> Option<usize>;

    /// Receive raw data from a TCP socket (non-blocking).
    ///
    /// Only works in raw mode.
    /// Returns `None` if no data is available.
    fn tcp_recv_raw(&mut self, socket: &TcpSocketHandle, buf: &mut [u8]) -> Option<usize>;

    /// Get number of buffered items waiting to be received.
    fn tcp_pending(&self, socket: &TcpSocketHandle) -> i32;

    /// Close a TCP socket.
    fn tcp_close(&mut self, socket: TcpSocketHandle);

    // -------------------------------------------------------------------------
    // Utility
    // -------------------------------------------------------------------------

    /// Get current timestamp in milliseconds since some epoch.
    ///
    /// Used for timeouts and rate limiting. The epoch doesn't matter as long
    /// as it's consistent within the session.
    fn current_time_ms(&self) -> u64;

    /// Log a debug message.
    ///
    /// On native, this goes to the logging framework.
    /// On WASM, this goes to SignalK's debug output.
    fn debug(&self, msg: &str);

    /// Log an info message.
    ///
    /// On native, this goes to the logging framework.
    /// On WASM, this goes to SignalK's info output.
    fn info(&self, msg: &str);
}

// =============================================================================
// Helper Methods
// =============================================================================

/// Extension methods for IoProvider.
pub trait IoProviderExt: IoProvider {
    /// Send a line over TCP with CRLF terminator.
    fn tcp_send_line(&mut self, socket: &TcpSocketHandle, line: &str) -> Result<usize, IoError> {
        let data = format!("{}\r\n", line);
        self.tcp_send(socket, data.as_bytes())
    }

    /// Receive a line as a String from TCP.
    fn tcp_recv_line_string(&mut self, socket: &TcpSocketHandle) -> Option<String> {
        let mut buf = [0u8; 1024];
        self.tcp_recv_line(socket, &mut buf)
            .map(|len| String::from_utf8_lossy(&buf[..len]).to_string())
    }
}

// Blanket implementation for all IoProvider types
impl<T: IoProvider> IoProviderExt for T {}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_io_error_display() {
        let err = IoError::new(-1, "Test error");
        assert_eq!(format!("{}", err), "Test error (code -1)");
    }

    #[test]
    fn test_io_error_would_block() {
        let err = IoError::would_block();
        assert!(err.is_would_block());
    }

    #[test]
    fn test_socket_handles() {
        let udp = UdpSocketHandle(42);
        let tcp = TcpSocketHandle(43);
        assert_eq!(udp.0, 42);
        assert_eq!(tcp.0, 43);
    }
}
