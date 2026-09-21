use crate::network::LinkKind;
use crate::radar::RadarError;

use futures::stream::StreamExt;
use libc::{RTM_DELADDR, RTM_NEWADDR};
use netlink_sys::{AsyncSocket, SocketAddr};
use rtnetlink::new_connection;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const RTNLGRP_IPV4_IFADDR: u32 = 5;

const fn nl_mgrp(group: u32) -> u32 {
    if group > 31 {
        panic!("use netlink_sys::Socket::add_membership() for this group");
    }
    if group == 0 { 0 } else { 1 << (group - 1) }
}

pub async fn spawn_wait_for_ip_addr_change(
    cancel_token: CancellationToken,
    tx_ip_change: broadcast::Sender<()>,
) {
    tokio::spawn(wait_for_ip_addr_change(cancel_token, tx_ip_change));
}

/// Waits asynchronously for an IPv4 address change on Linux.
/// Completes when the cancellation token is triggered.
/// Sends an empty message on tx_ip_change every time a change is detected
///
async fn wait_for_ip_addr_change(
    cancel_token: CancellationToken,
    tx_ip_change: broadcast::Sender<()>,
) -> Result<(), RadarError> {
    let (mut conn, mut _handle, mut messages) = new_connection().map_err(RadarError::Io)?;

    // These flags specify what kinds of broadcast messages we want to listen
    // for.
    let groups = nl_mgrp(RTNLGRP_IPV4_IFADDR);

    let addr = SocketAddr::new(0, groups);
    conn.socket_mut()
        .socket_mut()
        .bind(&addr)
        .expect("Failed to bind");

    // Spawn `Connection` to start polling netlink socket.
    tokio::spawn(conn);

    log::trace!("Waiting for IP address change");
    loop {
        tokio::select! {
            // Check for cancellation
            _ = cancel_token.cancelled() => {
                log::trace!("Shutdown requested");
                return Ok(());
            }

            // Wait for messages on the socket
            result = messages.next() => {
                if let Some((message, _)) = result {
                    if message.header.message_type == RTM_NEWADDR || message.header.message_type == RTM_DELADDR{
                        log::trace!("Received IP address change");
                        let _ = tx_ip_change.send(());
                    }
                    else {
                        log::trace!("Received message_type {}", message.header.message_type);
                    }
                } else {
                    log::error!("Failed to receive message");
                    return Err(RadarError::Io(std::io::Error::other(
                        "Failed to receive message",
                    )));
                }
            }
        }
    }
}

/// Classify an interface by the link technology behind it.
pub fn link_kind(interface_name: &str) -> LinkKind {
    if let Some(flags) = interface_flags(interface_name)
        && !can_carry_radar_traffic(flags)
    {
        LinkKind::Unusable
    } else if is_wireless_interface(interface_name) {
        LinkKind::Wireless
    } else {
        LinkKind::Wired
    }
}

/// A point-to-point link -- a tun VPN such as tinc, OpenVPN or WireGuard, a
/// PPP dial-up, an IP tunnel -- is not a LAN segment: it has no broadcast
/// address and no radar on the other end. Binding a multicast sender to one
/// fails with `EADDRNOTAVAIL`, so before this check every beacon round logged
/// a screenful of warnings per tunnel, and a radar was hunted for down the
/// tunnels as if they were the boat's network. A bridged tap VPN keeps its
/// broadcast flag and stays usable, which is right: it does carry the LAN.
///
/// Loopback has no broadcast address either, but it stays usable: the locator
/// searches it when `--interface` names it, which is how a capture is replayed.
fn can_carry_radar_traffic(flags: libc::c_short) -> bool {
    let flags = flags as libc::c_int;
    flags & libc::IFF_POINTOPOINT == 0 && flags & (libc::IFF_BROADCAST | libc::IFF_LOOPBACK) != 0
}

/// The interface flags, or `None` when the interface has just gone away.
fn interface_flags(interface_name: &str) -> Option<libc::c_short> {
    use libc::{AF_INET, Ioctl, c_void, ifreq, ioctl, strncpy};

    // musl types the request as c_int and glibc as c_ulong; the constant is
    // c_ulong in both, so cast it to whatever this target's ioctl() wants.
    const SIOCGIFFLAGS: Ioctl = libc::SIOCGIFFLAGS as Ioctl;
    use std::ffi::CString;

    let socket_fd = unsafe { libc::socket(AF_INET, libc::SOCK_DGRAM, 0) };
    if socket_fd < 0 {
        return None;
    }

    let mut ifr = unsafe { std::mem::zeroed::<ifreq>() };
    let iface_cstring = CString::new(interface_name).ok()?;
    unsafe {
        strncpy(
            ifr.ifr_name.as_mut_ptr(),
            iface_cstring.as_ptr(),
            ifr.ifr_name.len(),
        );
    }

    let res = unsafe { ioctl(socket_fd, SIOCGIFFLAGS, &mut ifr as *mut _ as *mut c_void) };
    unsafe { libc::close(socket_fd) };

    if res == 0 {
        Some(unsafe { ifr.ifr_ifru.ifru_flags })
    } else {
        None
    }
}

fn is_wireless_interface(interface_name: &str) -> bool {
    use libc::{AF_INET, Ioctl, c_void, ifreq, ioctl, strncpy};
    use std::ffi::CString;

    const SIOCGIWNAME: Ioctl = 0x8B01; // Wireless Extensions request to get interface name

    // Open a socket for ioctl operations
    let socket_fd = unsafe { libc::socket(AF_INET, libc::SOCK_DGRAM, 0) };
    if socket_fd < 0 {
        return false;
    }

    // Prepare the interface request structure
    let mut ifr = unsafe { std::mem::zeroed::<ifreq>() };
    let iface_cstring = CString::new(interface_name).expect("Invalid interface name");
    unsafe {
        strncpy(
            ifr.ifr_name.as_mut_ptr(),
            iface_cstring.as_ptr(),
            ifr.ifr_name.len(),
        );
    }

    // Perform the ioctl call
    let res = unsafe { ioctl(socket_fd, SIOCGIWNAME, &mut ifr as *mut _ as *mut c_void) };

    // Close the socket
    unsafe { libc::close(socket_fd) };

    match res {
        0 => true, // The interface supports wireless extensions
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Flag words as read from a Raspberry Pi running tinc.
    const LAN_BRIDGE_FLAGS: libc::c_short = 0x1003;
    const ETHERNET_FLAGS: libc::c_short = 0x1303;
    const TINC_TUN_FLAGS: libc::c_short = 0x1091;
    const LOOPBACK_FLAGS: libc::c_short = 0x49;

    #[test]
    fn a_lan_carries_radar_traffic_and_a_tunnel_does_not() {
        assert!(can_carry_radar_traffic(LAN_BRIDGE_FLAGS));
        assert!(can_carry_radar_traffic(ETHERNET_FLAGS));
        assert!(!can_carry_radar_traffic(TINC_TUN_FLAGS));
    }

    /// The locator decides about loopback itself: it is searched only when
    /// `--interface` names it, to replay a capture.
    #[test]
    fn loopback_carries_replayed_radar_traffic() {
        assert!(can_carry_radar_traffic(LOOPBACK_FLAGS));
    }

    #[test]
    fn an_interface_that_does_not_exist_has_no_flags() {
        assert_eq!(interface_flags("nosuchif0"), None);
    }

    #[test]
    fn loopback_is_classified_usable() {
        assert!(matches!(link_kind("lo"), LinkKind::Wired));
    }
}
