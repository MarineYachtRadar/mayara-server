// deprecated_marked_for_delete: This entire module is dead code - only used by legacy locator
// which has been removed. Will be deleted in a future cleanup.

/*
use crate::radar::RadarError;

use futures::stream::StreamExt;
use libc::{RTM_DELADDR, RTM_NEWADDR};
use netlink_sys::{AsyncSocket, SocketAddr};
use rtnetlink::new_connection;
use tokio_util::sync::CancellationToken;

const RTNLGRP_IPV4_IFADDR: u32 = 5;

const fn nl_mgrp(group: u32) -> u32 {
    if group > 31 {
        panic!("use netlink_sys::Socket::add_membership() for this group");
    }
    if group == 0 {
        0
    } else {
        1 << (group - 1)
    }
}

/// Waits asynchronously for an IPv4 address change on Linux.
/// Completes when an address change is detected or the cancellation token is triggered.
pub async fn wait_for_ip_addr_change(cancel_token: CancellationToken) -> Result<(), RadarError> {
    let (mut conn, mut _handle, mut messages) = new_connection().map_err(|e| RadarError::Io(e))?;

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
                        return Ok(());
                    }
                    else {
                        log::trace!("Received message_type {}", message.header.message_type);
                    }
                } else {
                    log::error!("Failed to receive message");
                    return Err(RadarError::Io(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Failed to receive message",
                    )));
                }
            }
        }
    }
}

pub fn is_wireless_interface(interface_name: &str) -> bool {
    use libc::{c_void, ifreq, ioctl, strncpy, Ioctl, AF_INET};
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
*/
