//! Server wrapper around mayara-core's RadarLocator.
//!
//! This module adapts the sync, poll-based RadarLocator from mayara-core
//! to work within the server's async architecture using TokioIoProvider.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────────┐
//! │ CoreLocatorAdapter (this module)                   │
//! │  - Runs in tokio task                              │
//! │  - Polls RadarLocator periodically                 │
//! │  - Sends discoveries to server via channels        │
//! └────────────────────────────────────────────────────┘
//!                    │
//!                    ▼
//! ┌────────────────────────────────────────────────────┐
//! │ TokioIoProvider (tokio_io.rs)                      │
//! │  - Implements IoProvider trait                     │
//! │  - Wraps tokio sockets in poll-based interface     │
//! └────────────────────────────────────────────────────┘
//!                    │
//!                    ▼
//! ┌────────────────────────────────────────────────────┐
//! │ mayara_core::RadarLocator                          │
//! │  - Same code as WASM uses                          │
//! │  - Platform-independent discovery logic            │
//! └────────────────────────────────────────────────────┘
//! ```

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use mayara_core::locator::{LocatorEvent, RadarLocator};
use mayara_core::radar::RadarDiscovery;
use mayara_core::Brand as CoreBrand;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::tokio_io::TokioIoProvider;
use crate::Brand;

/// Discovery message sent from the locator to the server.
#[derive(Debug, Clone)]
pub enum LocatorMessage {
    /// A new radar was discovered
    RadarDiscovered(RadarDiscovery),
    /// An existing radar's info was updated (e.g., model detected)
    RadarUpdated(RadarDiscovery),
    /// Locator has shut down
    Shutdown,
}

/// Adapter that wraps mayara-core's RadarLocator for use in the server.
///
/// This runs the core locator in a polling loop and sends discoveries
/// to the server via channels.
pub struct CoreLocatorAdapter {
    /// The core locator
    locator: RadarLocator,
    /// I/O provider for tokio sockets
    io: TokioIoProvider,
    /// Channel to send discoveries to server
    discovery_tx: mpsc::Sender<LocatorMessage>,
    /// Poll interval (how often to check for beacons)
    poll_interval: Duration,
    /// Session to update with locator status
    session: crate::Session,
}

impl CoreLocatorAdapter {
    /// Create a new locator adapter.
    ///
    /// # Arguments
    /// * `session` - Session to update with locator status
    /// * `discovery_tx` - Channel to send radar discoveries to the server
    /// * `poll_interval` - How often to poll for beacons (default: 100ms = 10 polls/sec)
    pub fn new(
        session: crate::Session,
        discovery_tx: mpsc::Sender<LocatorMessage>,
        poll_interval: Duration,
    ) -> Self {
        Self {
            locator: RadarLocator::new(),
            io: TokioIoProvider::new(),
            discovery_tx,
            poll_interval,
            session,
        }
    }

    /// Create with default poll interval (100ms).
    pub fn with_default_interval(
        session: crate::Session,
        discovery_tx: mpsc::Sender<LocatorMessage>,
    ) -> Self {
        Self::new(session, discovery_tx, Duration::from_millis(100))
    }

    /// Start the locator.
    ///
    /// This initializes all beacon sockets (Furuno, Navico, Raymarine, Garmin).
    pub fn start(&mut self) {
        log::info!("Starting core radar locator");

        // CRITICAL: Configure multicast interfaces for multi-NIC setups
        // Without this, multicast only joins on OS-chosen interface (often wrong one)
        let interfaces = find_all_interfaces();
        if interfaces.len() > 1 {
            log::info!(
                "Multi-NIC setup detected - joining multicast on {} interfaces: {:?}",
                interfaces.len(),
                interfaces
            );
        }
        for iface in &interfaces {
            self.locator.add_multicast_interface(iface);
        }

        // CRITICAL: Configure Furuno interface to prevent cross-NIC broadcast traffic
        // Furuno uses 172.31.x.x subnet - find the NIC that can reach it
        if let Some(furuno_nic) = find_furuno_interface() {
            log::info!(
                "Found Furuno-capable NIC: {} - broadcasts will use this interface",
                furuno_nic
            );
            self.locator.set_furuno_interface(&furuno_nic.to_string());
        } else {
            log::warn!("No NIC found for Furuno subnet (172.31.x.x) - broadcasts may go to wrong interface");
        }

        self.locator.start(&mut self.io);

        // Update session with locator status
        if let Ok(mut session) = self.session.write() {
            session.locator_status = self.locator.status().clone();
            log::info!(
                "Updated session with {} brand statuses",
                session.locator_status.brands.len()
            );
        }
    }

    /// Poll for discoveries once.
    ///
    /// Returns list of locator events (new discoveries and updates).
    pub fn poll(&mut self) -> Vec<LocatorEvent> {
        self.locator.poll(&mut self.io)
    }

    /// Send a Furuno announce packet.
    ///
    /// Call this before attempting TCP connections to Furuno radars.
    pub fn send_furuno_announce(&self) {
        // Note: We need mutable io for this, but the method only needs it for send
        // This is a design limitation - we might want to change the core API
    }

    /// Get list of all discovered radars.
    pub fn radars(&self) -> impl Iterator<Item = &RadarDiscovery> {
        self.locator.radars.values().map(|r| &r.discovery)
    }

    /// Shutdown the locator.
    pub fn shutdown(&mut self) {
        log::info!("Shutting down core radar locator");
        self.locator.shutdown(&mut self.io);
    }

    /// Run the locator as an async task.
    ///
    /// This is the main entry point for running the locator in a tokio task.
    /// It polls the core locator periodically and sends discoveries to the server.
    pub async fn run(
        mut self,
        subsys: SubsystemHandle,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        log::info!("CoreLocatorAdapter: Starting locator task");

        // Start the core locator (opens sockets)
        self.start();

        // Set up polling interval
        let mut poll_timer = interval(self.poll_interval);
        poll_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("CoreLocatorAdapter: Shutdown requested");
                    break;
                }
                _ = poll_timer.tick() => {
                    // Poll the core locator
                    let events = self.poll();

                    // Send events to the server
                    for event in events {
                        let message = match event {
                            LocatorEvent::RadarDiscovered(discovery) => {
                                log::info!(
                                    "CoreLocatorAdapter: Discovered {} radar '{}' at {}",
                                    discovery.brand, discovery.name, discovery.address
                                );
                                LocatorMessage::RadarDiscovered(discovery)
                            }
                            LocatorEvent::RadarUpdated(discovery) => {
                                log::info!(
                                    "CoreLocatorAdapter: Updated {} radar '{}' - model: {:?}",
                                    discovery.brand, discovery.name, discovery.model
                                );
                                LocatorMessage::RadarUpdated(discovery)
                            }
                        };

                        if self.discovery_tx.send(message).await.is_err() {
                            log::warn!("CoreLocatorAdapter: Discovery channel closed");
                            break;
                        }
                    }
                }
            }
        }

        // Cleanup
        self.shutdown();
        let _ = self.discovery_tx.send(LocatorMessage::Shutdown).await;

        log::info!("CoreLocatorAdapter: Locator task finished");
        Ok(())
    }
}

/// Create a locator subsystem for use with tokio-graceful-shutdown.
///
/// # Example
///
/// ```rust,ignore
/// use mayara_server::core_locator::{create_locator_subsystem, LocatorMessage};
/// use tokio::sync::mpsc;
///
/// let (tx, mut rx) = mpsc::channel::<LocatorMessage>(32);
///
/// // Add to subsystem
/// subsys.start(SubsystemBuilder::new("core-locator", |s| {
///     create_locator_subsystem(session.clone(), tx, s)
/// }));
///
/// // Receive discoveries
/// while let Some(msg) = rx.recv().await {
///     match msg {
///         LocatorMessage::RadarDiscovered(discovery) => {
///             println!("Found radar: {}", discovery.name);
///         }
///         LocatorMessage::Shutdown => break,
///     }
/// }
/// ```
pub async fn create_locator_subsystem(
    session: crate::Session,
    discovery_tx: mpsc::Sender<LocatorMessage>,
    subsys: SubsystemHandle,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let adapter = CoreLocatorAdapter::with_default_interval(session, discovery_tx);
    adapter.run(subsys).await
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Convert mayara-core Brand to server Brand
pub fn core_brand_to_server_brand(core_brand: CoreBrand) -> Brand {
    match core_brand {
        CoreBrand::Furuno => Brand::Furuno,
        CoreBrand::Navico => Brand::Navico,
        CoreBrand::Raymarine => Brand::Raymarine,
        CoreBrand::Garmin => Brand::Garmin,
    }
}

/// Parse address string to SocketAddrV4
pub fn parse_address(addr: &str) -> Option<SocketAddrV4> {
    // Address format: "ip:port" or just "ip"
    if let Some(colon_pos) = addr.rfind(':') {
        let ip_str = &addr[..colon_pos];
        let port_str = &addr[colon_pos + 1..];
        let ip: Ipv4Addr = ip_str.parse().ok()?;
        let port: u16 = port_str.parse().ok()?;
        Some(SocketAddrV4::new(ip, port))
    } else {
        let ip: Ipv4Addr = addr.parse().ok()?;
        Some(SocketAddrV4::new(ip, 0))
    }
}

/// Get the NIC address for a radar using network interface matching
pub fn get_nic_for_radar(addr: &SocketAddrV4) -> Ipv4Addr {
    crate::network::find_nic_for_radar(addr.ip()).unwrap_or(Ipv4Addr::UNSPECIFIED)
}

// =============================================================================
// Discovery Dispatch
// =============================================================================

use crate::radar::SharedRadars;
use crate::Session;

/// Dispatch a radar discovery to the appropriate brand-specific processor.
///
/// This routes `RadarDiscovery` from the core locator to the brand's
/// `process_discovery` function which creates a `RadarInfo` and spawns
/// the necessary subsystems.
///
/// If `--brand` was specified, only radars of that brand will be processed.
pub fn dispatch_discovery(
    session: Session,
    discovery: &RadarDiscovery,
    radars: &SharedRadars,
    subsys: &SubsystemHandle,
) -> Result<(), std::io::Error> {
    // Check brand filter - if --brand was specified, only process matching brands
    if let Some(allowed_brand) = session.read().unwrap().args.brand {
        let discovery_brand = core_brand_to_server_brand(discovery.brand);
        if discovery_brand != allowed_brand {
            log::debug!(
                "Ignoring {} radar '{}' (--brand {} specified)",
                discovery.brand,
                discovery.name,
                allowed_brand
            );
            return Ok(());
        }
    }

    // Determine NIC address for this radar
    let radar_addr = parse_address(&discovery.address);
    let nic_addr = radar_addr
        .map(|a| get_nic_for_radar(&a))
        .unwrap_or(Ipv4Addr::UNSPECIFIED);

    log::info!(
        "Processing {} discovery: {} at {} via {}",
        discovery.brand,
        discovery.name,
        discovery.address,
        nic_addr
    );

    match discovery.brand {
        #[cfg(feature = "furuno")]
        CoreBrand::Furuno => {
            crate::brand::furuno::process_discovery(session, discovery, nic_addr, radars, subsys)
        }
        #[cfg(feature = "navico")]
        CoreBrand::Navico => {
            crate::brand::navico::process_discovery(session, discovery, nic_addr, radars, subsys)
        }
        #[cfg(feature = "raymarine")]
        CoreBrand::Raymarine => {
            crate::brand::raymarine::process_discovery(session, discovery, nic_addr, radars, subsys)
        }
        // Garmin not yet implemented with process_discovery
        #[cfg(not(feature = "furuno"))]
        CoreBrand::Furuno => {
            log::warn!("Furuno support not compiled in");
            Ok(())
        }
        #[cfg(not(feature = "navico"))]
        CoreBrand::Navico => {
            log::warn!("Navico support not compiled in");
            Ok(())
        }
        #[cfg(not(feature = "raymarine"))]
        CoreBrand::Raymarine => {
            log::warn!("Raymarine support not compiled in");
            Ok(())
        }
        CoreBrand::Garmin => {
            log::warn!("Garmin process_discovery not implemented");
            Ok(())
        }
    }
}

// =============================================================================
// Interface Detection
// =============================================================================

/// Furuno subnet: 172.31.0.0/16
const FURUNO_SUBNET: Ipv4Addr = Ipv4Addr::new(172, 31, 0, 0);
const FURUNO_NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 0, 0);

/// Find the network interface that can reach the Furuno subnet (172.31.x.x).
///
/// This is critical for multi-NIC setups to ensure broadcast packets
/// go out on the correct interface.
fn find_furuno_interface() -> Option<Ipv4Addr> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    use std::net::IpAddr;

    let interfaces = NetworkInterface::show().ok()?;

    for itf in &interfaces {
        for addr in &itf.addr {
            if let (IpAddr::V4(nic_ip), Some(IpAddr::V4(netmask))) = (addr.ip(), addr.netmask()) {
                if !nic_ip.is_loopback() {
                    // Check if this NIC is on the Furuno subnet (172.31.x.x)
                    // We check if the NIC's subnet overlaps with Furuno's subnet
                    let nic_network = u32::from(nic_ip) & u32::from(netmask);
                    let furuno_network = u32::from(FURUNO_SUBNET) & u32::from(FURUNO_NETMASK);

                    // Check if this NIC can reach 172.31.x.x
                    // Either the NIC is directly on 172.31.x.x, or its network contains it
                    if nic_network == furuno_network
                        || (u32::from(nic_ip) & u32::from(FURUNO_NETMASK)) == furuno_network
                    {
                        log::debug!(
                            "Interface {} ({}) can reach Furuno subnet 172.31.x.x",
                            itf.name,
                            nic_ip
                        );
                        return Some(nic_ip);
                    }
                }
            }
        }
    }

    None
}

/// Find all non-loopback IPv4 interface addresses.
///
/// This is used to join multicast groups on all interfaces, which is
/// critical for multi-NIC setups where the radar might be on a different
/// interface than the OS default.
fn find_all_interfaces() -> Vec<String> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};
    use std::net::IpAddr;

    let mut interfaces = Vec::new();

    if let Ok(ifaces) = NetworkInterface::show() {
        for itf in &ifaces {
            for addr in &itf.addr {
                if let IpAddr::V4(nic_ip) = addr.ip() {
                    if !nic_ip.is_loopback() {
                        log::debug!("Found interface {} with IP {}", itf.name, nic_ip);
                        interfaces.push(nic_ip.to_string());
                    }
                }
            }
        }
    }

    interfaces
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    #[tokio::test]
    async fn test_locator_creation() {
        let session = Session::new_fake();
        let (tx, _rx) = mpsc::channel(32);
        let mut adapter = CoreLocatorAdapter::with_default_interval(session, tx);
        adapter.start();
        // Just verify it doesn't panic
        let radars = adapter.poll();
        assert!(radars.is_empty()); // No radars on test network
        adapter.shutdown();
    }

    #[test]
    fn test_parse_address() {
        let addr = parse_address("192.168.1.100:10010");
        assert!(addr.is_some());
        let addr = addr.unwrap();
        assert_eq!(addr.ip(), &Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(addr.port(), 10010);
    }

    #[test]
    fn test_brand_conversion() {
        assert!(matches!(
            core_brand_to_server_brand(CoreBrand::Furuno),
            Brand::Furuno
        ));
        assert!(matches!(
            core_brand_to_server_brand(CoreBrand::Navico),
            Brand::Navico
        ));
    }
}
