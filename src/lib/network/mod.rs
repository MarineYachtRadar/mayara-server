use socket2::{Domain, Protocol, Type};
use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddrV4;
use std::sync::atomic::AtomicBool;
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use tokio::net::UdpSocket;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;

#[cfg(target_os = "windows")]
pub(crate) mod windows;

pub mod arp;
pub mod devices;
pub mod mdns_advertise;
pub mod mdns_browse;
pub mod passive_capture;

static G_REPLAY: AtomicBool = AtomicBool::new(false);

pub fn set_replay(replay: bool) {
    G_REPLAY.store(replay, std::sync::atomic::Ordering::Relaxed);
}
use deku::DekuRead;

/// An address as Raymarine writes it: both halves little-endian, rather than
/// in the network order the rest of the world uses.
#[derive(DekuRead, Copy, Clone)]
#[repr(C)]
#[deku(
    ctx = "endian: deku::ctx::Endian",
    endian = "endian",
    ctx_default = "deku::ctx::Endian::Little"
)]
pub(crate) struct LittleEndianSocketAddrV4 {
    addr: [u8; 4],
    port: [u8; 2],
}

impl From<LittleEndianSocketAddrV4> for SocketAddrV4 {
    fn from(item: LittleEndianSocketAddrV4) -> Self {
        SocketAddrV4::new(
            u32::from_le_bytes(item.addr).into(),
            u16::from_le_bytes(item.port),
        )
    }
}

impl std::fmt::Display for LittleEndianSocketAddrV4 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            Ipv4Addr::from(u32::from_le_bytes(self.addr)),
            u16::from_le_bytes(self.port)
        )
    }
}

impl fmt::Debug for LittleEndianSocketAddrV4 {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("LittleEndianSocketAddrV4")
            .field("addr", &self.addr)
            .field("port", &format_args!("{}", u16::from_le_bytes(self.port)))
            .finish()
    }
}

// this will be common for all our sockets
pub(crate) fn new_socket() -> io::Result<socket2::Socket> {
    let socket = socket2::Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    // we're going to use read timeouts so that we don't hang waiting for packets
    socket.set_nonblocking(true)?;
    socket.set_reuse_address(true)?;

    Ok(socket)
}

/// On Windows, unlike all Unix variants, it is improper to bind to the multicast address
///
/// see https://msdn.microsoft.com/en-us/library/windows/desktop/ms737550(v=vs.85).aspx
#[cfg(windows)]
fn bind_to_multicast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    let nic_addr = if G_REPLAY.load(std::sync::atomic::Ordering::Relaxed) {
        &Ipv4Addr::UNSPECIFIED
    } else {
        nic_addr
    };

    socket.join_multicast_v4(addr.ip(), nic_addr)?;

    let socketaddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr))?;
    log::trace!("Binding multicast socket to {}", socketaddr);

    Ok(())
}

/// On unixes we bind to the multicast address, which causes multicast packets to be filtered
#[cfg(unix)]
fn bind_to_multicast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    // Linux is special, if we don't disable IP_MULTICAST_ALL the kernel forgets on
    // which device the multicast packet arrived and sends it to all sockets.
    #[cfg(target_os = "linux")]
    {
        use std::{io, mem, os::unix::io::AsRawFd};

        unsafe {
            let optval: libc::c_int = 0;
            let ret = libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_IP,
                libc::IP_MULTICAST_ALL,
                &optval as *const _ as *const libc::c_void,
                mem::size_of_val(&optval) as libc::socklen_t,
            );
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }

    socket.set_multicast_if_v4(nic_addr)?;

    let socketaddr = SocketAddr::new(IpAddr::V4(*addr.ip()), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr))?;

    socket.join_multicast_v4(addr.ip(), nic_addr)?;

    log::trace!(
        "Binding multicast socket to {} nic {}",
        socketaddr,
        nic_addr
    );

    Ok(())
}

/// On Windows, unlike all Unix variants, it is improper to bind to the multicast address
///
/// see https://msdn.microsoft.com/en-us/library/windows/desktop/ms737550(v=vs.85).aspx
#[cfg(windows)]
fn bind_to_broadcast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    let _ = socket.set_broadcast(true);
    let _ = addr; // Not used on Windows

    let socketaddr = SocketAddr::new(IpAddr::V4(*nic_addr), addr.port());

    socket.bind(&socket2::SockAddr::from(socketaddr))?;
    log::trace!("Binding broadcast socket to {}", socketaddr);
    Ok(())
}

/// On unixes we bind to the multicast address, which causes multicast packets to be filtered
#[cfg(unix)]
fn bind_to_broadcast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    let _ = socket.set_broadcast(true);
    let _ = nic_addr; // Not used on Linux

    // macOS and the BSDs refuse to bind 255.255.255.255; the wildcard
    // address receives limited broadcasts everywhere. Each interface gets its
    // own socket on that one port, which those systems only allow, and only
    // deliver the broadcast to all of, with SO_REUSEPORT.
    let bind_addr = if addr.ip().is_broadcast() {
        socket.set_reuse_port(true)?;
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, addr.port())
    } else {
        *addr
    };
    socket.bind(&socket2::SockAddr::from(bind_addr))?;
    log::trace!("Binding broadcast socket to {}", bind_addr);
    Ok(())
}

/// Socket type for `create_udp_listen`.
pub(crate) enum SocketType {
    /// Auto-detect from address: multicast if the IP is in a multicast
    /// range, broadcast if in a broadcast range, unicast otherwise.
    Any,
    /// Unicast/plain: bind to INADDR_ANY on the given port.
    #[allow(dead_code)]
    Unicast,
    /// Broadcast: set SO_BROADCAST and bind to the broadcast address.
    Broadcast,
    /// Multicast: join the multicast group on the given NIC.
    Multicast,
}

/// Create a `RadarSocket` for a listen address. If pcap replay is
/// active, returns a replay-backed socket. Otherwise creates a real
/// UDP socket bound according to `socket_type`.
pub(crate) fn create_udp_listen(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
    socket_type: SocketType,
) -> io::Result<crate::replay::RadarSocket> {
    if let Some(rx) = crate::replay::create_listen(addr) {
        return Ok(crate::replay::RadarSocket::Replay(rx));
    }

    let socket: socket2::Socket = new_socket()?;

    // Multicast is detectable from the address. Broadcast is not, because
    // `Ipv4Addr::is_broadcast` only matches 255.255.255.255, while many
    // radars (e.g. Furuno) use subnet-directed broadcasts like
    // 172.31.255.255 that look like unicast to the stdlib. Trust the
    // caller-supplied SocketType for broadcast vs unicast.
    debug_assert!(
        matches!(socket_type, SocketType::Any)
            || matches!(socket_type, SocketType::Multicast) == addr.ip().is_multicast(),
        "SocketType::Multicast mismatch for address {}",
        addr,
    );

    let effective = match socket_type {
        SocketType::Any if addr.ip().is_multicast() => SocketType::Multicast,
        SocketType::Any if addr.ip().is_broadcast() => SocketType::Broadcast,
        other => other,
    };

    match effective {
        SocketType::Multicast => bind_to_multicast(&socket, addr, nic_addr)?,
        SocketType::Broadcast => bind_to_broadcast(&socket, addr, nic_addr)?,
        SocketType::Unicast | SocketType::Any => {
            let socketaddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), addr.port());
            socket.bind(&socket2::SockAddr::from(socketaddr))?;
            log::trace!("Binding socket to {}", socketaddr);
        }
    }

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(crate::replay::RadarSocket::Udp(socket))
}

/// Create a unicast UDP socket bound to `nic_addr` on `port` and connected to
/// `peer`. A single connected socket both sends commands to the radar and
/// receives the radar's unicast replies on the same source port.
///
/// This exists for the Raymarine "MFD as WiFi AP" topology, where the radar
/// streams reports/spokes unicast back to the command source port. Sharing one
/// connected socket avoids a same-port collision between a separate listen and
/// command socket (the connected command socket would otherwise win delivery
/// of the replies, starving the listen socket).
pub(crate) fn create_connected_unicast(
    nic_addr: &Ipv4Addr,
    port: u16,
    peer: &SocketAddrV4,
) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;
    let bind_addr = SocketAddr::new(IpAddr::V4(*nic_addr), port);
    socket.bind(&socket2::SockAddr::from(bind_addr))?;
    socket.connect(&socket2::SockAddr::from(SocketAddr::V4(*peer)))?;
    log::debug!(
        "Binding unicast socket to {} connected to {}",
        bind_addr,
        peer
    );

    UdpSocket::from_std(socket.into())
}

/// A UDP socket connected to `addr`, sourced from `nic_addr`.
///
/// Takes a multicast group or a unicast peer alike — half the callers send to
/// a radar's own address — but only multicast is pinned to an interface.
///
/// Note that the local port is bound to the *destination's* port, not an
/// ephemeral one: the radars expect commands to arrive from the port they are
/// sent to, and reply there.
///
/// Multicast is pinned to `nic_addr`'s interface. Unicast cannot be: binding to
/// `nic_addr` fixes the source address but not the outgoing interface, and the
/// kernel still routes by destination. For a unicast peer this host has no
/// route to, `connect` fails here; for one reachable only by some other
/// interface it succeeds and the datagrams quietly go the wrong way — see
/// [`can_reach`].
pub(crate) fn create_connected_send(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    // Send multicast out of the radar's own interface. Without this the kernel
    // chooses one by route, which on a boat with both WiFi and Ethernet — or a
    // cellular dongle holding the default route — is usually not the radar's
    // network, and the commands leave the wrong way. Unicast is unaffected by
    // this option; it is set unconditionally because it costs nothing.
    socket.set_multicast_if_v4(nic_addr)?;

    let socketaddr = SocketAddr::new(IpAddr::V4(*addr.ip()), addr.port());
    let socketaddr_nic = SocketAddr::new(IpAddr::V4(*nic_addr), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr_nic))?;
    socket.connect(&socket2::SockAddr::from(socketaddr))?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub(crate) fn match_ipv4(addr: &Ipv4Addr, bcast: &Ipv4Addr, netmask: &Ipv4Addr) -> bool {
    let r = addr & netmask;
    let b = bcast & netmask;
    r == b
}

/// Whether a unicast command sent to `dst` would actually leave by the
/// interface the radar was discovered on.
///
/// Binding a socket to a source address does not pin the outgoing interface:
/// the kernel routes by destination, so a destination the routing table does
/// not send that way leaves via whichever interface it prefers — usually the
/// default route, and never the radar.
///
/// The kernel is asked rather than the netmask compared, because the two
/// answers differ exactly where it matters. A user who fixes this by adding an
/// address in the radar's range, or a host route to the radar, changes the
/// routing table but not the address mayara discovered the radar on; comparing
/// subnets would keep calling the radar unreachable after the problem is
/// solved. Connecting a UDP socket performs the route lookup and sends
/// nothing.
///
/// `None` when nothing can be concluded — the interface went away, or the
/// source address the kernel picked belongs to no interface we can see.
pub(crate) fn can_reach(nic_addr: &Ipv4Addr, dst: &SocketAddrV4) -> Option<bool> {
    let (ifname, _) = interface_for(nic_addr)?;

    let local = match routed_source(dst) {
        Ok(local) => local,
        Err(e) if is_unroutable(&e) => return Some(false),
        Err(_) => return None,
    };

    // The source address the kernel chose identifies the interface it would
    // send from. If that is not the radar's interface, the command would go
    // out of the wrong one and be lost.
    let (chosen, _) = interface_for(&local)?;

    Some(chosen == ifname)
}

/// The source address the kernel would send from to reach `dst`, found by
/// connecting a UDP socket, which looks up the route and sends nothing. `Err`
/// when there is no route at all (see [`is_unroutable`]), but also when the
/// probe socket cannot be created or queried.
fn routed_source(dst: &SocketAddrV4) -> io::Result<Ipv4Addr> {
    let probe = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    probe.connect(*dst)?;
    match probe.local_addr()? {
        SocketAddr::V4(local) => Ok(*local.ip()),
        SocketAddr::V6(_) => Err(io::Error::other("IPv4 socket has an IPv6 address")),
    }
}

/// Whether a [`routed_source`] error means the routing table has no way to
/// `dst`, rather than that the probe itself failed.
fn is_unroutable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NetworkUnreachable | io::ErrorKind::HostUnreachable
    )
}

/// Every IPv4 address this host has, or `None` when the interfaces cannot be
/// read.
pub(crate) fn host_ipv4_addresses() -> Option<HashSet<Ipv4Addr>> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};

    let addresses = NetworkInterface::show()
        .ok()?
        .into_iter()
        .flat_map(|itf| itf.addr)
        .filter_map(|addr| match addr.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .collect();
    Some(addresses)
}

/// Find the name and netmask of the interface carrying `nic_addr`.
///
/// `None` when no interface has that address, in which case nothing can be
/// concluded about reachability.
pub(crate) fn interface_for(nic_addr: &Ipv4Addr) -> Option<(String, Ipv4Addr)> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};

    for itf in NetworkInterface::show().ok()? {
        for addr in &itf.addr {
            if let (IpAddr::V4(ip), Some(IpAddr::V4(mask))) = (addr.ip(), addr.netmask())
                && ip == *nic_addr
            {
                return Some((itf.name.clone(), mask));
            }
        }
    }
    None
}

/// The address to use for a radar heard via `nic_addr` and commanded at
/// `radar_send`: an address on the radar's subnet on `nic_addr`'s own
/// interface, else the address the kernel routes the commands from if that is
/// on the radar's subnet, else `nic_addr` itself.
///
/// Multicast reaches every address of an interface alike, so which of them
/// heard the radar first is chance. Nor is the interface certain: macOS hands a
/// group's datagrams to a socket joined on any interface, so a radar on the
/// Ethernet can appear to be heard on a VM bridge. Only an address on the
/// radar's subnet is one the radar can be commanded from, and of those only
/// the routed one is sure to leave by the interface the commands take.
pub(crate) fn preferred_nic_addr(nic_addr: &Ipv4Addr, radar_send: &SocketAddrV4) -> Ipv4Addr {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};

    let Ok(interfaces) = NetworkInterface::show() else {
        return *nic_addr;
    };
    let interfaces = interfaces.into_iter().map(|itf| {
        itf.addr
            .iter()
            .filter_map(|addr| match (addr.ip(), addr.netmask()) {
                (IpAddr::V4(ip), Some(IpAddr::V4(mask))) => Some((ip, mask)),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    // Probe the command port itself: policy routing can select by port.
    let routed = routed_source(radar_send).ok();
    preferred_among(interfaces, nic_addr, radar_send.ip(), routed)
}

/// [`preferred_nic_addr`] over `interfaces`, each given as its (address,
/// netmask) pairs, with `routed` the source the kernel would use for the radar.
fn preferred_among(
    interfaces: impl IntoIterator<Item = Vec<(Ipv4Addr, Ipv4Addr)>>,
    nic_addr: &Ipv4Addr,
    radar: &Ipv4Addr,
    routed: Option<Ipv4Addr>,
) -> Ipv4Addr {
    let interfaces: Vec<_> = interfaces.into_iter().collect();
    let own = interfaces
        .iter()
        .find(|addrs| addrs.iter().any(|(ip, _)| ip == nic_addr));
    let Some(own) = own else {
        return *nic_addr;
    };
    let routed_on_subnet = routed.filter(|routed| {
        interfaces
            .iter()
            .flatten()
            .any(|(ip, mask)| ip == routed && match_ipv4(radar, ip, mask))
    });
    address_on_subnet(own, radar)
        .or(routed_on_subnet)
        .unwrap_or(*nic_addr)
}

/// The first of `addrs` (address, netmask) whose subnet contains `radar`.
fn address_on_subnet(addrs: &[(Ipv4Addr, Ipv4Addr)], radar: &Ipv4Addr) -> Option<Ipv4Addr> {
    addrs
        .iter()
        .find(|(ip, mask)| match_ipv4(radar, ip, mask))
        .map(|(ip, _)| *ip)
}

/// What an interface's link type means for radar discovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// A wired link. Radars are searched for here.
    Wired,
    /// A wireless link. Searched only when `--allow-wifi` is given.
    Wireless,
    /// A link that cannot carry radar traffic at all, such as a Bluetooth
    /// personal area network or a VPN tunnel. Never searched.
    ///
    /// Windows reads the link type outright; Linux and macOS infer it from
    /// the interface flags (a point-to-point link is no LAN).
    Unusable,
}

#[cfg(target_os = "macos")]
pub(crate) use macos::link_kind;
#[cfg(target_os = "macos")]
pub(crate) use macos::spawn_wait_for_ip_addr_change;

#[cfg(target_os = "linux")]
pub(crate) use linux::link_kind;
#[cfg(target_os = "linux")]
pub(crate) use linux::spawn_wait_for_ip_addr_change;

#[cfg(target_os = "windows")]
pub(crate) use windows::link_kind;
#[cfg(target_os = "windows")]
pub(crate) use windows::spawn_wait_for_ip_addr_change;

#[cfg(test)]
mod reachability_tests {
    use super::{can_reach, is_unroutable};
    use std::io;
    use std::net::{Ipv4Addr, SocketAddrV4};

    /// Only a missing route says the radar is unreachable; a probe that fails
    /// for any other reason says nothing about it.
    #[test]
    fn only_a_missing_route_counts_as_unroutable() {
        assert!(is_unroutable(&io::ErrorKind::NetworkUnreachable.into()));
        assert!(is_unroutable(&io::ErrorKind::HostUnreachable.into()));
        assert!(!is_unroutable(&io::ErrorKind::AddrInUse.into()));
        assert!(!is_unroutable(&io::Error::other("probe failed")));
    }

    /// Loopback is the only pair guaranteed to exist wherever the tests run.
    #[test]
    fn a_destination_on_our_own_interface_is_reachable() {
        assert_eq!(
            can_reach(
                &Ipv4Addr::LOCALHOST,
                &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 2573)
            ),
            Some(true)
        );
    }

    /// The case this exists for: the radar is not on the interface it was
    /// discovered on, so the command would leave by the default route (or by
    /// nothing at all) and never arrive. Either way it is not reachable
    /// *from loopback*, which is what is being asked.
    #[test]
    fn a_destination_reached_by_another_interface_is_not() {
        assert_eq!(
            can_reach(
                &Ipv4Addr::LOCALHOST,
                &SocketAddrV4::new(Ipv4Addr::new(198, 18, 1, 200), 2573)
            ),
            Some(false)
        );
    }

    /// Nothing can be said when we do not hold the address the radar was
    /// found on, so the caller must not act.
    #[test]
    fn an_address_we_do_not_hold_concludes_nothing() {
        assert_eq!(
            can_reach(
                &Ipv4Addr::new(192, 0, 2, 1),
                &SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 2), 2573)
            ),
            None
        );
    }
}

#[cfg(test)]
mod send_socket_tests {
    use super::{UdpSocket, create_connected_send};
    use std::io;
    use std::net::{Ipv4Addr, SocketAddrV4};

    /// The all-hosts group. Any multicast destination proves the point; this
    /// one is guaranteed to exist wherever the tests run.
    const ALL_HOSTS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 1);

    /// How many ports to try before giving up. Losing the race below once is
    /// plausible; losing it this many times in a row means something other
    /// than a collision is wrong.
    const PORT_ATTEMPTS: usize = 16;

    /// The socket under test, on whichever local port we can get.
    ///
    /// [`create_connected_send`] binds locally to the *destination's* port, so
    /// this needs a port to itself. A port cannot be reserved and handed over —
    /// asking the OS for a free one releases it again the moment we look at it
    /// — and a duplicate bind is refused even with `SO_REUSEADDR`. Tests run in
    /// parallel processes under nextest, so losing the race is reachable rather
    /// than theoretical: on a collision, simply try another port.
    fn a_send_socket_on_some_free_port() -> UdpSocket {
        for _ in 0..PORT_ATTEMPTS {
            let port = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .expect("a loopback port should be available")
                .local_addr()
                .expect("a bound socket has an address")
                .port();
            let dst = SocketAddrV4::new(ALL_HOSTS_GROUP, port);
            match create_connected_send(&dst, &Ipv4Addr::LOCALHOST) {
                Ok(sock) => return sock,
                // Somebody took the port between our look and our bind.
                Err(e) if e.kind() == io::ErrorKind::AddrInUse => continue,
                // Anything else is the failure the test exists to catch, so
                // say what it was rather than blame the port.
                Err(e) => panic!("send socket could not be created: {e}"),
            }
        }
        panic!("no local port stayed free after {PORT_ATTEMPTS} attempts");
    }

    /// Multicast commands must leave by the interface the radar was found on,
    /// not by whichever one happens to hold the default route.
    #[tokio::test]
    async fn a_send_socket_pins_multicast_to_the_given_interface() {
        let sock = a_send_socket_on_some_free_port();

        assert_eq!(
            socket2::SockRef::from(&sock).multicast_if_v4().unwrap(),
            Ipv4Addr::LOCALHOST
        );
    }
}

#[cfg(test)]
mod address_choice_tests {
    use super::{address_on_subnet, preferred_among, preferred_nic_addr};
    use std::net::{Ipv4Addr, SocketAddrV4};

    const RADAR: Ipv4Addr = Ipv4Addr::new(172, 16, 2, 0);
    const OTHER: (Ipv4Addr, Ipv4Addr) =
        (Ipv4Addr::new(10, 56, 0, 4), Ipv4Addr::new(255, 255, 255, 0));
    const ON_SUBNET: (Ipv4Addr, Ipv4Addr) = (
        Ipv4Addr::new(172, 16, 3, 150),
        Ipv4Addr::new(255, 240, 0, 0),
    );

    #[test]
    fn the_address_on_the_radars_subnet_is_chosen() {
        assert_eq!(
            address_on_subnet(&[OTHER, ON_SUBNET], &RADAR),
            Some(ON_SUBNET.0)
        );
    }

    #[test]
    fn no_address_on_the_radars_subnet_chooses_none() {
        assert_eq!(address_on_subnet(&[OTHER], &RADAR), None);
    }

    /// The radar was heard via `OTHER`; its interface also has `ON_SUBNET`.
    /// Another interface on the radar's subnet is not the one it was heard on.
    #[test]
    fn the_on_subnet_address_of_the_same_interface_is_chosen() {
        let elsewhere = (Ipv4Addr::new(172, 16, 9, 9), Ipv4Addr::new(255, 240, 0, 0));
        let interfaces = vec![vec![elsewhere], vec![OTHER, ON_SUBNET]];
        assert_eq!(
            preferred_among(interfaces, &OTHER.0, &RADAR, Some(elsewhere.0)),
            ON_SUBNET.0
        );
    }

    /// macOS can deliver a radar's multicast to a socket on another
    /// interface, so the routed on-subnet address beats an unreachable one.
    #[test]
    fn without_an_on_subnet_address_on_its_interface_the_routed_one_is_used() {
        let interfaces = vec![vec![ON_SUBNET], vec![OTHER]];
        assert_eq!(
            preferred_among(interfaces, &OTHER.0, &RADAR, Some(ON_SUBNET.0)),
            ON_SUBNET.0
        );
    }

    /// A VM bridge or VPN overlapping the radar's subnet does not win over
    /// the interface the kernel routes the radar's traffic by.
    #[test]
    fn of_several_on_subnet_addresses_elsewhere_the_routed_one_is_used() {
        let bridge = (Ipv4Addr::new(172, 16, 9, 9), Ipv4Addr::new(255, 240, 0, 0));
        let interfaces = vec![vec![bridge], vec![ON_SUBNET], vec![OTHER]];
        assert_eq!(
            preferred_among(interfaces, &OTHER.0, &RADAR, Some(ON_SUBNET.0)),
            ON_SUBNET.0
        );
    }

    /// A route through a gateway gives a source the radar cannot answer
    /// either, so it is no better than the address the radar was heard on.
    #[test]
    fn a_routed_address_off_the_radars_subnet_is_not_used() {
        let gateway_side = (
            Ipv4Addr::new(192, 168, 8, 9),
            Ipv4Addr::new(255, 255, 255, 0),
        );
        let interfaces = vec![vec![gateway_side], vec![OTHER]];
        assert_eq!(
            preferred_among(interfaces, &OTHER.0, &RADAR, Some(gateway_side.0)),
            OTHER.0
        );
    }

    #[test]
    fn with_no_on_subnet_address_anywhere_the_address_is_kept() {
        let interfaces = vec![vec![OTHER]];
        assert_eq!(preferred_among(interfaces, &OTHER.0, &RADAR, None), OTHER.0);
    }

    /// An address that belongs to no interface, as in a replay, is kept.
    #[test]
    fn an_unknown_address_is_kept() {
        let made_up = Ipv4Addr::new(203, 0, 113, 7);
        let radar_send = SocketAddrV4::new(RADAR, 50101);
        assert_eq!(preferred_nic_addr(&made_up, &radar_send), made_up);
    }
}
