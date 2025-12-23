//! Passive multicast listener for observing chart plotter effects.
//!
//! This listener joins known multicast groups to see radar status broadcasts
//! even when triggered by chart plotters that we can't directly observe.

use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::broadcast;

use super::decoders::create_decoder;
use super::hub::DebugHub;
use super::{EventSource, IoDirection, ProtocolType};

// =============================================================================
// Multicast Groups by Brand
// =============================================================================

/// Known Navico multicast groups.
pub const NAVICO_MULTICAST_GROUPS: &[(&str, u16)] = &[
    ("236.6.7.4", 6680),
    ("236.6.7.5", 6680),
    ("239.238.55.73", 6680),
];

/// Known Garmin multicast groups.
pub const GARMIN_MULTICAST_GROUPS: &[(&str, u16)] = &[("239.254.2.0", 50100)];

/// Known Raymarine multicast groups.
pub const RAYMARINE_MULTICAST_GROUPS: &[(&str, u16)] = &[("224.0.0.1", 5800)];

// =============================================================================
// PassiveListener
// =============================================================================

/// Listens to multicast traffic to see state changes triggered by chart plotters.
pub struct PassiveListener {
    hub: Arc<DebugHub>,
    interface: String,
    shutdown_rx: broadcast::Receiver<()>,
}

impl PassiveListener {
    /// Create a new passive listener.
    pub fn new(
        hub: Arc<DebugHub>,
        interface: String,
        shutdown_rx: broadcast::Receiver<()>,
    ) -> Self {
        Self {
            hub,
            interface,
            shutdown_rx,
        }
    }

    /// Run the passive listener.
    ///
    /// This spawns tasks to listen on all known multicast groups.
    pub async fn run(mut self) {
        log::info!(
            "Starting passive multicast listener on interface {}",
            self.interface
        );

        let mut tasks = Vec::new();

        // Start listeners for each brand
        for (group, port) in NAVICO_MULTICAST_GROUPS {
            let hub = self.hub.clone();
            let interface = self.interface.clone();
            tasks.push(tokio::spawn(async move {
                listen_multicast(hub, &interface, group, *port, "navico").await;
            }));
        }

        for (group, port) in GARMIN_MULTICAST_GROUPS {
            let hub = self.hub.clone();
            let interface = self.interface.clone();
            tasks.push(tokio::spawn(async move {
                listen_multicast(hub, &interface, group, *port, "garmin").await;
            }));
        }

        for (group, port) in RAYMARINE_MULTICAST_GROUPS {
            let hub = self.hub.clone();
            let interface = self.interface.clone();
            tasks.push(tokio::spawn(async move {
                listen_multicast(hub, &interface, group, *port, "raymarine").await;
            }));
        }

        // Wait for shutdown
        let _ = self.shutdown_rx.recv().await;

        // Cancel all tasks
        for task in tasks {
            task.abort();
        }

        log::info!("Passive multicast listener stopped");
    }
}

/// Listen on a single multicast group.
async fn listen_multicast(
    hub: Arc<DebugHub>,
    interface: &str,
    group: &str,
    port: u16,
    brand: &str,
) {
    // Try to create and bind socket
    let socket = match create_multicast_socket(interface, group, port).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!(
                "Failed to create multicast socket for {}:{} ({}): {}",
                group,
                port,
                brand,
                e
            );
            return;
        }
    };

    log::debug!(
        "Passive listener joined multicast {}:{} for {}",
        group,
        port,
        brand
    );

    let decoder = create_decoder(brand);
    let mut buf = [0u8; 65536];

    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, addr)) => {
                let data = &buf[..len];
                let decoded = decoder.decode(data, IoDirection::Recv);

                // Create event with passive source
                let event = hub
                    .event_builder("passive", brand)
                    .source(EventSource::Passive)
                    .data(
                        IoDirection::Recv,
                        ProtocolType::Udp,
                        &addr.ip().to_string(),
                        addr.port(),
                        data,
                        Some(decoded),
                    );
                hub.submit(event);
            }
            Err(e) => {
                log::warn!("Passive listener error on {}:{}: {}", group, port, e);
                break;
            }
        }
    }
}

/// Create a multicast socket and join the group.
async fn create_multicast_socket(
    _interface: &str,
    group: &str,
    port: u16,
) -> Result<UdpSocket, std::io::Error> {
    use std::net::{Ipv4Addr, SocketAddrV4};

    // Parse multicast group
    let multicast_addr: Ipv4Addr = group
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    // Create socket
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;

    // Set socket options
    socket.set_reuse_address(true)?;
    #[cfg(not(windows))]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;

    // Bind to port
    let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
    socket.bind(&bind_addr.into())?;

    // Join multicast group
    socket.join_multicast_v4(&multicast_addr, &Ipv4Addr::UNSPECIFIED)?;

    // Convert to tokio socket
    let std_socket: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_socket)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multicast_groups_defined() {
        assert!(!NAVICO_MULTICAST_GROUPS.is_empty());
        assert!(!GARMIN_MULTICAST_GROUPS.is_empty());
        assert!(!RAYMARINE_MULTICAST_GROUPS.is_empty());
    }
}
