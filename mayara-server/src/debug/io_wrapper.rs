//! DebugIoProvider - Wrapper that captures all I/O for debugging.
//!
//! This wrapper implements IoProvider by delegating to an inner provider
//! while capturing all network traffic for the debug panel.

use std::collections::HashMap;
use std::sync::Arc;

use mayara_core::io::{IoError, IoProvider, TcpSocketHandle, UdpSocketHandle};

use super::decoders::ProtocolDecoder;
use super::hub::DebugHub;
use super::{DecodedMessage, EventSource, IoDirection, ProtocolType, SocketOperation};

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

    /// Track control values for state change detection.
    control_state: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Default)]
struct UdpSocketInfo {
    bound_port: Option<u16>,
    multicast_groups: Vec<String>,
}

impl<T: IoProvider> DebugIoProvider<T> {
    /// Create a new DebugIoProvider wrapping the given provider.
    pub fn new(inner: T, hub: Arc<DebugHub>, radar_id: String, brand: String) -> Self {
        let decoder = super::decoders::create_decoder(&brand);
        Self {
            inner,
            hub,
            radar_id,
            brand,
            decoder,
            tcp_destinations: HashMap::new(),
            udp_info: HashMap::new(),
            control_state: HashMap::new(),
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
        &mut self,
        direction: IoDirection,
        protocol: ProtocolType,
        remote_addr: &str,
        remote_port: u16,
        data: &[u8],
    ) {
        log::debug!(
            "[DebugIoProvider] {} {} {:?} {}:{} {} bytes",
            self.radar_id,
            self.brand,
            direction,
            remote_addr,
            remote_port,
            data.len()
        );
        let decoded = self.decoder.decode(data, direction);

        // Check for state changes from received responses
        if direction == IoDirection::Recv {
            self.check_state_changes(&decoded);
        }

        let event = self
            .hub
            .event_builder(&self.radar_id, &self.brand)
            .source(EventSource::IoProvider)
            .data(
                direction,
                protocol,
                remote_addr,
                remote_port,
                data,
                Some(decoded),
            );
        self.hub.submit(event);
    }

    /// Check for state changes in a decoded message and emit StateChange events.
    fn check_state_changes(&mut self, decoded: &DecodedMessage) {
        // Extract control values from decoded message
        let control_values = self.extract_control_values(decoded);

        for (control_id, new_value) in control_values {
            // Compare with previous state
            let changed = match self.control_state.get(&control_id) {
                Some(old_value) => old_value != &new_value,
                None => true, // First time seeing this control
            };

            if changed {
                let old_value = self
                    .control_state
                    .insert(control_id.clone(), new_value.clone())
                    .unwrap_or(serde_json::Value::Null);

                // Only emit if we had a previous value (not first observation)
                if old_value != serde_json::Value::Null {
                    log::debug!(
                        "[DebugIoProvider] State change: {} {} -> {}",
                        control_id,
                        old_value,
                        new_value
                    );

                    let event = self
                        .hub
                        .event_builder(&self.radar_id, &self.brand)
                        .source(EventSource::IoProvider)
                        .state_change(&control_id, old_value, new_value, None);
                    self.hub.submit(event);
                }
            }
        }
    }

    /// Extract control values from a decoded message.
    fn extract_control_values(&self, decoded: &DecodedMessage) -> Vec<(String, serde_json::Value)> {
        match decoded {
            DecodedMessage::Furuno {
                message_type,
                command_id,
                fields,
                ..
            } => self.extract_furuno_controls(message_type, command_id.as_deref(), fields),

            DecodedMessage::Navico {
                message_type,
                fields,
                ..
            } => self.extract_navico_controls(message_type, fields),

            DecodedMessage::Raymarine {
                message_type,
                fields,
                ..
            } => self.extract_raymarine_controls(message_type, fields),

            DecodedMessage::Garmin {
                message_type,
                fields,
                ..
            } => self.extract_garmin_controls(message_type, fields),

            DecodedMessage::Unknown { .. } => Vec::new(),
        }
    }

    /// Extract control values from Furuno decoded message.
    fn extract_furuno_controls(
        &self,
        message_type: &str,
        command_id: Option<&str>,
        fields: &serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        // Only process responses ($N messages)
        if message_type != "response" {
            return Vec::new();
        }

        let cmd = command_id.unwrap_or("");
        // Command IDs are like "N63", "N64", etc.
        let cmd_num = cmd.trim_start_matches('N');

        match cmd_num {
            // Gain (0x63)
            "63" => {
                let auto = fields
                    .get("auto")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let value = fields.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                vec![
                    ("gain".to_string(), serde_json::json!(value)),
                    ("gainAuto".to_string(), serde_json::json!(auto)),
                ]
            }
            // Sea clutter (0x64)
            "64" => {
                let auto = fields
                    .get("auto")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let value = fields.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                vec![
                    ("sea".to_string(), serde_json::json!(value)),
                    ("seaAuto".to_string(), serde_json::json!(auto)),
                ]
            }
            // Rain clutter (0x65)
            "65" => {
                let auto = fields
                    .get("auto")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let value = fields.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                vec![
                    ("rain".to_string(), serde_json::json!(value)),
                    ("rainAuto".to_string(), serde_json::json!(auto)),
                ]
            }
            // Status/Power (0x69)
            "69" => {
                // First param is mode: 1=standby, 2=transmit
                if let Some(params) = fields.get("allParams").and_then(|v| v.as_array()) {
                    if let Some(mode) = params.first().and_then(|v| v.as_str()) {
                        let power = match mode {
                            "1" => "standby",
                            "2" => "transmit",
                            _ => "unknown",
                        };
                        return vec![("power".to_string(), serde_json::json!(power))];
                    }
                }
                // Fallback to level field (from Bird Mode decoding, reused for status)
                if let Some(level) = fields.get("level").and_then(|v| v.as_i64()) {
                    let power = match level {
                        1 => "standby",
                        2 => "transmit",
                        _ => "unknown",
                    };
                    return vec![("power".to_string(), serde_json::json!(power))];
                }
                Vec::new()
            }
            // Unknown commands: track raw params for reverse engineering
            _ => {
                // Use allParams if available, otherwise the whole fields object
                if let Some(params) = fields.get("allParams") {
                    vec![(cmd.to_string(), params.clone())]
                } else if !fields.is_null() && fields.as_object().map_or(false, |o| !o.is_empty()) {
                    vec![(cmd.to_string(), fields.clone())]
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Extract control values from Navico decoded message.
    fn extract_navico_controls(
        &self,
        message_type: &str,
        fields: &serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        let mut controls = Vec::new();

        match message_type {
            "status" => {
                // Power state from status report
                if let Some(power) = fields.get("power") {
                    controls.push(("power".to_string(), power.clone()));
                }
                if let Some(power_str) = fields.get("powerStr") {
                    controls.push(("powerStr".to_string(), power_str.clone()));
                }
            }
            "settings" => {
                // Gain, sea, rain from settings report
                if let Some(gain) = fields.get("gain") {
                    controls.push(("gain".to_string(), gain.clone()));
                }
                if let Some(gain_auto) = fields.get("gainAuto") {
                    controls.push(("gainAuto".to_string(), gain_auto.clone()));
                }
                if let Some(sea) = fields.get("sea") {
                    controls.push(("sea".to_string(), sea.clone()));
                }
                if let Some(sea_auto) = fields.get("seaAuto") {
                    controls.push(("seaAuto".to_string(), sea_auto.clone()));
                }
                if let Some(rain) = fields.get("rain") {
                    controls.push(("rain".to_string(), rain.clone()));
                }
                if let Some(interference) = fields.get("interference") {
                    controls.push(("interference".to_string(), interference.clone()));
                }
            }
            "range" => {
                if let Some(range) = fields.get("rangeRaw") {
                    controls.push(("range".to_string(), range.clone()));
                }
            }
            _ => {}
        }

        controls
    }

    /// Extract control values from Raymarine decoded message.
    fn extract_raymarine_controls(
        &self,
        message_type: &str,
        fields: &serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        // Only process status messages (both Quantum and RD)
        if message_type != "status" {
            return Vec::new();
        }

        let mut controls = Vec::new();

        // Common controls for both Quantum and RD status packets
        if let Some(power) = fields.get("power") {
            controls.push(("power".to_string(), power.clone()));
        }
        if let Some(power_str) = fields.get("powerStr") {
            controls.push(("powerStr".to_string(), power_str.clone()));
        }
        if let Some(gain) = fields.get("gain") {
            controls.push(("gain".to_string(), gain.clone()));
        }
        if let Some(gain_auto) = fields.get("gainAuto") {
            controls.push(("gainAuto".to_string(), gain_auto.clone()));
        }
        if let Some(sea) = fields.get("sea") {
            controls.push(("sea".to_string(), sea.clone()));
        }
        if let Some(sea_auto) = fields.get("seaAuto") {
            controls.push(("seaAuto".to_string(), sea_auto.clone()));
        }
        if let Some(rain) = fields.get("rain") {
            controls.push(("rain".to_string(), rain.clone()));
        }

        controls
    }

    /// Extract control values from Garmin decoded message.
    fn extract_garmin_controls(
        &self,
        message_type: &str,
        fields: &serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        // Only process status messages
        if message_type != "status" {
            return Vec::new();
        }

        let mut controls = Vec::new();

        // Garmin sends separate packets for each control, so we extract what's available
        if let Some(power) = fields.get("power") {
            controls.push(("power".to_string(), power.clone()));
        }
        if let Some(power_str) = fields.get("powerStr") {
            controls.push(("powerStr".to_string(), power_str.clone()));
        }
        if let Some(gain) = fields.get("gain") {
            controls.push(("gain".to_string(), gain.clone()));
        }
        if let Some(gain_auto) = fields.get("gainAuto") {
            controls.push(("gainAuto".to_string(), gain_auto.clone()));
        }
        if let Some(sea) = fields.get("sea") {
            controls.push(("sea".to_string(), sea.clone()));
        }
        if let Some(sea_auto) = fields.get("seaAuto") {
            controls.push(("seaAuto".to_string(), sea_auto.clone()));
        }
        if let Some(rain) = fields.get("rain") {
            controls.push(("rain".to_string(), rain.clone()));
        }
        if let Some(rain_auto) = fields.get("rainAuto") {
            controls.push(("rainAuto".to_string(), rain_auto.clone()));
        }
        if let Some(range) = fields.get("range") {
            controls.push(("range".to_string(), range.clone()));
        }

        controls
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

    #[test]
    fn test_state_change_detection() {
        use super::super::DebugEventPayload;

        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "furuno".to_string(),
        );

        // Simulate receiving gain response - first time (no state change event)
        let decoded1 = DecodedMessage::Furuno {
            message_type: "response".to_string(),
            command_id: Some("N63".to_string()),
            fields: serde_json::json!({"auto": false, "value": 50}),
            description: Some("Gain: 50 (Manual)".to_string()),
        };
        provider.check_state_changes(&decoded1);

        // Should have recorded state but no event (first observation)
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert_eq!(
            state_events.len(),
            0,
            "First observation should not emit event"
        );

        // Simulate receiving gain response with changed value
        let decoded2 = DecodedMessage::Furuno {
            message_type: "response".to_string(),
            command_id: Some("N63".to_string()),
            fields: serde_json::json!({"auto": false, "value": 75}),
            description: Some("Gain: 75 (Manual)".to_string()),
        };
        provider.check_state_changes(&decoded2);

        // Now we should have a state change event
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert_eq!(state_events.len(), 1, "Changed value should emit event");

        // Verify the event content
        if let DebugEventPayload::StateChange {
            control_id,
            before,
            after,
            ..
        } = &state_events[0].payload
        {
            assert_eq!(control_id, "gain");
            assert_eq!(*before, serde_json::json!(50));
            assert_eq!(*after, serde_json::json!(75));
        } else {
            panic!("Expected StateChange event");
        }
    }

    #[test]
    fn test_state_change_no_change() {
        use super::super::DebugEventPayload;

        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "furuno".to_string(),
        );

        // First observation
        let decoded = DecodedMessage::Furuno {
            message_type: "response".to_string(),
            command_id: Some("N63".to_string()),
            fields: serde_json::json!({"auto": false, "value": 50}),
            description: Some("Gain: 50 (Manual)".to_string()),
        };
        provider.check_state_changes(&decoded);

        // Same value again - no change
        provider.check_state_changes(&decoded);

        // Should still have no state change events
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert_eq!(state_events.len(), 0, "Same value should not emit event");
    }

    #[test]
    fn test_state_change_unknown_command() {
        use super::super::DebugEventPayload;

        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "furuno".to_string(),
        );

        // Simulate receiving unknown command N68 - first observation
        let decoded1 = DecodedMessage::Furuno {
            message_type: "response".to_string(),
            command_id: Some("N68".to_string()),
            fields: serde_json::json!({"allParams": ["1", "0", "50"]}),
            description: Some("Unknown command 68".to_string()),
        };
        provider.check_state_changes(&decoded1);

        // First observation - no event
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert_eq!(
            state_events.len(),
            0,
            "First observation should not emit event"
        );

        // Simulate receiving same command with different params
        let decoded2 = DecodedMessage::Furuno {
            message_type: "response".to_string(),
            command_id: Some("N68".to_string()),
            fields: serde_json::json!({"allParams": ["1", "0", "75"]}),
            description: Some("Unknown command 68".to_string()),
        };
        provider.check_state_changes(&decoded2);

        // Now we should have a state change event
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert_eq!(state_events.len(), 1, "Changed params should emit event");

        // Verify the event content - control_id should be "N68"
        if let DebugEventPayload::StateChange {
            control_id,
            before,
            after,
            ..
        } = &state_events[0].payload
        {
            assert_eq!(control_id, "N68");
            assert_eq!(*before, serde_json::json!(["1", "0", "50"]));
            assert_eq!(*after, serde_json::json!(["1", "0", "75"]));
        } else {
            panic!("Expected StateChange event");
        }
    }

    #[test]
    fn test_navico_state_change_detection() {
        use super::super::DebugEventPayload;

        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "navico".to_string(),
        );

        // First observation - Navico settings report
        let decoded1 = DecodedMessage::Navico {
            message_type: "settings".to_string(),
            report_id: Some(0x02),
            fields: serde_json::json!({
                "gain": 50,
                "gainAuto": false,
                "sea": 30,
                "rain": 20
            }),
            description: Some("Settings report".to_string()),
        };
        provider.check_state_changes(&decoded1);

        // Change gain value
        let decoded2 = DecodedMessage::Navico {
            message_type: "settings".to_string(),
            report_id: Some(0x02),
            fields: serde_json::json!({
                "gain": 75,
                "gainAuto": false,
                "sea": 30,
                "rain": 20
            }),
            description: Some("Settings report".to_string()),
        };
        provider.check_state_changes(&decoded2);

        // Should have a state change event for gain
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert!(state_events.len() >= 1, "Changed gain should emit event");

        // Find the gain change event
        let gain_event = state_events.iter().find(|e| {
            if let DebugEventPayload::StateChange { control_id, .. } = &e.payload {
                control_id == "gain"
            } else {
                false
            }
        });
        assert!(gain_event.is_some(), "Should have gain state change");
    }

    #[test]
    fn test_raymarine_state_change_detection() {
        use super::super::DebugEventPayload;

        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "raymarine".to_string(),
        );

        // First observation - Raymarine Quantum status
        let decoded1 = DecodedMessage::Raymarine {
            message_type: "status".to_string(),
            variant: Some("quantum".to_string()),
            fields: serde_json::json!({
                "power": 0,
                "powerStr": "standby",
                "gain": 50,
                "sea": 30,
                "rain": 20
            }),
            description: Some("Quantum status".to_string()),
        };
        provider.check_state_changes(&decoded1);

        // Change to transmit
        let decoded2 = DecodedMessage::Raymarine {
            message_type: "status".to_string(),
            variant: Some("quantum".to_string()),
            fields: serde_json::json!({
                "power": 1,
                "powerStr": "transmit",
                "gain": 50,
                "sea": 30,
                "rain": 20
            }),
            description: Some("Quantum status".to_string()),
        };
        provider.check_state_changes(&decoded2);

        // Should have state change events for power
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert!(state_events.len() >= 1, "Changed power should emit event");
    }

    #[test]
    fn test_garmin_state_change_detection() {
        use super::super::DebugEventPayload;

        let hub = Arc::new(DebugHub::new());
        let mut provider = DebugIoProvider::new(
            MockIoProvider,
            hub.clone(),
            "radar-1".to_string(),
            "garmin".to_string(),
        );

        // First observation - Garmin gain status
        let decoded1 = DecodedMessage::Garmin {
            message_type: "status".to_string(),
            fields: serde_json::json!({
                "gain": 50
            }),
            description: Some("Gain: 50".to_string()),
        };
        provider.check_state_changes(&decoded1);

        // Change gain value
        let decoded2 = DecodedMessage::Garmin {
            message_type: "status".to_string(),
            fields: serde_json::json!({
                "gain": 80
            }),
            description: Some("Gain: 80".to_string()),
        };
        provider.check_state_changes(&decoded2);

        // Should have a state change event
        let events = hub.get_all_events();
        let state_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.payload, DebugEventPayload::StateChange { .. }))
            .collect();
        assert_eq!(state_events.len(), 1, "Changed gain should emit event");

        // Verify the event content
        if let DebugEventPayload::StateChange {
            control_id,
            before,
            after,
            ..
        } = &state_events[0].payload
        {
            assert_eq!(control_id, "gain");
            assert_eq!(*before, serde_json::json!(50));
            assert_eq!(*after, serde_json::json!(80));
        } else {
            panic!("Expected StateChange event");
        }
    }
}
