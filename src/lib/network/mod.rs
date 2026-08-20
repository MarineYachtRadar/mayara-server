use serde::Deserialize;
use socket2::{Domain, Protocol, Type};
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
// This is like a SocketAddrV4 but with known layout
#[derive(Deserialize, Copy, Clone)]
#[repr(C)]
pub(crate) struct NetworkSocketAddrV4 {
    addr: [u8; 4],
    port: [u8; 2],
}

impl From<NetworkSocketAddrV4> for SocketAddrV4 {
    fn from(item: NetworkSocketAddrV4) -> Self {
        SocketAddrV4::new(
            u32::from_be_bytes(item.addr).into(),
            u16::from_be_bytes(item.port),
        )
    }
}

impl std::fmt::Display for NetworkSocketAddrV4 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            Ipv4Addr::from(u32::from_be_bytes(self.addr)),
            u16::from_be_bytes(self.port)
        )
    }
}

impl fmt::Debug for NetworkSocketAddrV4 {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("NetworkSocketAddrV4")
            .field("addr", &self.addr)
            .field("port", &format_args!("{}", u16::from_be_bytes(self.port)))
            .finish()
    }
}

#[derive(Deserialize, Copy, Clone)]
#[repr(C)]
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

    socket.bind(&socket2::SockAddr::from(*addr))?;
    log::trace!("Binding broadcast socket to {}", *addr);
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

pub(crate) fn create_multicast_send(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

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

/// True when a unicast destination cannot be reached directly on the wire the
/// interface `nic`/`netmask` sits on.
///
/// Binding a socket to a source address does not pin the outgoing interface:
/// the kernel routes by destination, so an off-link destination silently
/// leaves via whichever interface the routing table prefers — usually the
/// default route, and never the radar. Multicast and broadcast are exempt;
/// they are delivered on the wire regardless of subnet.
pub(crate) fn is_offlink(nic: &Ipv4Addr, netmask: &Ipv4Addr, dst: &Ipv4Addr) -> bool {
    !dst.is_multicast() && !dst.is_broadcast() && !match_ipv4(dst, nic, netmask)
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

#[cfg(target_os = "macos")]
pub(crate) use macos::is_wireless_interface;
#[cfg(target_os = "macos")]
pub(crate) use macos::spawn_wait_for_ip_addr_change;

#[cfg(target_os = "linux")]
pub(crate) use linux::is_wireless_interface;
#[cfg(target_os = "linux")]
pub(crate) use linux::spawn_wait_for_ip_addr_change;

#[cfg(target_os = "windows")]
pub(crate) use windows::is_wireless_interface;
#[cfg(target_os = "windows")]
pub(crate) use windows::spawn_wait_for_ip_addr_change;

#[cfg(test)]
mod reachability_tests {
    use super::is_offlink;
    use std::net::Ipv4Addr;

    #[test]
    fn rd_radar_on_another_subnet_is_offlink() {
        // Wire-observed: RD radomes are addressed in 10/8 while a host on the
        // RayNet network gets 198.18.x from the MFD's DHCP server. Commands
        // are unicast, so they never reach the radar.
        assert!(is_offlink(
            &Ipv4Addr::new(198, 18, 1, 200),
            &Ipv4Addr::new(255, 255, 0, 0),
            &Ipv4Addr::new(10, 18, 203, 88)
        ));
    }

    #[test]
    fn quantum_on_the_same_subnet_is_reachable() {
        // A Quantum is addressed in 198.18.x like the host, which is why
        // Quantum control has always worked.
        assert!(!is_offlink(
            &Ipv4Addr::new(198, 18, 7, 45),
            &Ipv4Addr::new(255, 255, 0, 0),
            &Ipv4Addr::new(198, 18, 0, 158)
        ));
    }

    #[test]
    fn multicast_and_broadcast_are_never_offlink() {
        let nic = Ipv4Addr::new(198, 18, 1, 200);
        let mask = Ipv4Addr::new(255, 255, 0, 0);
        assert!(!is_offlink(&nic, &mask, &Ipv4Addr::new(232, 1, 1, 1)));
        assert!(!is_offlink(&nic, &mask, &Ipv4Addr::new(255, 255, 255, 255)));
    }
}
