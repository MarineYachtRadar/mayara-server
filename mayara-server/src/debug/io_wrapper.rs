//! DebugIoProvider - Wrapper that captures all I/O for debugging.
//!
//! This wrapper implements IoProvider by delegating to an inner provider
//! while capturing all network traffic for the debug panel.

use std::collections::HashMap;
use std::sync::Arc;

use mayara_core::io::{IoError, IoProvider, TcpSocketHandle, UdpSocketHandle};

use super::decoders::ProtocolDecoder;
use super::hub::DebugHub;
use super::{EventSource, IoDirection, ProtocolType, SocketOperation};

// =============================================================================
// DebugIoProvider
// =============================================================================

/// Wrapper around any IoProvider that captures all I/O for debugging.
///
/// All operations are delegated to the inner provider, with events
/// submitted to the DebugHub for real-time display.
pub struct DebugIoProvider<T: IoProvider> {
    /// The inner provider to delegate to.
    inner: T,

    /// Debug hub for event submission.
    hub: Arc<DebugHub>,

    /// Radar identifier.
    radar_id: String,

    /// Brand name (furuno, navico, etc.).
    brand: String,

    /// Protocol decoder for this brand.
    decoder: Box<dyn ProtocolDecoder + Send + Sync>,

    /// Track TCP socket destinations for logging recv events.
    tcp_destinations: HashMap<i32, (String, u16)>,

    /// Track UDP socket info.
    udp_info: HashMap<i32, UdpSocketInfo>,
}

#[derive(Clone, Default)]
struct UdpSocketInfo {
    bound_port: Option<u16>,
    multicast_groups: Vec<String>,
}

impl<T: IoProvider> DebugIoProvider<T> {
    /// Create a new DebugIoProvider wrapping the given provider.
    pub fn new(
        inner: T,
        hub: Arc<DebugHub>,
        radar_id: String,
        brand: String,
    ) -> Self {
        let decoder = super::decoders::create_decoder(&brand);
        Self {
            inner,
            hub,
            radar_id,
            brand,
            decoder,
            tcp_destinations: HashMap::new(),
            udp_info: HashMap::new(),
        }
    }

    /// Get a reference to the inner provider.
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Get a mutable reference to the inner provider.
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Submit a data event to the hub.
    fn submit_data(
        &self,
        direction: IoDirection,
        protocol: ProtocolType,
        remote_addr: &str,
        remote_port: u16,
        data: &[u8],
    ) {
        log::debug!(
            "[DebugIoProvider] {} {} {:?} {}:{} {} bytes",
            self.radar_id, self.brand, direction, remote_addr, remote_port, data.len()
        );
        let decoded = self.decoder.decode(data, direction);
        let event = self
            .hub
            .event_builder(&self.radar_id, &self.brand)
            .source(EventSource::IoProvider)
            .data(direction, protocol, remote_addr, remote_port, data, Some(decoded));
        self.hub.submit(event);
    }

    /// Submit a socket operation event.
    fn submit_socket_op(&self, operation: SocketOperation, success: bool, error: Option<String>) {
        let event = self
            .hub
            .event_builder(&self.radar_id, &self.brand)
            .source(EventSource::IoProvider)
            .socket_op(operation, success, error);
        self.hub.submit(event);
    }
}

// =============================================================================
// IoProvider Implementation
// =============================================================================

impl<T: IoProvider> IoProvider for DebugIoProvider<T> {
    // -------------------------------------------------------------------------
    // UDP Operations
    // -------------------------------------------------------------------------

    fn udp_create(&mut self) -> Result<UdpSocketHandle, IoError> {
        let result = self.inner.udp_create();
        self.submit_socket_op(
            SocketOperation::Create {
                socket_type: ProtocolType::Udp,
            },
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string()),
        );
        if let Ok(handle) = &result {
            self.udp_info.insert(handle.0, UdpSocketInfo::default());
        }
        result
    }

    fn udp_bind(&mut self, socket: &UdpSocketHandle, port: u16) -> Result<(), IoError> {
        let result = self.inner.udp_bind(socket, port);
        self.submit_socket_op(
            SocketOperation::Bind { port },
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string()),
        );
        if result.is_ok() {
            if let Some(info) = self.udp_info.get_mut(&socket.0) {
                info.bound_port = Some(port);
            }
        }
        result
    }

    fn udp_set_broadcast(
        &mut self,
        socket: &UdpSocketHandle,
        enabled: bool,
    ) -> Result<(), IoError> {
        let result = self.inner.udp_set_broadcast(socket, enabled);
        self.submit_socket_op(
            SocketOperation::SetBroadcast { enabled },
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string()),
        );
        result
    }

    fn udp_join_multicast(
        &mut self,
        socket: &UdpSocketHandle,
        group: &str,
        interface: &str,
    ) -> Result<(), IoError> {
        let result = self.inner.udp_join_multicast(socket, group, interface);
        self.submit_socket_op(
            SocketOperation::JoinMulticast {
                group: group.to_string(),
                interface: interface.to_string(),
            },
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string()),
        );
        if result.is_ok() {
            if let Some(info) = self.udp_info.get_mut(&socket.0) {
                info.multicast_groups.push(group.to_string());
            }
        }
        result
    }

    fn udp_send_to(
        &mut self,
        socket: &UdpSocketHandle,
        data: &[u8],
        addr: &str,
        port: u16,
    ) -> Result<usize, IoError> {
        let result = self.inner.udp_send_to(socket, data, addr, port);
        if result.is_ok() {
            self.submit_data(IoDirection::Send, ProtocolType::Udp, addr, port, data);
        }
        result
    }

    fn udp_recv_from(
        &mut self,
        socket: &UdpSocketHandle,
        buf: &mut [u8],
    ) -> Option<(usize, String, u16)> {
        let result = self.inner.udp_recv_from(socket, buf);
        if let Some((len, addr, port)) = &result {
            self.submit_data(
                IoDirection::Recv,
                ProtocolType::Udp,
                addr,
                *port,
                &buf[..*len],
            );
        }
        result
    }

    fn udp_pending(&self, socket: &UdpSocketHandle) -> i32 {
        self.inner.udp_pending(socket)
    }

    fn udp_close(&mut self, socket: UdpSocketHandle) {
        self.udp_info.remove(&socket.0);
        self.submit_socket_op(SocketOperation::Close, true, None);
        self.inner.udp_close(socket);
    }

    fn udp_bind_interface(
        &mut self,
        socket: &UdpSocketHandle,
        interface: &str,
    ) -> Result<(), IoError> {
        self.inner.udp_bind_interface(socket, interface)
    }

    // -------------------------------------------------------------------------
    // TCP Operations
    // -------------------------------------------------------------------------

    fn tcp_create(&mut self) -> Result<TcpSocketHandle, IoError> {
        let result = self.inner.tcp_create();
        self.submit_socket_op(
            SocketOperation::Create {
                socket_type: ProtocolType::Tcp,
            },
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string()),
        );
        result
    }

    fn tcp_connect(
        &mut self,
        socket: &TcpSocketHandle,
        addr: &str,
        port: u16,
    ) -> Result<(), IoError> {
        let result = self.inner.tcp_connect(socket, addr, port);
        self.submit_socket_op(
            SocketOperation::Connect {
                addr: addr.to_string(),
                port,
            },
            result.is_ok(),
            result.as_ref().err().map(|e| e.to_string()),
        );
        if result.is_ok() {
            self.tcp_destinations
                .insert(socket.0, (addr.to_string(), port));
        }
        result
    }

    fn tcp_is_connected(&self, socket: &TcpSocketHandle) -> bool {
        self.inner.tcp_is_connected(socket)
    }

    fn tcp_is_valid(&self, socket: &TcpSocketHandle) -> bool {
        self.inner.tcp_is_valid(socket)
    }

    fn tcp_set_line_buffering(
        &mut self,
        socket: &TcpSocketHandle,
        enabled: bool,
    ) -> Result<(), IoError> {
        self.inner.tcp_set_line_buffering(socket, enabled)
    }

    fn tcp_send(&mut self, socket: &TcpSocketHandle, data: &[u8]) -> Result<usize, IoError> {
        let result = self.inner.tcp_send(socket, data);
        if result.is_ok() {
            let (addr, port) = self
                .tcp_destinations
                .get(&socket.0)
                .cloned()
                .unwrap_or_else(|| ("unknown".to_string(), 0));
            self.submit_data(IoDirection::Send, ProtocolType::Tcp, &addr, port, data);
        }
        result
    }

    fn tcp_recv_line(&mut self, socket: &TcpSocketHandle, buf: &mut [u8]) -> Option<usize> {
        let result = self.inner.tcp_recv_line(socket, buf);
        if let Some(len) = result {
            let (addr, port) = self
                .tcp_destinations
                .get(&socket.0)
                .cloned()
                .unwrap_or_else(|| ("unknown".to_string(), 0));
            self.submit_data(
                IoDirection::Recv,
                ProtocolType::Tcp,
                &addr,
                port,
                &buf[..len],
            );
        }
        result
    }

    fn tcp_recv_raw(&mut self, socket: &TcpSocketHandle, buf: &mut [u8]) -> Option<usize> {
        let result = self.inner.tcp_recv_raw(socket, buf);
        if let Some(len) = result {
            let (addr, port) = self
                .tcp_destinations
                .get(&socket.0)
                .cloned()
                .unwrap_or_else(|| ("unknown".to_string(), 0));
            self.submit_data(
                IoDirection::Recv,
                ProtocolType::Tcp,
                &addr,
                port,
                &buf[..len],
            );
        }
        result
    }

    fn tcp_pending(&self, socket: &TcpSocketHandle) -> i32 {
        self.inner.tcp_pending(socket)
    }

    fn tcp_close(&mut self, socket: TcpSocketHandle) {
        self.tcp_destinations.remove(&socket.0);
        self.submit_socket_op(SocketOperation::Close, true, None);
        self.inner.tcp_close(socket);
    }

    // -------------------------------------------------------------------------
    // Utility
    // -------------------------------------------------------------------------

    fn current_time_ms(&self) -> u64 {
        self.inner.current_time_ms()
    }

    fn debug(&self, msg: &str) {
        self.inner.debug(msg);
    }

    fn info(&self, msg: &str) {
        self.inner.info(msg);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock IoProvider for testing.
    struct MockIoProvider;

    impl IoProvider for MockIoProvider {
        fn udp_create(&mut self) -> Result<UdpSocketHandle, IoError> {
            Ok(UdpSocketHandle(1))
        }

        fn udp_bind(&mut self, _socket: &UdpSocketHandle, _port: u16) -> Result<(), IoError> {
            Ok(())
        }

        fn udp_set_broadcast(
            &mut self,
            _socket: &UdpSocketHandle,
            _enabled: bool,
        ) -> Result<(), IoError> {
            Ok(())
        }

        fn udp_join_multicast(
            &mut self,
            _socket: &UdpSocketHandle,
            _group: &str,
            _interface: &str,
        ) -> Result<(), IoError> {
            Ok(())
        }

        fn udp_send_to(
            &mut self,
            _socket: &UdpSocketHandle,
            data: &[u8],
            _addr: &str,
            _port: u16,
        ) -> Result<usize, IoError> {
            Ok(data.len())
        }

        fn udp_recv_from(
            &mut self,
            _socket: &UdpSocketHandle,
            _buf: &mut [u8],
        ) -> Option<(usize, String, u16)> {
            None
        }

        fn udp_pending(&self, _socket: &UdpSocketHandle) -> i32 {
            0
        }

        fn udp_close(&mut self, _socket: UdpSocketHandle) {}

        fn tcp_create(&mut self) -> Result<TcpSocketHandle, IoError> {
            Ok(TcpSocketHandle(1))
        }

        fn tcp_connect(
            &mut self,
            _socket: &TcpSocketHandle,
            _addr: &str,
            _port: u16,
        ) -> Result<(), IoError> {
            Ok(())
        }

        fn tcp_is_connected(&self, _socket: &TcpSocketHandle) -> bool {
            true
        }

        fn tcp_is_valid(&self, _socket: &TcpSocketHandle) -> bool {
            true
        }

        fn tcp_set_line_buffering(
            &mut self,
            _socket: &TcpSocketHandle,
            _enabled: bool,
        ) -> Result<(), IoError> {
            Ok(())
        }

        fn tcp_send(&mut self, _socket: &TcpSocketHandle, data: &[u8]) -> Result<usize, IoError> {
            Ok(data.len())
        }

        fn tcp_recv_line(&mut self, _socket: &TcpSocketHandle, _buf: &mut [u8]) -> Option<usize> {
            None
        }

        fn tcp_recv_raw(&mut self, _socket: &TcpSocketHandle, _buf: &mut [u8]) -> Option<usize> {
            None
        }

        fn tcp_pending(&self, _socket: &TcpSocketHandle) -> i32 {
            0
        }

        fn tcp_close(&mut self, _socket: TcpSocketHandle) {}

        fn current_time_ms(&self) -> u64 {
            0
        }

        fn debug(&self, _msg: &str) {}
        fn info(&self, _msg: &str) {}
    }

    #[test]
    fn test_debug_io_provider_creation() {
        let hub = Arc::new(DebugHub::new());
        let _provider = DebugIoProvider::new(
            MockIoProvider,
            hub,
            "radar-1".to_string(),
            "furuno".to_string(),
        );
    }

    #[test]
    fn test_debug_io_provider_captures_udp_send() {
        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "furuno".to_string(),
        );

        let socket = provider.udp_create().unwrap();
        provider.udp_bind(&socket, 0).unwrap();
        provider
            .udp_send_to(&socket, b"test data", "172.31.1.4", 10050)
            .unwrap();

        // Check that events were captured
        let events = hub.get_all_events();
        assert!(events.len() >= 3); // create, bind, send
    }

    #[test]
    fn test_debug_io_provider_captures_tcp_send() {
        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "furuno".to_string(),
        );

        let socket = provider.tcp_create().unwrap();
        provider.tcp_connect(&socket, "172.31.1.4", 10050).unwrap();
        provider.tcp_send(&socket, b"$S69,50\r\n").unwrap();

        // Check that events were captured
        let events = hub.get_all_events();
        assert!(events.len() >= 3); // create, connect, send
    }
}
