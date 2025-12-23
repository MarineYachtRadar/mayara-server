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
    #[cfg(target_os = "macos")]
    let nic_addr = if G_REPLAY.load(std::sync::atomic::Ordering::Relaxed) {
        &Ipv4Addr::UNSPECIFIED
    } else {
        nic_addr
    };

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

    // Bind to the multicast address itself for proper packet filtering on Unix.
    // This differs from WASM which must use 0.0.0.0 due to Node.js dgram limitations.
    // The IP_MULTICAST_ALL=0 above ensures we only receive packets for groups we've
    // joined on this specific interface.
    let socketaddr = SocketAddr::new(IpAddr::V4(*addr.ip()), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr))?;

    socket.join_multicast_v4(addr.ip(), nic_addr)?;

    log::trace!(
        "Binding multicast socket to {} for multicast group {} nic {}",
        socketaddr,
        addr.ip(),
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

pub fn create_udp_multicast_listen(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    socket.set_reuse_address(true)?;

    bind_to_multicast(&socket, addr, nic_addr)?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub fn create_udp_listen(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
    no_broadcast: bool,
) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    if addr.ip().is_multicast() {
        bind_to_multicast(&socket, addr, nic_addr)?;
    } else if !no_broadcast {
        bind_to_broadcast(&socket, addr, nic_addr)?;
    } else {
        let socketaddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), addr.port());

        socket.bind(&socket2::SockAddr::from(socketaddr))?;
        log::trace!("Binding socket to {}", socketaddr);
    }

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
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

/// Check if an IP address is in the link-local range (169.254.0.0/16)
fn is_link_local(ip: &Ipv4Addr) -> bool {
    ip.octets()[0] == 169 && ip.octets()[1] == 254
}

/// Find the NIC address that can reach a given radar IP.
///
/// Returns the first interface IP that matches the radar's subnet.
/// For link-local addresses (169.254.x.x), prefers the Furuno subnet (172.31.x.x).
/// Falls back to the first non-loopback, non-wireless interface if no match is found.
pub fn find_nic_for_radar(radar_ip: &Ipv4Addr) -> Option<Ipv4Addr> {
    use network_interface::{NetworkInterface, NetworkInterfaceConfig};

    let interfaces = NetworkInterface::show().ok()?;

    // First pass: find an interface on the same subnet
    for itf in &interfaces {
        for addr in &itf.addr {
            if let (IpAddr::V4(nic_ip), Some(IpAddr::V4(netmask))) = (addr.ip(), addr.netmask()) {
                if !nic_ip.is_loopback() && match_ipv4(&nic_ip, radar_ip, &netmask) {
                    log::debug!("Found NIC {} ({}) for radar {}", itf.name, nic_ip, radar_ip);
                    return Some(nic_ip);
                }
            }
        }
    }

    // Special case: link-local addresses (169.254.x.x)
    // These are typically used by Navico radars and are reachable from any ethernet interface.
    // Prefer the Furuno/Navico subnet (172.31.x.x) if available, as it's the dedicated radar network.
    if is_link_local(radar_ip) {
        // Look for the 172.31.x.x interface (Furuno/Navico subnet)
        for itf in &interfaces {
            for addr in &itf.addr {
                if let IpAddr::V4(nic_ip) = addr.ip() {
                    if nic_ip.octets()[0] == 172 && nic_ip.octets()[1] == 31 {
                        log::debug!(
                            "Using Furuno/Navico NIC {} ({}) for link-local radar {}",
                            itf.name,
                            nic_ip,
                            radar_ip
                        );
                        return Some(nic_ip);
                    }
                }
            }
        }

        // Link-local fallback: prefer wired ethernet interfaces (name starts with 'en' or 'eth')
        for itf in &interfaces {
            if itf.name.starts_with("en") || itf.name.starts_with("eth") {
                for addr in &itf.addr {
                    if let IpAddr::V4(nic_ip) = addr.ip() {
                        if !nic_ip.is_loopback() {
                            log::debug!(
                                "Using wired NIC {} ({}) for link-local radar {}",
                                itf.name,
                                nic_ip,
                                radar_ip
                            );
                            return Some(nic_ip);
                        }
                    }
                }
            }
        }
    }

    // Final fallback: return first non-loopback interface
    for itf in &interfaces {
        for addr in &itf.addr {
            if let IpAddr::V4(nic_ip) = addr.ip() {
                if !nic_ip.is_loopback() {
                    log::debug!(
                        "Fallback NIC {} ({}) for radar {} (no subnet match)",
                        itf.name,
                        nic_ip,
                        radar_ip
                    );
                    return Some(nic_ip);
                }
            }
        }
    }

    None
}

// deprecated_marked_for_delete: All platform-specific re-exports below are dead code
// Only used by legacy locator which has been removed. Will be deleted in a future cleanup.

// #[cfg(target_os = "macos")]
// pub(crate) use macos::is_wireless_interface;
// #[cfg(target_os = "macos")]
// pub(crate) use macos::wait_for_ip_addr_change;

// #[cfg(target_os = "linux")]
// pub(crate) use linux::is_wireless_interface;
// #[cfg(target_os = "linux")]
// pub(crate) use linux::wait_for_ip_addr_change;

// #[cfg(target_os = "windows")]
// pub(crate) use windows::is_wireless_interface;
// #[cfg(target_os = "windows")]
// pub(crate) use windows::wait_for_ip_addr_change;
