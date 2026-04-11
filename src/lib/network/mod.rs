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

static G_REPLAY: AtomicBool = AtomicBool::new(false);

pub fn set_replay(replay: bool) {
    G_REPLAY.store(replay, std::sync::atomic::Ordering::Relaxed);
}
// This is like a SocketAddrV4 but with known layout
#[derive(Deserialize, Copy, Clone)]
#[repr(C)]
pub struct NetworkSocketAddrV4 {
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
pub struct LittleEndianSocketAddrV4 {
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
pub fn new_socket() -> io::Result<socket2::Socket> {
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
pub enum SocketType {
    /// Auto-detect from address: multicast if the IP is in a multicast
    /// range, broadcast if in a broadcast range, unicast otherwise.
    Any,
    /// Unicast/plain: bind to INADDR_ANY on the given port.
    Unicast,
    /// Broadcast: set SO_BROADCAST and bind to the broadcast address.
    Broadcast,
    /// Multicast: join the multicast group on the given NIC.
    Multicast,
}

/// Create a `RadarSocket` for a listen address. If pcap replay is
/// active, returns a replay-backed socket. Otherwise creates a real
/// UDP socket bound according to `socket_type`.
pub fn create_udp_listen(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
    socket_type: SocketType,
) -> io::Result<crate::replay::RadarSocket> {
    if let Some(rx) = crate::replay::create_listen(addr) {
        return Ok(crate::replay::RadarSocket::Replay(rx));
    }

    let socket: socket2::Socket = new_socket()?;

    debug_assert!(
        matches!(socket_type, SocketType::Any)
            || matches!(socket_type, SocketType::Multicast) == addr.ip().is_multicast(),
        "SocketType::Multicast mismatch for address {}",
        addr,
    );
    debug_assert!(
        matches!(socket_type, SocketType::Any | SocketType::Multicast)
            || matches!(socket_type, SocketType::Broadcast) == addr.ip().is_broadcast(),
        "SocketType::Broadcast mismatch for address {}",
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

pub fn create_multicast_send(addr: &SocketAddrV4, nic_addr: &Ipv4Addr) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    let socketaddr = SocketAddr::new(IpAddr::V4(*addr.ip()), addr.port());
    let socketaddr_nic = SocketAddr::new(IpAddr::V4(*nic_addr), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr_nic))?;
    socket.connect(&socket2::SockAddr::from(socketaddr))?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub fn match_ipv4(addr: &Ipv4Addr, bcast: &Ipv4Addr, netmask: &Ipv4Addr) -> bool {
    let r = addr & netmask;
    let b = bcast & netmask;
    r == b
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
