//! Tokio implementation of IoProvider for the native server.
//!
//! This module provides `TokioIoProvider` which implements `mayara_core::IoProvider`
//! using tokio's async sockets in a poll-based interface.
//!
//! The key insight is that tokio sockets can be used in non-blocking mode,
//! matching the poll-based interface required by mayara-core.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Instant;

use mayara_core::io::{IoError, IoProvider, TcpSocketHandle, UdpSocketHandle};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

/// Find the interface name for a given IPv4 address.
#[cfg(target_os = "linux")]
fn find_interface_name_for_ip(ip: &Ipv4Addr) -> Option<String> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    use std::net::IpAddr;

    let interfaces = NetworkInterface::show().ok()?;

    for itf in &interfaces {
        for addr in &itf.addr {
            if let IpAddr::V4(nic_ip) = addr.ip() {
                if &nic_ip == ip {
                    return Some(itf.name.clone());
                }
            }
        }
    }

    None
}

/// Internal state for a UDP socket
struct UdpSocketState {
    socket: UdpSocket,
}

/// Internal state for a TCP socket
struct TcpSocketState {
    socket: Option<tokio::net::TcpStream>,
    connecting: bool,
    line_buffer: String,
    line_buffered: bool,
}

/// Tokio implementation of IoProvider for the native server.
///
/// Wraps tokio sockets in a poll-based interface that matches the
/// IoProvider trait used by mayara-core's RadarLocator.
///
/// # Usage
///
/// ```rust,ignore
/// use mayara_core::locator::RadarLocator;
/// use mayara_server::tokio_io::TokioIoProvider;
///
/// let mut io = TokioIoProvider::new();
/// let mut locator = RadarLocator::new();
/// locator.start(&mut io);
///
/// // In your main loop:
/// let new_radars = locator.poll(&mut io);
/// ```
pub struct TokioIoProvider {
    /// Next socket handle ID
    next_handle: i32,
    /// UDP sockets by handle
    udp_sockets: HashMap<i32, UdpSocketState>,
    /// TCP sockets by handle
    tcp_sockets: HashMap<i32, TcpSocketState>,
    /// Start time for current_time_ms calculation
    start_time: Instant,
}

impl TokioIoProvider {
    /// Create a new Tokio I/O provider.
    pub fn new() -> Self {
        Self {
            next_handle: 1,
            udp_sockets: HashMap::new(),
            tcp_sockets: HashMap::new(),
            start_time: Instant::now(),
        }
    }

    fn alloc_handle(&mut self) -> i32 {
        let handle = self.next_handle;
        self.next_handle += 1;
        handle
    }
}

impl Default for TokioIoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl IoProvider for TokioIoProvider {
    // -------------------------------------------------------------------------
    // UDP Operations
    // -------------------------------------------------------------------------

    fn udp_create(&mut self) -> Result<UdpSocketHandle, IoError> {
        // Create a socket using socket2 for more control
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| IoError::new(-1, format!("Failed to create socket: {}", e)))?;

        // Set non-blocking mode
        socket
            .set_nonblocking(true)
            .map_err(|e| IoError::new(-1, format!("Failed to set non-blocking: {}", e)))?;

        // Allow address reuse
        socket
            .set_reuse_address(true)
            .map_err(|e| IoError::new(-1, format!("Failed to set reuse address: {}", e)))?;

        #[cfg(unix)]
        {
            socket
                .set_reuse_port(true)
                .map_err(|e| IoError::new(-1, format!("Failed to set reuse port: {}", e)))?;
        }

        // Convert to tokio socket
        let std_socket: std::net::UdpSocket = socket.into();
        let tokio_socket = UdpSocket::from_std(std_socket)
            .map_err(|e| IoError::new(-1, format!("Failed to convert to tokio socket: {}", e)))?;

        let handle = self.alloc_handle();
        self.udp_sockets.insert(
            handle,
            UdpSocketState {
                socket: tokio_socket,
            },
        );
        Ok(UdpSocketHandle(handle))
    }

    fn udp_bind(&mut self, socket: &UdpSocketHandle, port: u16) -> Result<(), IoError> {
        // For tokio, binding happens at socket creation time via socket2
        // We need to rebind if the socket was created without binding
        let state = self
            .udp_sockets
            .get_mut(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;

        // Get the raw socket and rebind
        // Note: tokio sockets don't support rebinding, so we need to recreate
        let local_addr = state.socket.local_addr().ok();

        // If already bound to the right port, we're done
        if let Some(addr) = local_addr {
            if addr.port() == port || port == 0 {
                return Ok(());
            }
        }

        // Need to recreate the socket with the new bind
        // This is a limitation - socket2 must bind before converting to tokio
        let new_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| IoError::new(-1, format!("Failed to create socket: {}", e)))?;

        new_socket
            .set_nonblocking(true)
            .map_err(|e| IoError::new(-1, format!("Failed to set non-blocking: {}", e)))?;
        new_socket
            .set_reuse_address(true)
            .map_err(|e| IoError::new(-1, format!("Failed to set reuse address: {}", e)))?;

        #[cfg(unix)]
        {
            let _ = new_socket.set_reuse_port(true);
        }

        // Bind to all interfaces
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
        new_socket
            .bind(&bind_addr.into())
            .map_err(|e| IoError::new(-1, format!("Failed to bind to port {}: {}", port, e)))?;

        let std_socket: std::net::UdpSocket = new_socket.into();
        let tokio_socket = UdpSocket::from_std(std_socket)
            .map_err(|e| IoError::new(-1, format!("Failed to convert to tokio socket: {}", e)))?;

        state.socket = tokio_socket;
        Ok(())
    }

    fn udp_set_broadcast(
        &mut self,
        socket: &UdpSocketHandle,
        enabled: bool,
    ) -> Result<(), IoError> {
        let state = self
            .udp_sockets
            .get(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;

        state
            .socket
            .set_broadcast(enabled)
            .map_err(|e| IoError::new(-1, format!("Failed to set broadcast: {}", e)))
    }

    fn udp_join_multicast(
        &mut self,
        socket: &UdpSocketHandle,
        group: &str,
        interface: &str,
    ) -> Result<(), IoError> {
        let state = self
            .udp_sockets
            .get(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;

        let multicast_addr: Ipv4Addr = group.parse().map_err(|e| {
            IoError::new(-1, format!("Invalid multicast address '{}': {}", group, e))
        })?;

        let interface_addr: Ipv4Addr = if interface.is_empty() {
            Ipv4Addr::UNSPECIFIED
        } else {
            interface.parse().map_err(|e| {
                IoError::new(
                    -1,
                    format!("Invalid interface address '{}': {}", interface, e),
                )
            })?
        };

        // CRITICAL: Linux requires disabling IP_MULTICAST_ALL
        // Without this, the kernel delivers multicast packets to ALL sockets that joined
        // ANY multicast group, not just the specific one we want.
        // See: https://man7.org/linux/man-pages/man7/ip.7.html (IP_MULTICAST_ALL)
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;

            // IP_MULTICAST_ALL = 49 on Linux
            const IP_MULTICAST_ALL: libc::c_int = 49;

            unsafe {
                let optval: libc::c_int = 0; // Disable IP_MULTICAST_ALL
                let ret = libc::setsockopt(
                    state.socket.as_raw_fd(),
                    libc::SOL_IP,
                    IP_MULTICAST_ALL,
                    &optval as *const _ as *const libc::c_void,
                    std::mem::size_of_val(&optval) as libc::socklen_t,
                );
                if ret != 0 {
                    log::warn!(
                        "Failed to disable IP_MULTICAST_ALL: {}",
                        std::io::Error::last_os_error()
                    );
                } else {
                    log::debug!("Disabled IP_MULTICAST_ALL for multicast group {}", group);
                }
            }
        }

        state
            .socket
            .join_multicast_v4(multicast_addr, interface_addr)
            .map_err(|e| IoError::new(-1, format!("Failed to join multicast {}: {}", group, e)))
    }

    fn udp_send_to(
        &mut self,
        socket: &UdpSocketHandle,
        data: &[u8],
        addr: &str,
        port: u16,
    ) -> Result<usize, IoError> {
        let state = self
            .udp_sockets
            .get(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;

        let ip: Ipv4Addr = addr
            .parse()
            .map_err(|e| IoError::new(-1, format!("Invalid address '{}': {}", addr, e)))?;
        let target = SocketAddr::V4(SocketAddrV4::new(ip, port));

        // Use try_send_to for non-blocking send
        state
            .socket
            .try_send_to(data, target)
            .map_err(|e| IoError::new(-1, format!("Send failed: {}", e)))
    }

    fn udp_recv_from(
        &mut self,
        socket: &UdpSocketHandle,
        buf: &mut [u8],
    ) -> Option<(usize, String, u16)> {
        let state = self.udp_sockets.get(&socket.0)?;

        // Use try_recv_from for non-blocking receive
        match state.socket.try_recv_from(buf) {
            Ok((len, addr)) => {
                let ip = match addr {
                    SocketAddr::V4(v4) => v4.ip().to_string(),
                    SocketAddr::V6(v6) => v6.ip().to_string(),
                };
                Some((len, ip, addr.port()))
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => None,
            Err(_) => None,
        }
    }

    fn udp_pending(&self, socket: &UdpSocketHandle) -> i32 {
        // Tokio doesn't have a direct pending check, return -1 for unknown
        // The caller should use try_recv_from instead
        self.udp_sockets.get(&socket.0).map(|_| -1).unwrap_or(-1)
    }

    fn udp_close(&mut self, socket: UdpSocketHandle) {
        self.udp_sockets.remove(&socket.0);
    }

    fn udp_bind_interface(
        &mut self,
        socket: &UdpSocketHandle,
        interface: &str,
    ) -> Result<(), IoError> {
        let state = self
            .udp_sockets
            .get_mut(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;

        // Get current port from the socket
        let current_port = state.socket.local_addr().map(|a| a.port()).unwrap_or(0);

        // Parse the interface IP address
        let interface_ip: Ipv4Addr = interface.parse().map_err(|e| {
            IoError::new(
                -1,
                format!("Invalid interface address '{}': {}", interface, e),
            )
        })?;

        // Recreate the socket bound to the specific interface
        let new_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| IoError::new(-1, format!("Failed to create socket: {}", e)))?;

        new_socket
            .set_nonblocking(true)
            .map_err(|e| IoError::new(-1, format!("Failed to set non-blocking: {}", e)))?;
        new_socket
            .set_reuse_address(true)
            .map_err(|e| IoError::new(-1, format!("Failed to set reuse address: {}", e)))?;

        #[cfg(unix)]
        {
            let _ = new_socket.set_reuse_port(true);
        }

        // Re-enable broadcast mode (was set on original socket)
        new_socket
            .set_broadcast(true)
            .map_err(|e| IoError::new(-1, format!("Failed to set broadcast: {}", e)))?;

        // IMPORTANT: Bind to 0.0.0.0:port to receive broadcast responses
        // (binding to interface_ip:port would prevent receiving broadcasts)
        // We use IP_MULTICAST_IF to control OUTGOING packets only
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, current_port);
        new_socket
            .bind(&bind_addr.into())
            .map_err(|e| IoError::new(-1, format!("Failed to bind to {}: {}", bind_addr, e)))?;

        // Set the outgoing interface for multicast/broadcast packets
        // This ensures broadcasts go out on the correct NIC without affecting receive
        new_socket
            .set_multicast_if_v4(&interface_ip)
            .map_err(|e| IoError::new(-1, format!("Failed to set multicast interface: {}", e)))?;

        // On Linux, also bind to the device to ensure proper routing
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            // Find the interface name for this IP
            if let Some(iface_name) = find_interface_name_for_ip(&interface_ip) {
                unsafe {
                    let iface_bytes = iface_name.as_bytes();
                    let ret = libc::setsockopt(
                        new_socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_BINDTODEVICE,
                        iface_bytes.as_ptr() as *const libc::c_void,
                        iface_bytes.len() as libc::socklen_t,
                    );
                    if ret == 0 {
                        log::debug!("Bound socket to device {}", iface_name);
                    } else {
                        log::debug!(
                            "SO_BINDTODEVICE failed (may need CAP_NET_RAW): {}",
                            std::io::Error::last_os_error()
                        );
                    }
                }
            }
        }

        log::debug!(
            "UDP socket configured for interface {} port {}",
            interface_ip,
            current_port
        );

        let std_socket: std::net::UdpSocket = new_socket.into();
        let tokio_socket = UdpSocket::from_std(std_socket)
            .map_err(|e| IoError::new(-1, format!("Failed to convert to tokio socket: {}", e)))?;

        state.socket = tokio_socket;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // TCP Operations
    // -------------------------------------------------------------------------

    fn tcp_create(&mut self) -> Result<TcpSocketHandle, IoError> {
        let handle = self.alloc_handle();
        self.tcp_sockets.insert(
            handle,
            TcpSocketState {
                socket: None,
                connecting: false,
                line_buffer: String::new(),
                line_buffered: false,
            },
        );
        Ok(TcpSocketHandle(handle))
    }

    fn tcp_connect(
        &mut self,
        socket: &TcpSocketHandle,
        addr: &str,
        port: u16,
    ) -> Result<(), IoError> {
        let state = self
            .tcp_sockets
            .get_mut(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;

        let ip: Ipv4Addr = addr
            .parse()
            .map_err(|e| IoError::new(-1, format!("Invalid address '{}': {}", addr, e)))?;
        let target = SocketAddr::V4(SocketAddrV4::new(ip, port));

        // Start async connect - we'll poll for completion
        state.connecting = true;

        // For sync interface, we use std::net::TcpStream with non-blocking connect
        // then convert to tokio later when connected
        // This is simplified - real implementation would use tokio spawn
        match std::net::TcpStream::connect_timeout(&target, std::time::Duration::from_secs(5)) {
            Ok(stream) => {
                stream
                    .set_nonblocking(true)
                    .map_err(|e| IoError::new(-1, format!("Failed to set non-blocking: {}", e)))?;
                let tokio_stream = tokio::net::TcpStream::from_std(stream)
                    .map_err(|e| IoError::new(-1, format!("Failed to convert to tokio: {}", e)))?;
                state.socket = Some(tokio_stream);
                state.connecting = false;
                Ok(())
            }
            Err(e) => {
                state.connecting = false;
                Err(IoError::new(-1, format!("Connect failed: {}", e)))
            }
        }
    }

    fn tcp_is_connected(&self, socket: &TcpSocketHandle) -> bool {
        self.tcp_sockets
            .get(&socket.0)
            .map(|s| s.socket.is_some() && !s.connecting)
            .unwrap_or(false)
    }

    fn tcp_is_valid(&self, socket: &TcpSocketHandle) -> bool {
        self.tcp_sockets.get(&socket.0).is_some()
    }

    fn tcp_set_line_buffering(
        &mut self,
        socket: &TcpSocketHandle,
        enabled: bool,
    ) -> Result<(), IoError> {
        let state = self
            .tcp_sockets
            .get_mut(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;
        state.line_buffered = enabled;
        Ok(())
    }

    fn tcp_send(&mut self, socket: &TcpSocketHandle, data: &[u8]) -> Result<usize, IoError> {
        let state = self
            .tcp_sockets
            .get(&socket.0)
            .ok_or_else(|| IoError::new(-1, "Invalid socket handle"))?;

        let stream = state
            .socket
            .as_ref()
            .ok_or_else(|| IoError::not_connected())?;

        stream
            .try_write(data)
            .map_err(|e| IoError::new(-1, format!("Write failed: {}", e)))
    }

    fn tcp_recv_line(&mut self, socket: &TcpSocketHandle, buf: &mut [u8]) -> Option<usize> {
        let state = self.tcp_sockets.get_mut(&socket.0)?;
        let stream = state.socket.as_ref()?;

        // Read into internal buffer
        let mut temp_buf = [0u8; 1024];
        match stream.try_read(&mut temp_buf) {
            Ok(0) => {
                log::debug!("tcp_recv_line: EOF");
                return None;
            }
            Ok(n) => {
                let data = String::from_utf8_lossy(&temp_buf[..n]);
                log::debug!("tcp_recv_line: read {} bytes: {:?}", n, data);
                state.line_buffer.push_str(&data);
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                // No data available yet - this is normal for non-blocking I/O
            }
            Err(e) => {
                log::debug!("tcp_recv_line: error: {}", e);
                return None;
            }
        }

        // Check for complete line
        if let Some(pos) = state.line_buffer.find('\n') {
            let line = state.line_buffer[..pos].trim_end_matches('\r').to_string();
            let line_bytes = line.as_bytes();
            let len = line_bytes.len().min(buf.len());
            buf[..len].copy_from_slice(&line_bytes[..len]);

            // Remove the line from buffer (including newline)
            state.line_buffer = state.line_buffer[pos + 1..].to_string();
            log::debug!("tcp_recv_line: returning line ({} bytes): {:?}", len, line);
            Some(len)
        } else {
            if !state.line_buffer.is_empty() {
                log::debug!(
                    "tcp_recv_line: buffer has {} bytes but no newline yet",
                    state.line_buffer.len()
                );
            }
            None
        }
    }

    fn tcp_recv_raw(&mut self, socket: &TcpSocketHandle, buf: &mut [u8]) -> Option<usize> {
        let state = self.tcp_sockets.get(&socket.0)?;
        let stream = state.socket.as_ref()?;

        match stream.try_read(buf) {
            Ok(0) => None, // EOF
            Ok(n) => Some(n),
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => None,
            Err(_) => None,
        }
    }

    fn tcp_pending(&self, socket: &TcpSocketHandle) -> i32 {
        self.tcp_sockets
            .get(&socket.0)
            .map(|s| s.line_buffer.len() as i32)
            .unwrap_or(-1)
    }

    fn tcp_close(&mut self, socket: TcpSocketHandle) {
        self.tcp_sockets.remove(&socket.0);
    }

    // -------------------------------------------------------------------------
    // Utility
    // -------------------------------------------------------------------------

    fn current_time_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    fn debug(&self, msg: &str) {
        log::debug!("{}", msg);
    }

    fn info(&self, msg: &str) {
        log::info!("{}", msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_time_ms() {
        let io = TokioIoProvider::new();
        let time1 = io.current_time_ms();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let time2 = io.current_time_ms();
        assert!(time2 >= time1 + 10);
    }

    #[test]
    fn test_handle_allocation() {
        let mut io = TokioIoProvider::new();
        let h1 = io.alloc_handle();
        let h2 = io.alloc_handle();
        assert_ne!(h1, h2);
    }
}
