//
// The locator finds all radars by listening for known packets on the network.
//
// Some radars can only be found by this method because they use fluent multicast
// addresses, some are "easier" to locate by a fixed method, or just assuming they
// are present.
// Still, we use this location method for all radars so the process is uniform.
//

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use miette::Result;
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::sync::{broadcast, mpsc};
use tokio::{task::JoinSet, time::sleep};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::brand::{LocatorId, RadarLocator, create_brand_listeners};
use crate::radar::{RadarError, SharedRadars};
use crate::{Brand, Cli, InterfaceApi, InterfaceId, InterfaceStatus, RadarInterfaceApi, network};

const LOCATOR_PACKET_BUFFER_LEN: usize = 300; // Long enough for any location packet

pub(crate) struct LocatorAddress {
    pub id: LocatorId,
    pub address: SocketAddr,
    pub brand: Brand,
    pub beacon_request_packets: Vec<&'static [u8]>, // Optional messages to send to ask radar for address
    pub locator: Box<dyn RadarLocator>,
}

// The only part of RadioListenAddress that isn't Send is process, but since this is static it really
// is safe to send.
// unsafe impl Send for LocatorAddress {}

impl LocatorAddress {
    pub fn new(
        id: LocatorId,
        address: &SocketAddr,
        brand: Brand,
        beacon_request_packets: Vec<&'static [u8]>,
        locator: Box<dyn RadarLocator>,
    ) -> LocatorAddress {
        LocatorAddress {
            id,
            address: address.clone(),
            brand,
            beacon_request_packets,
            locator,
        }
    }
}

struct LocatorSocket {
    sock: crate::replay::RadarSocket,
    nic_addr: Ipv4Addr,
    state: Box<dyn RadarLocator>,
}

struct InterfaceState {
    active_nic_addresses: Vec<Ipv4Addr>,
    inactive_nic_names: HashSet<String>,
    lost_nic_names: HashSet<String>,
    interface_api: InterfaceApi,
    first_loop: bool,
}

enum ResultType {
    Locator(LocatorSocket, SocketAddrV4, Vec<u8>),
    InterfaceRequest(mpsc::Sender<InterfaceApi>),
}

pub(crate) struct Locator {
    pub radars: SharedRadars,
    pub args: Cli,
}

impl Locator {
    pub fn new(args: Cli, radars: SharedRadars) -> Self {
        Locator { radars, args }
    }

    pub async fn run(
        self,
        subsys: SubsystemHandle,
        tx_ip_change: broadcast::Sender<()>,
        tx_interface_request: broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
    ) -> Result<(), RadarError> {
        let radars = &self.radars;

        // Handle emulator mode - create radar directly without network discovery
        #[cfg(feature = "emulator")]
        if self.args.emulator {
            log::info!("Emulator mode: creating emulator radar directly");
            crate::brand::emulator::create_emulator_radar(&self.args, &radars, &subsys);

            // Keep the locator running to handle interface requests
            loop {
                tokio::select! {
                    _ = subsys.on_shutdown_requested() => {
                        log::debug!("Emulator locator shutdown");
                        return Ok(());
                    }
                }
            }
        }

        log::debug!("Entering loop, listening for radars");
        let mut interface_state = InterfaceState {
            active_nic_addresses: Vec::new(),
            inactive_nic_names: HashSet::new(),
            lost_nic_names: HashSet::new(),
            interface_api: InterfaceApi {
                brands: HashSet::new(),
                interfaces: HashMap::new(),
            },
            first_loop: true,
        };

        let listen_addresses = self.compute_listen_addresses(&mut interface_state);

        // Make a copy of the beacon request packets to send them later, as LocatorAddress is not 'Send'.
        let beacon_messages = listen_addresses
            .iter()
            .filter(|x| !x.beacon_request_packets.is_empty())
            .map(|x| (x.address, x.beacon_request_packets.clone()))
            .collect::<Vec<(SocketAddr, Vec<&[u8]>)>>();
        log::debug!("beacon_messages = {:?}", beacon_messages);

        let cancellation_token = subsys.create_cancellation_token();
        network::spawn_wait_for_ip_addr_change(cancellation_token.clone(), tx_ip_change.clone())
            .await;

        loop {
            let mut set = JoinSet::new();
            let cancellation_token = cancellation_token.clone();

            if self.args.multiple_radar || !radars.have_active() {
                // actively listening for new radars
                // create a list of sockets for all listen addresses
                for socket in
                    match self.create_listen_sockets(&listen_addresses, &mut interface_state) {
                        Err(e) => {
                            if self.args.interface.is_some() {
                                return Err(e);
                            }
                            log::debug!("No NIC addresses found");
                            // Still enter the main loop so we listen to subsys requests. The main
                            // loop will time out in 2 or 20 secs. We will get here again when the
                            // IP address change message causes the main loop to break.
                            Vec::new()
                        }
                        Ok(sockets) => sockets,
                    }
                {
                    spawn_receive(&mut set, socket);
                }
            }
            set.spawn(async move {
                cancellation_token.cancelled().await;
                Err(RadarError::Shutdown)
            });
            let mut rx_ip_change = tx_ip_change.subscribe();
            set.spawn(async move {
                rx_ip_change
                    .recv()
                    .await
                    .map_err(|_| RadarError::Shutdown)?;
                Err(RadarError::IPAddressChanged)
            });

            // Add a timeout to the task set to handle cases where no packets are received,
            // and we need to send a wakeup packet
            set.spawn(async move {
                sleep(Duration::from_secs(2)).await;
                Err(RadarError::Timeout)
            });

            // Spawn a task to listen for interface list requests from the clients
            spawn_interface_request_handler(&mut set, &tx_interface_request);

            while let Some(join_result) = set.join_next().await {
                match join_result {
                    Ok(join_result) => {
                        match join_result {
                            Ok(ResultType::Locator(mut locator_socket, addr, buf)) => {
                                log::trace!(
                                    "{} via {} -> {:02X?}",
                                    &addr,
                                    &locator_socket.nic_addr,
                                    &buf
                                );

                                let _ = locator_socket.state.process(
                                    &buf,
                                    &addr,
                                    &locator_socket.nic_addr,
                                    &radars,
                                    &subsys,
                                );
                                if self.args.multiple_radar || !radars.have_active() {
                                    // Respawn this task
                                    spawn_receive(&mut set, locator_socket);
                                } else {
                                    // we have found a radar
                                    if self.args.transmit {
                                        radars.request_transmit_all();
                                    }
                                    break; // Restart the loop but now without locators
                                }
                            }
                            Ok(ResultType::InterfaceRequest(reply_channel)) => {
                                // Send an answer to the request
                                self.reply_with_interface_state(&interface_state, reply_channel)
                                    .await;

                                // Respawn this task
                                spawn_interface_request_handler(&mut set, &tx_interface_request);
                            }
                            Err(e) => {
                                match e {
                                    RadarError::Shutdown => {
                                        log::debug!("Locator shutdown");
                                        return Ok(());
                                    }
                                    RadarError::IPAddressChanged => {
                                        // Loop, reread everything
                                        break;
                                    }
                                    RadarError::Timeout => {
                                        if !self.args.is_replay() {
                                            let _ = send_beacon_requests(
                                                &beacon_messages,
                                                &interface_state.active_nic_addresses,
                                            )
                                            .await;
                                        }

                                        // Periodically request transmit mode for standby radars
                                        if self.args.transmit {
                                            radars.request_transmit_all();
                                        }

                                        // Respawn this task
                                        set.spawn(async move {
                                            sleep(Duration::from_secs(20)).await;
                                            Err(RadarError::Timeout)
                                        });
                                    }
                                    _ => {
                                        log::warn!("receive error: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::debug!("JoinError: {}", e);
                    }
                };
            }
        }
    }

    async fn reply_with_interface_state(
        &self,
        interface_state: &InterfaceState,
        reply_channel: mpsc::Sender<InterfaceApi>,
    ) {
        let mut interface_api = interface_state.interface_api.clone();

        for (_, radar_interface_api) in interface_api.interfaces.iter_mut() {
            if let (Some(listeners), Some(ip)) =
                (&mut radar_interface_api.listeners, &radar_interface_api.ip)
            {
                for (brand, status) in listeners.iter_mut() {
                    if self.radars.is_radar_active_on_nic(brand, ip) {
                        *status = "Active".to_owned();
                    }
                }
            }
        }
        let _ = reply_channel.send(interface_api).await;
    }

    fn compute_listen_addresses(
        &self,
        interface_state: &mut InterfaceState,
    ) -> Vec<LocatorAddress> {
        let mut listen_addresses: Vec<LocatorAddress> = Vec::new();

        let brands = &mut interface_state.interface_api.brands;
        brands.clear();

        let args = &self.args;

        create_brand_listeners(&mut listen_addresses, brands, args);

        listen_addresses
    }

    fn create_listen_sockets(
        &self,
        listen_addresses: &Vec<LocatorAddress>,
        interface_state: &mut InterfaceState,
    ) -> Result<Vec<LocatorSocket>, RadarError> {
        // In pcap replay mode, skip real NIC enumeration and create a
        // fake interface with replay-backed sockets for all listen addresses.
        if crate::replay::is_active() {
            return self.create_replay_sockets(listen_addresses, interface_state);
        }

        let only_interface = &self.args.interface;
        let avoid_wifi = !self.args.allow_wifi;

        let if_api = &mut interface_state.interface_api.interfaces;
        if_api.clear();

        match NetworkInterface::show() {
            Ok(interfaces) => {
                log::trace!("getifaddrs() dump {:#?}", interfaces);
                let mut sockets = Vec::new();
                for itf in interfaces {
                    let mut active: bool = false;

                    if only_interface.is_none() || only_interface.as_ref() == Some(&itf.name) {
                        for nic_addr in itf.addr {
                            if let (IpAddr::V4(nic_ip), Some(IpAddr::V4(nic_netmask))) =
                                (nic_addr.ip(), nic_addr.netmask())
                            {
                                if avoid_wifi && network::is_wireless_interface(&itf.name) {
                                    log::trace!("Ignoring wireless interface '{}'", itf.name);
                                    if_api.insert(
                                        InterfaceId::new(&itf.name, Some(nic_ip)),
                                        RadarInterfaceApi::new(
                                            InterfaceStatus::WirelessIgnored,
                                            Some(nic_ip),
                                            Some(nic_netmask),
                                            None,
                                        ),
                                    );
                                    continue;
                                }
                                let mut listeners = HashMap::new();

                                if !nic_ip.is_loopback() || only_interface.is_some() {
                                    if interface_state.lost_nic_names.contains(&itf.name)
                                        || !interface_state.active_nic_addresses.contains(&nic_ip)
                                    {
                                        if interface_state.inactive_nic_names.remove(&itf.name) {
                                            log::info!(
                                                "Searching for radars on interface '{}' address {} (added/modified)",
                                                itf.name,
                                                &nic_ip,
                                            );
                                        } else {
                                            log::info!(
                                                "Searching for radars on interface '{}' address {}",
                                                itf.name,
                                                &nic_ip,
                                            );
                                        }
                                        interface_state.active_nic_addresses.push(nic_ip.clone());
                                        interface_state.lost_nic_names.remove(&itf.name);
                                    }

                                    for radar_listen_address in listen_addresses {
                                        if let SocketAddr::V4(listen_addr) =
                                            radar_listen_address.address
                                        {
                                            if !listen_addr.ip().is_multicast()
                                                && !network::match_ipv4(
                                                    &nic_ip,
                                                    listen_addr.ip(),
                                                    &nic_netmask,
                                                )
                                                && only_interface.is_none()
                                            {
                                                listeners.insert(
                                                    radar_listen_address.brand.clone(),
                                                    format!("No match for {}", listen_addr.ip()),
                                                );
                                                continue;
                                            }

                                            let socket = crate::network::create_udp_listen(
                                                &listen_addr,
                                                &nic_ip,
                                                crate::network::SocketType::Any,
                                            );

                                            let status = match socket {
                                                Ok(socket) => {
                                                    sockets.push(LocatorSocket {
                                                        sock: socket,
                                                        nic_addr: nic_ip.clone(),
                                                        state: radar_listen_address.locator.clone(),
                                                    });
                                                    log::debug!(
                                                        "Listening on '{}' address {} for address {}",
                                                        itf.name,
                                                        nic_ip,
                                                        listen_addr,
                                                    );
                                                    "Listening".to_owned()
                                                }
                                                Err(e) => {
                                                    log::warn!(
                                                        "Cannot listen on '{}' address {} for address {}: {}",
                                                        itf.name,
                                                        nic_ip,
                                                        listen_addr,
                                                        e
                                                    );
                                                    e.to_string()
                                                }
                                            };
                                            listeners
                                                .insert(radar_listen_address.brand.clone(), status);
                                        } else {
                                            log::trace!(
                                                "Ignoring IPv6 address {:?}",
                                                &radar_listen_address.address
                                            );
                                        }
                                    }
                                    active = true;
                                }

                                if_api.insert(
                                    InterfaceId::new(&itf.name, Some(nic_ip)),
                                    RadarInterfaceApi::new(
                                        InterfaceStatus::Ok,
                                        Some(nic_ip),
                                        Some(nic_netmask),
                                        Some(listeners),
                                    ),
                                );
                            }
                        }
                        if self.args.interface.is_some()
                            && interface_state.active_nic_addresses.len() == 0
                        {
                            return Err(RadarError::InterfaceNoV4(
                                self.args.interface.clone().unwrap(),
                            ));
                        }
                    }
                    if !active && only_interface.is_none() {
                        if interface_state
                            .inactive_nic_names
                            .insert(itf.name.to_owned())
                        {
                            if interface_state.first_loop {
                                log::trace!(
                                    "Interface '{}' does not have an IPv4 address",
                                    itf.name
                                );
                            } else {
                                log::warn!(
                                    "Interface '{}' became inactive or lost its IPv4 address",
                                    itf.name
                                );
                                interface_state.lost_nic_names.insert(itf.name.to_owned());
                            }
                        }
                        if_api.insert(
                            InterfaceId::new(&itf.name, None),
                            RadarInterfaceApi::new(
                                InterfaceStatus::NoIPv4Address,
                                None,
                                None,
                                None,
                            ),
                        );
                    }
                }
                interface_state.first_loop = false;

                if self.args.interface.is_some() && interface_state.active_nic_addresses.len() == 0
                {
                    return Err(RadarError::InterfaceNotFound(
                        self.args.interface.clone().unwrap(),
                    ));
                }

                log::trace!("lost_nic_names = {:?}", interface_state.lost_nic_names);
                log::trace!(
                    "active_nic_addresses = {:?}",
                    interface_state.active_nic_addresses
                );
                Ok(sockets)
            }
            Err(_) => Err(RadarError::EnumerationFailed),
        }
    }

    /// Create replay-backed sockets for all listen addresses using a
    /// fake interface. This bypasses real NIC enumeration entirely —
    /// in pcap replay mode, packets come from the dispatcher, not the
    /// network, so no real interface is needed.
    fn create_replay_sockets(
        &self,
        listen_addresses: &Vec<LocatorAddress>,
        interface_state: &mut InterfaceState,
    ) -> Result<Vec<LocatorSocket>, RadarError> {
        let fake_nic = Ipv4Addr::new(10, 0, 0, 1);
        let mut sockets = Vec::new();

        for addr in listen_addresses {
            if let SocketAddr::V4(listen_addr) = addr.address {
                match crate::network::create_udp_listen(
                    &listen_addr,
                    &fake_nic,
                    crate::network::SocketType::Any,
                ) {
                    Ok(socket) => {
                        sockets.push(LocatorSocket {
                            sock: socket,
                            nic_addr: fake_nic,
                            state: addr.locator.clone(),
                        });
                        log::debug!("Replay: listening for {}", listen_addr);
                    }
                    Err(e) => {
                        log::warn!("Replay: failed to create socket for {}: {}", listen_addr, e);
                    }
                }
            }
        }

        debug_assert!(
            !sockets.is_empty(),
            "Replay: no sockets created — listen_addresses may be empty"
        );

        interface_state.first_loop = false;
        interface_state
            .interface_api
            .interfaces
            .insert(
                InterfaceId::new("replay", Some(fake_nic)),
                RadarInterfaceApi::new(
                    InterfaceStatus::Ok,
                    Some(fake_nic),
                    Some(Ipv4Addr::new(255, 0, 0, 0)),
                    None,
                ),
            );

        Ok(sockets)
    }
}

fn spawn_interface_request_handler(
    set: &mut JoinSet<std::result::Result<ResultType, RadarError>>,
    tx_interface_request: &broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
) {
    let mut rx_interface_request = tx_interface_request.subscribe();
    set.spawn(async move {
        match rx_interface_request.recv().await {
            Ok(Some(reply_channel)) => Ok(ResultType::InterfaceRequest(reply_channel)),
            _ => Err(RadarError::Shutdown),
        }
    });
}

fn spawn_receive(set: &mut JoinSet<Result<ResultType, RadarError>>, mut socket: LocatorSocket) {
    set.spawn(async move {
        let mut buf: Vec<u8> = Vec::with_capacity(LOCATOR_PACKET_BUFFER_LEN);
        let res = socket.sock.recv_buf_from(&mut buf).await;

        match res {
            Ok((_, addr)) => match addr {
                SocketAddr::V4(addr) => Ok(ResultType::Locator(socket, addr, buf)),
                SocketAddr::V6(addr) => Err(RadarError::InterfaceNoV4(format!("{}", addr))),
            },
            Err(e) => Err(RadarError::Io(e)),
        }
    });
}

async fn send_beacon_requests(
    beacon_messages: &Vec<(SocketAddr, Vec<&[u8]>)>,
    interface_addresses: &Vec<Ipv4Addr>,
) -> io::Result<()> {
    for x in beacon_messages {
        for beacon_request in &x.1 {
            if let Err(e) = send_beacon_request(interface_addresses, &x.0, beacon_request).await {
                log::warn!("Failed to send beacon request to {}: {}", x.0, e);
            }
        }
    }

    Ok(())
}

async fn send_beacon_request(
    interface_addresses: &Vec<Ipv4Addr>,
    addr: &SocketAddr,
    msg: &[u8],
) -> io::Result<()> {
    if let SocketAddr::V4(addr) = addr {
        if addr.ip().is_multicast() {
            // Broadcast on all interfaces

            log::debug!("Sending beacon request to {} via all interfaces", addr);

            for nic_addr in interface_addresses {
                match network::create_multicast_send(addr, nic_addr) {
                    Ok(sock) => {
                        sock.set_broadcast(true)?;
                        match sock.send(msg).await {
                            Ok(_) => {
                                log::debug!(
                                    "{} via {}: beacon request sent {:02X?}",
                                    addr,
                                    nic_addr,
                                    msg
                                );
                            }
                            Err(e) => {
                                log::warn!(
                                    "{} via {}: Failed to send beacon request: {}",
                                    addr,
                                    nic_addr,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "{} via {}: Failed to create multicast socket: {}",
                            addr,
                            nic_addr,
                            e
                        );
                    }
                }
            }
        } else {
            let sock =
                std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))?;
            sock.set_broadcast(true)?;
            sock.send_to(msg, addr)?;
            log::debug!("{}: beacon request sent {:02X?}", addr, msg);
        }
    }
    Ok(())
}
